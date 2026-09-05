//! Whether this PC can be woken remotely, and the MAC a Mac needs to do it.
//! One PowerShell call, on the service's refresh thread.

use anyhow::{Context, Result};
use std::net::Ipv4Addr;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct WakeInfo {
    /// The adapter that owns the LAN address, and that address.
    pub adapter: String,
    pub description: String,
    pub lan_ip: Option<Ipv4Addr>,
    pub mac: Option<String>,
    /// `Some(true)` when the adapter wakes on a magic packet *and* Windows
    /// lets the device wake the machine.
    pub magic_packet: Option<bool>,
}

/// The adapter Windows would use to reach the internet, which is the one a
/// wake packet arrives on.
pub fn probe() -> WakeInfo {
    #[cfg(windows)]
    {
        // The NDIS keyword in the adapter's class key is what the driver
        // reads; Get-NetAdapterPowerManagement fails outright on some
        // drivers ("a device attached to the system is not functioning").
        const SCRIPT: &str = "$r = Get-NetRoute -DestinationPrefix '0.0.0.0/0' -AddressFamily IPv4 -ErrorAction SilentlyContinue | Where-Object { $_.InterfaceAlias -ne 'Tailscale' } | Sort-Object RouteMetric,InterfaceMetric | Select-Object -First 1; \
             $n = Get-NetAdapter -InterfaceIndex $r.InterfaceIndex -ErrorAction SilentlyContinue | Select-Object -First 1; \
             $ip = (Get-NetIPAddress -InterfaceIndex $r.InterfaceIndex -AddressFamily IPv4 -ErrorAction SilentlyContinue | Select-Object -First 1).IPAddress; \
             $k = Get-ChildItem 'HKLM:\\SYSTEM\\CurrentControlSet\\Control\\Class\\{4d36e972-e325-11ce-bfc1-08002be10318}' -ErrorAction SilentlyContinue | Where-Object { (Get-ItemProperty $_.PSPath -Name NetCfgInstanceId -ErrorAction SilentlyContinue).NetCfgInstanceId -eq $n.InterfaceGuid } | Select-Object -First 1; \
             $wol = ''; if ($k) { $wol = (Get-ItemProperty $k.PSPath -ErrorAction SilentlyContinue).'*WakeOnMagicPacket' }; \
             $armed = @(powercfg /devicequery wake_armed) -contains $n.InterfaceDescription; \
             Write-Output \"$($n.MacAddress)|$($n.Name)|$($n.InterfaceDescription)|$ip|$wol|$armed\"";
        match powershell(SCRIPT) {
            Ok(o) => parse_probe(&o),
            Err(e) => {
                tracing::debug!("wake probe: {e:#}");
                WakeInfo::default()
            }
        }
    }
    #[cfg(not(windows))]
    WakeInfo::default()
}

/// Parse `mac|adapter|description|ip|*WakeOnMagicPacket|armed`, where the
/// keyword is `1`, `0`, or missing.
fn parse_probe(line: &str) -> WakeInfo {
    let mut f = line.trim().split('|').map(str::trim);
    let mac = f
        .next()
        .and_then(brolink_core::wake::MacAddr::parse)
        .map(|m| m.to_string());
    let adapter = f.next().unwrap_or("").to_string();
    let description = f.next().unwrap_or("").to_string();
    let lan_ip = f.next().and_then(|s| s.parse().ok());
    let wol = f.next().unwrap_or("");
    let armed = f.next().unwrap_or("").eq_ignore_ascii_case("true");
    let magic_packet = match wol {
        "1" | "Enabled" => Some(armed),
        "0" | "Disabled" | "Unsupported" => Some(false),
        _ => None,
    };
    WakeInfo {
        adapter,
        description,
        lan_ip,
        mac,
        magic_packet,
    }
}

#[cfg(windows)]
fn powershell(script: &str) -> Result<String> {
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
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
        let w = parse_probe("02-00-00-00-00-01|Ethernet|Example NIC|192.168.1.10|1|True\r\n");
        assert_eq!(w.mac.as_deref(), Some("02:00:00:00:00:01"));
        assert_eq!(w.adapter, "Ethernet");
        assert_eq!(w.description, "Example NIC");
        assert_eq!(w.lan_ip, Some("192.168.1.10".parse().unwrap()));
        assert_eq!(w.magic_packet, Some(true));

        // Enabled on the adapter but Windows will not let it wake the PC.
        let w = parse_probe("AA-BB-CC-DD-EE-FF|Wi-Fi|Intel Wi-Fi|10.0.0.5|1|False");
        assert_eq!(w.magic_packet, Some(false));

        let w = parse_probe("|||||False");
        assert!(w.mac.is_none() && w.lan_ip.is_none());
        assert_eq!(w.magic_packet, None);

        let w = parse_probe("00-00-00-00-00-00|vEthernet|Hyper-V|172.16.0.1|0|False");
        assert!(w.mac.is_none(), "a nil MAC is not a wake target");
        assert_eq!(w.magic_packet, Some(false));
    }
}
