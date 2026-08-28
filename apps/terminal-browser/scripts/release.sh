#!/bin/bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VERSION="${1:-dev}"
CHANNEL="${2:-dev}"
OUT="$ROOT/dist-release"
STAGE="$OUT/terminal-browser"

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) TARGET=darwin-arm64 ;;
  Linux-x86_64|Linux-amd64) TARGET=linux-x64 ;;
  Linux-aarch64|Linux-arm64) TARGET=linux-arm64 ;;
  *) echo "unsupported build host: $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac

rm -rf "$OUT"
mkdir -p "$STAGE"/{bin,cli/dist,browser/dist,browser/native,electron,agent-browser/bin,assets/fonts,scripts}

sign_macos() {
  local target="$1"
  local entitlements="${2:-}"
  local identity="${TERMINAL_BROWSER_CODE_SIGN_IDENTITY:?TERMINAL_BROWSER_CODE_SIGN_IDENTITY is required for macOS builds}"
  local args=(--force --options runtime --timestamp --sign "$identity")
  if ! security find-identity -v -p codesigning | grep -Fq "\"$identity\""; then
    echo "codesign identity is unavailable: $identity" >&2
    exit 1
  fi
  if [ -n "$entitlements" ]; then
    args+=(--entitlements "$entitlements")
  fi
  codesign "${args[@]}" "$target"
  codesign --verify --deep --strict --verbose=2 "$target"
}

sign_macos_app() {
  local app="$1"
  local identity="${TERMINAL_BROWSER_CODE_SIGN_IDENTITY:?TERMINAL_BROWSER_CODE_SIGN_IDENTITY is required for macOS builds}"
  local entitlements="$ROOT/scripts/entitlements.darwin.plist"
  local candidate nested expected_team actual_team

  while IFS= read -r -d '' candidate; do
    if file -b "$candidate" | grep -q '^Mach-O'; then
      codesign --force --options runtime --timestamp --sign "$identity" "$candidate"
    fi
  done < <(find "$app/Contents" -type f -print0)

  while IFS= read -r -d '' nested; do
    if [[ "$nested" = *.app ]]; then
      sign_macos "$nested" "$entitlements"
    else
      sign_macos "$nested"
    fi
  done < <(find "$app/Contents" -depth -type d \( -name '*.framework' -o -name '*.app' -o -name '*.xpc' \) -print0)

  sign_macos "$app" "$entitlements"
  expected_team="$(codesign -dv --verbose=4 "$app" 2>&1 | sed -n 's/^TeamIdentifier=//p')"
  while IFS= read -r -d '' candidate; do
    if file -b "$candidate" | grep -q '^Mach-O'; then
      actual_team="$(codesign -dv --verbose=4 "$candidate" 2>&1 | sed -n 's/^TeamIdentifier=//p')"
      if [ -z "$actual_team" ] || [ "$actual_team" != "$expected_team" ]; then
        echo "codesign team mismatch: $candidate has ${actual_team:-none}, expected $expected_team" >&2
        exit 1
      fi
    fi
  done < <(find "$app/Contents" -type f -print0)
}

(cd "$ROOT/engine" && cargo build -p pixel-node --release)
if [ "$TARGET" = darwin-arm64 ]; then
  NATIVE_LIB=libpixel_node.dylib
else
  NATIVE_LIB=libpixel_node.so
fi
cp "${CARGO_TARGET_DIR:-$ROOT/engine/target}/release/$NATIVE_LIB" "$STAGE/browser/native/pixel.node"
if [ "$TARGET" = darwin-arm64 ]; then
  sign_macos "$STAGE/browser/native/pixel.node"
fi

# the engine bakes in a path to its build directory, which only exists on this machine
if [ "$TARGET" = darwin-arm64 ]; then
  swiftc -O -target arm64-apple-macos11 "$ROOT/engine/crates/pixel-core/native-scroll-helper.swift" \
    -o "$STAGE/bin/native-scroll-helper"
  sign_macos "$STAGE/bin/native-scroll-helper"
fi

AGENT_BROWSER_BIN="$("$ROOT/scripts/agent-browser.sh" --path)"
cp "$AGENT_BROWSER_BIN" "$STAGE/agent-browser/bin/agent-browser"
if [ "$TARGET" = darwin-arm64 ]; then
  sign_macos "$STAGE/agent-browser/bin/agent-browser"
fi
cp "$ROOT/scripts/agent-browser.json" "$STAGE/agent-browser/config.json"

"$ROOT/scripts/bundle.sh" "$ROOT/cli/src/main.ts" "$STAGE/cli/dist/main.js"
"$ROOT/scripts/bundle.sh" "$ROOT/browser/src/main.tsx" "$STAGE/browser/dist/main.js"

cp "$ROOT/scripts/apparmor.sh" "$STAGE/scripts/apparmor.sh"

"$ROOT/scripts/generate-skill.sh"
cp -R "$ROOT/skill/build" "$STAGE/skills"

cp "$ROOT/assets/fonts/JetBrainsMono-Regular.ttf" "$STAGE/assets/fonts/"

ELECTRON_DIST="$(node -e 'const p=require("path");console.log(p.join(p.dirname(require.resolve("electron/package.json",{paths:[process.argv[1]]})),"dist"))' "$ROOT/browser")"
if [ ! -f "$ELECTRON_DIST/.zenbu-electron-sha256" ]; then
  echo "refusing to build: installed electron does not come from https://github.com/zenbu-labs/electron-releases" >&2
  exit 1
fi
if [ "$TARGET" = darwin-arm64 ]; then
  APP="$STAGE/electron/terminal-browser.app"
  ditto "$ELECTRON_DIST/Electron.app" "$APP"
  mv "$APP/Contents/MacOS/Electron" "$APP/Contents/MacOS/terminal-browser"
  /usr/libexec/PlistBuddy \
    -c "Set :CFBundleExecutable terminal-browser" \
    -c "Set :CFBundleName terminal-browser" \
    -c "Set :CFBundleDisplayName terminal-browser" \
    -c "Set :CFBundleIdentifier dev.zenbu.terminal-browser" \
    "$APP/Contents/Info.plist" >/dev/null
  sign_macos_app "$APP"
  ELECTRON_EXE="electron/terminal-browser.app/Contents/MacOS/terminal-browser"
  NATIVE_SCROLL='export NATIVE_SCROLL_HELPER="${NATIVE_SCROLL_HELPER:-$ROOT/bin/native-scroll-helper}"'
else
  cp -a "$ELECTRON_DIST/." "$STAGE/electron/"
  ELECTRON_EXE="electron/electron"
  NATIVE_SCROLL=""
fi

cat > "$STAGE/bin/terminal-browser" <<EOF
#!/bin/sh
ROOT="\$(CDPATH= cd -- "\$(dirname -- "\$0")/.." && pwd -P)"
export TERMINAL_BROWSER_DIST_ROOT="\$ROOT"
export ELECTRON_RUN_AS_NODE=1
$NATIVE_SCROLL
exec "\$ROOT/$ELECTRON_EXE" "\$ROOT/cli/dist/main.js" "\$@"
EOF
chmod +x "$STAGE/bin/terminal-browser"
echo "$VERSION" > "$STAGE/VERSION"
echo "$CHANNEL" > "$STAGE/CHANNEL"

TARBALL="$OUT/terminal-browser-$TARGET.tar.gz"
tar -czf "$TARBALL" -C "$OUT" terminal-browser

if [ "$TARGET" = darwin-arm64 ]; then
  SHA256="$(shasum -a 256 "$TARBALL" | cut -d' ' -f1)"
  SIZE="$(stat -f%z "$TARBALL")"
else
  SHA256="$(sha256sum "$TARBALL" | cut -d' ' -f1)"
  SIZE="$(stat -c%s "$TARBALL")"
fi

cat > "$OUT/manifest-$TARGET.json" <<EOF
{
  "version": "$VERSION",
  "channel": "$CHANNEL",
  "platform": "$TARGET",
  "file": "$(basename "$TARBALL")",
  "sha256": "$SHA256",
  "size": $SIZE,
  "published": "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
}
EOF

du -h "$TARBALL"
