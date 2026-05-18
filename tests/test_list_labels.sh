#!/usr/bin/env bash
# test_list_labels.sh — seznam všech Gmail labels
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
exec "$SCRIPT_DIR/mcp_call.sh" list_labels '{}'
