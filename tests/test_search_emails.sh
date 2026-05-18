#!/usr/bin/env bash
# test_search_emails.sh — fulltext vyhledávání přes Gmail query syntax
#
# Použití:
#   ./tests/test_search_emails.sh "from:github"
#   ./tests/test_search_emails.sh "label:INBOX is:unread" 10
#   ./tests/test_search_emails.sh "subject:invoice after:2024/1/1" 20

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

QUERY="${1:-in:anywhere}"
MAX="${2:-5}"

ARGS="{\"query\": \"$QUERY\", \"max_results\": $MAX}"
exec "$SCRIPT_DIR/mcp_call.sh" search_emails "$ARGS"
