#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod export;
mod input;
mod model;
mod scan;
mod text;
mod tls;
mod worker;

fn main() -> anyhow::Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    if std::env::args().any(|a| a == "--worker") {
        return worker::run_worker();
    }
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1360.0, 760.0])
            .with_position([24.0, 24.0])
            .with_min_inner_size([960.0, 640.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Web Service Fingerprint",
        options,
        Box::new(|cc| Ok(Box::new(app::ScannerApp::new(cc)))),
    )
    .map_err(|e| anyhow::anyhow!(e.to_string()))
}
