#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

if [ "$(uname -s)" = "Darwin" ] && [ -z "${TERMINAL_BROWSER_CODE_SIGN_IDENTITY:-}" ]; then
  identities="$(security find-identity -v -p codesigning | sed -n 's/.*"\(Developer ID Application:[^"]*\)".*/\1/p')"
  if [ "$(printf '%s\n' "$identities" | sed '/^$/d' | wc -l | tr -d ' ')" != "1" ]; then
    echo "set TERMINAL_BROWSER_CODE_SIGN_IDENTITY to one available Developer ID Application identity" >&2
    exit 1
  fi
  export TERMINAL_BROWSER_CODE_SIGN_IDENTITY="$identities"
fi

cd "$ROOT"
CI=1 corepack pnpm install --frozen-lockfile
corepack pnpm build:dist
