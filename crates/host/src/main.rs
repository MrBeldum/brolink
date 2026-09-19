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
}

fn main() -> Result<()> {
    let args = Args::parse();
    init_logging(if args.background {
        "service.log"
    } else {
        "panel.log"
    });
    if args.background {
        return Service::new().run_arc(args.replaces.is_some());
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

/// Logs go to a file in the data directory: neither mode has a console.
fn init_logging(file: &str) {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let sink: Box<dyn std::io::Write + Send> = match data_dir().and_then(|d| {
        Ok(std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(d.join(file))?)
    }) {
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
