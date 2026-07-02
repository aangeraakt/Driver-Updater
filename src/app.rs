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
    installable_ids: HashSet<String>,
    installable_count: usize,
    filter: String,
    filter_lower: String,
    filter_snapshot: String,
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
            installable_ids: HashSet::new(),
            installable_count: 0,
            filter: String::new(),
            filter_lower: String::new(),
            filter_snapshot: String::new(),
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

    fn sync_filter_cache(&mut self) {
        if self.filter != self.filter_snapshot {
            self.filter_snapshot.clone_from(&self.filter);
            self.filter_lower = self.filter.to_lowercase();
        }
    }

    fn device_visible(&self, device: &DeviceDriver) -> bool {
        if self.show_only_updates && device.status != DeviceStatus::UpdateAvailable {
            return false;
        }
        if self.filter_lower.is_empty() {
            return true;
        }
        device.name_lower.contains(&self.filter_lower)
            || device.manufacturer_lower.contains(&self.filter_lower)
            || device.class_lower.contains(&self.filter_lower)
    }

    fn poll_worker(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                WorkerMsg::ScanDone(result) => match result {
                    Ok(devices) => {
                        self.devices = devices;
                        self.installable_ids.clear();
                        self.installable_count = 0;
                        self.start_check(ctx);
                    }
                    Err(e) => {
                        self.phase = AppPhase::Idle;
                        self.error_message = Some(e);
                        self.status_message = "Scan mislukt.".into();
                    }
                },
                WorkerMsg::CheckDone(result) => match result {
                    Ok(updates) => {
                        self.installable_ids = match_updates_to_devices(&mut self.devices, &updates);
                        self.installable_count = self.installable_ids.len();
                        self.updates = updates;
                        self.phase = AppPhase::Ready;
                        self.status_message = if self.installable_count == 0 {
                            format!(
                                "{} onderdelen gescand — alle drivers zijn up-to-date.",
                                self.devices.len()
                            )
                        } else {
                            format!(
                                "{} onderdelen gescand — {} driver update(s) beschikbaar.",
                                self.devices.len(),
                                self.installable_count
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
                        let mut successful_titles = HashSet::with_capacity(summary.results.len());
                        let mut success_count = 0;

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
                                success_count += 1;
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
            ctx.request_repaint();
        }
    }

    fn start_scan(&mut self, ctx: &egui::Context) {
        self.phase = AppPhase::Scanning;
        self.error_message = None;
        self.status_message = "Hardware scannen...".into();
        self.install_log.clear();
        self.show_restart_prompt = false;
        self.updates.clear();
        self.installable_ids.clear();
        self.installable_count = 0;

        let tx = self.tx.clone();
        let ctx = ctx.clone();
        thread::spawn(move || {
            let result = scan_devices().map_err(|e| e.to_string());
            let _ = tx.send(WorkerMsg::ScanDone(result));
            ctx.request_repaint();
        });
    }

    fn start_check(&mut self, ctx: &egui::Context) {
        self.phase = AppPhase::Checking;
        self.status_message = "Drivers controleren via Windows Update...".into();

        let tx = self.tx.clone();
        let ctx = ctx.clone();
        thread::spawn(move || {
            let result = search_driver_updates().map_err(|e| e.to_string());
            let _ = tx.send(WorkerMsg::CheckDone(result));
            ctx.request_repaint();
        });
    }

    fn start_install(&mut self, ctx: &egui::Context) {
        self.phase = AppPhase::Installing;
        self.status_message = "Drivers installeren...".into();
        self.confirm_install = false;
        self.show_restart_prompt = false;

        let update_ids: Vec<String> = self.installable_ids.iter().cloned().collect();
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        thread::spawn(move || {
            let result = install_driver_updates(&update_ids).map_err(|e| e.to_string());
            let _ = tx.send(WorkerMsg::InstallDone(result));
            ctx.request_repaint();
        });
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
        ui.label(&self.status_message);
        if let Some(err) = &self.error_message {
            ui.colored_label(egui::Color32::from_rgb(255, 100, 100), err);
        }
    }

    fn render_toolbar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!self.is_busy(), egui::Button::new("Scannen"))
                .clicked()
            {
                self.start_scan(ctx);
            }

            if ui
                .add_enabled(
                    !self.is_busy() && self.installable_count > 0,
                    egui::Button::new(format!(
                        "Alle drivers updaten ({})",
                        self.installable_count
                    ))
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
        egui::ScrollArea::vertical()
            .id_salt("device_list")
            .show(ui, |ui| {
                let mut visible = false;
                let mut last_class: Option<&str> = None;

                for device in &self.devices {
                    if !self.device_visible(device) {
                        continue;
                    }
                    visible = true;

                    if last_class != Some(device.device_class.as_str()) {
                        last_class = Some(device.device_class.as_str());
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

                if !visible {
                    ui.label("Geen onderdelen gevonden.");
                }
            });
    }

    fn render_updates_panel(&mut self, ui: &mut egui::Ui) {
        if self.installable_count == 0 {
            return;
        }

        ui.add_space(8.0);
        ui.label(
            egui::RichText::new("Beschikbare updates voor jouw hardware")
                .strong(),
        );
        egui::ScrollArea::vertical()
            .id_salt("updates_panel")
            .max_height(120.0)
            .show(ui, |ui| {
                for update in &self.updates {
                    if !self.installable_ids.contains(&update.update_id) {
                        continue;
                    }
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
                    self.installable_count
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
                        self.start_install(ctx);
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
        self.sync_filter_cache();
        self.poll_worker(ctx);

        egui::CentralPanel::default().show(ctx, |ui| {
            self.render_header(ui);
            ui.separator();
            self.render_toolbar(ui, ctx);
            ui.separator();
            self.render_device_list(ui);
            self.render_updates_panel(ui);
            self.render_install_log(ui);
        });

        self.render_confirm_dialog(ctx);
        self.render_restart_dialog(ctx);
    }
}
