# Windows host

BroLink Host is a native app. You leave it running on the PC you want to
play from a Mac. It captures the desktop, encodes it, and waits for a
trusted client.

## Requirements

- Windows 10 1903+ or Windows 11
- A GPU (AMD, NVIDIA, or Intel). BroLink will use:
  1. AMD AMF (`h264_amf`, ultra-low-latency)
  2. NVIDIA NVENC (`h264_nvenc`, `tune=ull`)
  3. Intel Quick Sync (`h264_qsv`)
  4. Media Foundation (`h264_mf`)
  5. libx264 `ultrafast` / `zerolatency`
- FFmpeg is **downloaded automatically** the first time the host cannot find
  it. You can also drop `ffmpeg.exe` next to `brolink-host.exe`, put it on
  `PATH`, or install a Gyan essentials build at `C:\ffmpeg\bin\ffmpeg.exe`.
- Optional: [ViGEmBus](https://github.com/nefarius/ViGEmBus/releases) for a virtual Xbox 360 controller
- Optional: [Tailscale](https://tailscale.com) if UPnP is blocked (CGNAT, campus, locked router)

## Install from this repo

```powershell
rustup default stable
cargo build --release -p brolink-host
.\scripts\install-host.ps1
```

That copies the binary to `%LOCALAPPDATA%\BroLink`, fetches FFmpeg if
needed, writes a desktop shortcut, and tries to add a firewall allow rule.
Tick **Start with Windows** in the app if you want the host waiting after a
reboot.

Headless (prints the ticket, no GUI):

```powershell
.\target\release\brolink-host.exe --headless --name OFFICE-PC
```

Useful flags:

| Flag | Effect |
|------|--------|
| `--headless` | No control panel; prints the ticket and logs to stdout |
| `--name NAME` | Override the advertised PC name |
| `--port N` | UDP port (default 47850) |
| `--no-pin` | Trust any client that can reach this PC (LAN testing only; not saved) |
| `--relay HOST:PORT` | Advertise a `brolink-relay` (relay + rendezvous) for hard-NAT clients and changing IPs |
| `--no-firewall` | Do not try to add a Windows Firewall rule on startup |
| `--no-audio` | Do not capture or stream system audio for this run |

## Internet access

The host tries, in order, to make the ticket work from another network:

1. **UPnP / NAT-PMP** (on by default) — asks the router to forward UDP 47850
2. **Public IPv6** — advertised when the PC has a global address
3. **STUN** — learns the reflexive address; only works on full-cone NAT by itself
4. **Tailscale** — if a `100.x` interface exists
5. **Relay** — if you set one in the UI

If the Internet card in the UI says the PC is not reachable from outside,
either enable UPnP on the router, install Tailscale on both machines, or run
`brolink-relay` on a small VPS and paste `host:47851` into **Relay**.

## Waking it from the Mac, and turning it off

The host window has a **Wake and power from the Mac** card. It shows the LAN
adapter, its MAC, and whether Windows will wake the PC on a magic packet. If
not, **Enable Wake-on-LAN** fixes it through a UAC prompt (it turns on
magic-packet wake and ARP offload on the adapter and lets the device wake the
PC). Once a Mac has connected, it remembers the MAC, and **Connect** on that
PC wakes it automatically.

What works, honestly:

| PC state | Wake from the same LAN | Wake from the internet |
|----------|-----------------------|------------------------|
| Sleep (S3 / modern standby, plugged in) | yes | yes, via the router mapping the host created |
| Hibernate / shut down with Fast Startup | usually | rarely: the router forgets the PC's address within minutes |
| Shut down, Fast Startup off | if the NIC/BIOS allow wake from S5 | rarely, same reason |

So: leave the PC **asleep**, not shut down, when you are away. The client's
**PC ▾** menu (press **F8** first to free the mouse) offers Sleep, Restart,
and Shut down; untick **Let a paired Mac sleep, restart, or shut down this
PC** if you would rather it could not. Tick **Start with Windows** so the host
is back after a restart. Remote power actions force-close programs, because
nobody is there to answer a save prompt.

Wi-Fi adapters often cannot wake the PC at all; use Ethernet for the host.

## Firewall

BroLink tries to add an inbound UDP rule for port **47850**. If you are not
elevated, add it yourself, or run `.\scripts\install-host.ps1` from an
elevated PowerShell.

### If clients still cannot connect

Two things silently defeat the allow rule. The host warns about both at
startup. To fix them, from an **elevated** PowerShell:

```powershell
.\scripts\fix-firewall.ps1 -DryRun   # show what would change
.\scripts\fix-firewall.ps1           # remove blocks, add a LocalSubnet allow
.\scripts\fix-firewall.ps1 -Wan      # ...or open the port to any address
```

The allow rule is scoped to the local subnet unless you pass `-Wan`, so
playing on your own network does not expose the port to the internet. UPnP
mapping is a separate, explicit hole for worldwide access. The script never
changes your network category.

**Block rules win.** Windows evaluates block rules before allow rules, so a
leftover "Query User" block — which Windows writes whenever its network
prompt is dismissed or cancelled — makes the host unreachable no matter what
allow rules exist.

**Public networks.** Windows blocks inbound connections and LAN discovery on
networks classed as Public. On a home network you control:

```powershell
Set-NetConnectionProfile -InterfaceAlias "Ethernet" -NetworkCategory Private
```

On a cafe or hotel network leave it Public and reach the host over Tailscale
or a relay.

## Games

Use **borderless windowed** (or windowed) mode. Exclusive fullscreen can
bypass Desktop Duplication on some titles.

Competitive preset: 1080p60 at 15 Mbps. Increase bitrate on a LAN; drop it
on a long-haul link. Adaptive bitrate will also step down if the client
reports loss.

Keep the Windows session unlocked. v1.0 runs in the user session (it does
not capture the login screen).
