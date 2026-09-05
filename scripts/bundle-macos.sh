#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/aarch64-apple-darwin/release/brolink-client"
if [[ ! -f "$BIN" ]]; then
  BIN="$ROOT/target/release/brolink-client"
fi
if [[ ! -f "$BIN" ]]; then
  echo "build the client first: cargo build --release -p brolink-client" >&2
  exit 1
fi
APP="$ROOT/dist/BroLink.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/BroLink"
chmod +x "$APP/Contents/MacOS/BroLink"
cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>BroLink</string>
  <key>CFBundleDisplayName</key><string>BroLink</string>
  <key>CFBundleIdentifier</key><string>dev.brolink.client</string>
  <key>CFBundleVersion</key><string>1.1.0</string>
  <key>CFBundleShortVersionString</key><string>1.1.0</string>
  <key>CFBundleExecutable</key><string>BroLink</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>LSApplicationCategoryType</key>
  <string>public.app-category.entertainment</string>
  <key>NSHumanReadableCopyright</key>
  <string>Copyright © 2026 BroLink Contributors</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSLocalNetworkUsageDescription</key>
  <string>BroLink discovers your Windows PC on the local network.</string>
</dict>
</plist>
PLIST
echo "wrote $APP"
