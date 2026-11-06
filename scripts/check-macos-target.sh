#!/bin/bash
# Check that macOS binaries are built for the oldest macOS DuckLocal declares.
#
# assets/Info.plist's LSMinimumSystemVersion is the promise; the `minos` in
# each binary's LC_BUILD_VERSION is what the loader actually enforces. They
# drifted once: with MACOSX_DEPLOYMENT_TARGET unset, rustc stamped 11.0 and
# `cc` compiled DuckDB's C++ for the macOS of the build machine. Nothing
# failed until someone on an older macOS tried to open the app.
# .cargo/config.toml pins the target now, and this script fails the build if
# that pin goes missing or stops applying.
#
# Usage: scripts/check-macos-target.sh <binary or .app> [...]
# An .app is checked whole: its executable and every PlugIns/*.appex.
set -euo pipefail

cd "$(dirname "$0")/.."

EXPECTED=$(plutil -extract LSMinimumSystemVersion raw assets/Info.plist)
status=0

check() {
  local file="$1"
  local minos
  minos=$(otool -l "$file" | awk '/LC_BUILD_VERSION/ { found = 1 } found && $1 == "minos" { print $2; exit }')
  if [ "$minos" = "$EXPECTED" ]; then
    echo "ok: $file targets macOS $minos"
  else
    echo "::error::$file targets macOS ${minos:-?}, not $EXPECTED (assets/Info.plist)"
    status=1
  fi
}

[ $# -gt 0 ] || { echo "usage: $0 <binary or .app> [...]" >&2; exit 2; }

for target in "$@"; do
  if [ -d "$target" ]; then
    exe=$(plutil -extract CFBundleExecutable raw "$target/Contents/Info.plist")
    check "$target/Contents/MacOS/$exe"
    for appex in "$target"/Contents/PlugIns/*.appex; do
      [ -d "$appex" ] || continue
      exe=$(plutil -extract CFBundleExecutable raw "$appex/Contents/Info.plist")
      check "$appex/Contents/MacOS/$exe"
    done
  else
    check "$target"
  fi
done

exit $status
