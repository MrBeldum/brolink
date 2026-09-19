# Run the streaming engine as the logged-on user and put an active playback
# device back as the Windows default. The engine's own startup clears the
# default when Steam Streaming Speakers are the only active device.
$ErrorActionPreference = "Continue"
$engineDir = "C:\Program Files\BroLink\engine"
$exe = Join-Path $engineDir "sunshine.exe"
$conf = Join-Path $engineDir "config\sunshine.conf"

function Get-ActiveRender {
    $root = "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Render"
    if (-not (Test-Path $root)) { return @() }
    Get-ChildItem $root | ForEach-Object {
        $state = [int](Get-ItemProperty $_.PSPath).DeviceState
        if (($state -band 1) -eq 0) { return }
        $friendly = $null
        $desc = $null
        try { $friendly = (Get-ItemProperty -LiteralPath "$($_.PSPath)\Properties" -Name "{a45c254e-df1c-4efd-8020-67d146a850e0},2")."{a45c254e-df1c-4efd-8020-67d146a850e0},2" } catch {}
        try { $desc = (Get-ItemProperty -LiteralPath "$($_.PSPath)\Properties" -Name "{b3f8fa53-0004-438e-9003-51a46e139bfc},6")."{b3f8fa53-0004-438e-9003-51a46e139bfc},6" } catch {}
        [pscustomobject]@{
            Id       = "{0.0.0.00000000}." + $_.PSChildName
            Friendly = $friendly
            Desc     = $desc
        }
    }
}

function Get-EngineProcess {
    Get-CimInstance -ClassName Win32_Process -Filter "Name='sunshine.exe'" | Select-Object -First 1
}

function Get-EngineOwner {
    $p = Get-EngineProcess
    if (-not $p) { return $null }
    $o = Invoke-CimMethod -InputObject $p -MethodName GetOwner
    [pscustomobject]@{
        Pid     = $p.ProcessId
        Session = $p.SessionId
        User    = "$($o.Domain)\$($o.User)"
    }
}

Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
[ComImport, Guid("870AF99C-171D-4F9E-AF0D-E63DF40C2BC9")]
internal class PolicyConfigClient {}
[Guid("F8679F50-850A-41CF-9C72-430F290290C8"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
internal interface IPolicyConfig {
    [PreserveSig] int GetMixFormat(string pszDeviceName, IntPtr ppFormat);
    [PreserveSig] int GetDeviceFormat(string pszDeviceName, bool bDefault, IntPtr ppFormat);
    [PreserveSig] int ResetDeviceFormat(string pszDeviceName);
    [PreserveSig] int SetDeviceFormat(string pszDeviceName, IntPtr pEndpointFormat, IntPtr MixFormat);
    [PreserveSig] int GetProcessingPeriod(string pszDeviceName, bool bDefault, IntPtr pmftDefaultPeriod, IntPtr pmftMinimumPeriod);
    [PreserveSig] int SetProcessingPeriod(string pszDeviceName, IntPtr pmftPeriod);
    [PreserveSig] int GetShareMode(string pszDeviceName, IntPtr pMode);
    [PreserveSig] int SetShareMode(string pszDeviceName, IntPtr mode);
    [PreserveSig] int GetPropertyValue(string pszDeviceName, bool bFxStore, IntPtr key, IntPtr pv);
    [PreserveSig] int SetPropertyValue(string pszDeviceName, bool bFxStore, IntPtr key, IntPtr pv);
    [PreserveSig] int SetDefaultEndpoint(string pszDeviceName, int eRole);
    [PreserveSig] int SetEndpointVisibility(string pszDeviceName, bool bVisible);
}
public static class BroLinkAudio {
    public static int SetDefault(string id) {
        var cfg = (IPolicyConfig)new PolicyConfigClient();
        int hr = 0;
        for (int role = 0; role <= 2; role++) {
            hr = cfg.SetDefaultEndpoint(id, role);
            if (hr < 0) return hr;
        }
        return 0;
    }
}
"@

$me = "$env:USERDOMAIN\$env:USERNAME"
$sun = Get-EngineOwner
if (-not $sun -or $sun.User -ine $me) {
    Set-Service BroLinkStream -StartupType Manual -ErrorAction SilentlyContinue
    Stop-Service BroLinkStream -Force -ErrorAction SilentlyContinue
    Get-CimInstance -ClassName Win32_Process -Filter "Name='sunshine.exe'" | ForEach-Object {
        Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue
    }
    Start-Sleep -Seconds 2
    $left = Get-EngineOwner
    if ($left) {
        Write-Output ("STILL_RUNNING {0} pid={1}" -f $left.User, $left.Pid)
        exit 1
    }
    $created = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{
        CommandLine      = "`"$exe`" `"$conf`""
        CurrentDirectory = $engineDir
    }
    if ($created.ReturnValue -ne 0) {
        Write-Output ("CREATE_FAILED {0}" -f $created.ReturnValue)
        exit 1
    }
    # platf::init disables Steam Speakers when they are the default; wait
    # until that has run before putting a default back.
    Start-Sleep -Seconds 6
}

$devices = @(Get-ActiveRender)
if ($devices.Count -eq 0) { Write-Output "NO_ACTIVE_RENDER"; exit 0 }
$pick = $devices | Where-Object { $_.Desc -match "Steam Streaming" -or $_.Friendly -match "Steam Streaming" } | Select-Object -First 1
if (-not $pick) { $pick = $devices | Select-Object -First 1 }
$hr = [BroLinkAudio]::SetDefault($pick.Id)
Write-Output ("OK {0} hr=0x{1:X8}" -f $pick.Id, $hr)
