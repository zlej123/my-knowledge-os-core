#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../rust"
if [[ "${1:-}" == "--check" ]]; then
  cargo fmt --all -- --check
else
  cargo fmt --all
fi
