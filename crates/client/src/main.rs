#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! The viewer on its own, for development: `cargo run -p brolink-client`.
//! What ships is `brolink-host`, which is this window plus sharing.

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();
    eframe::run_native(
        "BroLink",
        brolink_client::native_options(),
        Box::new(|cc| Ok(Box::new(brolink_client::app::ClientApp::new(cc)))),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}
