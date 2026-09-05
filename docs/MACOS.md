# macOS client (Apple Silicon)

BroLink for the Mac is a small native window: your PCs, a Connect button
each, a power menu, and the settings Moonlight is started with. Moonlight
does the streaming, with VideoToolbox hardware decode of HEVC and AV1.

## Requirements

- macOS 13+ on Apple Silicon (M3 or newer gets AV1 decode; any M-series
  works with HEVC)
- [Tailscale](https://tailscale.com/download/mac), signed in with the same
  account as the PC
- Moonlight, which BroLink installs for you

## Install a prebuilt app

Download `brolink-macos-arm64.tar.gz` from the
[latest release](https://github.com/MrBeldum/brolink/releases/latest), then

```bash
tar xzf brolink-macos-arm64.tar.gz
xattr -dr com.apple.quarantine BroLink.app
open BroLink.app
```

The `xattr` step is required for a browser download. The app is ad-hoc
signed rather than signed with an Apple Developer ID, and macOS 15 no longer
offers "Open anyway" from the context menu for such apps; it reports them as
damaged. Removing the quarantine flag is how you tell macOS you fetched it
deliberately.

With the [GitHub CLI](https://cli.github.com) installed and signed in
(`brew install gh && gh auth login`), one command does all of that:

```bash
bash scripts/install-macos-release.sh
```

It fetches the release through `gh` (the repository is private, so a plain
`curl` cannot), strips the flag, copies the app to `/Applications` and
opens it.

## Build on a Mac

```bash
./scripts/install-macos.sh
```

builds for `aarch64-apple-darwin`, bundles `dist/BroLink.app`, and installs
it to `/Applications`. A locally built app never carries the flag.

### Signing

`bundle-macos.sh` signs ad-hoc by default. For a build other people can
download without the quarantine dance, sign with a Developer ID and
notarize:

```bash
CODESIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)" ./scripts/bundle-macos.sh
ditto -c -k --keepParent dist/BroLink.app dist/BroLink.zip
xcrun notarytool submit dist/BroLink.zip --keychain-profile "notary" --wait
xcrun stapler staple dist/BroLink.app
```

## First run

1. If Tailscale or Moonlight is missing, the top card says so. **Get
   Tailscale** opens the download page; **Install Moonlight** fetches the
   latest DMG from GitHub into `/Applications`.
2. **Your PCs** lists the Windows machines on your Tailscale account.
   Each line says whether it is ready, asleep, or missing something on the
   PC side.
3. Click **Connect**. The first time with a PC, BroLink pairs Moonlight
   with it: Moonlight waits with a random PIN, BroLink sends the PIN to
   BroLink Host over Tailscale, and the host types it into Sunshine. You
   see the PIN but never need it.
4. Moonlight opens full screen.

## While streaming

Moonlight owns the screen. Its defaults on a Mac:

| Key | Action |
|-----|--------|
| Ctrl+Alt+Shift+Q | End the session |
| Ctrl+Alt+Shift+Z | Toggle mouse/keyboard capture |
| Ctrl+Alt+Shift+X | Toggle full screen |
| Ctrl+Alt+Shift+S | Performance overlay |

In **Desktop** mouse mode the Mac cursor maps 1:1 onto the PC. Cmd is sent
as the Windows key; system shortcuts such as Cmd-Tab go to the PC while
Moonlight is full screen.

Back in the BroLink window: **Disconnect** ends the session; **Disconnect
and sleep the PC** does both. When a session ends on its own, BroLink asks
whether to sleep the PC.

## The PC menu

Next to a ready PC, **PC** offers **Sleep**, **Restart…** and **Shut
down…**. Restart and shut down ask for a second click; both force-close
programs on the PC. The menu only appears if the host allows remote power
actions.

**Wake** appears next to a PC that is asleep and whose wake details BroLink
has learned. It sends the packet without connecting.

## Settings

| Setting | What it changes |
|---------|-----------------|
| Mouse: Desktop / Game | Moonlight's absolute-mouse flag |
| Resolution | 1080p, 1440p, 4K, or this Mac's own pixel size |
| Frame rate | 60, 90, 120 |
| Bitrate | 5 to 150 Mbps |
| Codec | Auto, HEVC, AV1, H.264 |
| Full screen | Off opens a window |
| App | The Sunshine app to launch; the list fills in after the first connection |
| Offer to sleep the PC after each session | The prompt when Moonlight exits |

Settings are saved in `~/Library/Application Support/BroLink/client.toml`,
along with the MAC and LAN address of each PC BroLink has seen.

## Waking a PC from another network

The wake packet is sent to the LAN broadcast and to the PC's LAN address.
Over the internet only the second can arrive, and only if something on the
PC's network forwards it: a [Tailscale subnet
router](https://tailscale.com/kb/1019/subnets) advertising the PC's subnet
is the usual answer (a NAS, a Raspberry Pi, or a router that runs
Tailscale). Without one, waking works from the same network only; the PC
still streams fine from anywhere once it is on.

## Permissions

- **Local network**: macOS 15 may ask; allow it so the wake broadcast can
  go out.
- Nothing else. BroLink captures no input and no screen; Moonlight asks for
  what it needs.
