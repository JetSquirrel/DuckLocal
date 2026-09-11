#!/bin/bash
# Build DuckLocal.app with icon and Info.plist
set -euo pipefail

cd "$(dirname "$0")/.."

cargo build --release

APP="target/release/DuckLocal.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

cp target/release/ducklocal "$APP/Contents/MacOS/ducklocal"
cp assets/Info.plist "$APP/Contents/Info.plist"
cp assets/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"

echo "Built $APP"
