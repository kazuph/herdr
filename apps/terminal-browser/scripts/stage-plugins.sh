#!/bin/bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
STAGE="${1:?stage directory required}"
VERSION="${2:?version required}"
for runtime in herdr ikesu; do
  cp -R "$ROOT/$runtime-plugin-release" "$STAGE/$runtime-plugin"
  sed "s/^version = .*/version = \"$VERSION\"/" \
    "$ROOT/$runtime-plugin-release/$runtime-plugin.toml" > "$STAGE/$runtime-plugin/$runtime-plugin.toml"
done
