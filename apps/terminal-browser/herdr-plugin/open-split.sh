#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BROWSER="$ROOT/dist-release/terminal-browser/bin/terminal-browser"

if [ ! -x "$BROWSER" ]; then
  echo "terminal-browser has not been built — run: just terminal-browser-build" >&2
  exit 1
fi

exec "$BROWSER" open --split right
