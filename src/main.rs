#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod hardware;
mod updater;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 760.0])
            .with_min_inner_size([900.0, 600.0])
            .with_title("Rust Driver Updater"),
        ..Default::default()
    };

    eframe::run_native(
        "Rust Driver Updater",
        options,
        Box::new(|cc| Ok(Box::new(app::DriverUpdaterApp::new(cc)))),
    )
}
