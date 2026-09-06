# macOS client (Apple Silicon)

BroLink for the Mac is one native app: your PCs, a Connect button each, a
power menu, and the stream itself. The GameStream client library from
Moonlight is compiled in, video is decoded by VideoToolbox (H.264 and
HEVC), audio by Opus, and the picture is drawn in BroLink's own window
under a toolbar.

## Requirements

- macOS 13+ on Apple Silicon
- [Tailscale](https://tailscale.com/download/mac), signed in with the same
  account as the PC

## Install a prebuilt app

With the [GitHub CLI](https://cli.github.com) installed and signed in
(`brew install gh && gh auth login`):

```bash
curl -fsSL https://raw.githubusercontent.com/MrBeldum/brolink/main/scripts/install-macos-release.sh | bash
```

It fetches the release through `gh` (the repository is private, so a plain
`curl` cannot), clears the quarantine flag, copies the app to
`/Applications` and opens it.

By hand: download `brolink-macos-arm64.tar.gz` from the
[latest release](https://github.com/MrBeldum/brolink/releases/latest), then

```bash
tar xzf brolink-macos-arm64.tar.gz
xattr -dr com.apple.quarantine BroLink.app
open BroLink.app
```

The `xattr` step is required for a browser download. The app is ad-hoc
signed rather than signed with an Apple Developer ID, and macOS 15 reports
such apps as damaged instead of offering "Open anyway". Removing the
quarantine flag is how you tell macOS you fetched it deliberately.

## Build on a Mac

```bash
./scripts/install-macos.sh
```

builds for `aarch64-apple-darwin` (Xcode command line tools and CMake are
needed for the C parts), bundles `dist/BroLink.app`, and installs it to
`/Applications`. A locally built app never carries the quarantine flag.

### Signing

`bundle-macos.sh` signs ad-hoc by default. For a build other people can
download without the quarantine step, sign with a Developer ID and
notarize:

```bash
CODESIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)" ./scripts/bundle-macos.sh
ditto -c -k --keepParent dist/BroLink.app dist/BroLink.zip
xcrun notarytool submit dist/BroLink.zip --keychain-profile "notary" --wait
xcrun stapler staple dist/BroLink.app
```

## First run

1. If Tailscale is missing or signed out, the top card says so and **Get
   Tailscale** opens the download page.
2. **Your PCs** lists the Windows machines on your Tailscale account. Each
   line says whether it is ready, asleep, or missing something on the PC
   side.
3. Click **Connect**. The first time with a PC, BroLink pairs with it: it
   picks a random PIN, sends it to BroLink Host over Tailscale, and the host
   enters it in Sunshine. You see the PIN but never need it. If there is no
   BroLink Host on the PC, the PIN stays on screen for someone at the PC to
   type into Sunshine's web page.
4. The PC's desktop appears.

## While streaming

The picture is letterboxed to the PC's aspect ratio. The toolbar sits above
it (in the top letterbox bar in full screen when there is one; otherwise it
drops down when the pointer touches the top edge) and the status line, with
the session clock and the stream stats when they are on, sits below.

| Toolbar item | What it does |
|--------------|--------------|
| **● PC name · 1920×1080 · HEVC · 60 fps** | The stream as negotiated; the dot turns amber when the connection is poor |
| **Mouse: free / captured** | Free: the Mac cursor moves 1:1 on the PC and leaves the window normally. Captured: the cursor is hidden and raw movement is sent, for games. **Ctrl+Alt** toggles; a click on the picture captures |
| **⌘ = Ctrl / ⌘ = Win** | What the Command key does on the PC; click to switch |
| **Keys** | Ctrl+Alt+Del, Windows key, Alt+Tab, Esc, Print Screen, for keys macOS keeps for itself |
| **Stats** | fps, bitrate, round trip, decode time, decoder name in the status line |
| **Full screen** | Toggle; the setting decides how a session starts |
| **PC** | Sleep, Restart…, Shut down… (the latter two ask twice) |
| **Disconnect** | End the session; BroLink then offers to sleep the PC |

Everything else on the keyboard goes to the PC as pressed. With **⌘ =
Ctrl** on (the default), ⌘C, ⌘V, ⌘Z and the rest do on Windows what they
do on the Mac.

## The PC menu in the lobby

Next to a ready PC, **PC** offers **Sleep**, **Restart…**, **Shut
down…** and **Test wake**. Restart and shut down ask for a second click;
both force-close programs on the PC. The menu only appears if the host
allows remote power actions.

**Test wake** sends the wake packet to the awake PC and asks BroLink Host
whether it arrived. A pass means waking the PC from where you are will
work; a failure means the packets are not reaching it from this network
(see below). **Wake** appears next to a PC that is asleep and whose wake
details BroLink has learned; it sends the packet without connecting.

## Settings

| Setting | What it changes |
|---------|-----------------|
| Resolution | 1080p, 1440p, 4K, or this screen's own pixel size. Exact only with a virtual display on the PC ([Apollo](https://github.com/ClassicOldSong/Apollo) provides one); otherwise the PC's monitor is scaled |
| Frame rate | 60, 90, 120 |
| Bitrate | 5 to 150 Mbps |
| Codec | Auto (HEVC when the PC can encode it), HEVC, H.264 |
| Full screen | How a session starts |
| App | The Sunshine app to launch; "Desktop" is the whole PC. The list fills in after the first connection |
| Command key acts as Ctrl | Off makes ⌘ the Windows key |
| Offer to sleep the PC after each session | The prompt when a session ends |

Settings are saved in `~/Library/Application Support/BroLink/client.toml`,
along with the MAC, LAN address and Sunshine certificate of each PC BroLink
has paired with.

## Waking a PC from another network

The wake packet is sent to the LAN broadcast, to the PC's LAN address and
to the PC's public address. Over the internet only the last can arrive,
and only if the PC's router forwards UDP 9 to the PC. A [Tailscale subnet
router](https://tailscale.com/kb/1019/subnets) on the PC's network (a NAS,
a Raspberry Pi, or a router that runs Tailscale) is the alternative. Use
**Test wake** while the PC is on to find out which situation you are in.

## Permissions

- **Local network**: macOS 15 may ask; allow it so the wake broadcast can
  go out.
- Nothing else. BroLink captures no screen and reads the keyboard and
  mouse only in its own window.
