#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod hardware;
mod updater;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([960.0, 720.0])
            .with_min_inner_size([640.0, 480.0])
            .with_title("Rust Driver Updater"),
        ..Default::default()
    };

    eframe::run_native(
        "Rust Driver Updater",
        options,
        Box::new(|cc| Ok(Box::new(app::DriverUpdaterApp::new(cc)))),
    )
}
