# Install BroLink Host for the current user: copy the exe and the bundled
# Sunshine installer into %LOCALAPPDATA%\BroLink, add Start Menu and Desktop
# shortcuts, and open it. The app itself runs the one administrator step
# (Sunshine, firewall, Wake-on-LAN) when you click "Set up this PC".
[CmdletBinding()]
param(
    [string]$Exe = "",
    [switch]$NoStart
)

$ErrorActionPreference = "Stop"
if (-not $Exe) {
    # Beside this script in the release zip; in the repo, under target\release.
    $Here = Split-Path -Parent $MyInvocation.MyCommand.Path
    $Root = Split-Path -Parent $Here
    foreach ($c in @(
            (Join-Path $Here "brolink-host.exe"),
            (Join-Path $Root "brolink-host.exe"),
            (Join-Path $Root "target\release\brolink-host.exe")
        )) {
        if (Test-Path $c) { $Exe = $c; break }
    }
}
if (-not $Exe -or -not (Test-Path $Exe)) {
    Write-Error "brolink-host.exe not found. Download a release or build with: cargo build --release -p brolink-host"
}

$DestDir = Join-Path $env:LOCALAPPDATA "BroLink"
New-Item -ItemType Directory -Force -Path $DestDir | Out-Null
$DestExe = Join-Path $DestDir "brolink-host.exe"

# A running service or panel holds the old exe open; stop them first.
try {
    Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:47850/v1/quit" -ContentType 'application/json' -Body '{}' -TimeoutSec 2 | Out-Null
    Start-Sleep -Milliseconds 800
} catch {}
Get-Process brolink-host -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 300
Copy-Item $Exe $DestExe -Force
Write-Host "Installed: $DestExe"
$Msi = Join-Path (Split-Path -Parent $Exe) "Sunshine-Windows-AMD64-installer.msi"
if (Test-Path $Msi) {
    Copy-Item $Msi $DestDir -Force
    Write-Host "Bundled Sunshine installer copied; setup will not need to download it."
}

$Wsh = New-Object -ComObject WScript.Shell
$Desktop = [Environment]::GetFolderPath("Desktop")
$StartMenu = Join-Path ([Environment]::GetFolderPath("StartMenu")) "Programs"
New-Item -ItemType Directory -Force -Path $StartMenu | Out-Null
foreach ($folder in @($Desktop, $StartMenu)) {
    $Lnk = $Wsh.CreateShortcut((Join-Path $folder "BroLink Host.lnk"))
    $Lnk.TargetPath = $DestExe
    $Lnk.WorkingDirectory = $DestDir
    $Lnk.Description = "Your PC, from your Mac"
    $Lnk.Save()
}

# The background service at logon; the app offers the same toggle.
$Run = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"
New-ItemProperty -Path $Run -Name "BroLinkHost" -Value "`"$DestExe`" --background" -PropertyType String -Force | Out-Null

Write-Host "Shortcut: Desktop\BroLink Host.lnk"
Write-Host "Open BroLink Host and click 'Set up this PC' once."
if (-not $NoStart) {
    Start-Process -FilePath $DestExe -WorkingDirectory $DestDir
}
