#!/bin/bash
# Build the Quick Look preview extension and put it in an app bundle.
#
# The extension is a Swift shell (quicklook/extension) around the Rust
# preview engine (quicklook/src), linked in as a static library. It goes in
# the app's Contents/PlugIns, where macOS finds it once the app has been
# launched or registered.
#
# Usage: scripts/build-quicklook.sh <DuckLocal.app> [signing identity]
#
# Without an identity the extension is signed ad hoc, which is enough for a
# local build (bundle.sh). package-macos.sh passes its Developer ID and
# signs the app around it afterwards: inside out.
#
# RUST_TARGET, when set, builds the engine for that target triple, as the
# release workflow builds the app: the engine then reuses the app's DuckDB
# build instead of compiling it a second time.
set -euo pipefail

cd "$(dirname "$0")/.."

APP="$1"
IDENTITY="${2:--}"
NAME="DuckLocalPreview"
SRC="quicklook/extension"
APPEX="$APP/Contents/PlugIns/$NAME.appex"
# The deployment target the host app declares (assets/Info.plist), and the
# first macOS with data-based Quick Look previews.
TARGET="arm64-apple-macos12.0"

if [ -n "${RUST_TARGET:-}" ]; then
  cargo build --release --locked -p ducklocal-quicklook --target "$RUST_TARGET"
  ENGINE="target/$RUST_TARGET/release/libducklocal_quicklook.a"
else
  cargo build --release --locked -p ducklocal-quicklook
  ENGINE="target/release/libducklocal_quicklook.a"
fi

rm -rf "$APPEX"
mkdir -p "$APPEX/Contents/MacOS"

# `_NSExtensionMain` is an extension's entry point: it connects to the
# process that launched it and instantiates NSExtensionPrincipalClass.
# DuckDB is C++, hence libc++.
xcrun swiftc \
  -target "$TARGET" \
  -O \
  -application-extension \
  -parse-as-library \
  -module-name "$NAME" \
  -import-objc-header "$SRC/ducklocal_quicklook.h" \
  "$SRC/PreviewProvider.swift" \
  "$ENGINE" \
  -framework QuickLookUI \
  -lc++ \
  -Xlinker -e -Xlinker _NSExtensionMain \
  -o "$APPEX/Contents/MacOS/$NAME"

cp "$SRC/Info.plist" "$APPEX/Contents/Info.plist"
# Same version as the app it ships in.
VERSION=$(plutil -extract CFBundleShortVersionString raw "$APP/Contents/Info.plist")
plutil -replace CFBundleShortVersionString -string "$VERSION" "$APPEX/Contents/Info.plist"
plutil -replace CFBundleVersion -string "$VERSION" "$APPEX/Contents/Info.plist"

if [ "$IDENTITY" = "-" ]; then
  codesign --force --sign - --entitlements "$SRC/entitlements.plist" "$APPEX"
else
  codesign --force --timestamp --options runtime \
    --entitlements "$SRC/entitlements.plist" \
    --sign "$IDENTITY" "$APPEX"
fi

echo "Built $APPEX"
