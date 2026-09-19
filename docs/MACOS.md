# macOS client (Apple Silicon)

BroLink for the Mac is one native app: your PCs, a Connect button each, a
power menu, and the stream itself. BroLink's stream protocol is compiled
in, video is decoded by VideoToolbox (H.264 and HEVC), audio by Opus, and
the picture is drawn in BroLink's own window under a toolbar.

## Requirements

- macOS 13+ on Apple Silicon
- [Tailscale](https://tailscale.com/download/mac), signed in with the same
  account as the PC

## Install a prebuilt app

Public releases download with `curl`. If the repository is private, install
the [GitHub CLI](https://cli.github.com) and sign in
(`brew install gh && gh auth login`) so the script can fall back to `gh`.

```bash
curl -fsSL https://raw.githubusercontent.com/MrBeldum/brolink/main/scripts/install-macos-release.sh | bash
```

It checks the release SHA-256, verifies the ad-hoc signature, swaps the app
into `/Applications` (the live copy is not deleted first) and opens it.

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
   enters it for you. You see the PIN but never need it. If there is no
   BroLink Host on the PC, pairing cannot finish: open BroLink Host there
   and run setup first.
4. The PC's desktop appears.

## While streaming

The picture fills the window and nothing else is drawn over it. With
**Match screen** the picture is this display's own size, so there is no
bar on any side; a window is resized to the stream's proportions when the
session starts, for the same reason. A manual 16:9 size on a display of
another shape leaves a bar above and below.

**Ctrl+Alt** is the host key, as in VMware or VirtualBox. Press it and the
mouse is freed and the toolbar drops over the top of the picture; press it
again, or click the picture, and the toolbar goes away and the mouse is
captured back. A menu or a question keeps the toolbar there until it is
answered. The status line, with the session clock, the frame rate and
bitrate against what was asked for, and the round trip, is drawn over the
picture's bottom-left corner while **Stats** is open, or in the bar below
the picture when a manual size leaves one.

The mouse is captured by default: the Mac cursor is hidden and held in
place and raw movement is sent, which is what games read for the camera
(a game that ignores the cursor's position, such as Genshin Impact, turns
only with this). The PC draws its own cursor in the picture, so only one
pointer is ever visible. The **Mouse** menu can switch to **Free**, where
the Mac cursor's position is sent 1:1 instead and nothing is captured.

| Toolbar item | What it does |
|--------------|--------------|
| **● PC name · 1920×1080 · 60 fps · HEVC** | The stream as negotiated; the dot turns red while the connection is poor |
| **Direct · 38 ms / Relayed via Tokyo · 210 ms / Via your relay · 60 ms** | The path Tailscale found to the PC and its round trip. Red means every packet goes through a Tailscale relay, which adds delay; the lobby's **Connection details** say why and what would give a direct path. What BroLink asks for is the same on every path |
| **Mouse: captured / free** | Captured (the default): a click on the picture hides the cursor and sends raw movement, which games read; **Ctrl+Alt** frees it. Free: the Mac cursor's position is sent 1:1 and nothing is captured. The choice is remembered |
| **Keys** | Ctrl+Alt+Del, Windows key, Alt+Tab, Esc, Print Screen; and the switch for what ⌘ does on the PC |
| **Stats** | A Stream performance window: received against target bitrate and frame rate, round trip, packet loss, host, assembly, queue and decode times, and whether the decoder is hardware. **Copy diagnostics** puts it all on the clipboard |
| **Full screen** | Toggle; the setting decides how a session starts |
| **PC** | Sleep, Restart…, Shut down… (the latter two ask twice); **Update BroLink Host…** for a PC whose host is older than 3.1 |
| **Stream settings** | The same panel as in the lobby: quick profiles, picture quality, resolution, frame rate, bitrate target and video format. **Apply and reconnect** restarts the session with them in a few seconds |
| **Disconnect** | End the session |

Everything else on the keyboard goes to the PC as pressed. With **⌘ acts
as Ctrl** on (the default), ⌘C, ⌘V, ⌘Z and the rest do on Windows what
they do on the Mac, and ⌘ can stay held across several of them.

### Clipboard

With BroLink Host 3.1 or newer on the PC, the clipboard follows you both
ways: text copied on the PC is in the Mac's clipboard a second later, and
⌘V on the PC pastes the text the Mac has (BroLink sends it to the PC first,
then presses Ctrl+V there). Text only, up to 32 KB; an image or a file on
either clipboard is left alone. Short notices under the toolbar say when
something crossed. With an older host, ⌘V pastes what the PC last copied.

### Laggy? Look at the path

Tailscale connects the Mac and the PC directly when it can punch through
both routers. When it cannot, every packet goes through a relay: your own
peer relay if you run one (the Relay card in Settings), otherwise one of
Tailscale's (DERP). A relay adds a detour, and the round trip it adds is a
floor under how quickly the PC answers a click; it changes nothing about
what BroLink asks for. Recommended quality is this screen's size at 35
Mbps on every path, so a relayed stream is laggier, not blurrier.
Tailscale's own relays are shared, so a stream through one can also
stutter at busy times; a peer relay you run is yours alone.

BroLink shows which case you are in next to each PC (**Relayed via Tokyo ·
210 ms**, **Via your relay · 60 ms**) and, when relayed, **Connection
details** under the list says which router is in the way and what would
fix it: UPnP or NAT-PMP turned on in the PC's router, a UDP port forwarded
to the PC, or IPv6 on both networks. Both machines report their own side
(`tailscale netcheck`); the PC's report needs BroLink Host 3.1.

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
| Quick profiles | **Smooth** 1080p · 60 fps · 12 Mbps, **Balanced** match screen · 60 · 35, **Sharp** match screen · 60 · 65; **Reset** returns to Recommended |
| Picture quality | **Recommended** (the default) asks for this screen's own size at 35 Mbps, on every path. **Manual** uses the rows below; changing the resolution or the bitrate switches to it |
| Resolution | **Match screen** is this display's own pixel size (3024×1964 on a 14" MacBook Pro), so the picture fills it exactly. **1080p**, **1440p** and **4K** are the standard 16:9 sizes, 1920×1080, 2560×1440 and 3840×2160; on a display of another shape they leave a bar above and below. The list shows the pixel size of each. The PC switches its display to the size asked for; a PC with no monitor needs a virtual display that lists that size, which setup on the PC arranges for the Virtual Display Driver |
| Frame rate | 30 to 240; the PC's display is switched to match when it can |
| Bitrate target | 2 to 150 Mbps. What arrives varies with what is on screen: a still desktop uses very little |
| Video format | Auto (HEVC when the PC can encode it), HEVC, H.264 |
| Full screen | How a session starts |
| App | The app on the PC to launch; "Desktop" is the whole PC. The list fills in after the first connection |
| Command key acts as Ctrl | Off makes ⌘ the Windows key |
| Offer to sleep the PC after each session | Off by default. Asleep, Tailscale is off; this Mac can only wake the PC from that PC's own network |

The stream rows are the panel that **Stream settings** opens during a
session, and the pill under them says exactly what the next connect asks
for; the lobby's **Next session** card shows the same. A session also
reconnects by itself, at the new size, when this Mac's window moves to a
display of a different size.

Settings are saved in `~/Library/Application Support/BroLink/client.toml`,
along with the MAC, LAN address and pairing certificate of each PC BroLink
has paired with.

## Waking a PC from another network

Asleep, the PC's Tailscale is off, so BroLink cannot reach it over the
tailnet. Leave the PC on (BroLink Host keeps it awake while plugged in)
if you want **Connect** from anywhere.

The wake packet is sent to the LAN broadcast, to the PC's LAN address and
to the PC's public address. Over the internet only the last can arrive,
and only if the PC's router forwards UDP 9 to the PC. A [Tailscale subnet
router](https://tailscale.com/kb/1019/subnets) on the PC's network (a NAS,
a Raspberry Pi, or a router that runs Tailscale) is the alternative. Use
**Test wake** while the PC is on to find out which situation you are in.

## Permissions

- **Local network**: macOS 15 may ask; allow it so the wake broadcast can
  go out.
- **Incoming connections**: if the macOS firewall is on it may ask once,
  the first time the Mac serves a BroLink Host install through the stream.
- Nothing else. BroLink captures no screen and reads the keyboard and
  mouse only in its own window.

## Connected, but the picture is black

A connection can carry valid video that contains only black pixels.
BroLink now detects this after five seconds and shows a persistent message
instead of presenting the stream as healthy. **Restart stream** reconnects
and, for Desktop, resets the PC's capture session. If no video arrives or
decoding fails, the message distinguishes those problems too.

The notice then asks the PC what it can see and says which cause it is.

On a PC with no monitor attached, it is usually the colour mode. Windows
carries on composing the desktop in HDR or wide colour, as the monitor
that is now gone once asked for, on a placeholder display that reports no
luminance at all. Anything converting that desktop to an ordinary picture
turns every frame black, even though the PC's own desktop draws perfectly
normally — which is why the stream looks broken and the PC does not.

Where that mode is one the display supports, the notice offers **Turn off
HDR on the PC**, which does exactly that over the control API and starts a
fresh capture. Where Windows is enforcing it on a placeholder display it
refuses every switch away from it: the mode is a consequence of having no
display rather than a setting, and only giving the PC a display clears it.
Attach a monitor, plug in an HDMI/DisplayPort dummy plug, or install a
virtual display driver on the PC, and select that display for streaming.

The notice names the other causes too — a desktop that is genuinely black
because the display is asleep, a lock screen the PC cannot capture, or a
capture failing while the desktop plainly has a picture.

## Updates

The app checks GitHub for a new release about every six hours and twenty
seconds after it starts. A newer version is downloaded to
`~/Library/Application Support/BroLink/updates/<tag>/`, checked against the
SHA-256 GitHub publishes for the asset and against its own code signature,
and moved over `/Applications/BroLink.app` once no stream is running; the
app then relaunches. The Mac also sends the new `brolink-host.exe` to each
PC whose BroLink Host is older and already speaks `/v1/update` (3.1+).

A PC still on 3.0 cannot take that, and nobody may be at the PC to install
by hand. So the stream's **PC → Update BroLink Host…** does it through
the stream: this Mac serves the new `brolink-host.exe` and a short
PowerShell script on its own Tailscale address (TCP 47851, to that one PC
only, for ten minutes), presses Win+R on the PC, types one line
(`powershell -ep bypass -c "irm http://<mac>:47851/u.ps1|iex"`) and
presses Enter. A PowerShell window on the PC fetches the executable, checks
its SHA-256 against the one in the script, stops the old service, swaps
the file where it stands (found from the running process, or
`%LOCALAPPDATA%\BroLink`), registers it to start at logon and starts it.
The stream is not interrupted; the toolbar reports each step and the lobby
shows the new version a few seconds later. The PC's desktop has to be
unlocked and in front, since the Run box needs it. From then on updates
arrive by themselves.

Public releases need no GitHub login. If the repository is private, the
app uses, in order: `github_token` in `client.toml`, `BROLINK_GITHUB_TOKEN`
in the environment, and the token git has stored for github.com (which is
there after `install-macos-release.sh` or any `git` use with the osxkeychain
helper). Settings has the switch and a **Check now** button; the line under
it says what happened last. A copy that is not running from an app bundle
(a development build) reports the new version but does not replace itself.

## Staying reachable

- Every PC in the list, and this Mac, shows a warning while its Tailscale
  node key expires. Open the machine in the
  [admin console](https://login.tailscale.com/admin/machines) and choose
  **Disable key expiry**; the warning goes away at the next scan.
- PCs are remembered in `client.toml` with their Tailscale address, MAC and
  the certificate from pairing. While Tailscale on this Mac is off they stay
  listed with when they were last seen, and **Wake** still works over the
  LAN or the public address.
