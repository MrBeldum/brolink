#!/bin/bash
# Start a virtual desktop, the streaming engine, BroLink's control service,
# and (optionally) Tailscale so this container is a machine on the tailnet.
set -euo pipefail

log() { echo "[brolink-node] $*"; }

install -d -m 700 /root/.config/sunshine /root/.local/share/brolink /var/run/tailscale
if [[ ! -x /usr/local/bin/brolink-engine && -x /usr/bin/sunshine ]]; then
  cp /usr/bin/sunshine /usr/local/bin/brolink-engine
fi
ENGINE=/usr/local/bin/brolink-engine
if [[ ! -x "$ENGINE" ]]; then
  ENGINE=/usr/bin/sunshine
fi

if [[ -n "${TS_AUTHKEY:-}" ]]; then
  log "starting Tailscale (userspace) as ${TS_HOSTNAME:-brolink-node}"
  tailscaled --tun=userspace-networking --state=/var/lib/tailscale/tailscaled.state \
    --socket=/var/run/tailscale/tailscaled.sock >/var/log/tailscaled.log 2>&1 &
  for _ in $(seq 1 20); do
    tailscale status >/dev/null 2>&1 && break
    sleep 0.5
  done
  tailscale up --authkey="$TS_AUTHKEY" --hostname="${TS_HOSTNAME:-brolink-node}" \
    --accept-dns=false
fi

log "starting Xvfb ${DISPLAY:-:1}"
rm -f /tmp/.X1-lock
Xvfb "${DISPLAY:-:1}" -screen 0 "${SCREEN_SIZE:-1920x1080x24}" -ac +extension RANDR >/var/log/xvfb.log 2>&1 &
sleep 0.4

pulseaudio --start --exit-idle-time=-1 >/var/log/pulse.log 2>&1 || true

log "starting xfce"
startxfce4 >/var/log/xfce.log 2>&1 &
sleep 1

CONF=/root/.config/sunshine/sunshine.conf
cat >"$CONF" <<'EOF'
system_tray = disabled
origin_web_ui_allowed = pc
credentials_file = /root/.config/sunshine/brolink-web.json
max_bitrate = 0
minimum_fps_target = 60
fec_percentage = 20
packetsize = 1184
amd_rc = cbr
vaapi_rc = cbr
sw_preset = ultrafast
sw_tune = zerolatency
min_threads = 4
capture = x11
encoder = software
EOF
cat >/root/.config/sunshine/apps.json <<'EOF'
{
  "env": {},
  "apps": [
    {
      "name": "Desktop",
      "image-path": "desktop.png"
    }
  ]
}
EOF

# One persisted source of truth for the host and engine. Fails closed on a
# damaged config, and never exposes the engine password in process arguments.
python3 /opt/brolink/bootstrap.py

log "starting streaming engine"
"$ENGINE" "$CONF" >/var/log/sunshine.log 2>&1 &
ENGINE_PID=$!
# The control service starts an engine if this port is not listening yet.
# Wait for our child so its first refresh cannot race a second engine onto
# the same ports. A failed startup exits the container for Docker to retry.
python3 - "$ENGINE_PID" <<'PY'
import os
import socket
import sys
import time

deadline = time.monotonic() + 15
while time.monotonic() < deadline:
    try:
        os.kill(int(sys.argv[1]), 0)
    except ProcessLookupError:
        raise SystemExit("Streaming engine exited; see /var/log/sunshine.log")
    try:
        with socket.create_connection(("127.0.0.1", 47984), timeout=0.2):
            break
    except OSError:
        time.sleep(0.1)
else:
    raise SystemExit("Streaming engine did not start listening within 15 seconds")
PY

log "starting BroLink control service"
exec /usr/local/bin/brolink-host --background
