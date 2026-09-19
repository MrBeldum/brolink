# BroLink

Your machines, on any of your machines: stream a desktop full screen over
Tailscale. Mac, Windows, Linux, and a VPS container all run the same BroLink
app. Each one lists the others and can Connect; each one can share its own
desktop.

On a Mac, frames are decoded with VideoToolbox. On Windows and Linux the
viewer uses OpenH264. Sharing uses the streaming engine BroLink installs
(Sunshine): bundled in the Windows zip, downloaded on first Mac setup, and
shipped in the Docker node image for a VPS.

[Tailscale](https://tailscale.com) is the path and the identity check. A
machine answers only peers signed in to the same Tailscale account.

## How a session goes

1. Open BroLink. Every machine on your Tailscale account is listed with
   what it can do right now. **Share this machine** sets up the streaming
   engine so others can Connect here.
2. Click **Connect**. If a Windows PC is asleep, BroLink wakes it and waits.
   The first time, it pairs by itself: the PIN goes to BroLink on that
   machine over Tailscale, which enters it for you.
3. The desktop appears, full screen by default, and nothing else: the
   mouse is captured the moment you click the picture, as in a game, so
   games that read raw mouse movement work. **Ctrl+Alt** is the host key,
   as in a hypervisor: it frees the mouse and drops a toolbar over the
   picture with the stream details, whether the path is direct or relayed,
   the mouse mode, a **Keys** menu for Ctrl+Alt+Del and friends and what the
   Command key does, stats, full screen, the PC's power menu, **Stream
   settings** (the default asks for this screen's own size at 50 Mbps, on
   every path) and **Disconnect**. Click the picture, or press Ctrl+Alt
   again, and the toolbar goes away. The clipboard follows you both ways.
4. Leave the PC on if you want to connect from anywhere. Asleep, Tailscale
   is asleep too: this Mac can wake it only from that PC's own network
   (or if the router forwards UDP 9). Settings can offer sleep after a
   session; that is off by default.

## Install

### Windows PC

1. Install [Tailscale](https://tailscale.com/download/windows) and sign in
   with the account you use on the Mac.
2. Download `brolink-windows-x64.zip` from the
   [latest release](https://github.com/MrBeldum/brolink/releases/latest)
   and unzip it. Run `brolink-host.exe`, or `install-host.ps1` for
   shortcuts and start-at-logon.
3. Click **Share this machine**. One administrator prompt installs the bundled
   streaming engine as a Windows service (if none is installed), gives
   BroLink a login to it, opens the control port to your tailnet only,
   turns Fast Startup off and arms the network card for Wake-on-LAN.
   The same window lists every other machine on the account; **Connect**
   opens their desktop.

Advanced engine settings are not exposed; BroLink configures the engine
itself. Details in [docs/WINDOWS.md](docs/WINDOWS.md).

### Mac (Apple Silicon)

Install [Tailscale](https://tailscale.com/download/mac) and sign in. One
command downloads the app, clears the quarantine flag and copies it to
`/Applications`:

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
ad-hoc signed rather than Developer-ID signed. Open BroLink and click
**Share this machine** so a Windows PC (or another Mac) can Connect here;
macOS will ask for Screen Recording the first time. Details, including the
toolbar and keyboard behaviour, in [docs/MACOS.md](docs/MACOS.md).

### Linux / VPS (Docker)

A container that shares a virtual desktop on the tailnet, so Mac and
Windows BroLink can Connect to it. Several copies on one host are several
machines. See [deploy/node/NODE.md](deploy/node/NODE.md).

```bash
cp deploy/node/env.example deploy/node/.env
# paste a Tailscale auth key, pick TS_HOSTNAME
docker compose -f deploy/node/docker-compose.yml up -d --build
```

The packet relay (when two NATs cannot punch through) is a separate kit:
[deploy/relay/RELAY.md](deploy/relay/RELAY.md).

## Updates

BroLink keeps itself current. Every few hours the Mac app asks GitHub for
the latest release. A newer app is downloaded, checked against the digest
GitHub publishes and its own code signature, swapped into `/Applications`
once no stream is running, and relaunched. A newer BroLink Host is sent
from the Mac to every PC whose host reports an older version, over the same
Tailscale-authenticated control API that can put the PC to sleep; the host
verifies the digest, replaces its executable and restarts. That path
updates only `brolink-host.exe`. The Windows zip on GitHub also contains
the pinned engine archive for first-time **Set up this PC**; a host update
does not install or migrate the engine. A PC that is asleep gets the host
update the next time the Mac sees it. Nothing is downloaded on the PC, and
no GitHub login is needed there.

Public releases need no GitHub login. If the repository is private, the Mac
uses the GitHub token git has stored for github.com,
`BROLINK_GITHUB_TOKEN`, or `github_token` in `client.toml`. Settings has the
switch and a **Check now** button. Hosts installed before 3.1 do not have the
update route: install that release on the PC once (through the stream works),
after which updates are automatic.

## Staying reachable

A PC nobody can get to in person stays reachable when three things hold.
BroLink Host starts with Windows by default and turns this back on at every
start unless the owner switches it off in the host window. The streaming
engine runs as a Windows service, so streaming works even before anyone
logs in. And the
PC's Tailscale node key must not expire: Tailscale keys expire after 180
days unless key expiry is disabled for that machine in the
[admin console](https://login.tailscale.com/admin/machines), and an expired
key needs a sign-in at the PC. BroLink shows the expiry of every machine it
lists, this Mac included, until expiry is disabled. Pairing and the PC's
addresses are saved on the Mac, so PCs stay listed even while Tailscale on
the Mac is off.

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
- The stream is BroLink's stream protocol over Tailscale (WireGuard), with
  certificate pairing on top; BroLink pins the PC's certificate after the
  first pairing.
- No BroLink account or password exists. The engine login BroLink
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
that run against a real engine.

## Layout

```
crates/core     control API types, small HTTP, Tailscale CLI, wake packets, config
crates/stream   the stream client: pairing, launch, protocol, decode, audio
crates/host     node: control service, engine setup, unified window (view + share)
crates/client   viewer UI: machine list, wake, pair, stream window and toolbar
crates/ui       theme and widgets shared by both windows
deploy/node/    Docker kit: virtual desktop + engine + BroLink on a VPS
deploy/relay/   Docker kit: Tailscale peer relay
third_party/    moonlight-common-c (GPL-3.0), vendored
docs/           platform notes
scripts/        installers and the macOS bundle
```

## License

GPL-3.0-or-later; see [LICENSE](LICENSE). BroLink compiles in
moonlight-common-c (GPL-3.0) and ships Sunshine's Windows lite archive
(GPL-3.0) unmodified; on the PC, setup gives the unpacked executables
BroLink's name and icon and keeps their copyright and licence strings.
Third-party notices are in [NOTICE](NOTICE).
