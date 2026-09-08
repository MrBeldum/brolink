# Windows host

BroLink Host is one small program with two jobs: a background service a Mac
talks to over Tailscale, and a window that shows what it knows and runs the
setup. Sunshine, which ships in the same zip, does the streaming.

## Requirements

- Windows 10 1809+ or Windows 11, 64-bit
- A GPU Sunshine can encode on (AMD, NVIDIA, Intel)
- [Tailscale](https://tailscale.com/download/windows), signed in with the
  same account as the Mac
- Wired Ethernet if you want to wake the PC from the Mac

## Install

1. Unzip the release and run `brolink-host.exe`, or run `install-host.ps1`
   to copy it (and the Sunshine installer) to `%LOCALAPPDATA%\BroLink`, add
   shortcuts, and register the background service to start at logon.
2. Click **Set up this PC**. One UAC prompt runs a script that:
   - installs the bundled `Sunshine-Windows-AMD64-installer.msi` silently
     (skipped if Sunshine or Apollo is already installed; downloads the
     latest release if the MSI is not beside the exe);
   - sets a Sunshine web login for BroLink (`sunshine --creds`) and
     restarts the Sunshine service;
   - adds firewall rules: TCP 47850 inbound from `100.64.0.0/10` only, UDP 9
     for the wake-packet listener, and Sunshine's ports from the tailnet;
   - turns Fast Startup off (`HiberbootEnabled = 0`);
   - on the adapter that has the default route, enables wake on magic
     packet and writes the driver keywords `*WakeOnMagicPacket`,
     `*ModernStandbyWoLMagicPacket`, `S5WakeOnLan`, `*PMARPOffload` and
     `*PMNSOffload`, restarts the adapter if any changed, and lets the
     device wake the PC (`powercfg /deviceenablewake`).

   The script's transcript is in `%LOCALAPPDATA%\BroLink\setup.log` and is
   shown in the window if something fails. Every step is idempotent; run
   setup again after fixing whatever it complained about.
3. The status pill turns green. Leave the window closed; the service keeps
   running.

The window shows the Sunshine login it generated. Use it at
`https://localhost:47990` for Sunshine's own settings (encoder, which
display to stream, HDR, audio device, apps).

## Slow streams: the network, or the encoder

The **This PC** card has a **Network** line from `tailscale netcheck`. "Hard
NAT with no UPnP" means a Mac on another network can only reach this PC
through a Tailscale relay, which adds a detour and holds the stream to a
few megabits; the Mac shows the same thing as **Relayed via …** next to
the PC. Turn UPnP (or NAT-PMP) on in the router, or forward a UDP port to
this PC, and Tailscale connects directly; IPv6 on both ends works too. The
service logs the finding each time it changes.

The **Streaming** line names the encoder Sunshine settled on, read from
its log. "software" means no GPU encoder worked (a missing or broken
driver): frames are slow to make whatever the network does. Fix the GPU
driver, then restart the Sunshine service.

## What the service does

`brolink-host.exe --background` listens on TCP 47850 and answers these
requests, all JSON:

| Request | Effect |
|---------|--------|
| `GET /v1/status` | Name, Tailscale login and IP, LAN IP and MAC, wake state, Fast Startup state, seconds since the last wake packet arrived, Sunshine state and the encoder it uses, this PC's NAT report |
| `POST /v1/pin {"pin","name"}` | Passes the PIN to Sunshine's `/api/pin`, so pairing never needs the PC's screen |
| `POST /v1/power {"action"}` | `sleep`, `restart`, or `shutdown` (closes the running Sunshine app first) |
| `GET /v1/clipboard` | The clipboard as text, with Windows' clipboard sequence number |
| `POST /v1/clipboard {"text"}` | Replaces the clipboard, so a ⌘V on the Mac pastes the Mac's text |
| `POST /v1/update` | A new `brolink-host.exe`; see Updates |

A request is answered only if it comes from loopback or from a Tailscale
address that `tailscale whois` attributes to the account this PC is signed
in as. Anything else is refused with a 403 before any action.

The service also listens on UDP 9. A magic packet for this PC's MAC that
arrives while it is awake is logged and reported in the status, which is
what **Test wake** on the Mac reads.

Logs: `%LOCALAPPDATA%\BroLink\service.log` and `panel.log`.

## Waking it from the Mac, and turning it off

The **This PC** card shows the adapter, its MAC, whether Windows will wake
on a magic packet, whether Fast Startup is on, and when a wake packet last
arrived. Setup arms all of it. The Mac learns the MAC and LAN address the
first time it sees this PC awake, and from then on **Connect** on the Mac
wakes it.

| PC state | Wake from the same network | Wake from elsewhere |
|----------|----------------------------|---------------------|
| Sleep (S3 or modern standby, plugged in) | yes | if the router forwards UDP 9 to the PC, or a Tailscale subnet router is on the PC's network |
| Shut down, Fast Startup off | if the firmware allows wake from power off (often called "Power on by PCI-E" or "Wake on LAN from S5"; ErP must be off) | same, plus the router condition |
| Shut down with Fast Startup on | no; setup turns it off | no |

Leave the PC **on** if you want Tailscale from another network. Asleep,
Tailscale is off; a Mac that is not on this LAN cannot wake it unless the
router forwards UDP 9. **Keep this PC awake while plugged in** (on by
default) stops idle sleep; Sleep from the Mac or the Start menu still
works. Untick **Let a paired Mac sleep, restart, or shut down this PC**
if you would rather the Mac could not. Remote power actions force-close
programs, because nobody is there to answer a save prompt.

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
  usual causes are the UAC prompt being dismissed, or a driver that refuses
  the wake keywords.
- **The Mac lists the PC as online but with no BroLink Host**: the control
  service is not running or the firewall rule is missing. Open BroLink Host
  (it restarts the service) and run setup again.
- **The Mac says the PC is asleep but it is on**: Tailscale's online flag
  lags by up to half a minute; Connect probes the PC directly and will work.
- **Test wake on the Mac says the packet did not arrive**: the packet is
  being sent but not delivered. From another network the PC's router has to
  forward UDP 9 to the PC's LAN address (give the PC a DHCP reservation so
  the address is stable). On the same network, check that Windows classes
  it as Private; a Public network drops the broadcast.
- **Test wake passes but the PC does not wake**: the network path is fine
  and Windows is the problem. Check the This PC card for "off" or "Fast
  Startup on" and run setup again; for wake from power off, look for the
  firmware setting named above.

## Updates

BroLink Host does not download anything. The Mac fetches each release and
POSTs the new `brolink-host.exe` to `/v1/update` on the control port with
its version and SHA-256, from a machine on the PC's own Tailscale account,
the same check that guards remote power actions. The service verifies the
digest, that the bytes are a Windows executable and a newer version, writes
`brolink-host.exe.new` beside itself, renames the running file to
`brolink-host.exe.old`, moves the new one in and starts it with
`--replaces <pid>`; the new service waits for the old one to release the
port, then removes the `.old` file. Both events appear in the host log and
the Sunshine session, if any, is not interrupted.

Hosts older than 3.1 have no update route, and the Mac does not POST the
executable at them (that used to show as a broken pipe). Instead the Mac
installs 3.1 through the stream: from the stream's **PC → Update BroLink
Host…**, the Mac serves the new executable on its Tailscale address,
presses Win+R here, types `powershell -ep bypass -c "irm
http://<mac>:47851/u.ps1|iex"` and presses Enter. The script fetches the
executable from the Mac, checks its SHA-256, asks the running service to
quit, replaces the file where it is, registers it under the Run key and
starts it. The desktop has to be unlocked. After that, updates are
automatic.

## Staying reachable

The background service starts with Windows by default and sets that again
at every start, so a PC nobody can reach in person comes back after a
restart; the toggle in Settings is the only thing that turns it off.
It also holds Windows awake while plugged in (same Settings card), because
a sleeping PC's Tailscale is asleep and a Mac on another network cannot
wake it.
Sunshine runs as a Windows service and streams the sign-in screen, so a
Mac can still connect after a reboot before anyone logs in. Keep the PC's
Tailscale key from expiring by disabling key expiry for it in the
[admin console](https://login.tailscale.com/admin/machines); the Mac warns
about this for every PC it lists.

