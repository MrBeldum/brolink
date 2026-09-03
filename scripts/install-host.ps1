# Install ForgeLink Host for the current user.
$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$Exe = Join-Path $Root "target\release\forgelink-host.exe"
if (-not (Test-Path $Exe)) {
    Write-Error "Build first: cargo build --release -p forgelink-host"
}
$DestDir = Join-Path $env:LOCALAPPDATA "ForgeLink"
New-Item -ItemType Directory -Force -Path $DestDir | Out-Null
Copy-Item $Exe (Join-Path $DestDir "forgelink-host.exe") -Force
$Ffmpeg = "C:\ffmpeg\bin\ffmpeg.exe"
if (Test-Path $Ffmpeg) {
    Copy-Item $Ffmpeg (Join-Path $DestDir "ffmpeg.exe") -ErrorAction SilentlyContinue
}
$Wsh = New-Object -ComObject WScript.Shell
$Desktop = [Environment]::GetFolderPath("Desktop")
$Lnk = $Wsh.CreateShortcut((Join-Path $Desktop "ForgeLink Host.lnk"))
$Lnk.TargetPath = Join-Path $DestDir "forgelink-host.exe"
$Lnk.WorkingDirectory = $DestDir
$Lnk.Save()
Write-Host "Installed to $DestDir"
Write-Host "Shortcut: Desktop\ForgeLink Host.lnk"
