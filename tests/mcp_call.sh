#!/usr/bin/env bash
# mcp_call.sh — posílá MCP session přes stdin, vypíše všechny odpovědi
# Použití: ./tests/mcp_call.sh <tool> [json_args]

set -euo pipefail

BINARY="${BINARY:-./target/release/email-tool}"

if [[ ! -x "$BINARY" ]]; then
  echo "Binary not found: $BINARY — run 'cargo build --release' first" >&2
  exit 1
fi

TOOL="${1:-list_labels}"
if [[ -z "${2:-}" ]]; then ARGS='{}'; else ARGS="$2"; fi

echo "=== Calling tool: $TOOL ===" >&2
echo "=== Args: $ARGS ===" >&2
echo "" >&2

# JSON stavíme jako proměnné — vyhneme se problémům s {} v printf format stringu
INIT='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}'
NOTIF='{"jsonrpc":"2.0","method":"notifications/initialized"}'
CALL="{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"${TOOL}\",\"arguments\":${ARGS}}}"

# printf '%s\n' garantuje newline za každým řádkem bez interpolace format stringu
printf '%s\n%s\n%s\n' "$INIT" "$NOTIF" "$CALL" | "$BINARY" | while IFS= read -r line; do
  echo "$line" | python3 -m json.tool 2>/dev/null || echo "$line"
  echo "---"
done
