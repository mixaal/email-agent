mod gmail;

use anyhow::Result;
use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::HashMap, env, sync::Arc};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::RwLock,
};

use gmail::GmailClient;

// ─── JSON-RPC types ───────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct Request {
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    params: Option<Value>,
}

#[derive(Serialize)]
struct Response {
    jsonrpc: &'static str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

#[derive(Serialize)]
struct RpcError {
    code: i32,
    message: String,
}

impl Response {
    fn ok(id: Value, result: Value) -> Self {
        Self { jsonrpc: "2.0", id, result: Some(result), error: None }
    }
    fn err(id: Value, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(RpcError { code, message: message.into() }),
        }
    }
}

// ─── Server state ─────────────────────────────────────────────────────────────

#[derive(Clone)]
struct State {
    client: Arc<RwLock<Option<GmailClient>>>,
}

impl State {
    fn new() -> Self {
        Self { client: Arc::new(RwLock::new(None)) }
    }

    async fn ensure_auth(&self) -> Result<()> {
        if self.client.read().await.is_some() {
            return Ok(());
        }
        let client_id = env::var("GMAIL_CLIENT_ID")
            .map_err(|_| anyhow::anyhow!("GMAIL_CLIENT_ID env var not set"))?;
        let client_secret = env::var("GMAIL_CLIENT_SECRET")
            .map_err(|_| anyhow::anyhow!("GMAIL_CLIENT_SECRET env var not set"))?;
        let token = gmail::authenticate(&client_id, &client_secret).await?;
        *self.client.write().await = Some(GmailClient::new(token));
        Ok(())
    }
}

// ─── Tool definitions ─────────────────────────────────────────────────────────

fn tools_list() -> Value {
    json!({
        "tools": [
            {
                "name": "list_labels",
                "description": "List all Gmail labels (system + user-defined) with message counts.",
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                    "required": []
                }
            },
            {
                "name": "analyze_labels",
                "description": "Sample random emails and build a label frequency + co-occurrence report.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "sample_size": {
                            "type": "integer",
                            "description": "Number of emails to sample (1–2000, default 500)"
                        },
                        "label_filter": {
                            "type": "string",
                            "description": "Optional label ID to restrict sampling to (e.g. INBOX)"
                        }
                    },
                    "required": []
                }
            },
            {
                "name": "create_label",
                "description": "Create a new Gmail label. Use '/' for nesting, e.g. 'Bydleni/CimickyHaj'.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "Label name, e.g. 'Finance/CSOB' or 'Nakupy/Alza'"
                        }
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "apply_label",
                "description": "Apply a label to one or more emails by message ID. Use list_labels to get label IDs.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "message_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "List of Gmail message IDs to label"
                        },
                        "label_id": {
                            "type": "string",
                            "description": "Label ID (from list_labels or create_label response)"
                        }
                    },
                    "required": ["message_ids", "label_id"]
                }
            },
            {
                "name": "remove_label",
                "description": "Remove a label from one or more emails by message ID.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "message_ids": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "List of Gmail message IDs"
                        },
                        "label_id": {
                            "type": "string",
                            "description": "Label ID to remove"
                        }
                    },
                    "required": ["message_ids", "label_id"]
                }
            },
            {
                "name": "search_emails",
                "description": "Search Gmail using standard query syntax. Returns id, subject, from, snippet, labels, and next_page_token for pagination.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Gmail search query, e.g. 'from:noreply label:INBOX'"
                        },
                        "max_results": {
                            "type": "integer",
                            "description": "Max results (default 20, max 100)"
                        },
                        "page_token": {
                            "type": "string",
                            "description": "Page token from previous search result for pagination (next_page_token field)"
                        }
                    },
                    "required": ["query"]
                }
            }
        ]
    })
}

// ─── Tool handlers ────────────────────────────────────────────────────────────

async fn handle_list_labels(state: &State) -> Result<Value> {
    state.ensure_auth().await?;
    let guard = state.client.read().await;
    let client = guard.as_ref().unwrap();
    let labels = client.list_labels().await?;
    Ok(json!({
        "count": labels.len(),
        "labels": labels.iter().map(|l| json!({
            "id": l.id,
            "name": l.name,
            "type": l.label_type,
            "messages_total": l.messages_total,
        })).collect::<Vec<_>>()
    }))
}

async fn handle_analyze_labels(state: &State, args: &Value) -> Result<Value> {
    let n = args["sample_size"].as_u64().unwrap_or(500).min(2000).max(1) as u32;
    let label_filter = args["label_filter"].as_str().map(|s| s.to_string());

    state.ensure_auth().await?;
    let guard = state.client.read().await;
    let client = guard.as_ref().unwrap();

    // Collect IDs: aim for 3× sample then shuffle
    let collect_target = (n * 3).min(2000) as usize;
    let label_slice: Vec<&str> = label_filter.as_deref().map(|l| vec![l]).unwrap_or_default();
    let label_opt: Option<&[&str]> = if label_slice.is_empty() { None } else { Some(&label_slice) };

    let mut all_ids: Vec<String> = Vec::new();
    let mut page_token: Option<String> = None;

    loop {
        let batch = client
            .list_messages(500, page_token.as_deref(), label_opt)
            .await?;
        if let Some(msgs) = batch.messages {
            all_ids.extend(msgs.into_iter().map(|m| m.id));
        }
        page_token = batch.next_page_token;
        if all_ids.len() >= collect_target || page_token.is_none() {
            break;
        }
    }

    all_ids.shuffle(&mut rand::thread_rng());
    all_ids.truncate(n as usize);
    let total = all_ids.len();

    let mut label_counts: HashMap<String, u64> = HashMap::new();
    let mut co_occur: HashMap<(String, String), u64> = HashMap::new();
    let mut errors = 0u64;

    for chunk in all_ids.chunks(20) {
        let futs: Vec<_> = chunk.iter().map(|id| client.get_message(id)).collect();
        for res in futures::future::join_all(futs).await {
            match res {
                Ok(msg) => {
                    if let Some(labels) = &msg.label_ids {
                        for lbl in labels {
                            *label_counts.entry(lbl.clone()).or_insert(0) += 1;
                        }
                        for i in 0..labels.len() {
                            for j in (i + 1)..labels.len() {
                                let (mut a, mut b) = (labels[i].clone(), labels[j].clone());
                                if a > b { std::mem::swap(&mut a, &mut b); }
                                *co_occur.entry((a, b)).or_insert(0) += 1;
                            }
                        }
                    }
                }
                Err(_) => errors += 1,
            }
        }
    }

    let mut sorted: Vec<_> = label_counts.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));

    let mut co_sorted: Vec<_> = co_occur.into_iter().collect();
    co_sorted.sort_by(|a, b| b.1.cmp(&a.1));
    let top_co: Vec<_> = co_sorted.iter().take(10).map(|((a, b), c)| {
        json!({"label_a": a, "label_b": b, "count": c})
    }).collect();

    Ok(json!({
        "emails_sampled": total,
        "fetch_errors": errors,
        "label_frequency": sorted.iter().map(|(k, v)| json!({
            "label": k,
            "count": v,
            "pct": format!("{:.1}%", (*v as f64 / total as f64) * 100.0)
        })).collect::<Vec<_>>(),
        "top_co_occurrences": top_co,
    }))
}

async fn handle_create_label(state: &State, args: &Value) -> Result<Value> {
    let name = args["name"].as_str()
        .ok_or_else(|| anyhow::anyhow!("missing 'name'"))?;
    state.ensure_auth().await?;
    let guard = state.client.read().await;
    let label = guard.as_ref().unwrap().create_label(name).await?;
    Ok(json!({
        "created": true,
        "id": label.id,
        "name": label.name,
    }))
}

async fn handle_apply_label(state: &State, args: &Value) -> Result<Value> {
    let label_id = args["label_id"].as_str()
        .ok_or_else(|| anyhow::anyhow!("missing 'label_id'"))?;
    let ids: Vec<&str> = args["message_ids"].as_array()
        .ok_or_else(|| anyhow::anyhow!("missing 'message_ids'"))?
        .iter().filter_map(|v| v.as_str()).collect();

    state.ensure_auth().await?;
    let guard = state.client.read().await;
    let client = guard.as_ref().unwrap();
    let mut ok = 0u32;
    for id in &ids {
        client.apply_label(id, label_id).await?;
        ok += 1;
    }
    Ok(json!({ "labeled": ok, "label_id": label_id }))
}

async fn handle_remove_label(state: &State, args: &Value) -> Result<Value> {
    let label_id = args["label_id"].as_str()
        .ok_or_else(|| anyhow::anyhow!("missing 'label_id'"))?;
    let ids: Vec<&str> = args["message_ids"].as_array()
        .ok_or_else(|| anyhow::anyhow!("missing 'message_ids'"))?
        .iter().filter_map(|v| v.as_str()).collect();

    state.ensure_auth().await?;
    let guard = state.client.read().await;
    let client = guard.as_ref().unwrap();
    let mut ok = 0u32;
    for id in &ids {
        client.remove_label(id, label_id).await?;
        ok += 1;
    }
    Ok(json!({ "removed": ok, "label_id": label_id }))
}

async fn handle_search_emails(state: &State, args: &Value) -> Result<Value> {
    let query = args["query"].as_str()
        .ok_or_else(|| anyhow::anyhow!("missing 'query' argument"))?;
    let max = args["max_results"].as_u64().unwrap_or(20).min(100).max(1) as u32;
    let page_token = args["page_token"].as_str();

    state.ensure_auth().await?;
    let guard = state.client.read().await;
    let client = guard.as_ref().unwrap();

    let batch = client.search_messages(query, max, page_token).await?;
    let next_page_token = batch.next_page_token.clone();
    let total_estimate = batch.result_size_estimate;
    let ids: Vec<String> = batch.messages.unwrap_or_default().into_iter().map(|m| m.id).collect();

    let futs: Vec<_> = ids.iter().map(|id| client.get_message(id)).collect();
    let messages: Vec<Value> = futures::future::join_all(futs)
        .await
        .into_iter()
        .filter_map(|r| r.ok())
        .map(|msg| {
            let headers = msg.payload.as_ref().and_then(|p| p.headers.as_ref());
            let subject = headers
                .and_then(|h| h.iter().find(|x| x.name == "Subject"))
                .map(|h| h.value.clone()).unwrap_or_default();
            let from = headers
                .and_then(|h| h.iter().find(|x| x.name == "From"))
                .map(|h| h.value.clone()).unwrap_or_default();
            json!({
                "id": msg.id,
                "subject": subject,
                "from": from,
                "snippet": msg.snippet.unwrap_or_default(),
                "labels": msg.label_ids.unwrap_or_default(),
            })
        })
        .collect();

    Ok(json!({
        "query": query,
        "count": messages.len(),
        "total_estimate": total_estimate,
        "next_page_token": next_page_token,
        "messages": messages
    }))
}

// ─── Tool dispatch ────────────────────────────────────────────────────────────

async fn call_tool(state: &State, name: &str, args: &Value) -> Result<Value> {
    match name {
        "list_labels"    => handle_list_labels(state).await,
        "analyze_labels" => handle_analyze_labels(state, args).await,
        "search_emails"  => handle_search_emails(state, args).await,
        "create_label"   => handle_create_label(state, args).await,
        "apply_label"    => handle_apply_label(state, args).await,
        "remove_label"   => handle_remove_label(state, args).await,
        _ => Err(anyhow::anyhow!("Unknown tool: {}", name)),
    }
}

fn tool_result_content(text: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": text }] })
}

// ─── Main stdio loop ──────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let state = State::new();

    let stdin  = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin).lines();
    let mut out = tokio::io::BufWriter::new(stdout);

    while let Some(line) = reader.next_line().await? {
        let line = line.trim().to_string();
        if line.is_empty() { continue; }

        let req: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let resp = Response::err(Value::Null, -32700, format!("Parse error: {e}"));
                let mut s = serde_json::to_string(&resp)?;
                s.push('\n');
                out.write_all(s.as_bytes()).await?;
                out.flush().await?;
                continue;
            }
        };

        let id = req.id.clone().unwrap_or(Value::Null);

        let resp: Response = match req.method.as_str() {
            "initialize" => Response::ok(id, json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "email-tool", "version": "0.1.0" }
            })),

            "notifications/initialized" => continue,

            "tools/list" => Response::ok(id, tools_list()),

            "tools/call" => {
                let params = req.params.as_ref().and_then(|p| p.as_object());
                let tool_name = params
                    .and_then(|p| p.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let args = params
                    .and_then(|p| p.get("arguments"))
                    .cloned()
                    .unwrap_or(json!({}));

                match call_tool(&state, tool_name, &args).await {
                    Ok(val) => {
                        let text = serde_json::to_string_pretty(&val)?;
                        Response::ok(id, tool_result_content(&text))
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        // Auth prompts go to stderr so they don't corrupt stdout JSON stream
                        Response::ok(id, tool_result_content(&format!("Error: {msg}")))
                    }
                }
            }

            other => Response::err(id, -32601, format!("Method not found: {other}")),
        };

        let mut s = serde_json::to_string(&resp)?;
        s.push('\n');
        out.write_all(s.as_bytes()).await?;
        out.flush().await?;
    }

    Ok(())
}
