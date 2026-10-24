#!/usr/bin/env bash
# Point dsh at whalegate. Thin wrapper over `whalegate setup-dsh` (the binary does the work — no python needed).
# Usage:  deploy/setup-dsh.sh [profile_dir] [base_url]
#   DSH_PRESET=standard deploy/setup-dsh.sh      → keep the classic preset (default: ptc)
#   DS_CONTEXT_TOKENS=300000 deploy/setup-dsh.sh → smaller context window
# Idempotent; backs up cordis.patch.yml to .bak.<timestamp> first.
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
bin=""
for c in "$(command -v whalegate || true)" "$here/target/release/deepseek-harness" "$here/target/release/whalegate"; do
  [ -n "$c" ] && [ -x "$c" ] && { bin="$c"; break; }
done
[ -n "$bin" ] || { echo "whalegate binary not found — install a release or run: cargo build --release" >&2; exit 1; }
args=()
[ $# -ge 1 ] && args+=(--profile "$1")
[ $# -ge 2 ] && args+=(--url "$2")
exec "$bin" setup-dsh "${args[@]}"
