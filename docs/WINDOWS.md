# Windows host

## Requirements

- Windows 10 1903+ or Windows 11
- A GPU (AMD, NVIDIA, or Intel). ForgeLink will use:
  1. AMD AMF (`h264_amf`, ultra-low-latency)
  2. NVIDIA NVENC (`h264_nvenc`, `tune=ull`)
  3. Intel Quick Sync (`h264_qsv`)
  4. Media Foundation (`h264_mf`)
  5. libx264 `ultrafast` / `zerolatency`
- [FFmpeg](https://www.gyan.dev/ffmpeg/builds/) on `PATH`, at `C:\ffmpeg\bin\ffmpeg.exe`, or next to `forgelink-host.exe`. A build with AMF/NVENC/libx264 is required (the Gyan essentials build is fine).
- Optional: [ViGEmBus](https://github.com/nefarius/ViGEmBus/releases) for a virtual Xbox 360 controller
- Optional: [Tailscale](https://tailscale.com) for one-click worldwide access behind CGNAT

## Build

```powershell
rustup default stable
cargo build --release -p forgelink-host
```

The binary is `target\release\forgelink-host.exe`.

## Run

```powershell
.\target\release\forgelink-host.exe
```

Headless (prints the ticket, no GUI):

```powershell
.\target\release\forgelink-host.exe --headless --name OFFICE-PC
```

## Firewall

ForgeLink tries to add an inbound UDP rule for port **47850**. If you are not elevated, add it yourself:

```powershell
netsh advfirewall firewall add rule name="ForgeLink Host" dir=in action=allow protocol=UDP localport=47850
```

## Games

Use **borderless windowed** (or windowed) mode. Exclusive fullscreen can bypass Desktop Duplication on some titles.

Competitive preset: 1080p60 at 15 Mbps. Increase bitrate on a LAN; drop it on a long-haul link.

Keep the Windows session unlocked. v0.1 runs in the user session (it does not capture the login screen).
