//! Whether this PC can be woken remotely, and the MAC a client needs to do it.
//!
//! Everything here shells out to PowerShell once, on a background thread: the
//! adapter/power cmdlets are the documented way to read and change these
//! settings, and a probe every start is cheap next to the encoder probe.

use anyhow::{Context, Result};
use std::net::Ipv4Addr;

#[derive(Debug, Clone, Default)]
pub struct WakeInfo {
    /// `aa:bb:cc:dd:ee:ff` of the adapter that owns the LAN address.
    pub mac: Option<String>,
    pub adapter: String,
    /// `Some(true)` when the adapter is set to wake on a magic packet *and*
    /// Windows allows the device to wake the machine.
    pub magic_packet: Option<bool>,
    /// Fast Startup makes "shut down" a hybrid hibernate that most adapters
    /// cannot wake from. Sleep is unaffected.
    pub fast_startup: Option<bool>,
}

/// Look up the adapter that owns `lan` and how it is set up for waking.
pub fn probe(lan: Ipv4Addr) -> WakeInfo {
    #[cfg(windows)]
    {
        let script = format!(
            "$a = Get-NetIPAddress -AddressFamily IPv4 -IPAddress '{lan}' -ErrorAction SilentlyContinue | Select-Object -First 1; \
             $n = Get-NetAdapter -InterfaceIndex $a.InterfaceIndex -ErrorAction SilentlyContinue | Select-Object -First 1; \
             $p = Get-NetAdapterPowerManagement -Name $n.Name -ErrorAction SilentlyContinue; \
             $armed = @(powercfg /devicequery wake_armed) -contains $n.InterfaceDescription; \
             $h = (Get-ItemProperty 'HKLM:\\SYSTEM\\CurrentControlSet\\Control\\Session Manager\\Power' -Name HiberbootEnabled -ErrorAction SilentlyContinue).HiberbootEnabled; \
             Write-Output \"$($n.MacAddress)|$($n.Name)|$($p.WakeOnMagicPacket)|$armed|$h\""
        );
        let out = match powershell(&["-NoProfile", "-NonInteractive", "-Command", &script]) {
            Ok(o) => o,
            Err(e) => {
                tracing::debug!("wake probe: {e:#}");
                return WakeInfo::default();
            }
        };
        parse_probe(&out)
    }
    #[cfg(not(windows))]
    {
        let _ = lan;
        WakeInfo::default()
    }
}

/// Parse `mac|adapter|WakeOnMagicPacket|armed|hiberboot`.
fn parse_probe(line: &str) -> WakeInfo {
    let mut f = line.trim().split('|');
    let mac = f
        .next()
        .and_then(brolink_core::wake::MacAddr::parse)
        .map(|m| m.to_string());
    let adapter = f.next().unwrap_or("").trim().to_string();
    let wol = f.next().unwrap_or("").trim().to_string();
    let armed = f.next().unwrap_or("").trim().eq_ignore_ascii_case("true");
    let magic_packet = match wol.as_str() {
        "Enabled" => Some(armed),
        "Disabled" | "Unsupported" => Some(false),
        _ => None,
    };
    let fast_startup = match f.next().unwrap_or("").trim() {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    };
    WakeInfo {
        mac,
        adapter,
        magic_packet,
        fast_startup,
    }
}

/// Turn on magic-packet wake for `adapter`, through a UAC prompt. Blocks until
/// the elevated PowerShell has finished, so the caller can re-probe.
pub fn enable_magic_packet(adapter: &str) -> Result<()> {
    #[cfg(windows)]
    {
        let dir = crate::ffmpeg_setup::install_dir();
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("enable-wake.ps1");
        let name = adapter.replace('\'', "''");
        std::fs::write(
            &path,
            format!(
                "$ErrorActionPreference = 'Stop'\n\
                 $n = Get-NetAdapter -Name '{name}'\n\
                 Set-NetAdapterPowerManagement -Name '{name}' -WakeOnMagicPacket Enabled\n\
                 # ARP offload lets the card answer for the PC's address while it sleeps,\n\
                 # which is what makes a wake packet from the internet reach it.\n\
                 Set-NetAdapterPowerManagement -Name '{name}' -ArpOffload Enabled -ErrorAction SilentlyContinue\n\
                 powercfg /deviceenablewake \"$($n.InterfaceDescription)\"\n"
            ),
        )?;
        let file = path.display().to_string().replace('\'', "''");
        let launch = format!(
            "Start-Process powershell -Verb RunAs -Wait -ArgumentList @('-NoProfile','-ExecutionPolicy','Bypass','-WindowStyle','Hidden','-File','{file}')"
        );
        powershell(&["-NoProfile", "-NonInteractive", "-Command", &launch])
            .map(|_| ())
            .context("elevated PowerShell")
    }
    #[cfg(not(windows))]
    {
        anyhow::bail!("Wake-on-LAN setup for {adapter} is only supported on Windows")
    }
}

#[cfg(windows)]
fn powershell(args: &[&str]) -> Result<String> {
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new("powershell")
        .args(args)
        .creation_flags(0x0800_0000)
        .output()
        .context("run powershell")?;
    anyhow::ensure!(
        out.status.success(),
        "powershell exited with {}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_output_is_parsed() {
        let w = parse_probe("AA-BB-CC-DD-EE-FF|Ethernet|Enabled|True|1\r\n");
        assert_eq!(w.mac.as_deref(), Some("aa:bb:cc:dd:ee:ff"));
        assert_eq!(w.adapter, "Ethernet");
        assert_eq!(w.magic_packet, Some(true));
        assert_eq!(w.fast_startup, Some(true));

        // Enabled on the adapter but Windows will not let it wake the PC.
        let w = parse_probe("AA-BB-CC-DD-EE-FF|Wi-Fi|Enabled|False|0");
        assert_eq!(w.magic_packet, Some(false));
        assert_eq!(w.fast_startup, Some(false));

        let w = parse_probe("|||False|");
        assert!(w.mac.is_none());
        assert_eq!(w.magic_packet, None);
        assert_eq!(w.fast_startup, None);

        let w = parse_probe("00-00-00-00-00-00|vEthernet|Unsupported|False|1");
        assert!(w.mac.is_none(), "a nil MAC is not a wake target");
        assert_eq!(w.magic_packet, Some(false));
    }
}
