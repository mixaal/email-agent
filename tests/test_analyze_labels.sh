#!/usr/bin/env bash
# test_analyze_labels.sh — analýza labelů na vzorku emailů
#
# Použití:
#   ./tests/test_analyze_labels.sh           # default: 50 emailů
#   SAMPLE=200 ./tests/test_analyze_labels.sh
#   SAMPLE=200 LABEL=INBOX ./tests/test_analyze_labels.sh

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

SAMPLE="${SAMPLE:-50}"
LABEL="${LABEL:-}"

if [[ -n "$LABEL" ]]; then
  ARGS="{\"sample_size\": $SAMPLE, \"label_filter\": \"$LABEL\"}"
else
  ARGS="{\"sample_size\": $SAMPLE}"
fi

exec "$SCRIPT_DIR/mcp_call.sh" analyze_labels "$ARGS"
