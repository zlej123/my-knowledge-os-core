#!/usr/bin/env bash
set -euo pipefail
exec "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/../skills/codex/my-knowledge-os/scripts/install-from-source.sh" "$@"
