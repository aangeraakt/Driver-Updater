use crate::hardware::{class_display_name, scan_devices, DeviceDriver, DeviceStatus};
use crate::updater::{
    format_size, install_all_driver_updates, is_elevated, match_updates_to_devices,
    request_elevation, result_code_label, search_driver_updates, DriverUpdate, InstallProgress,
};
use eframe::egui;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

#[derive(PartialEq, Eq)]
enum AppPhase {
    Idle,
    Scanning,
    Checking,
    Ready,
    Installing,
    Done,
}

enum WorkerMsg {
    ScanDone(Result<Vec<DeviceDriver>, String>),
    CheckDone(Result<Vec<DriverUpdate>, String>),
    InstallDone(Result<Vec<InstallProgress>, String>),
}

pub struct DriverUpdaterApp {
    phase: AppPhase,
    devices: Vec<DeviceDriver>,
    updates: Vec<DriverUpdate>,
    filter: String,
    show_only_updates: bool,
    status_message: String,
    error_message: Option<String>,
    install_log: Vec<String>,
    confirm_install: bool,
    tx: Sender<WorkerMsg>,
    rx: Receiver<WorkerMsg>,
    elevated: bool,
}

impl DriverUpdaterApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            phase: AppPhase::Idle,
            devices: Vec::new(),
            updates: Vec::new(),
            filter: String::new(),
            show_only_updates: false,
            status_message: "Klik op 'Scannen' om je PC te analyseren.".into(),
            error_message: None,
            install_log: Vec::new(),
            confirm_install: false,
            tx,
            rx,
            elevated: is_elevated(),
        }
    }

    fn poll_worker(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                WorkerMsg::ScanDone(result) => match result {
                    Ok(devices) => {
                        self.devices = devices;
                        self.start_check();
                    }
                    Err(e) => {
                        self.phase = AppPhase::Idle;
                        self.error_message = Some(e);
                        self.status_message = "Scan mislukt.".into();
                    }
                },
                WorkerMsg::CheckDone(result) => match result {
                    Ok(updates) => {
                        self.updates = updates.clone();
                        match_updates_to_devices(&mut self.devices, &updates);
                        self.phase = AppPhase::Ready;
                        self.status_message = if updates.is_empty() {
                            format!(
                                "{} onderdelen gescand — alle drivers zijn up-to-date.",
                                self.devices.len()
                            )
                        } else {
                            format!(
                                "{} onderdelen gescand — {} driver update(s) beschikbaar.",
                                self.devices.len(),
                                updates.len()
                            )
                        };
                    }
                    Err(e) => {
                        self.phase = AppPhase::Ready;
                        self.error_message = Some(e);
                        self.status_message = format!(
                            "{} onderdelen gescand — updatecontrole mislukt.",
                            self.devices.len()
                        );
                    }
                },
                WorkerMsg::InstallDone(result) => match result {
                    Ok(results) => {
                        self.install_log.clear();
                        for r in &results {
                            self.install_log.push(format!(
                                "[{}/{}] {} — {}",
                                r.current,
                                r.total,
                                r.title,
                                result_code_label(r.result_code)
                            ));
                        }
                        for device in &mut self.devices {
                            if device.status == DeviceStatus::UpdateAvailable {
                                device.status = DeviceStatus::Installed;
                            }
                        }
                        self.phase = AppPhase::Done;
                        self.status_message = if results.is_empty() {
                            "Geen updates geïnstalleerd.".into()
                        } else {
                            format!("{} driver(s) geïnstalleerd.", results.len())
                        };
                    }
                    Err(e) => {
                        self.phase = AppPhase::Ready;
                        self.error_message = Some(e);
                        self.status_message = "Installatie mislukt.".into();
                    }
                },
            }
        }
    }

    fn start_scan(&mut self) {
        self.phase = AppPhase::Scanning;
        self.error_message = None;
        self.status_message = "Hardware scannen...".into();
        self.install_log.clear();

        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = scan_devices().map_err(|e| e.to_string());
            let _ = tx.send(WorkerMsg::ScanDone(result));
        });
    }

    fn start_check(&mut self) {
        self.phase = AppPhase::Checking;
        self.status_message = "Drivers controleren via Windows Update...".into();

        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = search_driver_updates().map_err(|e| e.to_string());
            let _ = tx.send(WorkerMsg::CheckDone(result));
        });
    }

    fn start_install(&mut self) {
        self.phase = AppPhase::Installing;
        self.status_message = "Drivers installeren...".into();
        self.confirm_install = false;

        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = install_all_driver_updates().map_err(|e| e.to_string());
            let _ = tx.send(WorkerMsg::InstallDone(result));
        });
    }

    fn filtered_devices(&self) -> Vec<&DeviceDriver> {
        self.devices
            .iter()
            .filter(|d| {
                if self.show_only_updates && d.status != DeviceStatus::UpdateAvailable {
                    return false;
                }
                if self.filter.is_empty() {
                    return true;
                }
                let f = self.filter.to_lowercase();
                d.name.to_lowercase().contains(&f)
                    || d.manufacturer.to_lowercase().contains(&f)
                    || d.device_class.to_lowercase().contains(&f)
            })
            .collect()
    }

    fn updates_count(&self) -> usize {
        self.updates.len()
    }

    fn is_busy(&self) -> bool {
        matches!(
            self.phase,
            AppPhase::Scanning | AppPhase::Checking | AppPhase::Installing
        )
    }

    fn render_header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Rust Driver Updater");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !self.elevated {
                    ui.colored_label(
                        egui::Color32::from_rgb(255, 180, 60),
                        "Niet als administrator — sommige updates vereisen admin rechten",
                    );
                }
            });
        });
        ui.add_space(4.0);
        ui.label(self.status_message.clone());
        if let Some(err) = &self.error_message {
            ui.colored_label(egui::Color32::from_rgb(255, 100, 100), err);
        }
    }

    fn render_toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!self.is_busy(), egui::Button::new("Scannen"))
                .clicked()
            {
                self.start_scan();
            }

            let updates = self.updates_count();
            if ui
                .add_enabled(
                    !self.is_busy() && updates > 0,
                    egui::Button::new(format!("Alle drivers updaten ({updates})"))
                        .fill(egui::Color32::from_rgb(40, 120, 200)),
                )
                .clicked()
            {
                self.confirm_install = true;
            }

            ui.checkbox(&mut self.show_only_updates, "Alleen updates tonen");
            ui.label(format!("{} onderdelen", self.devices.len()));
        });

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("Zoeken:");
            ui.text_edit_singleline(&mut self.filter);
        });
    }

    fn render_device_list(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            let devices = self.filtered_devices();
            if devices.is_empty() {
                ui.label("Geen onderdelen gevonden.");
                return;
            }

            let mut last_class = String::new();
            for device in devices {
                if device.device_class != last_class {
                    last_class = device.device_class.clone();
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(class_display_name(&device.device_class))
                            .strong()
                            .size(14.0),
                    );
                    ui.separator();
                }

                ui.horizontal(|ui| {
                    let (color, label) = status_badge(&device.status);
                    ui.colored_label(color, label);

                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new(&device.name).strong());
                        ui.horizontal(|ui| {
                            if !device.manufacturer.is_empty() {
                                ui.label(format!("Fabrikant: {}", device.manufacturer));
                            }
                            if !device.driver_version.is_empty() {
                                ui.label(format!("Driver: v{}", device.driver_version));
                            }
                            if let Some(date) = device.driver_date {
                                ui.label(format!("Datum: {}", date.format("%d-%m-%Y")));
                            }
                            if !device.inf_name.is_empty() {
                                ui.label(format!("INF: {}", device.inf_name));
                            }
                        });
                        if let Some(title) = &device.update_title {
                            ui.colored_label(
                                egui::Color32::from_rgb(80, 180, 255),
                                format!("Update: {title}"),
                            );
                        }
                    });
                });
                ui.add_space(4.0);
            }
        });
    }

    fn render_updates_panel(&mut self, ui: &mut egui::Ui) {
        if self.updates.is_empty() {
            return;
        }
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new("Beschikbare updates via Windows Update")
                .strong(),
        );
        egui::ScrollArea::vertical()
            .max_height(120.0)
            .show(ui, |ui| {
                for update in &self.updates {
                    ui.horizontal(|ui| {
                        ui.label(&update.title);
                        ui.label(format_size(update.size_bytes));
                    });
                }
            });
    }

    fn render_install_log(&mut self, ui: &mut egui::Ui) {
        if self.install_log.is_empty() {
            return;
        }
        ui.add_space(8.0);
        ui.label(egui::RichText::new("Installatie log").strong());
        for line in &self.install_log {
            ui.label(line);
        }
    }

    fn render_confirm_dialog(&mut self, ctx: &egui::Context) {
        if !self.confirm_install {
            return;
        }

        egui::Window::new("Bevestig installatie")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(format!(
                    "Wil je {} driver update(s) installeren via Windows Update?",
                    self.updates_count()
                ));
                ui.label(
                    "Dit kan enkele minuten duren. Een herstart kan nodig zijn na installatie.",
                );
                ui.add_space(8.0);

                ui.horizontal(|ui| {
                    if ui.button("Ja, installeer alle driver updates").clicked() {
                        self.confirm_install = false;
                        if !self.elevated {
                            let _ = request_elevation();
                            self.elevated = is_elevated();
                        }
                        self.start_install();
                    }
                    if ui.button("Annuleren").clicked() {
                        self.confirm_install = false;
                    }
                });
            });
    }
}

fn status_badge(status: &DeviceStatus) -> (egui::Color32, &str) {
    match status {
        DeviceStatus::Unknown => (egui::Color32::GRAY, "Onbekend"),
        DeviceStatus::UpToDate => (egui::Color32::from_rgb(80, 200, 120), "Up-to-date"),
        DeviceStatus::UpdateAvailable => {
            (egui::Color32::from_rgb(80, 180, 255), "Update beschikbaar")
        }
        DeviceStatus::Installed => (egui::Color32::from_rgb(80, 200, 120), "Geïnstalleerd"),
    }
}

impl eframe::App for DriverUpdaterApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_worker();

        egui::CentralPanel::default().show(ctx, |ui| {
            self.render_header(ui);
            ui.separator();
            self.render_toolbar(ui);
            ui.separator();
            self.render_device_list(ui);
            self.render_updates_panel(ui);
            self.render_install_log(ui);
        });

        self.render_confirm_dialog(ctx);

        if self.is_busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
    }
}
