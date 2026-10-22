#!/bin/bash
# Build the unsigned payload. Only herdr-sign-release.sh may publish it.
set -euo pipefail
[[ "${1:-}" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]] || { echo 'invalid mac-dialog version' >&2; exit 1; }
[ "$(uname -sm)" = 'Darwin arm64' ] || { echo 'mac-dialog requires macOS Apple silicon' >&2; exit 1; }
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
mkdir -p "$2"
OUT="$(cd "$2" && pwd)"
PAYLOAD="$OUT/mac-dialog-decisions"
# Refuse to reuse a payload; a prior signature or file must never leak in.
mkdir "$PAYLOAD"
APP="$PAYLOAD/herdr-decision-dialog.app"
mkdir -p "$APP/Contents/MacOS"
env -u DEVELOPER_DIR swiftc -O "$ROOT/dialog.swift" -o "$APP/Contents/MacOS/herdr-decision-dialog"
python3 - "$APP/Contents/Info.plist" "$1" <<'PY'
import plistlib,sys
with open(sys.argv[1], 'wb') as f:
    plistlib.dump(dict(CFBundleExecutable='herdr-decision-dialog', CFBundleIdentifier='dev.herdr.mac-dialog-decisions', CFBundleName='herdr 判断待ち', CFBundlePackageType='APPL', CFBundleShortVersionString=sys.argv[2], CFBundleVersion=sys.argv[2], NSHighResolutionCapable=True), f)
PY
cp "$ROOT/herdr-plugin.toml" "$ROOT/mac-dialog-decisions.mjs" "$ROOT/ensure-daemon" "$ROOT/README.md" "$PAYLOAD/"
chmod 755 "$PAYLOAD/ensure-daemon" "$PAYLOAD/mac-dialog-decisions.mjs"
printf '%s\n' "$1" > "$PAYLOAD/VERSION"
printf '%s\n' darwin-arm64 > "$PAYLOAD/PLATFORM"
