#!/usr/bin/env bash
# Build and install the BroLink Mac client into /Applications.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
rustup target add aarch64-apple-darwin >/dev/null
cargo build --release -p brolink-client --target aarch64-apple-darwin
bash "$ROOT/scripts/bundle-macos.sh"
APP="$ROOT/dist/BroLink.app"
if [[ ! -d "$APP" ]]; then
  echo "bundle failed" >&2
  exit 1
fi
xattr -dr com.apple.quarantine "$APP" 2>/dev/null || true
rm -rf /Applications/BroLink.app
cp -R "$APP" /Applications/BroLink.app
echo "installed /Applications/BroLink.app"
echo "open it, paste the ticket from your PC, click Connect."
