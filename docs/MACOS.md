# macOS client (Apple Silicon)

BroLink's client is native Rust (`eframe` + OpenH264). It compiles for
`aarch64-apple-darwin` and `x86_64-apple-darwin`. M-series Macs are the
target; Intel Macs work but are not the design point.

## Install a prebuilt client

Download `brolink-macos-arm64.tar.gz` from the GitHub Actions **release**
run, then:

```bash
tar xzf brolink-macos-arm64.tar.gz
xattr -dr com.apple.quarantine BroLink.app
open BroLink.app
```

The `xattr` step is not optional. The app is ad-hoc signed rather than signed
with an Apple Developer ID, so Gatekeeper quarantines anything downloaded
through a browser and reports it as damaged. Removing the quarantine flag is
what tells macOS you fetched it deliberately.

To watch the logs, run the binary inside the bundle directly:

```bash
RUST_LOG=info BroLink.app/Contents/MacOS/BroLink
```

## Build on a Mac

```bash
rustup target add aarch64-apple-darwin
cargo build --release -p brolink-client --target aarch64-apple-darwin
./scripts/bundle-macos.sh
```

The script writes `dist/BroLink.app`. Drag it to `/Applications`.

## Run from the repo

```bash
cargo run --release -p brolink-client
```

## First connection

1. On the PC, copy the ticket from BroLink Host.
2. On the Mac, paste it and click **Connect** (or pick the PC from **PCs on
   this network** if you are on the same LAN).
3. Type the 6-digit PIN shown on the PC. After that the Mac is on the
   allow-list and will not be asked again.
4. Click the picture to capture the mouse.

The client remembers the PC. Next time, click it in **Your PCs**. If the
session drops, it reconnects on its own unless you disconnected.

## Permissions

- **Microphone / camera**: not required (the Mac is the client).
- **Input Monitoring**: not required; we only capture input inside our window.
- **Local network**: macOS 15+ may prompt. Allow it so LAN discovery works.
- Game controllers work through HID (Xbox, DualSense, etc.).

## Controls while streaming

| Key | Action |
|-----|--------|
| Click the picture | Capture mouse (relative, for games) |
| **F8** | Release / recapture mouse |
| **F11** | Fullscreen |
| **F7** | Hide / show the HUD |
| **Ctrl+Shift+Q** | Disconnect |

## Connecting across the world

Same ticket. The host's UPnP mapping, public IPv6, Tailscale address, or
relay is already inside it. You do not configure NAT on the Mac.

If the connection times out:

1. Confirm the host window says it is reachable (UPnP mapped, Tailscale, or
   relay). A STUN address alone is often not enough.
2. Confirm the Windows firewall is not blocking UDP 47850 (see
   [WINDOWS.md](WINDOWS.md)).
3. On CGNAT (many mobile ISPs, some fibre), set a relay on the host or
   install Tailscale on both machines.
