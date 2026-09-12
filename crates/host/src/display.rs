//! Display diagnostics for a PC whose stream cannot show its own settings.

use anyhow::Result;
use serde_json::Value;

/// This is a fixed read-only probe, not a remote command runner.
pub fn probe() -> Result<Value> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let output = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", PROBE])
            .creation_flags(0x0800_0000)
            .output()?;
        anyhow::ensure!(
            output.status.success(),
            "Windows display probe failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8_lossy(&output.stdout);
        Ok(serde_json::from_str(
            text.trim_start_matches('\u{feff}').trim(),
        )?)
    }
    #[cfg(not(windows))]
    {
        Ok(serde_json::json!({"supported": false}))
    }
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn windows_probe_reports_displays_without_modifying_them() {
        let status = super::probe().expect("Windows display probe");
        assert_eq!(status["supported"], true);
        assert!(status["elevated"].is_boolean());
        assert!(status["screens"].is_array());
        assert!(status["monitors"].is_array());
        assert!(status["gpus"].is_array());
    }
}

#[cfg(windows)]
const PROBE: &str = r#"
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
Add-Type -AssemblyName System.Windows.Forms
$me = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object Security.Principal.WindowsPrincipal($me)
$screens = @([Windows.Forms.Screen]::AllScreens | ForEach-Object {
    @{name=$_.DeviceName; primary=$_.Primary; width=$_.Bounds.Width; height=$_.Bounds.Height}
})
$monitors = @(Get-PnpDevice -Class Monitor -PresentOnly -ErrorAction SilentlyContinue | Select-Object FriendlyName,Status,InstanceId)
$gpus = @(Get-CimInstance Win32_VideoController | Select-Object Name,DriverVersion,CurrentHorizontalResolution,CurrentVerticalResolution,Status)
$sunshine = @(Get-CimInstance Win32_Process -Filter "Name='sunshine.exe'" | Select-Object SessionId,ExecutablePath)
@{supported=$true; elevated=$principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator); session=[Diagnostics.Process]::GetCurrentProcess().SessionId; screens=$screens; monitors=$monitors; gpus=$gpus; sunshine=$sunshine} | ConvertTo-Json -Depth 5 -Compress
"#;
