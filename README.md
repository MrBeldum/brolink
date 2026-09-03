# ForgeLink

**Your Windows PC, from your Mac — at game-streaming quality.**

ForgeLink is an open-source personal-cloud layer for a Windows desktop,
controlled from an Apple Silicon Mac (or another Windows machine). This
release is **Remote Play**: a fully working, low-latency H.264 desktop
stream with keyboard, mouse, gamepad, and system audio, on the LAN and
across the internet.

Later releases add backup and extra compute on the same pair of apps. The
protocol, pairing, and networking are built so those features plug in
without replacing the remote-play path.

```
  MacBook (M-series)                         Windows PC
  ┌──────────────────┐                       ┌──────────────────┐
  │  ForgeLink       │   encrypted UDP       │  ForgeLink Host  │
  │  Client          │◄──── H.264 + PCM ────►│  DXGI / FFmpeg   │
  │  OpenH264 decode │      input + pad      │  AMF / NVENC     │
  └──────────────────┘                       └──────────────────┘
         ▲                                          ▲
         └──────── ticket / Tailscale / STUN ───────┘
```

## What works right now (v0.1 Remote Play)

- Hardware-accelerated capture and encode on the PC (AMD AMF, NVIDIA NVENC, Intel QSV, Media Foundation, libx264 fallback)
- 720p–1440p, 30–120 fps, 5–60 Mbps
- Keyboard, relative mouse (games), absolute mouse (desktop), Xbox-style gamepad via ViGEmBus
- System audio loopback
- PIN pairing with persistent identities
- LAN discovery
- Across the internet via Tailscale `100.x` addresses, a self-hosted UDP relay, or a manual port-forward. See [How worldwide access works](#how-worldwide-access-works) -- the STUN address alone is not enough on most routers
- Native GUIs on both sides (no browser, no Electron)

This is **not** a thin wrapper around Sunshine/Moonlight. Those projects are
excellent and inspired the encoder flags and the “short GOP, drop stale
frames” playbook. ForgeLink is its own protocol, apps, and pairing model so
backup and compute can share the same session later.

## Quick start

### 1. Windows PC (host)

Install [Rust](https://rustup.rs) and [FFmpeg](https://www.gyan.dev/ffmpeg/builds/)
(the essentials build is enough). Then:

```powershell
git clone https://github.com/MrBeldum/forgelink
cd forgelink
cargo build --release -p forgelink-host
.\target\release\forgelink-host.exe
```

Leave the window open. Copy the **ticket**.

Useful flags:

| Flag | Effect |
|------|--------|
| `--headless` | No control panel; prints the ticket and logs to stdout |
| `--name NAME` | Override the advertised PC name |
| `--port N` | UDP port (default 47850) |
| `--no-pin` | Trust any client that can reach this PC (LAN testing only) |
| `--relay HOST:PORT` | Advertise a `forgelink-relay` for hard-NAT clients |
| `--no-firewall` | Do not try to add a Windows Firewall rule on startup |
| `--no-audio` | Do not capture or stream system audio for this run |

Optional but recommended for games:

- [ViGEmBus](https://github.com/nefarius/ViGEmBus/releases) — virtual Xbox 360 controller
- [Tailscale](https://tailscale.com) — worldwide access with no port-forward

### 2. Mac (client)

On the MacBook:

```bash
cargo build --release -p forgelink-client
./scripts/bundle-macos.sh   # optional .app
./target/release/forgelink-client
```

Paste the ticket, click **Connect**, enter the PIN once, click the picture to
capture the mouse. **F8** releases the mouse. **F11** fullscreen.
**Ctrl+Shift+Q** disconnects.

Use **borderless windowed** in games.

## Quality presets

| Preset | Resolution | FPS | Bitrate | Use |
|--------|------------|-----|---------|-----|
| Competitive | 1080p | 60 | 15 Mbps | Fast-twitch, long-haul |
| Balanced | 1080p | 60 | 25 Mbps | Default |
| Quality | 1440p | 60 | 40 Mbps | LAN / fat pipe |

## How worldwide access works

The host binds **one UDP socket** (default `47850`) and:

1. Advertises its LAN address on the local broadcast / multicast group
2. Asks Google/Cloudflare STUN for a reflexive address and keeps the mapping alive
3. Shows a Tailscale address if a `100.64/10` interface exists
4. Packs all of that into a pasteable `flk1_…` ticket

The client sends `Hello` to every candidate. The first `HelloAck` wins.

**The STUN candidate is a bonus, not the internet path.** The host is purely
reactive -- it only ever replies to an address a packet arrived from, and
never sends first. So your client's opening packet reaches it only if the
PC's router uses endpoint-independent filtering ("full cone"). A
restricted-cone or symmetric NAT drops it, and ForgeLink has no signalling
channel to coordinate a simultaneous open and no UPnP/NAT-PMP to request a
forward. On most home routers, expect the WAN candidate to time out.

For reliable access from anywhere, pick one of these three:

| Path | Router changes | Notes |
|------|----------------|-------|
| **Tailscale** | none | Easiest. Install on both machines, connect to the `100.x` address. Tailscale handles traversal and falls back to its own relays. |
| **Self-hosted relay** | none | Both sides send *outbound* to the relay, so no NAT has to accept an unsolicited packet. Costs you a VPS. |
| **Port-forward** | UDP 47850 | Direct and lowest latency, but exposes the port and needs router access. |

To run the relay yourself:

```bash
cargo run --release -p forgelink-relay -- --bind 0.0.0.0:47851
```

then point the host at it:

```powershell
.\target\release\forgelink-host.exe --relay your.vps.example:47851
```

The relay address and a random per-host token ride along inside the ticket, so
clients pick the fallback up automatically. The relay only ever sees
already-encrypted bytes.

## Testing

Unit tests cover the protocol, crypto, ticket parsing, frame assembly,
colour conversion, input mapping, audio resampling, and relay routing:

```powershell
cargo test --workspace
```

The check that actually matters is the end-to-end loopback, which runs a real
host and a real client against the real GPU encoder on one machine:

```powershell
.\scripts\test-loopback.ps1
```

It builds first, waits for the host to publish a ticket, then connects twice --
once by bare address and once by ticket -- and requires 30 decoded frames each
time. On failure it prints the host's log, because most real failures are on
the capture/encode side.

It also plays a 440 Hz tone for the host to capture and requires 100 ms of it
to reach the client's output device, with a non-zero peak. The peak matters:
WASAPI loopback reports silent buffers rather than stopping, so counting
frames alone would pass on a stream of digital silence. Pass `-NoAudio` on a
machine with no output device.

### What this does not cover

The loopback runs a Windows client against a Windows host over `127.0.0.1`.
It says nothing about the Mac client, the relay, or NAT traversal, all of
which are only unit-tested. Treat a green loopback as "the pipeline works",
not "the product works".

## Repository layout

```
crates/core     protocol, crypto, tickets, STUN, discovery
crates/host     Windows host (capture / encode / input / GUI)
crates/client   Mac + Windows client (decode / display / input / GUI)
crates/relay    optional UDP relay
docs/           protocol and platform notes
scripts/        macOS .app bundle, Windows install shortcut
```

## License

MIT. See [LICENSE](LICENSE) and [NOTICE](NOTICE) (FFmpeg and OpenH264 are
separate programs/libraries with their own terms).

## Roadmap

See [docs/ROADMAP.md](docs/ROADMAP.md). Next up: backup (the PC as a
personal file vault) and extra compute (jobs on the Windows box from the Mac).
