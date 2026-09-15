//! BroLink Host. One executable, two jobs:
//!
//! * `brolink-host --background`: the control service a Mac talks to. Runs
//!   at logon with no window (see [`setup::set_start_with_windows`]).
//! * `brolink-host`: the control panel. Starts the service if it is not
//!   running, shows what it knows, and runs the one administrator setup.
//!
//! `--replaces <pid>` is how an update hands over: the new executable waits
//! for the old service to release the port (see [`update`]).
//!
//! `--brand-engine <dir>` is run by the elevated setup script: it gives the
//! streaming engine's executables BroLink's name and icon (see [`brand`]).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod brand;
mod clipboard;
mod config;
mod display;
mod migrate;
mod power;
mod service;
mod setup;
mod streamer;
mod update;
#[cfg_attr(not(windows), allow(dead_code))]
mod verinfo;
mod wake;

use anyhow::Result;
use brolink_core::config::data_dir;
use brolink_core::http;
use brolink_core::CONTROL_PORT;
use clap::Parser;
use std::sync::Arc;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(
    name = "brolink-host",
    version,
    about = "BroLink Host: your PC, from your Mac"
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
        return service::Service::new().run_arc(args.replaces.is_some());
    }
    if let Some(dir) = &args.brand_engine {
        return brand::brand(dir).map_err(|e| {
            tracing::error!("brand engine at {}: {e:#}", dir.display());
            e
        });
    }
    ensure_service_running();
    let native = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([720.0, 860.0])
            .with_min_inner_size([560.0, 600.0])
            .with_title("BroLink Host")
            .with_icon(eframe::egui::IconData {
                rgba: brolink_core::icon::render(64),
                width: 64,
                height: 64,
            }),
        vsync: true,
        ..Default::default()
    };
    eframe::run_native(
        "BroLink Host",
        native,
        Box::new(|cc| Ok(Box::new(app::HostApp::new(cc)))),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}

impl service::Service {
    fn run_arc(self, replacing: bool) -> Result<()> {
        Arc::new(self).run(replacing)
    }
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

/// True when a service answers on loopback.
pub fn service_alive() -> bool {
    http::request(
        ("127.0.0.1", CONTROL_PORT),
        "GET",
        "/v1/status",
        None,
        Duration::from_millis(600),
    )
    .map(|r| r.status == 200)
    .unwrap_or(false)
}

/// Start `--background` if nothing answers on loopback.
pub fn ensure_service_running() {
    if service_alive() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut c = std::process::Command::new(exe);
    c.arg("--background")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0000_0008 | 0x0800_0000); // DETACHED_PROCESS | CREATE_NO_WINDOW
    }
    match c.spawn() {
        Ok(_) => tracing::info!("started the background service"),
        Err(e) => tracing::error!("could not start the background service: {e}"),
    }
}
