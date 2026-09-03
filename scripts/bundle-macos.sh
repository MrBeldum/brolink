#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/aarch64-apple-darwin/release/forgelink-client"
if [[ ! -x "$BIN" ]]; then
  BIN="$ROOT/target/release/forgelink-client"
fi
if [[ ! -x "$BIN" ]]; then
  echo "build the client first: cargo build --release -p forgelink-client" >&2
  exit 1
fi
APP="$ROOT/dist/ForgeLink.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/ForgeLink"
cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>ForgeLink</string>
  <key>CFBundleDisplayName</key><string>ForgeLink</string>
  <key>CFBundleIdentifier</key><string>dev.forgelink.client</string>
  <key>CFBundleVersion</key><string>0.1.0</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundleExecutable</key><string>ForgeLink</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSLocalNetworkUsageDescription</key>
  <string>ForgeLink discovers your Windows PC on the local network.</string>
</dict>
</plist>
PLIST
echo "wrote $APP"
