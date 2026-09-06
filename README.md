# BroLink

Your Windows PC, on your Mac, from anywhere: wake it, stream it full
screen with hardware decode, put it back to sleep.

BroLink is two programs. **BroLink Host** runs on the Windows PC. **BroLink**
runs on the Mac and contains the whole streaming client: Moonlight's
GameStream library is compiled in, frames are decoded with VideoToolbox and
drawn in BroLink's own window. Nothing else has to be installed on the Mac.
On the PC, BroLink Host installs the [Sunshine](https://github.com/LizardByte/Sunshine)
streaming server that ships in its zip and configures it, so the PC needs
no download either.

[Tailscale](https://tailscale.com) connects the two. It carries the stream
through any NAT and it is the identity check: the host answers only a Mac
signed in to the same Tailscale account as the PC.

## How a session goes

1. Open BroLink on the Mac. The Windows PCs on your Tailscale account are
   listed with what each one can do right now.
2. Click **Connect**. If the PC is asleep, BroLink wakes it and waits. The
   first time, it pairs with the PC by itself: the PIN goes to BroLink Host
   over Tailscale, which enters it in Sunshine.
3. The desktop appears, full screen by default. A toolbar above the picture
   has the stream details, mouse capture, what the Command key does, a
   **Keys** menu for Ctrl+Alt+Del and friends, stats, full screen, the PC's
   power menu and **Disconnect**.
4. When you disconnect, BroLink offers to put the PC to sleep. Asleep is
   the state to leave it in: it wakes in seconds and draws almost nothing.

## Install

### Windows PC

1. Install [Tailscale](https://tailscale.com/download/windows) and sign in
   with the account you use on the Mac.
2. Download `brolink-windows-x64.zip` from the
   [latest release](https://github.com/MrBeldum/brolink/releases/latest)
   and unzip it. Run `brolink-host.exe`, or `install-host.ps1` for
   shortcuts and start-at-logon.
3. Click **Set up this PC**. One administrator prompt installs the bundled
   Sunshine as a Windows service (if none is installed), gives BroLink a
   login to it, opens the control port to your tailnet only, turns Fast
   Startup off and arms the network card for Wake-on-LAN.

Sunshine's own settings (encoder, display, HDR, audio device) stay at
`https://localhost:47990`; the login is shown in the BroLink Host window.
Details in [docs/WINDOWS.md](docs/WINDOWS.md).

### Mac (Apple Silicon)

Install [Tailscale](https://tailscale.com/download/mac) and sign in. With
the [GitHub CLI](https://cli.github.com) signed in (the repository is
private), one command downloads the app, clears the quarantine flag and
copies it to `/Applications`:

```bash
curl -fsSL https://raw.githubusercontent.com/MrBeldum/brolink/main/scripts/install-macos-release.sh | bash
```

Or download `brolink-macos-arm64.tar.gz` from the
[latest release](https://github.com/MrBeldum/brolink/releases/latest) and

```bash
tar xzf brolink-macos-arm64.tar.gz
xattr -dr com.apple.quarantine BroLink.app
open BroLink.app
```

The `xattr` step is needed for a browser download because the app is
ad-hoc signed rather than Developer-ID signed. Details, including the
toolbar and keyboard behaviour, in [docs/MACOS.md](docs/MACOS.md).

## Waking the PC

A wake packet has to reach the PC's network card on the PC's own network.
Tailscale cannot deliver it, because the sleeping PC's Tailscale is asleep
too. BroLink sends the packet to the LAN broadcast, to the PC's LAN address
and to the PC's public address.

| Where the Mac is | Asleep | Shut down |
|------------------|--------|-----------|
| Same network as the PC | works | works if the board's firmware allows wake from power off |
| Elsewhere | needs the PC's router to forward UDP 9 to the PC, or a Tailscale subnet router on that network | same, and the firmware condition |

**Test wake** in the PC menu on the Mac settles it for the network you are
on: it sends the packet while the PC is awake and asks BroLink Host whether
it arrived. Setup on the PC takes care of Windows' side (Fast Startup off,
the adapter's wake keywords, wake allowed in power management). Wired
Ethernet is strongly preferred; most Wi-Fi adapters cannot wake a PC.

## Security

- The host's control service listens on TCP 47850 and answers only
  loopback and Tailscale addresses that `tailscale whois` attributes to the
  account the PC is signed in as. Everyone else gets a 403. The firewall
  rule setup adds is scoped to `100.64.0.0/10`.
- The stream is Sunshine's GameStream protocol over Tailscale (WireGuard),
  with Sunshine's certificate pairing on top; BroLink pins the PC's
  certificate after the first pairing.
- No BroLink account or password exists. The Sunshine login BroLink
  generates stays on the PC.
- Remote power actions can be turned off in the host window.

## Building

Needs Rust, a C compiler (MSVC Build Tools or Xcode command line tools) and
CMake (for the Opus decoder).

```powershell
cargo build --release -p brolink-host          # Windows
```

```bash
./scripts/install-macos.sh                     # macOS: builds, bundles, installs
```

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for the UI snapshots and the tests
that run against a real Sunshine.

## Layout

```
crates/core     control API types, small HTTP, Tailscale CLI, wake packets, config
crates/stream   the GameStream client: pairing, launch, moonlight-common-c, decode, audio
crates/host     Windows: background control service, control panel, setup script
crates/client   macOS: PC list, wake, pair, stream window and toolbar
crates/ui       theme and widgets shared by both windows
third_party/    moonlight-common-c (GPL-3.0), vendored
docs/           platform notes
scripts/        installers and the macOS bundle
```

## License

GPL-3.0-or-later; see [LICENSE](LICENSE). BroLink compiles in
moonlight-common-c (GPL-3.0) and ships Sunshine's installer (GPL-3.0)
unmodified. Third-party notices are in [NOTICE](NOTICE).
