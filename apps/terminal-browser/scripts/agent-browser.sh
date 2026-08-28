#!/bin/bash
set -euo pipefail

REV="1ed371f3af472cc0d6cd8fdaea75d1a085ff7534"

CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/terminal-browser/agent-browser/$REV"
BIN="$CACHE/bin/agent-browser"

if [ "${1:-}" = "--rev" ]; then
  echo "$REV"
  exit 0
fi

if [ ! -x "$BIN" ] || [ "${1:-}" = "--force" ]; then
  echo "building agent-browser $REV (first run, a few minutes)…" >&2
  cargo install --git https://github.com/vercel-labs/agent-browser \
    --rev "$REV" --locked --root "$CACHE" agent-browser >&2
fi

echo "$BIN"
