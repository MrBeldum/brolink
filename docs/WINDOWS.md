# Windows host

BroLink Host is one small program with two jobs: a background service a Mac
talks to over Tailscale, and a window that shows what it knows and runs the
setup. Sunshine does the streaming.

## Requirements

- Windows 10 1809+ or Windows 11, 64-bit
- A GPU Sunshine can encode on (AMD, NVIDIA, Intel; AV1 needs RX 7000 /
  RTX 40 / Arc)
- [Tailscale](https://tailscale.com/download/windows), signed in with the
  same account as the Mac
- Wired Ethernet if you want to wake the PC from the Mac

## Install

1. Unzip the release and run `brolink-host.exe`, or run `install-host.ps1`
   to copy it to `%LOCALAPPDATA%\BroLink`, add shortcuts, and register the
   background service to start at logon.
2. Click **Set up this PC**. One UAC prompt runs a script that:
   - downloads the latest Sunshine MSI from GitHub and installs it silently
     (skipped if Sunshine or Apollo is already installed);
   - sets a Sunshine web login for BroLink (`sunshine --creds`) and
     restarts the Sunshine service;
   - adds a firewall rule for TCP 47850, inbound from `100.64.0.0/10` only;
   - enables Wake-on-Magic-Packet and ARP offload on the adapter that has
     the default route, and lets the device wake the PC (`powercfg`).

   The script's transcript is in `%LOCALAPPDATA%\BroLink\setup.log` and is
   shown in the window if something fails. Run setup again after fixing
   whatever it complained about; every step is idempotent.
3. The status pill turns green. Leave the window closed; the service keeps
   running.

The window shows the Sunshine login it generated. Use it at
`https://localhost:47990` for Sunshine's own settings (encoder, which
display to stream, HDR, audio device, apps).

## What the service does

`brolink-host.exe --background` listens on TCP 47850 and answers three
requests, all JSON:

| Request | Effect |
|---------|--------|
| `GET /v1/status` | Name, Tailscale login and IP, LAN IP and MAC, wake state, Sunshine state |
| `POST /v1/pin {"pin","name"}` | Passes the PIN to Sunshine's `/api/pin`, so pairing never needs the PC's screen |
| `POST /v1/power {"action"}` | `sleep`, `restart`, or `shutdown` (closes the running Sunshine app first) |

A request is answered only if it comes from loopback or from a Tailscale
address that `tailscale whois` attributes to the account this PC is signed
in as. Anything else is refused with a 403 before any action.

Logs: `%LOCALAPPDATA%\BroLink\service.log` and `panel.log`.

## Waking it from the Mac, and turning it off

The **This PC** card shows the adapter, its MAC, and whether Windows will
wake on a magic packet. Setup arms it. The Mac learns the MAC and LAN
address the first time it sees this PC awake, and from then on **Connect**
on the Mac wakes it.

| PC state | Wake from the same network | Wake from elsewhere |
|----------|----------------------------|---------------------|
| Sleep (S3 or modern standby, plugged in) | yes | yes, if a Tailscale subnet router or similar is on the PC's network |
| Hibernate / shut down with Fast Startup | usually | rarely |
| Shut down, Fast Startup off | if the NIC and BIOS allow wake from S5 | rarely |

Leave the PC **asleep**, not shut down. Untick **Let a paired Mac sleep,
restart, or shut down this PC** in Settings if you would rather it could
not. Remote power actions force-close programs, because nobody is there to
answer a save prompt.

After a **restart**, Sunshine is back before anyone logs in (it is a
service), so the Mac can stream the login screen and sign in. BroLink's
service starts at logon, so sleep and shutdown from the Mac return once
someone is signed in.

## Apollo instead of Sunshine

[Apollo](https://github.com/ClassicOldSong/Apollo) is a Sunshine fork with
a built-in virtual display that matches the Mac's resolution exactly, which
is the only way to get a pixel-for-pixel 16:10 desktop on a MacBook screen.
If it is installed in `C:\Program Files\Apollo`, BroLink uses it instead of
installing Sunshine; the API and config layout are the same. Install it
yourself from its releases page before running setup.

## Gamepads

Sunshine no longer installs the virtual controller driver (ViGEmBus) by
itself. The **This PC** card shows whether it is present and offers
**Install controller driver**; a reboot afterwards is recommended.

## Troubleshooting

- **Orange "Needs setup" that will not clear**: read `setup.log`. The
  usual causes are no internet on the PC during the Sunshine download, or
  the UAC prompt being dismissed.
- **The Mac lists the PC as "Sunshine only"**: the control service is not
  running or the firewall rule is missing. Open BroLink Host (it restarts
  the service) and run setup again.
- **The Mac says the PC is asleep but it is on**: Tailscale's online flag
  lags by up to half a minute; Connect probes the PC directly and will work.
- **Windows classes the network as Public**: Tailscale traffic is
  unaffected, but the LAN wake broadcast from a Mac on the same network
  may be dropped. Set the network to Private for a home LAN.
