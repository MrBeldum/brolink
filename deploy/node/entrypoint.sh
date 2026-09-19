#!/bin/bash
# Start a virtual desktop, Sunshine, BroLink's control service, and
# (optionally) Tailscale so this container is a machine on the tailnet.
set -euo pipefail

log() { echo "[brolink-node] $*"; }

install -d -m 700 /root/.config/sunshine /root/.local/share/brolink/BroLink /var/run/tailscale

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
if [[ ! -f "$CONF" ]]; then
  cat >"$CONF" <<'EOF'
system_tray = disabled
origin_web_ui_allowed = pc
max_bitrate = 0
minimum_fps_target = 60
fec_percentage = 20
packetsize = 1184
amd_rc = cbr
vaapi_rc = cbr
sw_tune = zerolatency
capture = x11
encoder = software
EOF
fi

USER_NAME="${BROLINK_USER:-brolink}"
PASS_NAME="${BROLINK_PASS:-}"
if [[ -z "$PASS_NAME" ]]; then
  PASS_NAME="$(tr -dc 'abcdefghjkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789' </dev/urandom | head -c 20)"
  log "generated Sunshine login (user ${USER_NAME}); set BROLINK_PASS to pin it"
fi
sunshine "$CONF" --creds "$USER_NAME" "$PASS_NAME" >/dev/null 2>&1 || true

HOST_TOML=/root/.local/share/brolink/BroLink/host.toml
if [[ ! -f "$HOST_TOML" ]]; then
  cat >"$HOST_TOML" <<EOF
power_allowed = true
start_with_windows = true
stay_awake = true
sunshine_user = "${USER_NAME}"
sunshine_pass = "${PASS_NAME}"
EOF
fi

log "starting Sunshine"
sunshine "$CONF" >/var/log/sunshine.log 2>&1 &

log "starting BroLink control service"
exec /usr/local/bin/brolink-host --background
