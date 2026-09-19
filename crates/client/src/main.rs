#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn viewport_icon() -> egui::IconData {
    if cfg!(target_os = "macos") {
        // eframe calls NSApplication.setApplicationIconImage with this
        // bitmap, which replaces BroLink.icns in the Dock with an unmasked
        // square. An empty icon leaves the bundle icon in place.
        egui::IconData::default()
    } else {
        egui::IconData {
            rgba: brolink_core::icon::render(64),
            width: 64,
            height: 64,
        }
    }
}

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
            .with_icon(viewport_icon()),
        renderer: eframe::Renderer::Wgpu,
        // A decoded frame goes to the screen as soon as it is drawn rather
        // than waiting for the display's next refresh: up to a whole
        // refresh interval less between the PC's picture and the eye. The
        // picture only changes when a frame arrives, so there is nothing
        // to tear.
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
        Box::new(|cc| Ok(Box::new(brolink_client::app::ClientApp::new(cc)))),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}
