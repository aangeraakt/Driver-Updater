use crate::hardware::{class_display_name, scan_devices, DeviceDriver, DeviceStatus};
use crate::updater::{
    format_size, install_driver_updates, is_elevated, is_install_success, match_updates_to_devices,
    request_elevation, restart_computer, result_code_label, search_driver_updates, DriverUpdate,
    InstallSummary,
};
use eframe::egui;
use std::collections::HashSet;
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
    InstallDone(Result<InstallSummary, String>),
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
    show_restart_prompt: bool,
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
            show_restart_prompt: false,
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
                        let installable = self.installable_updates_count();
                        self.status_message = if installable == 0 {
                            format!(
                                "{} onderdelen gescand — alle drivers zijn up-to-date.",
                                self.devices.len()
                            )
                        } else {
                            format!(
                                "{} onderdelen gescand — {} driver update(s) beschikbaar.",
                                self.devices.len(),
                                installable
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
                    Ok(summary) => {
                        self.install_log.clear();
                        let mut successful_titles = HashSet::new();
                        for r in &summary.results {
                            self.install_log.push(format!(
                                "[{}/{}] {} — {}",
                                r.current,
                                r.total,
                                r.title,
                                result_code_label(r.result_code)
                            ));
                            if is_install_success(r.result_code) {
                                successful_titles.insert(r.title.clone());
                            }
                        }

                        for device in &mut self.devices {
                            if device.status != DeviceStatus::UpdateAvailable {
                                continue;
                            }
                            let Some(title) = &device.update_title else {
                                continue;
                            };
                            if successful_titles.contains(title) {
                                device.status = DeviceStatus::Installed;
                            }
                        }

                        let success_count = summary
                            .results
                            .iter()
                            .filter(|r| is_install_success(r.result_code))
                            .count();

                        self.phase = AppPhase::Done;
                        self.status_message = if success_count == 0 {
                            "Geen updates geïnstalleerd.".into()
                        } else {
                            format!("{success_count} driver(s) geïnstalleerd.")
                        };

                        let needs_restart = summary.reboot_required
                            || summary
                                .results
                                .iter()
                                .any(|r| matches!(r.result_code, 3 | 4))
                            || success_count > 0;
                        self.show_restart_prompt = needs_restart;
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
        self.show_restart_prompt = false;

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
        self.show_restart_prompt = false;

        let update_ids = self.installable_update_ids();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = install_driver_updates(&update_ids).map_err(|e| e.to_string());
            let _ = tx.send(WorkerMsg::InstallDone(result));
        });
    }

    fn installable_update_ids(&self) -> Vec<String> {
        let mut ids = Vec::new();
        let mut seen = HashSet::new();
        for device in &self.devices {
            if device.status != DeviceStatus::UpdateAvailable {
                continue;
            }
            if let Some(id) = &device.update_id {
                if seen.insert(id.clone()) {
                    ids.push(id.clone());
                }
            }
        }
        ids
    }

    fn installable_updates(&self) -> Vec<&DriverUpdate> {
        let ids = self.installable_update_ids();
        self.updates
            .iter()
            .filter(|u| ids.contains(&u.update_id))
            .collect()
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

    fn installable_updates_count(&self) -> usize {
        self.installable_update_ids().len()
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

            let updates = self.installable_updates_count();
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
        let installable = self.installable_updates();
        if installable.is_empty() {
            return;
        }
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new("Beschikbare updates voor jouw hardware")
                .strong(),
        );
        egui::ScrollArea::vertical()
            .max_height(120.0)
            .show(ui, |ui| {
                for update in installable {
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
                    self.installable_updates_count()
                ));
                ui.label(
                    "Dit kan enkele minuten duren. Na installatie moet je je PC herstarten om de drivers te activeren.",
                );
                ui.add_space(8.0);

                ui.horizontal(|ui| {
                    if ui.button("Ja, installeer driver updates").clicked() {
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

    fn render_restart_dialog(&mut self, ctx: &egui::Context) {
        if !self.show_restart_prompt {
            return;
        }

        egui::Window::new("Herstart vereist")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(
                    "De driver(s) zijn gedownload en geïnstalleerd, maar worden pas actief na een herstart van je PC.",
                );
                ui.label(
                    "Start je computer opnieuw op om de nieuwe drivers te gebruiken.",
                );
                ui.add_space(8.0);

                ui.horizontal(|ui| {
                    if ui
                        .button(
                            egui::Button::new("Herstart nu")
                                .fill(egui::Color32::from_rgb(40, 120, 200)),
                        )
                        .clicked()
                    {
                        self.show_restart_prompt = false;
                        if !self.elevated {
                            let _ = request_elevation();
                            self.elevated = is_elevated();
                        }
                        if let Err(e) = restart_computer() {
                            self.error_message = Some(e.to_string());
                        }
                    }
                    if ui.button("Later").clicked() {
                        self.show_restart_prompt = false;
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
        self.render_restart_dialog(ctx);

        if self.is_busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
    }
}
