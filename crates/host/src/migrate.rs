//! PowerShell fragments `setup::script` splices in when an MSI-installed
//! engine is migrated: copy → verify → start → prove → uninstall, never
//! uninstall-first. Elevation is why this is script, not Rust.
//!
//! Named list is the live Hermes inventory (task 14): required state is
//! `sunshine_state.json` plus `credentials/cacert.pem` and `cakey.pem`.
//! `apps.json` and `sunshine.conf` are best-effort. The log is regenerated.

/// MSI ProductCode of the live Sunshine install on Hermes.
pub const PRODUCT_CODE: &str = "{0B8229CA-3802-4716-88DD-5AA32BFCD8B1}";

pub const REQUIRED: &[&[&str]] = &[
    &["sunshine_state.json"],
    &["credentials", "cacert.pem"],
    &["credentials", "cakey.pem"],
];

pub const OPTIONAL: &[&[&str]] = &[&["apps.json"], &["sunshine.conf"]];

pub const OLD_CONFIG: &[&str] = &[
    r"C:\Program Files\Sunshine\config",
    r"C:\Program Files\Apollo\config",
];

/// Restart-Service match after a creds write; includes the BroLink service.
pub const SERVICE_MATCH: &str = "Sunshine|Apollo|BroLinkStream";

/// Stop+disable only the upstream engines, never BroLinkStream.
pub const OLD_SERVICE_MATCH: &str = "Sunshine|Apollo";

pub fn uses_old_engine(kind: &str) -> bool {
    matches!(kind, "Sunshine" | "Apollo")
}

fn ps_rel(parts: &[&str]) -> String {
    parts.join(r"\")
}

pub fn stop_old_ps() -> String {
    format!(
        r#"Step "Stopping the old streaming service so only BroLink listens"
Get-Service | Where-Object {{ $_.Name -match '{OLD_SERVICE_MATCH}' }} | ForEach-Object {{
    Stop-Service -Name $_.Name -Force -ErrorAction SilentlyContinue
    Set-Service -Name $_.Name -StartupType Disabled -ErrorAction SilentlyContinue
}}
"#
    )
}

pub fn copy_ps(engine_dir: &str) -> String {
    let required = REQUIRED
        .iter()
        .map(|p| format!("'{}'", ps_rel(p)))
        .collect::<Vec<_>>()
        .join(", ");
    let optional = OPTIONAL
        .iter()
        .map(|p| format!("'{}'", ps_rel(p)))
        .collect::<Vec<_>>()
        .join(", ");
    let sources = OLD_CONFIG
        .iter()
        .map(|d| format!("'{d}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        r#"Step "Copying streaming state into BroLink"
$src = @({sources}) | Where-Object {{ Test-Path (Join-Path $_ 'sunshine_state.json') }} | Select-Object -First 1
if (-not $src) {{ throw "no previous engine state to migrate" }}
$dest = Join-Path '{engine}' 'config'
$bak = Join-Path '{engine}' 'config.bak'
function Copy-EngineState($from, $to) {{
    New-Item -ItemType Directory -Force -Path $to | Out-Null
    New-Item -ItemType Directory -Force -Path (Join-Path $to 'credentials') | Out-Null
    foreach ($f in @({required})) {{
        $p = Join-Path $from $f
        if (-not (Test-Path -LiteralPath $p)) {{ throw "required engine state missing: $f" }}
        Copy-Item -LiteralPath $p -Destination (Join-Path $to $f) -Force
    }}
    foreach ($f in @({optional})) {{
        $p = Join-Path $from $f
        if (Test-Path -LiteralPath $p) {{ Copy-Item -LiteralPath $p -Destination (Join-Path $to $f) -Force }}
    }}
}}
function Assert-EngineState($dir) {{
    foreach ($f in @({required})) {{
        $p = Join-Path $dir $f
        if (-not (Test-Path -LiteralPath $p)) {{ throw "verify failed: $f is missing" }}
        if ((Get-Item -LiteralPath $p).Length -eq 0) {{ throw "verify failed: $f is empty" }}
    }}
}}
Copy-EngineState $src $dest
Copy-EngineState $src $bak
Assert-EngineState $dest
Assert-EngineState $bak
"#,
        engine = engine_dir,
        sources = sources,
        required = required,
        optional = optional,
    )
}

pub fn after_start_ps(dry_run: bool) -> String {
    if dry_run {
        return String::new();
    }
    format!(
        r#"Step "Waiting until the new engine is listening"
$ok = $false
foreach ($i in 1..40) {{
    try {{
        $t = New-Object Net.Sockets.TcpClient
        $t.Connect('127.0.0.1', {port})
        $t.Dispose()
        $ok = $true
        break
    }} catch {{ Start-Sleep -Milliseconds 250 }}
}}
if (-not $ok) {{ throw "the new engine did not start listening" }}
Step "Removing the old streaming installer"
$p = Start-Process msiexec -Wait -PassThru -ArgumentList @('/x','{code}','/qn')
if ($p.ExitCode -ne 0 -and $p.ExitCode -ne 1605) {{ throw "msiexec /x failed: $($p.ExitCode)" }}
Step "Removing what the old installer left behind"
# The uninstaller keeps the config folder as user data. Its contents are in
# config.bak beside the new engine, so once the old engine's exe is gone the
# folder is only a name on disk. An engine that is still installed (a
# different product) keeps its folder.
$oldRoot = Split-Path -Parent $src
if (-not (Test-Path -LiteralPath (Join-Path $oldRoot 'sunshine.exe'))) {{
    Remove-Item -LiteralPath $src -Recurse -Force -ErrorAction SilentlyContinue
    if ((Test-Path -LiteralPath $oldRoot) -and -not (Get-ChildItem -LiteralPath $oldRoot -Force | Select-Object -First 1)) {{
        Remove-Item -LiteralPath $oldRoot -Force -ErrorAction SilentlyContinue
    }}
}}
"#,
        port = brolink_core::SUNSHINE_PORT,
        code = PRODUCT_CODE,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_upstream_engines_need_migration() {
        assert!(uses_old_engine("Sunshine"));
        assert!(uses_old_engine("Apollo"));
        assert!(!uses_old_engine("BroLink"));
        assert!(!uses_old_engine(""));
    }

    #[test]
    fn dry_run_never_calls_msiexec() {
        assert!(after_start_ps(true).is_empty());
        assert!(after_start_ps(false).contains("msiexec"));
        assert!(after_start_ps(false).contains(PRODUCT_CODE));
        assert!(!after_start_ps(false).contains("/i"));
    }

    #[test]
    fn old_folder_goes_only_after_the_msi_and_only_when_its_exe_is_gone() {
        let s = after_start_ps(false);
        let uninstall = s.find("msiexec").expect("uninstall");
        let cleanup = s
            .find("Removing what the old installer left behind")
            .expect("cleanup");
        assert!(uninstall < cleanup, "{s}");
        // Guarded on the old engine's exe: a product that is still installed
        // keeps its folder.
        assert!(
            s.contains("if (-not (Test-Path -LiteralPath (Join-Path $oldRoot 'sunshine.exe')))"),
            "{s}"
        );
        // Only the folder the state was copied from, never the new engine.
        for line in s.lines().filter(|l| l.contains("Remove-Item")) {
            assert!(
                line.contains("-LiteralPath $src") || line.contains("-LiteralPath $oldRoot"),
                "{line}"
            );
        }
        assert!(!s.contains("BroLink\\engine"), "{s}");
        assert!(after_start_ps(true).is_empty());
    }

    #[test]
    fn copy_ps_names_the_live_inventory() {
        let s = copy_ps(r"C:\Program Files\BroLink\engine");
        assert!(s.contains("sunshine_state.json"), "{s}");
        assert!(s.contains(r"credentials\cacert.pem"), "{s}");
        assert!(s.contains(r"credentials\cakey.pem"), "{s}");
        assert!(s.contains("apps.json"), "{s}");
        assert!(s.contains("sunshine.conf"), "{s}");
        assert!(!s.contains("sunshine.log"), "{s}");
        assert!(!s.contains("'cert.pem'"), "{s}");
        assert!(s.contains(r"C:\Program Files\Sunshine\config"), "{s}");
        assert!(s.contains("config.bak"), "{s}");
        assert!(s.contains("required engine state missing"), "{s}");
        assert!(s.contains("verify failed"), "{s}");
    }
}
