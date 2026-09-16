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
# The workspace version, so the bundle cannot drift from the binary inside it.
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
# dist/ is a staging copy. Keep Spotlight from listing it next to
# /Applications/BroLink.app.
touch "$ROOT/dist/.metadata_never_index"
cp "$BIN" "$APP/Contents/MacOS/BroLink"
chmod +x "$APP/Contents/MacOS/BroLink"
# The Finder/Dock icon: crates/core/assets/logo-1024.png, the same logo the
# window uses (brolink_core::icon::render), given macOS's rounded shape and
# inset by scripts/make-macos-icon.py and checked in as .icns.
cp "$ROOT/crates/client/BroLink.icns" "$APP/Contents/Resources/BroLink.icns"
cp "$ROOT/LICENSE" "$ROOT/NOTICE" "$APP/Contents/Resources/"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>BroLink</string>
  <key>CFBundleDisplayName</key><string>BroLink</string>
  <key>CFBundleIdentifier</key><string>dev.brolink.client</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleExecutable</key><string>BroLink</string>
  <key>CFBundleIconFile</key><string>BroLink</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>LSApplicationCategoryType</key>
  <string>public.app-category.utilities</string>
  <key>NSHumanReadableCopyright</key>
  <string>Copyright © 2026 BroLink Contributors</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSLocalNetworkUsageDescription</key>
  <string>BroLink sends the wake-up packet to your PC over the local network.</string>
</dict>
</plist>
PLIST
# Sign the bundle. Ad-hoc by default: Apple Silicon refuses to launch an
# unsigned binary at all, and the linker's signature does not cover the
# bundle. Set CODESIGN_IDENTITY to a "Developer ID Application" certificate
# for a build Gatekeeper accepts once notarized (see docs/MACOS.md).
IDENTITY="${CODESIGN_IDENTITY:--}"
codesign --force --deep --sign "$IDENTITY" "$APP"
codesign --verify --deep --strict "$APP"
if [[ "$IDENTITY" == "-" ]]; then
  echo "wrote $APP (ad-hoc signed; set CODESIGN_IDENTITY for a Developer ID build)"
else
  echo "wrote $APP (signed as $IDENTITY; notarize before distributing)"
fi
