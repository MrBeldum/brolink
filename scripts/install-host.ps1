# Install BroLink Host for the current user.
#
# Copies the release binary into %LOCALAPPDATA%\BroLink, fetches FFmpeg if
# it is missing, writes Start Menu + Desktop shortcuts, and tries to add a
# firewall allow rule. Run from an elevated PowerShell if you want the
# firewall rule to succeed.
[CmdletBinding()]
param(
    [switch]$Start,
    [switch]$StartWithWindows,
    [switch]$SkipFfmpeg
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$Exe = Join-Path $Root "target\release\brolink-host.exe"
if (-not (Test-Path $Exe)) {
    Write-Error "Build first: cargo build --release -p brolink-host"
}

$DestDir = Join-Path $env:LOCALAPPDATA "BroLink"
New-Item -ItemType Directory -Force -Path $DestDir | Out-Null
$DestExe = Join-Path $DestDir "brolink-host.exe"
Copy-Item $Exe $DestExe -Force
Write-Host "Host: $DestExe"

function Install-Ffmpeg {
    $local = Join-Path $DestDir "ffmpeg.exe"
    if (Test-Path $local) {
        Write-Host "FFmpeg: $local"
        return
    }
    foreach ($c in @(
            "ffmpeg.exe",
            "C:\ffmpeg\bin\ffmpeg.exe",
            "$env:ProgramData\chocolatey\bin\ffmpeg.exe"
        )) {
        if (Test-Path $c) {
            Copy-Item $c $local -Force
            Write-Host "FFmpeg: copied $c"
            return
        }
    }
    $zip = Join-Path $env:TEMP "brolink-ffmpeg.zip"
    $url = "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip"
    Write-Host "Downloading FFmpeg (this is a one-time ~80 MB download)…"
    try {
        Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing
    } catch {
        Write-Warning "Could not download FFmpeg: $_. Place ffmpeg.exe in $DestDir and re-run."
        return
    }
    $extract = Join-Path $env:TEMP "brolink-ffmpeg"
    if (Test-Path $extract) { Remove-Item $extract -Recurse -Force }
    Expand-Archive -Path $zip -DestinationPath $extract -Force
    $found = Get-ChildItem -Path $extract -Filter ffmpeg.exe -Recurse | Select-Object -First 1
    if (-not $found) {
        Write-Warning "The FFmpeg zip did not contain ffmpeg.exe."
        return
    }
    Copy-Item $found.FullName $local -Force
    Write-Host "FFmpeg: $local"
}

if (-not $SkipFfmpeg) { Install-Ffmpeg }

# Desktop + Start Menu shortcuts.
$Wsh = New-Object -ComObject WScript.Shell
$Desktop = [Environment]::GetFolderPath("Desktop")
$StartMenu = Join-Path ([Environment]::GetFolderPath("StartMenu")) "Programs"
New-Item -ItemType Directory -Force -Path $StartMenu | Out-Null
foreach ($folder in @($Desktop, $StartMenu)) {
    $lnkPath = Join-Path $folder "BroLink Host.lnk"
    $Lnk = $Wsh.CreateShortcut($lnkPath)
    $Lnk.TargetPath = $DestExe
    $Lnk.WorkingDirectory = $DestDir
    $Lnk.Description = "Stream this PC to your Mac"
    $Lnk.Save()
}

if ($StartWithWindows) {
    $Run = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"
    New-ItemProperty -Path $Run -Name "BroLinkHost" -Value "`"$DestExe`"" -PropertyType String -Force | Out-Null
    Write-Host "Start with Windows: on"
}

# Firewall: best-effort, quiet if we are not elevated.
$rule = "BroLink Host"
$netsh = Get-Command netsh -ErrorAction SilentlyContinue
if ($netsh) {
    $exists = & netsh advfirewall firewall show rule name="$rule" 2>$null
    if ($LASTEXITCODE -ne 0) {
        & netsh advfirewall firewall add rule name="$rule" dir=in action=allow protocol=UDP localport=47850 | Out-Null
        if ($LASTEXITCODE -eq 0) {
            Write-Host "Firewall: allowed UDP 47850"
        } else {
            Write-Warning "Could not add a firewall rule (needs Administrator). Clients on the LAN may not connect."
        }
    }
}

Write-Host ""
Write-Host "Installed to $DestDir"
Write-Host "Shortcut: Desktop\BroLink Host.lnk"
Write-Host "Open the host, copy the ticket, paste it on your Mac."
if ($Start) {
    Start-Process -FilePath $DestExe -WorkingDirectory $DestDir
}
