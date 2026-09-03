# Make this PC reachable by a ForgeLink client, with the smallest change that
# works. Run from an ELEVATED PowerShell.
#
# Two things block a client, and neither is obvious from the host's own logs:
#
#   1. "Query User" BLOCK rules, which Windows writes whenever its network
#      prompt is dismissed. Windows evaluates block before allow, so these
#      defeat any allow rule the host adds.
#   2. No inbound allow rule for the UDP port.
#
# By default the allow rule is scoped to the local subnet, so the port is not
# exposed to the internet. Pass -Wan for play from outside your network.
#
# This script deliberately does NOT change your network category. Setting a
# network to Private tells Windows to trust everything on it, which is a
# bigger decision than streaming needs -- connecting by ticket works fine on
# a Public network. It only affects automatic LAN discovery.
[CmdletBinding()]
param(
    [int]$Port = 47850,
    # Path to the host binary. Defaults to the release build in this repo.
    [string]$HostExe,
    # Allow from any address, not just the local subnet. Needed for play over
    # the internet; leaves UDP $Port open to the world.
    [switch]$Wan,
    # Show what would change and exit.
    [switch]$DryRun
)

$ErrorActionPreference = "Stop"

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object Security.Principal.WindowsPrincipal($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw "run this from an elevated PowerShell (right-click, Run as administrator)"
}

if (-not $HostExe) {
    $root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
    $HostExe = Join-Path $root "target\release\forgelink-host.exe"
}
if (-not (Test-Path $HostExe)) { throw "no host binary at $HostExe -- pass -HostExe" }
$HostExe = (Resolve-Path $HostExe).Path
Write-Host "host binary: $HostExe"

# 1. Block rules naming this executable, whatever profile they are on.
$blocks = @(Get-NetFirewallApplicationFilter -Program $HostExe -ErrorAction SilentlyContinue |
    Get-NetFirewallRule -ErrorAction SilentlyContinue |
    Where-Object { $_.Action -eq "Block" -and $_.Direction -eq "Inbound" })

if ($blocks.Count -eq 0) {
    Write-Host "no inbound block rules for this executable" -ForegroundColor Green
} else {
    Write-Host "$($blocks.Count) inbound BLOCK rule(s) to remove:" -ForegroundColor Yellow
    $blocks | ForEach-Object { Write-Host "  [$($_.Profile)] $($_.DisplayName)" }
    if (-not $DryRun) {
        $blocks | Remove-NetFirewallRule
        Write-Host "removed" -ForegroundColor Green
    }
}

# 2. An inbound allow rule for the stream itself.
$scope = if ($Wan) { "Any" } else { "LocalSubnet" }
$ruleName = "ForgeLink Host (UDP $Port)"
$existing = Get-NetFirewallRule -DisplayName $ruleName -ErrorAction SilentlyContinue
if ($existing) {
    Write-Host "replacing the existing '$ruleName' rule"
    if (-not $DryRun) { $existing | Remove-NetFirewallRule }
}
Write-Host "allow rule: UDP $Port inbound, remote address = $scope"
if ($Wan) {
    Write-Warning "-Wan opens UDP $Port to any address. Only do this if you play from outside your network."
}
if (-not $DryRun) {
    New-NetFirewallRule -DisplayName $ruleName -Direction Inbound -Action Allow `
        -Protocol UDP -LocalPort $Port -Program $HostExe -RemoteAddress $scope `
        -Profile Any | Out-Null
    Write-Host "added" -ForegroundColor Green
}

# 3. Report the network category without changing it.
Write-Host ""
Write-Host "--- network profiles (unchanged) ---"
Get-NetConnectionProfile | Select-Object Name, InterfaceAlias, NetworkCategory | Format-Table -AutoSize
Write-Host "A Public network still allows the rule above. It only turns off automatic"
Write-Host "LAN discovery, so connect using the ticket the host prints rather than"
Write-Host "waiting for the PC to appear in the client's list."

if ($DryRun) { Write-Host "`n-DryRun: nothing was changed" -ForegroundColor Cyan }
