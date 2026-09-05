#!/usr/bin/env bash
# Install the prebuilt Mac app from the latest GitHub release without the
# Gatekeeper "damaged" dialog: files fetched by curl carry no quarantine
# flag, unlike a browser download. Usage:
#
#   curl -fsSL https://raw.githubusercontent.com/MrBeldum/brolink/main/scripts/install-macos-release.sh | bash
set -euo pipefail
REPO="${BROLINK_REPO:-MrBeldum/brolink}"
URL="https://github.com/$REPO/releases/latest/download/brolink-macos-arm64.tar.gz"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
echo "downloading $URL"
curl -fsSL -o "$TMP/brolink.tar.gz" "$URL"
tar xzf "$TMP/brolink.tar.gz" -C "$TMP"
xattr -dr com.apple.quarantine "$TMP/BroLink.app" 2>/dev/null || true
rm -rf /Applications/BroLink.app
cp -R "$TMP/BroLink.app" /Applications/BroLink.app
echo "installed /Applications/BroLink.app"
open /Applications/BroLink.app
