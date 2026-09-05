# macOS client (Apple Silicon)

BroLink's client is native Rust (`eframe` + OpenH264). It compiles for
`aarch64-apple-darwin` and `x86_64-apple-darwin`. M-series Macs are the
target; Intel Macs work but are not the design point.

## Install a prebuilt client

Download `brolink-macos-arm64.tar.gz` from the GitHub Actions **release**
run, then:

```bash
tar xzf brolink-macos-arm64.tar.gz
xattr -dr com.apple.quarantine BroLink.app
open BroLink.app
```

The `xattr` step is not optional. The app is ad-hoc signed rather than signed
with an Apple Developer ID, so Gatekeeper quarantines anything downloaded
through a browser and reports it as damaged. Removing the quarantine flag is
what tells macOS you fetched it deliberately.

This is not a fault in the app: macOS attaches the `com.apple.quarantine`
flag to every file a browser downloads, and only a Developer ID signature
plus notarization makes Gatekeeper wave one through. Two ways round it:

- **Build it yourself.** `scripts/install-macos.sh` compiles the client on
  your Mac and installs it to `/Applications`. A locally built app never
  carries the flag, so it opens without any prompt.
- **Sign it properly.** If you have an Apple Developer account, see
  [Signing](#signing) below.

To watch the logs, run the binary inside the bundle directly:

```bash
RUST_LOG=info BroLink.app/Contents/MacOS/BroLink
```

## Build on a Mac

```bash
rustup target add aarch64-apple-darwin
cargo build --release -p brolink-client --target aarch64-apple-darwin
./scripts/bundle-macos.sh
```

The script writes `dist/BroLink.app`, ad-hoc signed. Drag it to
`/Applications`. `scripts/install-macos.sh` does all of the above in one go.

### Signing

`bundle-macos.sh` signs the bundle ad-hoc by default, which is enough to run
a build you made yourself. To produce a build other people can download
without the quarantine dance, sign with a Developer ID certificate and
notarize it:

```bash
CODESIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)" ./scripts/bundle-macos.sh
ditto -c -k --keepParent dist/BroLink.app dist/BroLink.zip
xcrun notarytool submit dist/BroLink.zip --keychain-profile "notary" --wait
xcrun stapler staple dist/BroLink.app
```

`notarytool` needs a stored credential (`xcrun notarytool store-credentials`)
tied to an Apple Developer account. Nothing in the app changes; it is purely
a distribution step, which is why it is on the roadmap rather than in the
code.

## Run from the repo

```bash
cargo run --release -p brolink-client
```

## First connection

1. On the PC, copy the ticket from BroLink Host.
2. On the Mac, paste it and click **Connect** (or pick the PC from **PCs on
   this network** if you are on the same LAN).
3. Type the 6-digit PIN shown on the PC. After that the Mac is on the
   allow-list and will not be asked again.
4. Click the picture to capture the mouse. Press **fn+F8** to free it, or
   simply switch to another app: the client lets go of the mouse whenever its
   window loses focus, so Cmd-Tab and Mission Control always work.

The client remembers the PC. Next time, click it in **Your PCs**. If the
session drops, it reconnects on its own unless you disconnected.

## Waking the PC and turning it off

Once you have connected to a PC, the Mac remembers how to wake it. From then
on:

- **Connect** in **Your PCs** wakes the PC if it does not answer within a few
  seconds, then connects. A sleeping PC is usually back in 10–20 s.
- **Wake** sends the wake-up without connecting.
- While streaming, press **fn+F8** to free the mouse, then open **PC ▾** in
  the overlay: **Sleep** happens at once; **Restart…** and **Shut down…** ask you to
  confirm, since anything unsaved on the PC is lost.

Waking works from anywhere while the PC is *asleep*. After a full shut down
it only works from the PC's own network (see [WINDOWS.md](WINDOWS.md)), so
leave the PC asleep when you go.

## Permissions

- **Microphone / camera**: not required (the Mac is the client).
- **Input Monitoring**: not required; we only capture input inside our window.
- **Local network**: macOS 15+ may prompt. Allow it so LAN discovery works.
- Game controllers work through HID (Xbox, DualSense, etc.).

## Controls while streaming

| Key | Action |
|-----|--------|
| Click the picture | Capture mouse (relative, for games) |
| **fn+F8** | Release / recapture mouse |
| Switch app (Cmd-Tab, Mission Control) | Releases the mouse |
| **F11** | Fullscreen |
| **F7** | Hide / show the overlay |
| **Ctrl+Shift+Q** | Disconnect |

On a Mac keyboard F8 is a media key by default, so the app never sees a bare
press; hold **fn** with it. If you would rather press F8 on its own, turn on
*Use F1, F2, etc. keys as standard function keys* in System Settings →
Keyboard.

## Connecting across the world

Same ticket. The host's UPnP mapping, public IPv6, Tailscale address, or
relay is already inside it. You do not configure NAT on the Mac.

If the connection times out:

1. Confirm the host window says it is reachable (UPnP mapped, Tailscale, or
   relay). A STUN address alone is often not enough.
2. Confirm the Windows firewall is not blocking UDP 47850 (see
   [WINDOWS.md](WINDOWS.md)).
3. On CGNAT (many mobile ISPs, some fibre), set a relay on the host or
   install Tailscale on both machines.

### No router access, no port forwarding

If the PC's router will not open a port (UPnP blocked, CGNAT, a landlord's
router, a campus network), run `brolink-relay` on any cheap VPS and put its
address in the host's **Relay** field. Both the PC and the Mac connect
*outbound* to it, so nothing needs to be opened on either network, there is
no account to create, and the relay only ever sees encrypted bytes. It is
less to set up than Tailscale for the same result, and a direct connection
is still tried first. See [RELAY.md](RELAY.md) for a systemd unit and a
Dockerfile.
