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
  "apps": [
    {
      "name": "Desktop",
      "image-path": "desktop.png"
    }
  ]
}
EOF

USER_NAME="${BROLINK_USER:-brolink}"
PASS_NAME="${BROLINK_PASS:-}"
if [[ -z "$PASS_NAME" ]]; then
  # `head` closes the pipe after 20 bytes; `tr` then gets SIGPIPE. With
  # `pipefail` that would exit 141 and crash the container.
  set +o pipefail
  PASS_NAME="$(tr -dc 'abcdefghjkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789' </dev/urandom | head -c 20)"
  set -o pipefail
  log "generated engine login (user ${USER_NAME}); set BROLINK_PASS to pin it"
fi
"$ENGINE" "$CONF" --creds "$USER_NAME" "$PASS_NAME" >/dev/null 2>&1 || true

# Linux ProjectDirs for app "BroLink" is ~/.local/share/brolink, not
# ~/.local/share/brolink/BroLink (that extra folder is macOS-style).
HOST_TOML=/root/.local/share/brolink/host.toml
if [[ ! -f "$HOST_TOML" ]]; then
  cat >"$HOST_TOML" <<EOF
power_allowed = true
start_with_windows = true
stay_awake = true
sunshine_user = "${USER_NAME}"
sunshine_pass = "${PASS_NAME}"
EOF
fi

log "starting streaming engine"
"$ENGINE" "$CONF" >/var/log/sunshine.log 2>&1 &

log "starting BroLink control service"
exec /usr/local/bin/brolink-host --background
