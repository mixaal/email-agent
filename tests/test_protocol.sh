#!/usr/bin/env bash
# test_protocol.sh — testuje MCP handshake + tools/list bez Gmail credentials
set -euo pipefail

BINARY="${BINARY:-./target/release/email-tool}"

if [[ ! -x "$BINARY" ]]; then
  echo "Binary not found: $BINARY" >&2
  exit 1
fi

INIT='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}'
NOTIF='{"jsonrpc":"2.0","method":"notifications/initialized"}'
LIST='{"jsonrpc":"2.0","id":2,"method":"tools/list"}'

echo "=== Test: initialize + tools/list ===" >&2

printf '%s\n%s\n%s\n' "$INIT" "$NOTIF" "$LIST" | "$BINARY" | while IFS= read -r line; do
  echo "$line" | python3 -m json.tool 2>/dev/null || echo "$line"
  echo "---"
done

echo "=== Protocol OK ===" >&2
