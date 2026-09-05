# BroLink

**Your Windows PC, on your Mac. One click, from anywhere.**

BroLink sits at a Mac and treats a Windows PC in another room or another
country as if it were on the desk: full screen, hardware-decoded HEVC or
AV1, keyboard, mouse, gamepad and audio at game-streaming latency.

It does this by leaning on three tools that already do the hard part well,
and doing only what they leave out:

| Tool | Does | BroLink adds |
|------|------|--------------|
| [Sunshine](https://github.com/LizardByte/Sunshine) on the PC | GPU capture and encode (AMD, NVIDIA, Intel), input, audio, pairing | Installs and configures it, accepts the pairing PIN so you never touch the PC |
| [Moonlight](https://moonlight-stream.org) on the Mac | VideoToolbox decode, 120 fps, HDR, gamepads, clipboard | Installs it, starts it with the right flags, one click |
| [Tailscale](https://tailscale.com) on both | An encrypted private network between your own devices, through any NAT | Finds your PCs on it and uses it as the identity check |
| BroLink itself | | **Wake the PC** (Tailscale cannot reach a sleeping machine), **sleep / restart / shut it down** from the Mac, and the setup |

```
  MacBook (M3+)                                          Windows PC
  ┌─────────────────┐                                   ┌────────────────────┐
  │ BroLink         │── wake packet ──► (LAN / subnet router) ──►│ network card       │
  │   lists PCs,    │── /v1/pin, /v1/power ── Tailscale ────────►│ BroLink Host       │
  │   launches      │                                   │   control service  │
  │ Moonlight       │◄════ HEVC/AV1 + audio + input ═══ Tailscale ═══►│ Sunshine (GPU) │
  └─────────────────┘                                   └────────────────────┘
```

Nothing passes through a server of ours. There is no account, no relay, no
custom protocol: the stream is Sunshine's, the transport is WireGuard, and
the identity check is your Tailscale login.

## The everyday flow

1. Open BroLink on the Mac. Your Windows PCs are listed, with whether each
   is ready, asleep, or missing something.
2. Click **Connect**. If the PC is asleep, BroLink wakes it and waits.
   The first time, it pairs Moonlight with the PC by itself.
3. Moonlight opens full screen. Use the PC. Ctrl+Alt+Shift+Q ends the
   session, or switch back to BroLink and click **Disconnect**.
4. BroLink asks whether to put the PC to sleep. Asleep is the state to
   leave it in: it wakes in seconds and uses almost nothing.

## Install

### Windows PC (host)

1. Install [Tailscale](https://tailscale.com/download/windows) and sign
   in with the same account you use on the Mac.
2. Download `brolink-windows-x64.zip` from the
   [latest release](https://github.com/MrBeldum/brolink/releases/latest),
   unzip, and run **brolink-host.exe**. (Or `install-host.ps1` for
   shortcuts and start-at-logon.)
3. Click **Set up this PC**. One administrator prompt does everything:
   installs Sunshine silently if it is missing, gives BroLink a login to
   it, opens the control port to your tailnet only, and arms the network
   card for Wake-on-LAN. Sunshine runs as a Windows service, so it is up
   again after a restart without anyone logging in.

That is all. Sunshine's own settings (encoder, display, HDR, audio device)
stay available at `https://localhost:47990`; the login is shown in the
BroLink window.

Wired Ethernet is strongly preferred: most Wi-Fi adapters cannot wake a PC.

### Mac (client, Apple Silicon)

Install [Tailscale](https://tailscale.com/download/mac) and sign in. Then
download `brolink-macos-arm64.tar.gz` from the
[latest release](https://github.com/MrBeldum/brolink/releases/latest) and

```bash
tar xzf brolink-macos-arm64.tar.gz
xattr -dr com.apple.quarantine BroLink.app
open BroLink.app
```

The `xattr` step is required for a browser download: the app is ad-hoc
signed rather than Developer-ID signed, so Gatekeeper calls it damaged
otherwise. With the [GitHub CLI](https://cli.github.com) signed in,
`scripts/install-macos-release.sh` does the download, the flag and the
copy to `/Applications` in one go (the repository is private, so a plain
`curl` cannot fetch releases). See [docs/MACOS.md](docs/MACOS.md) for
signing it properly.

BroLink offers to install Moonlight if it is not in `/Applications`.

## Waking the PC, honestly

A magic packet has to reach the PC's network card on its own network.
Tailscale cannot deliver it, because the sleeping PC's Tailscale is asleep
too. BroLink sends the packet:

- to the LAN broadcast, which works whenever the Mac is on the same network;
- to the PC's LAN address, which works from anywhere **if** something on
  that network routes into it, such as a [Tailscale subnet
  router](https://tailscale.com/kb/1019/subnets) on a NAS, a Raspberry
  Pi, or a router that runs Tailscale. The card keeps answering ARP while
  asleep (BroLink turns ARP offload on), so a unicast reaches it.

| PC state | From the same network | From elsewhere |
|----------|----------------------|----------------|
| Asleep | yes | with a subnet router on the PC's network |
| Shut down | usually, if the BIOS allows wake from S5 | rarely |

So: sleep, do not shut down, when you leave. BroLink learns the MAC and LAN
address the first time it sees the PC awake with BroLink Host running.

## Settings that matter

| Setting | Default | Notes |
|---------|---------|-------|
| Mouse | Desktop | 1:1 cursor for desktop use; **Game** sends raw movement for first-person games |
| Resolution | 1440p | **This Mac** streams the Mac's exact pixel size. Pixel-for-pixel only with a virtual display on the PC ([Apollo](https://github.com/ClassicOldSong/Apollo) creates one automatically; BroLink works with Apollo as a drop-in for Sunshine). With a physical monitor, Sunshine scales it. |
| Frame rate | 60 | 120 on a ProMotion Mac with a fast link |
| Bitrate | 30 Mbps | Raise on a LAN or a direct Tailscale path; lower on a thin uplink |
| Codec | Auto | Moonlight negotiates AV1 if the M3 and the GPU both do it (RX 7000 / RTX 40 and up), else HEVC |

## Security model

- The host's control service listens on TCP 47850 but answers only
  loopback and Tailscale addresses that `tailscale whois` attributes to
  **the same account the PC is signed in as**. Everyone else gets a 403.
  The firewall rule setup adds is scoped to `100.64.0.0/10`.
- The stream itself is Moonlight to Sunshine over Tailscale (WireGuard),
  with Sunshine's own certificate pairing on top.
- No BroLink credentials exist. The Sunshine web login BroLink generates
  stays on the PC.
- Remote power actions can be turned off in the host window.

## Building from source

```powershell
cargo build --release -p brolink-host        # Windows
```

```bash
./scripts/install-macos.sh                    # macOS: builds, bundles, installs
```

Tests, lint and the UI snapshots (PNGs of every screen, no display needed):

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p brolink-client -p brolink-host snapshots -- --ignored
```

## Repository layout

```
crates/core     control API types, tiny HTTP, Tailscale CLI, wake packets, downloads
crates/host     Windows: background control service + control panel + setup
crates/client   macOS: PC list, wake → pair → Moonlight, power menu
crates/ui       the theme and widgets both windows are built from
docs/           platform notes
scripts/        installers and the macOS bundle
```

## License

MIT. See [LICENSE](LICENSE) and [NOTICE](NOTICE). Sunshine, Moonlight and
Tailscale are separate programs with their own licenses; BroLink downloads
them from their own release pages and never redistributes them.
