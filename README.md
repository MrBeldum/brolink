# BroLink

**Your Windows PC, from your Mac — at game-streaming quality.**

BroLink is a personal remote-play app: sit at an Apple Silicon Mac, play
and use the Windows desktop in the other room or on another continent. The
picture is a low-latency H.264 stream (hardware encode on the PC, decode on
the Mac) with keyboard, mouse, gamepad, system audio, and clipboard. Setup
is two native apps and a pasteable ticket — no browser, no account, no
Electron.

This is bring-your-own-device, not a cloud. The pixels never leave *your*
machines, and after pairing they are encrypted end-to-end (ChaCha20-Poly1305).
A relay, if you use one, only ever sees ciphertext.

```
  MacBook (M-series)                         Windows PC
  ┌──────────────────┐                       ┌──────────────────┐
  │  BroLink         │   encrypted UDP       │  BroLink Host    │
  │  Client          │◄──── H.264 + PCM ────►│  DXGI / FFmpeg   │
  │  OpenH264 decode │      input + pad      │  AMF / NVENC     │
  └──────────────────┘                       └──────────────────┘
         ▲                                          ▲
         └──── LAN / IPv6 / UPnP / Tailscale / relay ────┘
```

## The everyday flow

1. Open BroLink on the Mac and click your PC under **Your PCs**.
2. If the PC is asleep, the Mac wakes it (Wake-on-LAN, also from another
   network through the router mapping the host set up) and connects once it
   answers, usually within 15 s.
3. Play, work, whatever. Click the picture to capture the mouse; **F8**
   (**fn+F8** on a Mac keyboard) frees it, and so does switching to another
   app.
4. Done? Free the mouse, open **PC ▾** in the overlay, and pick **Sleep** (or
   restart / shut down). The session ends cleanly and the PC goes down.

Sleep is the state to leave the PC in: it comes back in seconds with every
window still open, and the network card keeps listening for the wake packet.

## What you get

- Wake the PC from the Mac, and put it to sleep, restart it, or shut it down when you are done
- Hardware-accelerated capture and encode on the PC (AMD AMF, NVIDIA NVENC, Intel QSV, Media Foundation, libx264 fallback)
- 720p–1440p, 30–120 fps, 5–60 Mbps, with adaptive bitrate when the path gets lossy
- Keyboard, relative mouse (games), absolute mouse (desktop), Xbox-style gamepad via ViGEmBus
- System audio and a shared clipboard
- PIN pairing with persistent identities; the ticket pins the host key so a machine that stole the IP cannot impersonate it
- LAN discovery, automatic UPnP/NAT-PMP port mapping, IPv6, Tailscale `100.x`, and an optional self-hosted relay
- With a relay, a rendezvous service: the ticket keeps working after your home IP changes, and the host punches through port-restricted NATs
- Native GUIs on both sides

This is **not** a wrapper around Sunshine/Moonlight. Those projects are
excellent and inspired the encoder flags. BroLink is its own protocol,
apps, and pairing model.

## Install (the real-user path)

### Windows PC (host)

1. Install [Rust](https://rustup.rs) only if you are building from source. A
   release build of `brolink-host.exe` is enough to run.
2. From this repo:

   ```powershell
   cargo build --release -p brolink-host
   .\scripts\install-host.ps1
   ```

   The installer copies the host into `%LOCALAPPDATA%\BroLink`, downloads
   FFmpeg if it is missing, adds a desktop shortcut, and tries to open the
   firewall. Double-click **BroLink Host**.

3. Optional but recommended for games: [ViGEmBus](https://github.com/nefarius/ViGEmBus/releases)
   (virtual Xbox 360 controller). The host works without it; gamepads just
   will not reach the PC.

Leave the host running (or tick **Start with Windows** in its settings).
Copy the ticket.

### Mac (client)

```bash
cargo build --release -p brolink-client --target aarch64-apple-darwin
./scripts/bundle-macos.sh
open dist/BroLink.app
```

Paste the ticket, click **Connect**, enter the PIN once. Click the picture to
capture the mouse. **fn+F8** releases it (so does switching to another app).
**F11** fullscreen. **F7** hides the overlay. **Ctrl+Shift+Q** disconnects.

Use **borderless windowed** in games. Exclusive fullscreen can bypass Desktop
Duplication on some titles.

From a browser download of the `.app`, also run:

```bash
xattr -dr com.apple.quarantine BroLink.app
```

The app is ad-hoc signed; Gatekeeper otherwise reports it as damaged.

## Quality presets

| Preset | Resolution | FPS | Bitrate | Use |
|--------|------------|-----|---------|-----|
| Competitive | 1080p | 60 | 15 Mbps | Fast-twitch, long-haul |
| Balanced | 1080p | 60 | 25 Mbps | Default |
| Quality | 1440p | 60 | 40 Mbps | LAN / fat pipe |

The client can ask the host to step the bitrate down when it sees loss, then
back up when the path is clean.

## How worldwide access works

The host binds **one UDP socket** (default `47850`) and publishes every
address it can be reached on inside a pasteable `blk1_…` ticket:

1. LAN IPv4, plus any globally-routable IPv6
2. A UPnP / NAT-PMP mapping, if the router will create one (this is what
   makes most home connections work from another country with no extra software)
3. A STUN reflexive address (Google, then Cloudflare)
4. A Tailscale `100.64/10` address, if Tailscale is up
5. An optional `brolink-relay`, if you configured one

The client sends `Hello` to every candidate. The first `HelloAck` wins. It
also includes *its* STUN address so the host can send a packet back and
finish a hole punch. When the ticket names a relay, the client also asks the
relay's rendezvous service where the host is *now* and the relay tells the
host to punch towards the client, so a months-old ticket still connects.

**UPnP is the default internet path.** Enable it in the host (on by default)
and, if your router allows local applications to map a port, the WAN
candidate in the ticket is a real forward, not a hope. The mapping is
requested as permanent, so it survives the PC sleeping for days and a wake
packet from outside still reaches it.

If UPnP is blocked (CGNAT, locked-down ISP router, campus NAT), pick one of
these — they all work, none require changing the protocol:

| Path | Router changes | Notes |
|------|----------------|-------|
| **UPnP / NAT-PMP** | none (router cooperates) | Default. Host does it for you. |
| **Public IPv6** | none | Advertised automatically when the PC has a global address. |
| **Tailscale** | none | Install on both machines; connect to the `100.x` address. |
| **Self-hosted relay** | none | Both sides send *outbound* to the relay. Any NAT works. |
| **Port-forward** | UDP 47850 | Direct and lowest latency; needs router access. |

To run the relay yourself:

```bash
cargo run --release -p brolink-relay -- --bind 0.0.0.0:47851
```

then in the host UI set **Relay** to `your.vps.example:47851` (or start with
`--relay your.vps.example:47851`). The address and a random per-host token
ride inside the ticket, so the Mac picks the fallback up automatically. The
relay only ever sees already-encrypted bytes. See [docs/RELAY.md](docs/RELAY.md)
for a systemd unit and a Dockerfile.

The raw STUN candidate without UPnP is a bonus, not the internet path. The
host is reactive — it replies to the address a packet arrived from. On a
restricted-cone or symmetric NAT the first client packet is dropped unless
UPnP, IPv6, Tailscale, a relay, or a manual forward is in play.

## Testing

Unit tests cover the protocol, crypto, ticket parsing (including IPv6),
UPnP/NAT-PMP message codecs, adaptive bitrate, frame assembly, colour
conversion, input mapping, audio resampling, and relay routing:

```powershell
cargo test --workspace
```

The check that actually matters is the end-to-end loopback, which runs a real
host and a real client against the real GPU encoder on one machine:

```powershell
.\scripts\test-loopback.ps1
```

It builds first, waits for the host to publish a ticket, then connects twice —
once by bare address and once by ticket — and requires 30 decoded frames each
time. It also plays a 440 Hz tone for the host to capture and requires 100 ms
of it to reach the client's output device, with a non-zero peak. Pass
`-NoAudio` on a machine with no output device.

### Looking at the UI without a PC

Both windows can be rendered to PNGs on any machine with a GPU, no host, no
display and no Windows PC required:

```bash
cargo test -p brolink-client -p brolink-host snapshots -- --ignored
```

The images land in `target/ui-snapshots/`. Use them to review a visual change
before shipping it; they are not compared against anything.

### What this does not cover

The loopback runs a Windows client against a Windows host over `127.0.0.1`.
It says nothing about the Mac client, the relay, or NAT traversal, all of
which are unit-tested. Treat a green loopback as "the pipeline works", not
"the product works".

## Repository layout

```
crates/core     protocol, crypto, tickets, STUN, UPnP, discovery, wake, rendezvous
crates/host     Windows host (capture / encode / input / power / GUI)
crates/client   Mac + Windows client (decode / display / input / GUI)
crates/ui       the theme and widgets both GUIs are built from
crates/relay    optional UDP relay + rendezvous
deploy/         systemd unit and Dockerfile for the relay
docs/           protocol and platform notes
scripts/        macOS .app bundle, Windows installer, loopback test
```

## License

MIT. See [LICENSE](LICENSE) and [NOTICE](NOTICE) (FFmpeg and OpenH264 are
separate programs/libraries with their own terms).
