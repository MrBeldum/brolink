# macOS client (Apple Silicon)

ForgeLink's client is native Rust (`eframe` + OpenH264). It compiles for
`aarch64-apple-darwin` and `x86_64-apple-darwin`.

## Install a prebuilt client

Download `forgelink-macos-arm64.tar.gz` from the GitHub Actions **release**
run, then:

```bash
tar xzf forgelink-macos-arm64.tar.gz
xattr -dr com.apple.quarantine ForgeLink.app
open ForgeLink.app
```

The `xattr` step is not optional. The app is ad-hoc signed rather than signed
with an Apple Developer ID, so Gatekeeper quarantines anything downloaded
through a browser and reports it as damaged. Removing the quarantine flag is
what tells macOS you fetched it deliberately.

To watch the logs, run the binary inside the bundle directly:

```bash
RUST_LOG=info ForgeLink.app/Contents/MacOS/ForgeLink
```

## Build on a Mac

```bash
rustup target add aarch64-apple-darwin
cargo build --release -p forgelink-client --target aarch64-apple-darwin
./scripts/bundle-macos.sh
```

The script writes `dist/ForgeLink.app`. Drag it to `/Applications`.

## Run from the repo

```bash
cargo run --release -p forgelink-client
```

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
| **Ctrl+Shift+Q** | Disconnect |

## Connecting across the world

1. Best: install [Tailscale](https://tailscale.com) on the Mac and the PC, then connect to the `100.x.y.z` address shown on the host.
2. Good: paste the host ticket. The WAN address inside it is a STUN mapping — works on most home routers.
3. Fallback: port-forward UDP 47850 on the PC's router, or run `forgelink-relay` on a VPS.
