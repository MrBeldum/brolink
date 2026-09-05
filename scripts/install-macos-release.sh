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
# The repository is private, so anonymous downloads 404: use the GitHub CLI
# when it is installed and signed in (brew install gh; gh auth login), and
# fall back to the public URL otherwise.
if command -v gh >/dev/null 2>&1; then
  echo "downloading with gh"
  gh release download --repo "$REPO" --pattern brolink-macos-arm64.tar.gz --dir "$TMP" --clobber
else
  echo "downloading $URL"
  curl -fsSL -o "$TMP/brolink-macos-arm64.tar.gz" "$URL"
fi
tar xzf "$TMP/brolink-macos-arm64.tar.gz" -C "$TMP"
xattr -dr com.apple.quarantine "$TMP/BroLink.app" 2>/dev/null || true
rm -rf /Applications/BroLink.app
cp -R "$TMP/BroLink.app" /Applications/BroLink.app
echo "installed /Applications/BroLink.app"
open /Applications/BroLink.app
