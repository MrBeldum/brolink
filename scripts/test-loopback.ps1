# Loopback smoke test: host + headless client on this machine.
#
# Proves the whole pipeline end to end -- capture, encode, encrypt, fragment,
# send, reassemble, decrypt, decode -- against the real ffmpeg and the real
# GPU encoder. It is the check that matters before shipping a change.
[CmdletBinding()]
param(
    # Which build to exercise. "release" is the default because that is what
    # people actually run.
    [ValidateSet("release", "debug")]
    [string]$Configuration = "release",
    # Skip `cargo build`. Only pass this if you just built.
    [switch]$NoBuild
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Push-Location $Root
try {
    if (-not $NoBuild) {
        Write-Host "Building ($Configuration)..."
        # Building first is not optional: the previous version of this script
        # preferred target\release and would happily test a binary from days
        # ago, reporting a pass for code that was never run.
        $buildArgs = @("build", "--workspace")
        if ($Configuration -eq "release") { $buildArgs += "--release" }
        & cargo @buildArgs
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed: $LASTEXITCODE" }
    }

    $BinDir = Join-Path $Root "target\$Configuration"
    $HostExe = Join-Path $BinDir "forgelink-host.exe"
    $ClientExe = Join-Path $BinDir "forgelink-client.exe"
    foreach ($exe in @($HostExe, $ClientExe)) {
        if (-not (Test-Path $exe)) { throw "missing $exe -- build the workspace first" }
    }
    Write-Host "host   $HostExe ($((Get-Item $HostExe).LastWriteTime))"
    Write-Host "client $ClientExe ($((Get-Item $ClientExe).LastWriteTime))"

    $HostOut = Join-Path $env:TEMP "fl-host.out.log"
    $HostErr = Join-Path $env:TEMP "fl-host.err.log"
    Remove-Item $HostOut, $HostErr -ErrorAction SilentlyContinue

    $env:RUST_LOG = "info"
    $hp = Start-Process -FilePath $HostExe `
        -ArgumentList "--headless", "--name", "TEST-PC", "--no-pin", "--no-firewall" `
        -PassThru -NoNewWindow `
        -RedirectStandardOutput $HostOut -RedirectStandardError $HostErr

    try {
        # Wait for the host to publish its ticket rather than guessing at a
        # sleep: encoder probing takes a variable couple of seconds.
        $deadline = (Get-Date).AddSeconds(45)
        $ready = $false
        while ((Get-Date) -lt $deadline) {
            if ($hp.HasExited) { throw "host exited early with code $($hp.ExitCode)" }
            if ((Test-Path $HostOut) -and (Select-String -Path $HostOut -Pattern "ForgeLink ticket" -Quiet)) {
                $ready = $true
                break
            }
            Start-Sleep -Milliseconds 200
        }
        if (-not $ready) { throw "host did not publish a ticket within 45s" }

        # The ticket is printed on the line after the "ForgeLink ticket:" header.
        $lines = Get-Content $HostOut
        $idx = ($lines | Select-String -Pattern "ForgeLink ticket" | Select-Object -First 1).LineNumber
        $Ticket = ($lines[$idx]).Trim()
        if (-not $Ticket) { throw "could not read the ticket from the host output" }
        Write-Host "ticket $($Ticket.Substring(0, [Math]::Min(24, $Ticket.Length)))..."

        # Bare address: no identity to pin, so this is the weakest path.
        Write-Host "--- connecting by address ---"
        & $ClientExe --connect 127.0.0.1 --headless
        if ($LASTEXITCODE -ne 0) { throw "client failed over a bare address: $LASTEXITCODE" }

        # Ticket: exercises the v2 ticket parse, candidate ordering, and the
        # host-identity check that a bare address cannot do.
        Write-Host "--- connecting by ticket ---"
        & $ClientExe --connect $Ticket --headless
        if ($LASTEXITCODE -ne 0) { throw "client failed over a ticket: $LASTEXITCODE" }

        Write-Host "loopback OK" -ForegroundColor Green
    } catch {
        # Without the host's own log a client-side failure says nothing about
        # why: almost every real failure is on the capture/encode side.
        Write-Host "--- host stdout ---" -ForegroundColor Yellow
        if (Test-Path $HostOut) { Get-Content $HostOut }
        Write-Host "--- host stderr ---" -ForegroundColor Yellow
        if (Test-Path $HostErr) { Get-Content $HostErr }
        throw
    } finally {
        if (-not $hp.HasExited) { Stop-Process -Id $hp.Id -Force }
    }
} finally {
    Pop-Location
}
