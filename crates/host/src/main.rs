//! BroLink. One executable, two jobs:
//!
//! * `--background`: the control service other machines talk to.
//! * no flags: the window — a list of machines to connect to, and setup
//!   to share this one.
//!
//! `--replaces <pid>` is how an update hands over. `--brand-engine` is
//! Windows setup rewriting the streaming engine's name and icon.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use anyhow::Result;
use brolink_core::config::data_dir;
use brolink_host::brand;
use brolink_host::logfile::RotatingLog;
use brolink_host::product::NodeApp;
use brolink_host::service::{self, Service};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "brolink-host",
    version,
    about = "BroLink: your machines, on any of your machines"
)]
struct Args {
    /// Run the control service with no window.
    #[arg(long)]
    background: bool,
    /// Started by an update: wait for this process to give up the port.
    #[arg(long, requires = "background")]
    replaces: Option<u32>,
    /// Give the streaming engine in DIR BroLink's name and icon. Run by
    /// setup, elevated, with the engine stopped; exits non-zero with the
    /// reason on stderr.
    #[arg(long, value_name = "DIR", conflicts_with = "background")]
    brand_engine: Option<std::path::PathBuf>,
    /// Install and start the streaming engine, then exit. Used by Mac/Linux
    /// setup and by the Docker node; on Windows this is the same path as
    /// Share this machine (UAC).
    #[arg(long, conflicts_with_all = ["background", "brand_engine"])]
    setup: bool,
    /// Run setup as administrator. The unelevated panel launches this so
    /// the script is generated after UAC, not from a user-writable file.
    #[arg(long, conflicts_with_all = ["background", "brand_engine"])]
    setup_elevated: bool,
    /// The launching user's %LOCALAPPDATA%, so an elevated setup approved
    /// with another administrator's password still uses that user's
    /// BroLink folder (host.toml, setup.log).
    #[arg(long, value_name = "DIR", requires = "setup_elevated")]
    local_app_data: Option<std::path::PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    if let Some(dir) = &args.local_app_data {
        // Before any thread or data_dir() call; edition 2021, so set_var is safe.
        std::env::set_var("LOCALAPPDATA", dir);
    }
    // The elevated setup helper logs with the panel; `setup.log` is the
    // script's own output, which the setup card shows.
    init_logging(if args.background {
        "service.log"
    } else {
        "panel.log"
    });
    if args.background {
        return Service::new().run_arc(args.replaces.is_some());
    }
    if args.setup {
        return run_setup();
    }
    if args.setup_elevated {
        return brolink_host::setup::run_as_admin();
    }
    if let Some(dir) = &args.brand_engine {
        return brand::brand(dir).map_err(|e| {
            tracing::error!("brand engine at {}: {e:#}", dir.display());
            e
        });
    }
    service::ensure_service_running();
    let icon = if cfg!(target_os = "macos") {
        eframe::egui::IconData::default()
    } else {
        eframe::egui::IconData {
            rgba: brolink_core::icon::render(64),
            width: 64,
            height: 64,
        }
    };
    let native = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([640.0, 420.0])
            .with_title("BroLink")
            .with_icon(icon),
        renderer: eframe::Renderer::Wgpu,
        vsync: false,
        wgpu_options: egui_wgpu::WgpuConfiguration {
            present_mode: egui_wgpu::wgpu::PresentMode::AutoNoVsync,
            desired_maximum_frame_latency: Some(1),
            ..Default::default()
        },
        ..Default::default()
    };
    eframe::run_native(
        "BroLink",
        native,
        Box::new(|cc| Ok(Box::new(NodeApp::new(cc)))),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}

fn run_setup() -> Result<()> {
    let mut cfg = brolink_host::config::HostConfig::load();
    if !cfg.has_creds() {
        cfg.sunshine_user = "brolink".into();
        cfg.sunshine_pass = brolink_host::config::random_password();
        cfg.save()?;
    }
    let exe = std::env::current_exe()?;
    brolink_host::setup::run(&brolink_host::setup::Plan {
        exe: &exe,
        install_engine: true,
        migrate: false,
        dry_run: false,
        sunshine_user: &cfg.sunshine_user,
        sunshine_pass: &cfg.sunshine_pass,
        adapter: "",
        adapter_description: "",
    })
}

/// What is logged when RUST_LOG says nothing.
///
/// wgpu's Vulkan backend warns once for every presented frame whose
/// swapchain reports suboptimal (`wgpu-hal`, `vulkan/mod.rs`). On Hermes
/// that is every frame the window draws, and it was **every line** of a
/// 32 MB `panel.log` — the condition is normal and wgpu recreates the
/// swapchain itself, so there is nothing to act on. BroLink stays at info
/// and that one target is heard from only when it is an error.
const DEFAULT_LOG: &str = "info,wgpu_hal=error";

/// Logs go to a file in the data directory: neither mode has a console.
fn init_logging(file: &str) {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG));
    let sink: Box<dyn std::io::Write + Send> =
        match data_dir().and_then(|d| Ok(RotatingLog::open(d.join(file))?)) {
            Ok(f) => Box::new(f),
            Err(_) => Box::new(std::io::stderr()),
        };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_ansi(false)
        .with_writer(std::sync::Mutex::new(sink))
        .init();
}

#[cfg(test)]
mod tests {
    use super::DEFAULT_LOG;
    use tracing_subscriber::EnvFilter;

    /// A typo here would be silent: `EnvFilter` drops directives it cannot
    /// parse and carries on, so a broken default would quietly restore the
    /// per-frame flood rather than fail the build.
    #[test]
    fn the_default_filter_parses_and_keeps_both_directives() {
        let parsed = EnvFilter::builder()
            .parse(DEFAULT_LOG)
            .expect("DEFAULT_LOG is a valid filter")
            .to_string();
        assert!(parsed.contains("wgpu_hal=error"), "{parsed}");
        assert!(parsed.contains("info"), "{parsed}");
    }
}
