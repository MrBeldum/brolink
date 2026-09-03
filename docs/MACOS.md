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

1. **Best: [Tailscale](https://tailscale.com)** on the Mac and the PC, then connect
   to the `100.x.y.z` address shown on the host. Tailscale does the NAT
   traversal, including its own relay fallback, and needs no router changes.
2. **Self-hosted relay**: run `forgelink-relay` on a VPS and start the host
   with `--relay host:port`. Both machines send *outbound* to the relay, so
   no router accepts an unsolicited packet and any NAT works.
3. **Manual port-forward** of UDP 47850 on the PC's router.

The STUN address in the ticket is **not** a fourth option on its own. It only
works if the PC's router uses endpoint-independent filtering ("full cone"),
because the host never sends anything to your Mac before your Mac's first
packet arrives -- so a restricted-cone or symmetric NAT drops it. There is no
signalling channel to coordinate a simultaneous open, and no UPnP. Try it if
you like; if the connection times out on the WAN candidate, that is why.
