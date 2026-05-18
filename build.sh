#!/usr/bin/env bash
set -euo pipefail
cargo build --release
echo "Binary: target/release/email-tool"
chmod +x tests/*.sh
echo "Test scripts ready in tests/"
