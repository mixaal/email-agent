use anyhow::{Context, Result};
use oauth2::{
    AuthUrl, ClientId, ClientSecret, CsrfToken, RedirectUrl, Scope, TokenUrl,
    basic::BasicClient,
    reqwest::async_http_client,
    TokenResponse,
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

// ─── Token cache ─────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone)]
pub struct CachedToken {
    pub access_token: String,
    pub refresh_token: Option<String>,
}

fn token_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("email-tool")
        .join("token.json")
}

pub fn load_cached_token() -> Option<CachedToken> {
    let path = token_path();
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
}

pub fn save_token(token: &CachedToken) -> Result<()> {
    let path = token_path();
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(&path, serde_json::to_string_pretty(token)?)?;
    Ok(())
}

// ─── OAuth2 installed-app flow (localhost:8080 redirect) ─────────────────────

pub async fn authenticate(client_id: &str, client_secret: &str) -> Result<String> {
    // Zkusíme cached token
    if let Some(cached) = load_cached_token() {
        let http = Client::new();
        let resp = http
            .get("https://gmail.googleapis.com/gmail/v1/users/me/profile")
            .bearer_auth(&cached.access_token)
            .send()
            .await?;
        if resp.status().is_success() {
            return Ok(cached.access_token);
        }
        if let Some(refresh_token) = &cached.refresh_token {
            if let Ok(new_access) =
                refresh_access_token(client_id, client_secret, refresh_token).await
            {
                let _ = save_token(&CachedToken {
                    access_token: new_access.clone(),
                    refresh_token: Some(refresh_token.clone()),
                });
                return Ok(new_access);
            }
        }
    }

    // Listener na localhost:8080 pro OAuth2 redirect
    let listener = TcpListener::bind("127.0.0.1:8080")
        .await
        .context("Cannot bind 127.0.0.1:8080 — port already in use?")?;

    let oauth_client = BasicClient::new(
        ClientId::new(client_id.to_string()),
        Some(ClientSecret::new(client_secret.to_string())),
        AuthUrl::new("https://accounts.google.com/o/oauth2/v2/auth".to_string())?,
        Some(TokenUrl::new("https://oauth2.googleapis.com/token".to_string())?),
    )
    .set_redirect_uri(RedirectUrl::new("http://127.0.0.1:8080".to_string())?);

    let (auth_url, csrf_token) = oauth_client
        .authorize_url(CsrfToken::new_random)
        .add_scope(Scope::new(
            "https://www.googleapis.com/auth/gmail.modify".to_string(),
        ))
        .add_extra_param("access_type", "offline")
        .add_extra_param("prompt", "consent")
        .url();

    eprintln!(
        "\n=== Gmail Authorization ===\nOtevři tento URL v browseru:\n\n  {}\n\nČekám na redirect...\n",
        auth_url
    );

    // Čekáme na HTTP request od browseru po redirectu
    let (mut stream, _) = listener.accept().await?;
    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf).await?;
    let request = String::from_utf8_lossy(&buf[..n]);

    // Odpovíme browseru
    stream
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\r\n\
              <html><body><h2>email-tool: autorizace OK, muzes zavrit okno.</h2></body></html>",
        )
        .await?;

    // Parsujeme code + state z GET /?code=...&state=... HTTP/1.1
    let path = request
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .nth(1)
        .unwrap_or("/?");

    let parsed = url::Url::parse(&format!("http://localhost{}", path))
        .context("failed to parse redirect URL")?;

    let mut code = None;
    let mut state = None;
    for (k, v) in parsed.query_pairs() {
        match k.as_ref() {
            "code"  => code  = Some(v.to_string()),
            "state" => state = Some(v.to_string()),
            "error" => return Err(anyhow::anyhow!("Google OAuth error: {v}")),
            _ => {}
        }
    }

    let code  = code.context("no code in redirect")?;
    let state = state.unwrap_or_default();

    if state != *csrf_token.secret() {
        return Err(anyhow::anyhow!("CSRF token mismatch"));
    }

    // Vyměníme code za access + refresh token
    let token = oauth_client
        .exchange_code(oauth2::AuthorizationCode::new(code))
        .request_async(async_http_client)
        .await
        .map_err(|e| anyhow::anyhow!("token exchange failed: {e}"))?;

    let access  = token.access_token().secret().clone();
    let refresh = token.refresh_token().map(|t| t.secret().clone());

    let _ = save_token(&CachedToken {
        access_token:  access.clone(),
        refresh_token: refresh,
    });

    eprintln!("Token uložen do ~/.config/email-tool/token.json\n");
    Ok(access)
}

// ─── Token refresh ────────────────────────────────────────────────────────────

async fn refresh_access_token(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
) -> Result<String> {
    let http = Client::new();
    let params = [
        ("client_id", client_id),
        ("client_secret", client_secret),
        ("refresh_token", refresh_token),
        ("grant_type", "refresh_token"),
    ];
    let resp: serde_json::Value = http
        .post("https://oauth2.googleapis.com/token")
        .form(&params)
        .send()
        .await?
        .json()
        .await?;

    resp["access_token"]
        .as_str()
        .map(|s| s.to_string())
        .context("no access_token in refresh response")
}

// ─── Gmail REST types ─────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct MessageListResponse {
    pub messages: Option<Vec<MessageRef>>,
    #[serde(rename = "nextPageToken")]
    pub next_page_token: Option<String>,
    #[serde(rename = "resultSizeEstimate")]
    pub result_size_estimate: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct MessageRef {
    pub id: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Message {
    pub id: String,
    #[serde(rename = "labelIds")]
    pub label_ids: Option<Vec<String>>,
    pub snippet: Option<String>,
    pub payload: Option<MessagePayload>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct MessagePayload {
    pub headers: Option<Vec<Header>>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Header {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Label {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub label_type: Option<String>,
    #[serde(rename = "messagesTotal")]
    pub messages_total: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct LabelListResponse {
    pub labels: Option<Vec<Label>>,
}

// ─── Gmail API client ─────────────────────────────────────────────────────────

pub struct GmailClient {
    http: Client,
    pub access_token: String,
}

impl GmailClient {
    pub fn new(access_token: String) -> Self {
        Self { http: Client::new(), access_token }
    }

    pub fn access_token(&self) -> &str {
        &self.access_token
    }

    pub async fn list_labels(&self) -> Result<Vec<Label>> {
        let resp: LabelListResponse = self
            .http
            .get("https://gmail.googleapis.com/gmail/v1/users/me/labels")
            .bearer_auth(&self.access_token)
            .send()
            .await?
            .json()
            .await?;
        Ok(resp.labels.unwrap_or_default())
    }

    pub async fn list_messages(
        &self,
        max_results: u32,
        page_token: Option<&str>,
        label_ids: Option<&[&str]>,
    ) -> Result<MessageListResponse> {
        let mut req = self
            .http
            .get("https://gmail.googleapis.com/gmail/v1/users/me/messages")
            .bearer_auth(&self.access_token)
            .query(&[("maxResults", max_results.to_string())]);
        if let Some(pt) = page_token {
            req = req.query(&[("pageToken", pt)]);
        }
        if let Some(labels) = label_ids {
            for lbl in labels {
                req = req.query(&[("labelIds", lbl)]);
            }
        }
        Ok(req.send().await?.json().await?)
    }

    pub async fn search_messages(&self, query: &str, max_results: u32, page_token: Option<&str>) -> Result<MessageListResponse> {
        let mut req = self
            .http
            .get("https://gmail.googleapis.com/gmail/v1/users/me/messages")
            .bearer_auth(&self.access_token)
            .query(&[("q", query), ("maxResults", &max_results.to_string())]);
        if let Some(pt) = page_token {
            req = req.query(&[("pageToken", pt)]);
        }
        Ok(req.send().await?.json().await?)
    }

    pub async fn create_label(&self, name: &str) -> Result<Label> {
        let body = serde_json::json!({ "name": name });
        let resp = self
            .http
            .post("https://gmail.googleapis.com/gmail/v1/users/me/labels")
            .bearer_auth(&self.access_token)
            .json(&body)
            .send()
            .await?;
        // 409 = already exists — fetch existing instead
        if resp.status() == 409 {
            let labels = self.list_labels().await?;
            return labels
                .into_iter()
                .find(|l| l.name == name)
                .ok_or_else(|| anyhow::anyhow!("Label '{}' already exists but not found", name));
        }
        Ok(resp.json().await?)
    }

    pub async fn apply_label(&self, message_id: &str, label_id: &str) -> Result<()> {
        let body = serde_json::json!({
            "addLabelIds": [label_id],
            "removeLabelIds": []
        });
        self.http
            .post(format!(
                "https://gmail.googleapis.com/gmail/v1/users/me/messages/{}/modify",
                message_id
            ))
            .bearer_auth(&self.access_token)
            .json(&body)
            .send()
            .await?;
        Ok(())
    }

    pub async fn remove_label(&self, message_id: &str, label_id: &str) -> Result<()> {
        let body = serde_json::json!({
            "addLabelIds": [],
            "removeLabelIds": [label_id]
        });
        self.http
            .post(format!(
                "https://gmail.googleapis.com/gmail/v1/users/me/messages/{}/modify",
                message_id
            ))
            .bearer_auth(&self.access_token)
            .json(&body)
            .send()
            .await?;
        Ok(())
    }

    pub async fn get_message(&self, id: &str) -> Result<Message> {
        Ok(self
            .http
            .get(format!(
                "https://gmail.googleapis.com/gmail/v1/users/me/messages/{}",
                id
            ))
            .bearer_auth(&self.access_token)
            .query(&[
                ("format", "metadata"),
                ("metadataHeaders", "From"),
                ("metadataHeaders", "Subject"),
            ])
            .send()
            .await?
            .json()
            .await?)
    }
}
