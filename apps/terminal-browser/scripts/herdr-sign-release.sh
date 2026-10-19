#!/bin/bash
# Invoked only on the tag workflow. No credential output or tracing.
set -euo pipefail
set +x
if [ -z "${MACOS_DEVELOPER_ID_P12:-}" ] || [ -z "${MACOS_DEVELOPER_ID_P12_PASSWORD:-}" ]; then
  echo 'certificate missing: MACOS_DEVELOPER_ID_P12 and MACOS_DEVELOPER_ID_P12_PASSWORD are required' >&2
  exit 1
fi
[ "$(uname -sm)" = 'Darwin arm64' ] || { echo 'darwin-arm64 required' >&2; exit 1; }
[[ "${1:-}" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]] || { echo 'invalid tb version' >&2; exit 1; }
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
umask 077
KEY_DIR="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/tb-sign.XXXXXX")"
KEYCHAIN="$KEY_DIR/signing.keychain-db"
P12="$KEY_DIR/certificate.p12"
# Preserve exact paths, including spaces, without eval.
security list-keychains -d user > "$KEY_DIR/search-list"
cleanup() {
  local status=$?
  trap - EXIT
  set +e
  python3 - "$KEY_DIR/search-list" <<'PY'
import shlex, subprocess, sys
with open(sys.argv[1]) as f:
    paths = shlex.split(f.read())
subprocess.run(['security', 'list-keychains', '-d', 'user', '-s', *paths], check=True)
PY
  local restore_status=$?
  security delete-keychain "$KEYCHAIN" >/dev/null 2>&1 || true
  rm -rf "$KEY_DIR"
  if [ "$restore_status" -ne 0 ]; then status="$restore_status"; fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
KEY_PASSWORD="$(openssl rand -base64 32)"
printf '%s' "$MACOS_DEVELOPER_ID_P12" | base64 --decode > "$P12"
unset MACOS_DEVELOPER_ID_P12
security create-keychain -p "$KEY_PASSWORD" "$KEYCHAIN"
security set-keychain-settings -lut 21600 "$KEYCHAIN"
security unlock-keychain -p "$KEY_PASSWORD" "$KEYCHAIN"
security import "$P12" -P "$MACOS_DEVELOPER_ID_P12_PASSWORD" -k "$KEYCHAIN" -T /usr/bin/codesign -T /usr/bin/security >/dev/null
unset MACOS_DEVELOPER_ID_P12_PASSWORD
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$KEY_PASSWORD" "$KEYCHAIN" >/dev/null
# Restrict identity lookup to the temporary keychain, restoring it in the trap.
security list-keychains -d user -s "$KEYCHAIN"
IDENTITY='Developer ID Application: KAZUHIRO HOMMA (67M8FDFZ28)'
security find-identity -v -p codesigning "$KEYCHAIN" | grep -Fq "\"$IDENTITY\"" || { echo 'required Developer ID certificate unavailable' >&2; exit 1; }
security find-certificate -c "$IDENTITY" -p "$KEYCHAIN" | openssl x509 -noout -subject -nameopt multiline | grep -Eq 'organizationalUnitName[[:space:]]*=[[:space:]]*67M8FDFZ28$' || { echo 'certificate Team ID mismatch' >&2; exit 1; }
export TERMINAL_BROWSER_CODE_SIGN_IDENTITY="$IDENTITY"
bash "$ROOT/scripts/release.sh" "$1" herdr
APP="$ROOT/dist-release/terminal-browser/electron/terminal-browser.app"
codesign --verify --deep --strict "$APP"
codesign -dv --verbose=4 "$APP" 2>&1 | grep -Fxq 'TeamIdentifier=67M8FDFZ28'
cd "$ROOT/dist-release"
shasum -a 256 terminal-browser-darwin-arm64.tar.gz > SHA256SUMS
shasum -a 256 -c SHA256SUMS
