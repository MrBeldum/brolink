#!/usr/bin/env bash
# Static checks for the node kit: compose file, entrypoint, Dockerfile pins.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
fail=0
need() {
  if ! grep -q "$2" "$1"; then
    echo "missing in $1: $2" >&2
    fail=1
  fi
}
need "$ROOT/Dockerfile" "brolink-host"
need "$ROOT/Dockerfile" "ubuntu:24.04"
need "$ROOT/Dockerfile" "v2026.906.222525"
need "$ROOT/entrypoint.sh" "brolink-host --background"
need "$ROOT/entrypoint.sh" "userspace-networking"
need "$ROOT/entrypoint.sh" "packetsize = 1184"
need "$ROOT/docker-compose.yml" "brolink-node"
need "$ROOT/docker-compose.yml" "node-tailscale"
if grep -q '47989:47989' "$ROOT/docker-compose.yml"; then
  echo "GameStream must not be published on the public internet" >&2
  fail=1
fi
test -x "$ROOT/entrypoint.sh" || chmod +x "$ROOT/entrypoint.sh"
exit "$fail"
