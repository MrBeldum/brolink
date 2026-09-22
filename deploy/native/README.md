# Native desktop on the relay VPS

The deployed `relay` desktop runs directly on Ubuntu as user `ubuntu`, not in
Docker. The four remaining containers provide the Tailscale peer relay and the
three website services. Tailscale identity and streaming pairing were preserved
when retiring `brolink-node` on 2026-09-21 UTC.

## Runtime

- Xvfb provides a headless 1920x1080 display on `:1`; XFCE runs on the VPS host.
- Sunshine v2026.906.222525 (official Ubuntu 24.04 ARM64 package) captures X11
  and encodes H.264 in software. The VPS has no GPU encoder.
- `/usr/local/bin/brolink-host` runs the BroLink control service.
- `/usr/local/bin/tailscale` is the CLI only. It uses the existing relay
  container's socket at `/var/run/tailscale/tailscaled.sock`; no second daemon
  or Tailscale identity is installed on the host.
- `ubuntu` has systemd lingering enabled so the desktop starts without SSH login.
- The host name is `relay`; cloud-init is configured to preserve it.

## Files and services

The `.service` files in this directory are installed in
`~/.config/systemd/user/`. BroLink maintains its own `brolink.service` base unit;
`brolink.service.d/desktop.conf` adds the display environment and engine startup
ordering without conflicting with BroLink's automatic registration.
`wait-engine.py` is installed as `/usr/local/libexec/brolink-wait-engine.py`.

`brolink.service` pulls in `brolink-engine.service`,
`brolink-desktop.service`, and `brolink-display.service`. PulseAudio runs as
its standard user service. Check them as `ubuntu`:

```sh
systemctl --user status brolink brolink-engine brolink-desktop brolink-display
journalctl --user -u brolink-engine -n 50
```

Streaming configuration, certificates, and paired clients live in
`~/.config/sunshine/`. BroLink's host configuration lives in
`~/.local/share/brolink/host.toml`; its `engine/config` path is a symlink to the
Sunshine directory. Keep these private and preserve them during upgrades.

The X server disables TCP and uses an authentication cookie in
`/run/user/1001/brolink.Xauthority`. The `ubuntu` account is deliberately **not**
in the `input` group: Sunshine must use its XTest fallback for this Xvfb session.
Granting `/dev/uinput` access makes input go to kernel devices that Xvfb does not
consume, resulting in visible video but no functioning mouse or keyboard.

## Migration and recovery

The old container's complete `.config` and `.local` directories are saved in
`/home/ubuntu/brolink-native-migration/container-backup/` with private permissions.
They contain pairing credentials; do not commit or publish them.
The original Docker recipe remains available in `deploy/node/` for other uses,
but it must not be started on this VPS alongside the native services because
both would claim the same streaming and control ports.

Verified through the installed Mac BroLink app: existing pairing, 1080p/60 H.264
video, mouse clicks, terminal keyboard input, host identity (`relay`, `ubuntu`,
`kvm`, `/home/ubuntu`), and clean disconnect. User-manager restart is checked to
verify automatic startup without rebooting the websites or packet relay.
