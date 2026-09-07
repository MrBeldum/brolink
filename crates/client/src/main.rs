#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod config;
mod input;
mod session;
mod stream;
mod update;
mod video;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();
    let native = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([640.0, 420.0])
            .with_title("BroLink")
            .with_icon(egui::IconData {
                rgba: brolink_core::icon::render(64),
                width: 64,
                height: 64,
            }),
        renderer: eframe::Renderer::Wgpu,
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
