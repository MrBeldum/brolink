#!/usr/bin/env bash
# Install the prebuilt Mac app from the latest GitHub release without the
# Gatekeeper "damaged" dialog: files fetched from the command line carry no
# quarantine flag, unlike a browser download. Usage, from a checkout or a
# copy of this file:
#
#   bash scripts/install-macos-release.sh
#
set -euo pipefail
REPO="${BROLINK_REPO:-MrBeldum/brolink}"
URL="https://github.com/$REPO/releases/latest/download/brolink-macos-arm64.tar.gz"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
# Public releases download anonymously. Keep the authenticated fallback so
# this script still works if the repository is made private again.
echo "downloading $URL"
if ! curl -fsSL -o "$TMP/brolink-macos-arm64.tar.gz" "$URL"; then
  if command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
    echo "anonymous download failed; downloading with gh"
    gh release download --repo "$REPO" --pattern brolink-macos-arm64.tar.gz --dir "$TMP" --clobber
  else
    echo "download failed; a private repository requires gh auth login" >&2
    exit 1
  fi
fi
tar xzf "$TMP/brolink-macos-arm64.tar.gz" -C "$TMP"
xattr -dr com.apple.quarantine "$TMP/BroLink.app" 2>/dev/null || true
rm -rf /Applications/BroLink.app
cp -R "$TMP/BroLink.app" /Applications/BroLink.app
echo "installed /Applications/BroLink.app"
open /Applications/BroLink.app
