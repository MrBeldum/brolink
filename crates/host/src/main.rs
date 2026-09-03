mod app;
mod audio;
mod encode;
mod engine;
mod input;

use anyhow::Result;
use clap::Parser;
use forgelink_core::config::HostConfig;
use forgelink_core::identity::Identity;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[derive(Parser, Debug)]
#[command(
    name = "forgelink-host",
    version,
    about = "ForgeLink Windows host — game-quality remote play"
)]
struct Args {
    /// Run without the control panel (prints the ticket and logs to stdout).
    #[arg(long)]
    headless: bool,
    /// Override the advertised PC name.
    #[arg(long)]
    name: Option<String>,
    /// UDP port (default 47850).
    #[arg(long)]
    port: Option<u16>,
    /// Skip the PIN prompt and auto-trust new clients (LAN testing only).
    #[arg(long)]
    no_pin: bool,
    /// Advertise a `forgelink-relay` at this `host:port` for hard-NAT clients.
    #[arg(long)]
    relay: Option<String>,
    /// Do not try to add a Windows Firewall rule on startup.
    #[arg(long)]
    no_firewall: bool,
    /// Do not capture or stream system audio for this run.
    #[arg(long)]
    no_audio: bool,
}

fn main() -> Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();

    let args = Args::parse();
    let mut cfg = HostConfig::load().unwrap_or_default();
    if let Some(n) = args.name {
        cfg.name = n;
    }
    if let Some(p) = args.port {
        cfg.port = p;
    }
    if let Some(r) = args.relay {
        cfg.relay = r;
    }
    cfg.quality = cfg.quality.sanitized();
    if let Err(e) = cfg.save() {
        tracing::warn!("could not save host config: {e:#}");
    }

    // Applied *after* the save, so they last one run. Persisting --no-pin
    // meant a single test left the host auto-trusting every future client,
    // with nothing in the UI to say pairing had been turned off.
    if args.no_pin {
        cfg.auto_trust = true;
        tracing::warn!("--no-pin: any client that can reach this PC will be trusted");
    }
    if args.no_audio {
        cfg.enable_audio = false;
        tracing::info!("--no-audio: system audio will not be captured");
    }

    let identity = Identity::load_or_create(&forgelink_core::config::host_identity_path()?)?;
    tracing::info!("host id {}", identity.short_id());

    raise_priority();
    if !args.no_firewall {
        ensure_firewall_rule(cfg.port);
    }
    // Read-only, so it runs even under --no-firewall: "do not touch my
    // firewall" is not the same as "do not tell me I am unreachable".
    warn_if_unreachable();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("forgelink-host")
        .build()?;
    let engine = Arc::new(engine::Engine::new(cfg.clone(), identity));
    let engine_run = engine.clone();
    rt.spawn(async move {
        if let Err(e) = engine_run.run().await {
            tracing::error!("engine: {e:#}");
            let mut st = engine_run.status.lock();
            st.last_error = Some(format!("{e:#}"));
            st.running = false;
        }
    });

    if args.headless {
        return run_headless(&engine);
    }

    let native = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([720.0, 900.0])
            .with_min_inner_size([520.0, 640.0])
            .with_title("ForgeLink Host"),
        vsync: true,
        ..Default::default()
    };
    let engine_ui = engine.clone();
    let result = eframe::run_native(
        "ForgeLink Host",
        native,
        Box::new(move |cc| Ok(Box::new(app::HostApp::new(cc, engine_ui, cfg)))),
    );
    // Stop the engine and let it tear down ffmpeg and the virtual pad before
    // the runtime goes away underneath them.
    engine.stop();
    rt.shutdown_timeout(std::time::Duration::from_secs(3));
    result.map_err(|e| anyhow::anyhow!("{e}"))
}

/// Print the ticket, then idle until interrupted.
fn run_headless(engine: &Arc<engine::Engine>) -> Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        {
            let st = engine.status.lock();
            if let Some(err) = &st.last_error {
                anyhow::bail!("{err}");
            }
            if !st.ticket_display.is_empty() {
                println!("ForgeLink ticket:\n{}\n", st.ticket_display);
                break;
            }
        }
        if std::time::Instant::now() > deadline {
            anyhow::bail!("host did not come up within 30s");
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    install_interrupt_handler();
    while !INTERRUPTED.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(200));
        if let Some(err) = engine.status.lock().last_error.clone() {
            anyhow::bail!("{err}");
        }
    }
    // Returning here drops the engine, which kills ffmpeg and unplugs the
    // virtual gamepad instead of leaving them behind.
    engine.stop();
    std::thread::sleep(std::time::Duration::from_millis(200));
    println!("ForgeLink host stopped.");
    Ok(())
}

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// Note Ctrl+C so headless mode can shut ffmpeg down cleanly instead of being
/// killed with the encoder still running.
fn install_interrupt_handler() {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::BOOL;
        use windows::Win32::System::Console::{
            SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_C_EVENT,
        };
        unsafe extern "system" fn handler(ctrl_type: u32) -> BOOL {
            if ctrl_type == CTRL_C_EVENT || ctrl_type == CTRL_BREAK_EVENT {
                INTERRUPTED.store(true, Ordering::Relaxed);
                return true.into();
            }
            false.into()
        }
        if let Err(e) = unsafe { SetConsoleCtrlHandler(Some(handler), true) } {
            tracing::debug!("could not install a console control handler: {e}");
        }
    }
}

fn raise_priority() {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::Threading::*;
        // Capture and encode must not be starved by whatever game is running.
        if SetPriorityClass(GetCurrentProcess(), HIGH_PRIORITY_CLASS).is_err() {
            tracing::debug!("could not raise process priority");
        }
    }
}

/// Add an inbound UDP allow rule, but only if one is not already there.
/// Say so when this PC cannot actually be reached.
///
/// Windows evaluates block rules before allow rules, so a leftover "Query
/// User" block -- what Windows writes when its prompt is dismissed -- quietly
/// defeats the allow rule added above. Without this the host prints a ticket,
/// logs a healthy startup, and is simply unreachable, which reads as a bug in
/// the client. Checked on a background thread so it never delays the ticket.
fn warn_if_unreachable() {
    #[cfg(windows)]
    std::thread::spawn(|| {
        let Ok(exe) = std::env::current_exe() else {
            return;
        };
        let exe = exe.to_string_lossy().replace("'", "''");
        let script = format!(
            "$b=@(Get-NetFirewallApplicationFilter -Program '{exe}' -ErrorAction SilentlyContinue| Get-NetFirewallRule -ErrorAction SilentlyContinue|Where-Object{{$_.Action -eq 'Block' -and $_.Enabled -eq 'True' -and $_.Direction -eq 'Inbound'}}).Count; $p=@(Get-NetConnectionProfile -ErrorAction SilentlyContinue| Where-Object{{$_.NetworkCategory -eq 'Public'}}).Count;Write-Output \"$b $p\""
        );
        let Ok(out) = std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .creation_flags(0x0800_0000)
            .output()
        else {
            return;
        };
        let text = String::from_utf8_lossy(&out.stdout);
        let mut fields = text.split_whitespace();
        let blocked: u32 = fields.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        let public: u32 = fields.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        if blocked > 0 {
            tracing::warn!(
                "{blocked} firewall rule(s) BLOCK inbound traffic to {exe}. Windows applies block rules before allow rules, so no client can reach this PC until they are gone. In an elevated PowerShell: Get-NetFirewallApplicationFilter -Program '{exe}' | Get-NetFirewallRule | Where-Object Action -eq Block | Remove-NetFirewallRule"
            );
        }
        if public > 0 {
            tracing::warn!(
                "this PC is on a network Windows classes as Public, where inbound connections and LAN discovery are blocked by default. For a home network: Set-NetConnectionProfile -InterfaceAlias '<adapter>' -NetworkCategory Private"
            );
        }
    });
}

fn ensure_firewall_rule(port: u16) {
    #[cfg(windows)]
    {
        const RULE: &str = "ForgeLink Host";
        // Arguments are passed as separate values: splitting a single string on
        // spaces would cut the quoted rule name in half.
        let exists = std::process::Command::new("netsh")
            .args([
                "advfirewall",
                "firewall",
                "show",
                "rule",
                &format!("name={RULE}"),
            ])
            .creation_flags(0x0800_0000)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if exists {
            tracing::debug!("firewall rule '{RULE}' already present");
            return;
        }
        let status = std::process::Command::new("netsh")
            .args([
                "advfirewall",
                "firewall",
                "add",
                "rule",
                &format!("name={RULE}"),
                "dir=in",
                "action=allow",
                "protocol=UDP",
                &format!("localport={port}"),
            ])
            .creation_flags(0x0800_0000)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        match status {
            Ok(s) if s.success() => tracing::info!("added firewall rule for UDP {port}"),
            _ => tracing::warn!(
                "could not add a firewall rule (needs administrator). If clients cannot reach \
                 this PC, run: netsh advfirewall firewall add rule name=\"{RULE}\" dir=in \
                 action=allow protocol=UDP localport={port}"
            ),
        }
    }
    #[cfg(not(windows))]
    let _ = port;
}
