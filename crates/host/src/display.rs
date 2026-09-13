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

/// Windows' "advanced colour" — HDR — for every display path, and the
/// power to turn it off.
///
/// A PC whose monitor is gone keeps the desktop its monitor last asked
/// for. If that was an HDR desktop, Windows still composes in half-float
/// but no longer knows the display's luminance, so a capture that converts
/// to SDR has nothing to scale by and every frame comes out black. Turning
/// advanced colour off restores an 8-bit desktop that captures normally.
pub fn advanced_color() -> Result<Value> {
    #[cfg(windows)]
    {
        win::report()
    }
    #[cfg(not(windows))]
    {
        Ok(serde_json::json!({"supported": false}))
    }
}

/// Turn advanced colour on or off wherever the display supports it, and
/// report what the displays say afterwards. Reversible, and never touches
/// a display that is already as asked.
pub fn set_advanced_color(on: bool) -> Result<Value> {
    #[cfg(windows)]
    {
        win::set(on)?;
        win::report()
    }
    #[cfg(not(windows))]
    {
        let _ = on;
        anyhow::bail!("only a Windows PC has advanced colour to turn off")
    }
}

#[cfg(windows)]
mod win {
    use anyhow::{bail, ensure, Result};
    use serde_json::Value;
    use std::mem::size_of;
    use windows::Win32::Devices::Display::{
        DisplayConfigGetDeviceInfo, DisplayConfigSetDeviceInfo, GetDisplayConfigBufferSizes,
        QueryDisplayConfig, DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
        DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
        DISPLAYCONFIG_DEVICE_INFO_SET_ADVANCED_COLOR_STATE, DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO,
        DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE,
        DISPLAYCONFIG_SOURCE_DEVICE_NAME, QDC_ALL_PATHS, QDC_ONLY_ACTIVE_PATHS,
    };

    const SUPPORTED: u32 = 1 << 0;
    const ENABLED: u32 = 1 << 1;
    const WIDE_COLOR_ENFORCED: u32 = 1 << 2;
    const FORCE_DISABLED: u32 = 1 << 3;

    /// What Windows is driving, or — when a PC has lost its monitor and
    /// drives nothing — every path it still knows about.
    fn paths() -> Result<Vec<DISPLAYCONFIG_PATH_INFO>> {
        let mut last = None;
        for flags in [QDC_ONLY_ACTIVE_PATHS, QDC_ALL_PATHS] {
            let (mut n_paths, mut n_modes) = (0u32, 0u32);
            let rc = unsafe { GetDisplayConfigBufferSizes(flags, &mut n_paths, &mut n_modes) };
            if rc.0 != 0 {
                last = Some(format!("display paths could not be counted ({})", rc.0));
                continue;
            }
            if n_paths == 0 {
                last = Some("Windows is driving no display".into());
                continue;
            }
            let mut ps = vec![DISPLAYCONFIG_PATH_INFO::default(); n_paths as usize];
            let mut ms = vec![DISPLAYCONFIG_MODE_INFO::default(); n_modes as usize];
            let rc = unsafe {
                QueryDisplayConfig(
                    flags,
                    &mut n_paths,
                    ps.as_mut_ptr(),
                    &mut n_modes,
                    ms.as_mut_ptr(),
                    None,
                )
            };
            if rc.0 != 0 {
                last = Some(format!("display paths could not be read ({})", rc.0));
                continue;
            }
            ps.truncate(n_paths as usize);
            if !ps.is_empty() {
                return Ok(ps);
            }
        }
        bail!(last.unwrap_or_else(|| "Windows lists no display".into()))
    }

    /// `\\.\DISPLAY1`, to line the state up with the screens Windows lists.
    fn source_name(path: &DISPLAYCONFIG_PATH_INFO) -> Option<String> {
        let mut name = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
            header: windows::Win32::Devices::Display::DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                size: size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                adapterId: path.sourceInfo.adapterId,
                id: path.sourceInfo.id,
            },
            ..Default::default()
        };
        if unsafe { DisplayConfigGetDeviceInfo(&mut name.header) } != 0 {
            return None;
        }
        let end = name
            .viewGdiDeviceName
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(name.viewGdiDeviceName.len());
        Some(String::from_utf16_lossy(&name.viewGdiDeviceName[..end]))
    }

    fn color_info(path: &DISPLAYCONFIG_PATH_INFO) -> Result<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO> {
        let mut info = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO {
            header: windows::Win32::Devices::Display::DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
                size: size_of::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>() as u32,
                adapterId: path.targetInfo.adapterId,
                id: path.targetInfo.id,
            },
            ..Default::default()
        };
        let rc = unsafe { DisplayConfigGetDeviceInfo(&mut info.header) };
        ensure!(rc == 0, "advanced colour state unavailable ({rc})");
        Ok(info)
    }

    pub fn report() -> Result<Value> {
        let mut displays = Vec::new();
        for path in paths()? {
            let name = source_name(&path);
            match color_info(&path) {
                Ok(info) => {
                    let bits = unsafe { info.Anonymous.value };
                    displays.push(serde_json::json!({
                        "display": name,
                        "supported": bits & SUPPORTED != 0,
                        "enabled": bits & ENABLED != 0,
                        "wide_color_enforced": bits & WIDE_COLOR_ENFORCED != 0,
                        "force_disabled": bits & FORCE_DISABLED != 0,
                        "bits_per_color": info.bitsPerColorChannel,
                        "color_encoding": info.colorEncoding.0,
                    }));
                }
                Err(e) => displays.push(serde_json::json!({
                    "display": name,
                    "error": e.to_string(),
                })),
            }
        }
        Ok(serde_json::json!({"supported": true, "displays": displays}))
    }

    pub fn set(on: bool) -> Result<()> {
        let mut refused = None;
        let mut touched = 0;
        for path in paths()? {
            let Ok(info) = color_info(&path) else {
                continue;
            };
            let bits = unsafe { info.Anonymous.value };
            if bits & SUPPORTED == 0 || (bits & ENABLED != 0) == on {
                continue;
            }
            let mut set = DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE {
                header: windows::Win32::Devices::Display::DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_SET_ADVANCED_COLOR_STATE,
                    size: size_of::<DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE>() as u32,
                    adapterId: path.targetInfo.adapterId,
                    id: path.targetInfo.id,
                },
                ..Default::default()
            };
            set.Anonymous.value = u32::from(on);
            let rc = unsafe { DisplayConfigSetDeviceInfo(&set.header) };
            if rc == 0 {
                touched += 1;
            } else {
                refused = Some(format!("Windows refused the change ({rc})"));
            }
        }
        match refused {
            // Nothing to do is success: the displays already read as asked.
            Some(e) if touched == 0 => bail!(e),
            _ => Ok(()),
        }
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

    /// Reading the state must never change it: a runner with no display
    /// says so, and one with a display answers for each of them.
    #[test]
    fn advanced_colour_is_reported_per_display() {
        let Ok(state) = super::advanced_color() else {
            return;
        };
        assert_eq!(state["supported"], true);
        let displays = state["displays"].as_array().expect("displays");
        for d in displays {
            if d.get("error").is_some() {
                continue;
            }
            assert!(d["supported"].is_boolean(), "{d}");
            assert!(d["enabled"].is_boolean(), "{d}");
            assert!(d["bits_per_color"].is_number(), "{d}");
        }
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
