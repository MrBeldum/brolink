//! BroLink for the Mac: the window with your PCs in it.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod config;
mod moonlight;
mod session;

use anyhow::Result;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "brolink-client",
    version,
    about = "BroLink: your Windows PC, on this Mac"
)]
struct Args {}

fn main() -> Result<()> {
    let _ = Args::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();
    let native = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([560.0, 720.0])
            .with_min_inner_size([460.0, 520.0])
            .with_title("BroLink")
            .with_icon(eframe::egui::IconData {
                rgba: brolink_core::icon::render(64),
                width: 64,
                height: 64,
            }),
        vsync: true,
        ..Default::default()
    };
    eframe::run_native(
        "BroLink",
        native,
        Box::new(|cc| Ok(Box::new(app::ClientApp::new(cc)))),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}
