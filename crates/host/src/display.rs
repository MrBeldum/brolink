//! Display diagnostics for a PC whose stream cannot show its own settings.
//!
//! A black stream has several causes that look identical from the Mac: no
//! monitor attached, a monitor that is powered down, a locked session, or a
//! capture that Sunshine cannot read. The probe answers all of them at once.
//! It reads the desktop's own brightness as a handful of sampled pixels —
//! numbers, never an image — so a black picture can be blamed on the PC's
//! desktop or on Sunshine's capture without anyone looking at the screen.

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
        assert!(status["locked"].is_boolean());
        assert!(status["screens"].is_array());
        assert!(status["monitors"].is_array());
        assert!(status["monitor_ids"].is_array());
        assert!(status["desktop_monitors"].is_array());
        assert!(status["gpus"].is_array());
        assert!(status["sunshine"].is_array());
        assert!(status["sunshine_service"].is_array());
        // Every section reports its own failure rather than losing the rest.
        assert!(status["errors"].is_object());
        let desktop = &status["desktop"];
        assert!(
            desktop.is_null() || desktop["max"].is_number(),
            "desktop brightness: {desktop}"
        );
    }
}

/// Each section stands on its own: a class that is missing on this Windows
/// build leaves one error behind instead of losing the whole report.
#[cfg(windows)]
const PROBE: &str = r#"
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
$errors = @{}

$me = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object Security.Principal.WindowsPrincipal($me)

$locked = $false
try { $locked = @(Get-Process -Name LogonUI -ErrorAction SilentlyContinue).Count -gt 0 }
catch { $errors['locked'] = $_.Exception.Message }

$screens = @()
try {
    $screens = @([Windows.Forms.Screen]::AllScreens | ForEach-Object {
        @{name=$_.DeviceName; primary=$_.Primary; width=$_.Bounds.Width;
          height=$_.Bounds.Height; left=$_.Bounds.X; top=$_.Bounds.Y; bpp=$_.BitsPerPixel}
    })
} catch { $errors['screens'] = $_.Exception.Message }

$monitors = @()
try {
    $monitors = @(Get-PnpDevice -Class Monitor -ErrorAction Stop |
        Select-Object FriendlyName,Status,Present,InstanceId)
} catch { $errors['monitors'] = $_.Exception.Message }

# Nothing here means Windows sees no monitor hardware at all: the desktop
# is then a phantom that Desktop Duplication captures as black.
$monitorIds = @()
try {
    $monitorIds = @(Get-CimInstance -Namespace root\wmi -ClassName WmiMonitorID -ErrorAction Stop |
        ForEach-Object {
            @{instance=$_.InstanceName; active=$_.Active;
              name=(-join ($_.UserFriendlyName | Where-Object {$_ -gt 0} | ForEach-Object {[char]$_}))}
        })
} catch { $errors['monitor_ids'] = $_.Exception.Message }

# Availability 3 is powered on; 8 means the monitor is off.
$desktopMonitors = @()
try {
    $desktopMonitors = @(Get-CimInstance Win32_DesktopMonitor -ErrorAction Stop |
        Select-Object Name,Availability,ScreenWidth,ScreenHeight,Status,PNPDeviceID)
} catch { $errors['desktop_monitors'] = $_.Exception.Message }

$connections = @()
try {
    $connections = @(Get-CimInstance -Namespace root\wmi -ClassName WmiMonitorConnectionParams -ErrorAction Stop |
        Select-Object InstanceName,VideoOutputTechnology,Active)
} catch { $errors['connections'] = $_.Exception.Message }

$gpus = @()
try {
    $gpus = @(Get-CimInstance Win32_VideoController -ErrorAction Stop |
        Select-Object Name,DriverVersion,CurrentHorizontalResolution,CurrentVerticalResolution,
                      CurrentRefreshRate,CurrentBitsPerPixel,VideoModeDescription,Status,Availability)
} catch { $errors['gpus'] = $_.Exception.Message }

$sunshine = @()
try {
    $sunshine = @(Get-CimInstance Win32_Process -Filter "Name='sunshine.exe'" -ErrorAction Stop |
        Select-Object ProcessId,SessionId,ExecutablePath)
} catch { $errors['sunshine'] = $_.Exception.Message }

$service = @()
try {
    $service = @(Get-CimInstance Win32_Service -Filter "Name like '%unshine%'" -ErrorAction Stop |
        Select-Object Name,State,StartMode,ProcessId,StartName)
} catch { $errors['sunshine_service'] = $_.Exception.Message }

# The desktop's own brightness, as sampled numbers rather than a picture:
# black here means the PC has nothing to show, not that capture failed.
$desktop = $null
try {
    $s = [Windows.Forms.Screen]::PrimaryScreen
    if ($s) {
        $w = $s.Bounds.Width
        $h = $s.Bounds.Height
        $bmp = New-Object Drawing.Bitmap($w, $h)
        $g = [Drawing.Graphics]::FromImage($bmp)
        $g.CopyFromScreen($s.Bounds.X, $s.Bounds.Y, 0, 0, (New-Object Drawing.Size($w, $h)))
        $g.Dispose()
        $cols = 40
        $rows = 25
        $max = 0
        $sum = 0
        $lit = 0
        for ($row = 0; $row -lt $rows; $row++) {
            for ($col = 0; $col -lt $cols; $col++) {
                $px = [int](($col + 0.5) * $w / $cols)
                $py = [int](($row + 0.5) * $h / $rows)
                $p = $bmp.GetPixel($px, $py)
                $v = [int]$p.R
                if ([int]$p.G -gt $v) { $v = [int]$p.G }
                if ([int]$p.B -gt $v) { $v = [int]$p.B }
                $sum += $v
                if ($v -gt $max) { $max = $v }
                if ($v -gt 16) { $lit++ }
            }
        }
        $bmp.Dispose()
        $desktop = @{width=$w; height=$h; samples=($cols*$rows); max=$max;
                     mean=[math]::Round($sum / ($cols*$rows), 1); lit=$lit}
    }
} catch { $errors['desktop'] = $_.Exception.Message }

@{supported=$true;
  elevated=$principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator);
  session=[Diagnostics.Process]::GetCurrentProcess().SessionId;
  user=$me.Name;
  locked=$locked;
  screens=$screens;
  monitors=$monitors;
  monitor_ids=$monitorIds;
  desktop_monitors=$desktopMonitors;
  connections=$connections;
  gpus=$gpus;
  sunshine=$sunshine;
  sunshine_service=$service;
  desktop=$desktop;
  errors=$errors} | ConvertTo-Json -Depth 6 -Compress
"#;
