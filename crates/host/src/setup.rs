//! The one administrator step, and the per-user autostart.
//!
//! Everything that needs elevation is done by a single PowerShell script
//! behind a single UAC prompt: install the bundled Sunshine if none is
//! present, give it the login BroLink will use, open the control port to the
//! tailnet, turn Fast Startup off and arm the network card for Wake-on-LAN.
//! Each step logs and carries on, so one failure does not undo the others;
//! the service re-probes afterwards and the setup card says what is still
//! missing.

#[cfg(windows)]
use anyhow::Context;
use anyhow::Result;
use brolink_core::config::data_dir;
use brolink_core::CONTROL_PORT;
use std::path::{Path, PathBuf};

pub struct Plan<'a> {
    pub exe: &'a Path,
    pub install_sunshine: bool,
    pub sunshine_user: &'a str,
    pub sunshine_pass: &'a str,
    /// Adapter name and interface description from the wake probe; empty
    /// when unknown, which skips that step.
    pub adapter: &'a str,
    pub adapter_description: &'a str,
}

/// Log the elevated script writes; the setup card shows its tail.
pub fn log_path() -> Option<PathBuf> {
    data_dir().ok().map(|d| d.join("setup.log"))
}

/// The Sunshine installer shipped beside `brolink-host.exe`.
pub const SUNSHINE_MSI: &str = "Sunshine-Windows-AMD64-installer.msi";

pub fn bundled_sunshine(exe: &Path) -> bool {
    exe.parent().is_some_and(|d| d.join(SUNSHINE_MSI).exists())
}

pub fn script(p: &Plan<'_>) -> String {
    let q = |s: &str| s.replace('\'', "''");
    let exe_dir = p.exe.parent().unwrap_or(p.exe).display().to_string();
    let install = if p.install_sunshine {
        format!(
            r#"if (-not $dir) {{
    $msi = Join-Path '{exe_dir}' '{msi}'
    $downloaded = $false
    if (Test-Path $msi) {{
        Step "Installing the Sunshine that ships with BroLink (silent)"
    }} else {{
        Step "Downloading Sunshine"
        $api = curl.exe -sSL -A brolink https://api.github.com/repos/{repo}/releases/latest | ConvertFrom-Json
        $asset = $api.assets | Where-Object {{ $_.name -like '*Windows-AMD64-installer.msi' }} | Select-Object -First 1
        if (-not $asset) {{ throw "no Windows installer in the latest Sunshine release" }}
        $msi = Join-Path $env:TEMP 'Sunshine-installer.msi'
        curl.exe -sSL -A brolink -o $msi $asset.browser_download_url
        $downloaded = $true
        Step "Installing Sunshine $($api.tag_name) (silent)"
    }}
    $r = Start-Process msiexec.exe -ArgumentList @('/i', "`"$msi`"", '/quiet', '/norestart') -Wait -PassThru
    if ($r.ExitCode -ne 0) {{ throw "msiexec exited with $($r.ExitCode)" }}
    $dir = 'C:\Program Files\Sunshine'
    if ($downloaded) {{ Remove-Item $msi -ErrorAction SilentlyContinue }}
}}
"#,
            exe_dir = q(&exe_dir),
            msi = SUNSHINE_MSI,
            repo = crate::streamer::REPO
        )
    } else {
        String::new()
    };
    let adapter = if p.adapter.is_empty() {
        "Step \"Wake-on-LAN: adapter unknown, skipped\"\n".to_string()
    } else {
        format!(
            r#"Step "Enabling Wake-on-LAN on '{adapter}'"
try {{
    Set-NetAdapterPowerManagement -Name '{adapter}' -WakeOnMagicPacket Enabled -ErrorAction Stop
}} catch {{ Write-Output "  cmdlet failed ($_); the driver keywords below still apply" }}
# The NDIS keywords are what the driver reads: magic packet from sleep, from
# modern standby, and (Realtek's own keyword) from a full shutdown. ARP and
# NS offload keep the card answering for the PC's address while it sleeps,
# which is what lets a unicast wake packet reach it through a router.
$g = (Get-NetAdapter -Name '{adapter}' -ErrorAction SilentlyContinue).InterfaceGuid
$k = Get-ChildItem 'HKLM:\SYSTEM\CurrentControlSet\Control\Class\{{4d36e972-e325-11ce-bfc1-08002be10318}}' -ErrorAction SilentlyContinue | Where-Object {{ (Get-ItemProperty $_.PSPath -Name NetCfgInstanceId -ErrorAction SilentlyContinue).NetCfgInstanceId -eq $g }} | Select-Object -First 1
if ($k) {{
    $changed = $false
    foreach ($kw in '*WakeOnMagicPacket', '*ModernStandbyWoLMagicPacket', 'S5WakeOnLan', '*PMARPOffload', '*PMNSOffload') {{
        if ((Get-ItemProperty $k.PSPath -ErrorAction SilentlyContinue).$kw -ne '1') {{
            Set-ItemProperty $k.PSPath -Name $kw -Value '1' -Type String
            $changed = $true
        }}
    }}
    if ($changed) {{ Restart-NetAdapter -Name '{adapter}' -ErrorAction SilentlyContinue }}
}} else {{ Write-Output "  no class key for the adapter; keywords unchanged" }}
try {{ powercfg /deviceenablewake '{desc}' | Out-Null }} catch {{ Write-Output "  powercfg: $_" }}
"#,
            adapter = q(p.adapter),
            desc = q(p.adapter_description)
        )
    };
    format!(
        r#"# BroLink setup. Generated; re-run "Set up this PC" in BroLink Host rather than editing.
$ErrorActionPreference = 'Continue'
function Step($m) {{ Write-Output "[$(Get-Date -Format HH:mm:ss)] $m" }}
Step "BroLink setup started"
$dir = @('C:\Program Files\Sunshine', 'C:\Program Files\Apollo') | Where-Object {{ Test-Path (Join-Path $_ 'sunshine.exe') }} | Select-Object -First 1
try {{
{install}
    if ($dir) {{
        Step "Setting the Sunshine web login BroLink uses"
        Push-Location $dir
        & (Join-Path $dir 'sunshine.exe') --creds '{user}' '{pass}' 2>&1 | Out-Null
        Pop-Location
        Step "Restarting the Sunshine service"
        Get-Service | Where-Object {{ $_.Name -match 'Sunshine|Apollo' }} | Restart-Service -ErrorAction SilentlyContinue
    }} else {{
        Step "Sunshine is not installed and was not requested"
    }}
}} catch {{ Write-Output "  sunshine: $_" }}
Step "Opening TCP {port} to the tailnet for BroLink Host"
netsh advfirewall firewall delete rule name="BroLink Host" | Out-Null
netsh advfirewall firewall add rule name="BroLink Host" dir=in action=allow protocol=TCP localport={port} remoteip=100.64.0.0/10 program="{exe}" | Out-Null
Step "Opening UDP 9 so a Mac can check its wake path while this PC is awake"
netsh advfirewall firewall delete rule name="BroLink wake" | Out-Null
netsh advfirewall firewall add rule name="BroLink wake" dir=in action=allow protocol=UDP localport=9 program="{exe}" | Out-Null
if ($dir) {{
    # Sunshine's installer adds its own rules; these make sure the tailnet
    # can reach it even if that step was skipped or the rules were removed.
    Step "Opening Sunshine's ports to the tailnet"
    netsh advfirewall firewall delete rule name="BroLink Sunshine TCP" | Out-Null
    netsh advfirewall firewall delete rule name="BroLink Sunshine UDP" | Out-Null
    netsh advfirewall firewall add rule name="BroLink Sunshine TCP" dir=in action=allow protocol=TCP localport=47984-48010 remoteip=100.64.0.0/10 | Out-Null
    netsh advfirewall firewall add rule name="BroLink Sunshine UDP" dir=in action=allow protocol=UDP localport=47998-48010 remoteip=100.64.0.0/10 | Out-Null
}}
Step "Turning Fast Startup off: a PC shut down with it on cannot be woken"
Set-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Power' -Name HiberbootEnabled -Value 0 -Type DWord
Step "Never idle-sleep when plugged in, so Tailscale stays up from anywhere"
powercfg /change standby-timeout-ac 0
powercfg /change hibernate-timeout-ac 0
{adapter}Step "BroLink setup finished"
"#,
        install = install,
        user = q(p.sunshine_user),
        pass = q(p.sunshine_pass),
        port = CONTROL_PORT,
        exe = p.exe.display(),
        adapter = adapter,
    )
}

/// Write the script and run it elevated. Blocks until the elevated
/// PowerShell exits (or the UAC prompt is declined, which is an error).
pub fn run(p: &Plan<'_>) -> Result<()> {
    let dir = data_dir()?;
    let path = dir.join("setup.ps1");
    // PowerShell 5.1 reads a BOM-less file as the system ANSI code page, so
    // a Korean/Japanese username or adapter name ("이더넷") would be mangled
    // and the firewall rule would point at a path that does not exist.
    let mut bytes = b"\xEF\xBB\xBF".to_vec();
    bytes.extend(script(p).as_bytes());
    std::fs::write(&path, bytes)?;
    let log = dir.join("setup.log");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let file = path.display().to_string().replace('\'', "''");
        let logq = log.display().to_string().replace('\'', "''");
        let launch = format!(
            "$p = Start-Process powershell -Verb RunAs -Wait -PassThru -WindowStyle Hidden -ArgumentList @('-NoProfile','-ExecutionPolicy','Bypass','-Command','& ''{file}'' *>&1 | Out-File -Encoding utf8 ''{logq}'''); exit $p.ExitCode"
        );
        let status = std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &launch])
            .creation_flags(0x0800_0000)
            .status()
            .context("launch elevated PowerShell")?;
        anyhow::ensure!(
            status.success(),
            "the administrator prompt was declined or setup failed (see {})",
            log.display()
        );
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = log;
        anyhow::bail!("setup runs on Windows only")
    }
}

/// Register or remove `brolink-host.exe --background` under the current
/// user's Run key.
pub fn set_start_with_windows(enable: bool, exe: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
        let mut c = std::process::Command::new("reg");
        if enable {
            c.args([
                "add",
                KEY,
                "/v",
                "BroLinkHost",
                "/t",
                "REG_SZ",
                "/d",
                &run_value(exe),
                "/f",
            ]);
        } else {
            c.args(["delete", KEY, "/v", "BroLinkHost", "/f"]);
        }
        let status = c
            .creation_flags(0x0800_0000)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;
        anyhow::ensure!(status.success() || !enable, "could not write the Run key");
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = (enable, exe);
        Ok(())
    }
}

pub fn starts_with_windows() -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("reg")
            .args([
                "query",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                "/v",
                "BroLinkHost",
            ])
            .creation_flags(0x0800_0000)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
    #[cfg(not(windows))]
    false
}

pub fn run_value(exe: &Path) -> String {
    format!("\"{}\" --background", exe.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_value_quotes_the_path_and_asks_for_background() {
        let p = PathBuf::from(r"C:\Users\Ada\AppData\Local\BroLink\brolink-host.exe");
        assert_eq!(
            run_value(&p),
            r#""C:\Users\Ada\AppData\Local\BroLink\brolink-host.exe" --background"#
        );
    }

    #[test]
    fn script_escapes_quotes_and_skips_what_is_not_wanted() {
        let exe = PathBuf::from(r"C:\x\brolink-host.exe");
        let s = script(&Plan {
            exe: &exe,
            install_sunshine: false,
            sunshine_user: "brolink",
            sunshine_pass: "p'w",
            adapter: "Ethernet",
            adapter_description: "Realtek PCIe GbE",
        });
        assert!(s.contains("--creds 'brolink' 'p''w'"), "{s}");
        assert!(!s.contains("Downloading Sunshine"));
        assert!(s.contains("Set-NetAdapterPowerManagement -Name 'Ethernet'"));
        assert!(s.contains("Restart-NetAdapter -Name 'Ethernet'"));
        assert!(s.contains("'S5WakeOnLan'"));
        assert!(s.contains("HiberbootEnabled -Value 0"));
        assert!(s.contains("powercfg /change standby-timeout-ac 0"));
        assert!(s.contains("powercfg /change hibernate-timeout-ac 0"));
        assert!(s.contains("protocol=UDP localport=9 program=\"C:\\x\\brolink-host.exe\""));
        assert!(s.contains("localport=47984-48010 remoteip=100.64.0.0/10"));
        assert!(s.contains("powercfg /deviceenablewake 'Realtek PCIe GbE'"));
        assert!(s.contains(
            "localport=47850 remoteip=100.64.0.0/10 program=\"C:\\x\\brolink-host.exe\""
        ));

        let s = script(&Plan {
            exe: &exe,
            install_sunshine: true,
            sunshine_user: "u",
            sunshine_pass: "p",
            adapter: "",
            adapter_description: "",
        });
        assert!(s.contains("Downloading Sunshine"));
        assert!(s.contains("Sunshine-Windows-AMD64-installer.msi"));
        assert!(s.contains("Join-Path"));
        assert!(
            s.contains(r#"@('/i', "`"$msi`"", '/quiet', '/norestart')"#),
            "{s}"
        );
        assert!(s.contains("adapter unknown, skipped"));
        assert!(!s.contains("Set-NetAdapterPowerManagement"));
    }
}
