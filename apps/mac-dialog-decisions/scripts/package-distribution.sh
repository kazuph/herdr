#!/bin/bash
# This packaging step is also used for local unsigned installation tests.
set -euo pipefail
cd "$1"
test -s mac-dialog-decisions/VERSION
test -x mac-dialog-decisions/herdr-decision-dialog.app/Contents/MacOS/herdr-decision-dialog
COPYFILE_DISABLE=1 tar -czf mac-dialog-decisions-darwin-arm64.tar.gz mac-dialog-decisions
shasum -a 256 mac-dialog-decisions-darwin-arm64.tar.gz > SHA256SUMS
shasum -a 256 -c SHA256SUMS
