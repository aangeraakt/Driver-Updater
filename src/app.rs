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

struct Theme;

impl Theme {
    const BG: egui::Color32 = egui::Color32::from_rgb(22, 24, 30);
    const PANEL: egui::Color32 = egui::Color32::from_rgb(30, 33, 42);
    const CARD: egui::Color32 = egui::Color32::from_rgb(38, 42, 54);
    const CARD_HOVER: egui::Color32 = egui::Color32::from_rgb(44, 49, 64);
    const ACCENT: egui::Color32 = egui::Color32::from_rgb(56, 132, 255);
    const ACCENT_DIM: egui::Color32 = egui::Color32::from_rgb(36, 88, 175);
    const SUCCESS: egui::Color32 = egui::Color32::from_rgb(52, 199, 123);
    const WARNING: egui::Color32 = egui::Color32::from_rgb(255, 184, 77);
    const ERROR: egui::Color32 = egui::Color32::from_rgb(255, 95, 95);
    const INFO: egui::Color32 = egui::Color32::from_rgb(92, 188, 255);
    const MUTED: egui::Color32 = egui::Color32::from_rgb(150, 158, 178);
    const TEXT: egui::Color32 = egui::Color32::from_rgb(230, 234, 242);
    const BORDER: egui::Color32 = egui::Color32::from_rgb(55, 60, 74);
}

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

struct DeviceStats {
    total: usize,
    up_to_date: usize,
    updates: usize,
    installed: usize,
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
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_theme(&cc.egui_ctx);
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

    fn device_stats(&self) -> DeviceStats {
        let mut stats = DeviceStats {
            total: self.devices.len(),
            up_to_date: 0,
            updates: 0,
            installed: 0,
        };
        for device in &self.devices {
            match device.status {
                DeviceStatus::UpToDate | DeviceStatus::Unknown => stats.up_to_date += 1,
                DeviceStatus::UpdateAvailable => stats.updates += 1,
                DeviceStatus::Installed => stats.installed += 1,
            }
        }
        stats
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

    fn phase_label(&self) -> (&str, egui::Color32) {
        match self.phase {
            AppPhase::Idle => ("Klaar om te scannen", Theme::MUTED),
            AppPhase::Scanning => ("Hardware scannen...", Theme::INFO),
            AppPhase::Checking => ("Updates zoeken...", Theme::INFO),
            AppPhase::Ready => ("Scan voltooid", Theme::SUCCESS),
            AppPhase::Installing => ("Drivers installeren...", Theme::WARNING),
            AppPhase::Done => ("Installatie voltooid", Theme::SUCCESS),
        }
    }

    fn render_header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(
                    egui::RichText::new("Rust Driver Updater")
                        .size(24.0)
                        .strong()
                        .color(Theme::TEXT),
                );
                ui.label(
                    egui::RichText::new("Scan hardware, controleer drivers en installeer updates")
                        .size(13.0)
                        .color(Theme::MUTED),
                );
            });

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.elevated {
                    badge(ui, "Administrator", Theme::SUCCESS);
                } else {
                    badge(ui, "Standaard gebruiker", Theme::WARNING);
                }
            });
        });
    }

    fn render_status_banner(&mut self, ui: &mut egui::Ui) {
        let (phase_label, phase_color) = self.phase_label();
        card_frame(Theme::CARD).show(ui, |ui| {
            ui.set_min_height(52.0);
            ui.horizontal(|ui| {
                if self.is_busy() {
                    ui.add(egui::Spinner::new().size(18.0));
                } else {
                    ui.label(egui::RichText::new("●").size(18.0).color(phase_color));
                }

                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new(phase_label)
                            .strong()
                            .color(phase_color),
                    );
                    ui.label(
                        egui::RichText::new(&self.status_message)
                            .size(13.0)
                            .color(Theme::TEXT),
                    );
                });
            });

            if let Some(err) = &self.error_message {
                ui.add_space(6.0);
                ui.colored_label(Theme::ERROR, err);
            }
        });
    }

    fn render_stats(&self, ui: &mut egui::Ui) {
        let stats = self.device_stats();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            stat_card(ui, "Apparaten", stats.total, Theme::TEXT);
            stat_card(ui, "Up-to-date", stats.up_to_date, Theme::SUCCESS);
            stat_card(ui, "Updates", stats.updates, Theme::INFO);
            stat_card(ui, "Geïnstalleerd", stats.installed, Theme::ACCENT);
        });
    }

    fn render_toolbar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        card_frame(Theme::PANEL).show(ui, |ui| {
            ui.horizontal(|ui| {
                let scan = egui::Button::new(
                    egui::RichText::new("  Scannen  ").size(14.0).strong(),
                )
                .fill(if self.is_busy() {
                    Theme::CARD
                } else {
                    Theme::ACCENT
                })
                .stroke(egui::Stroke::NONE);

                if ui.add_enabled(!self.is_busy(), scan).clicked() {
                    self.start_scan(ctx);
                }

                let update_label = if self.installable_count > 0 {
                    format!("  Drivers updaten ({})  ", self.installable_count)
                } else {
                    "  Drivers updaten  ".into()
                };
                let update_btn = egui::Button::new(egui::RichText::new(update_label).size(14.0))
                    .fill(Theme::ACCENT_DIM)
                    .stroke(egui::Stroke::NONE);

                if ui
                    .add_enabled(!self.is_busy() && self.installable_count > 0, update_btn)
                    .clicked()
                {
                    self.confirm_install = true;
                }

                ui.separator();

                ui.checkbox(&mut self.show_only_updates, "Alleen updates");
            });

            ui.add_space(8.0);

            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Zoeken").color(Theme::MUTED));
                ui.add(
                    egui::TextEdit::singleline(&mut self.filter)
                        .hint_text("Zoek op naam, fabrikant of categorie...")
                        .desired_width(ui.available_width()),
                );
            });
        });
    }

    fn render_main_content(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if self.devices.is_empty() && !self.is_busy() {
            self.render_empty_state(ui, ctx);
            return;
        }

        let sidebar_width = 300.0;
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_min_width(ui.available_width() - sidebar_width - 12.0);
                ui.set_max_width(ui.available_width() - sidebar_width - 12.0);

                ui.label(
                    egui::RichText::new("Apparaten")
                        .size(16.0)
                        .strong()
                        .color(Theme::TEXT),
                );
                ui.add_space(6.0);
                self.render_device_list(ui);
            });

            ui.add_space(12.0);

            ui.vertical(|ui| {
                ui.set_min_width(sidebar_width);
                ui.set_max_width(sidebar_width);
                self.render_sidebar(ui);
            });
        });
    }

    fn render_empty_state(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.22);

            card_frame(Theme::CARD).show(ui, |ui| {
                ui.set_width(420.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        egui::RichText::new("Welkom")
                            .size(22.0)
                            .strong()
                            .color(Theme::TEXT),
                    );
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(
                            "Start een scan om alle hardware en driver updates op je PC te vinden.",
                        )
                        .color(Theme::MUTED),
                    );
                    ui.add_space(16.0);

                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new("  Start scan  ")
                                    .size(15.0)
                                    .strong()
                                    .color(egui::Color32::WHITE),
                            )
                            .fill(Theme::ACCENT),
                        )
                        .clicked()
                    {
                        self.start_scan(ctx);
                    }
                });
            });

            ui.add_space(16.0);
            ui.label(
                egui::RichText::new("Windows Update wordt gebruikt om driver updates te vinden")
                    .size(12.0)
                    .color(Theme::MUTED),
            );
        });
    }

    fn render_device_list(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .id_salt("device_list")
            .auto_shrink([false, false])
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
                        ui.add_space(10.0);
                        ui.label(
                            egui::RichText::new(class_display_name(&device.device_class))
                                .size(13.0)
                                .strong()
                                .color(Theme::MUTED),
                        );
                        ui.add_space(4.0);
                    }

                    render_device_card(ui, device);
                    ui.add_space(6.0);
                }

                if !visible && !self.devices.is_empty() {
                    ui.vertical_centered(|ui| {
                        ui.add_space(40.0);
                        ui.label(
                            egui::RichText::new("Geen apparaten gevonden voor dit filter")
                                .color(Theme::MUTED),
                        );
                    });
                }
            });
    }

    fn render_sidebar(&mut self, ui: &mut egui::Ui) {
        section_frame(ui, "Updates", |ui| {
            if self.installable_count == 0 {
                ui.label(
                    egui::RichText::new("Geen updates beschikbaar")
                        .color(Theme::MUTED)
                        .italics(),
                );
                return;
            }

            egui::ScrollArea::vertical()
                .id_salt("updates_panel")
                .max_height(220.0)
                .show(ui, |ui| {
                    for update in &self.updates {
                        if !self.installable_ids.contains(&update.update_id) {
                            continue;
                        }
                        card_frame(Theme::BG).show(ui, |ui| {
                            ui.label(
                                egui::RichText::new(&update.title)
                                    .size(12.0)
                                    .color(Theme::TEXT),
                            );
                            ui.label(
                                egui::RichText::new(format_size(update.size_bytes))
                                    .size(11.0)
                                    .color(Theme::MUTED),
                            );
                        });
                        ui.add_space(4.0);
                    }
                });
        });

        ui.add_space(10.0);

        section_frame(ui, "Installatie log", |ui| {
            if self.install_log.is_empty() {
                ui.label(
                    egui::RichText::new("Nog geen installaties uitgevoerd")
                        .color(Theme::MUTED)
                        .italics(),
                );
                return;
            }

            egui::ScrollArea::vertical()
                .id_salt("install_log")
                .max_height(180.0)
                .show(ui, |ui| {
                    for line in &self.install_log {
                        ui.label(
                            egui::RichText::new(line)
                                .size(11.0)
                                .color(Theme::TEXT),
                        );
                    }
                });
        });

        if !self.elevated {
            ui.add_space(10.0);
            section_frame(ui, "Tip", |ui| {
                ui.label(
                    egui::RichText::new(
                        "Start als administrator voor betrouwbaardere driver installatie.",
                    )
                    .size(12.0)
                    .color(Theme::WARNING),
                );
            });
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
                    if ui
                        .add(
                            egui::Button::new(egui::RichText::new("Ja, installeer").strong())
                                .fill(Theme::ACCENT),
                        )
                        .clicked()
                    {
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
                        .add(
                            egui::Button::new(egui::RichText::new("Herstart nu").strong())
                                .fill(Theme::ACCENT),
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

fn setup_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = Theme::BG;
    visuals.window_fill = Theme::PANEL;
    visuals.extreme_bg_color = Theme::BG;
    visuals.faint_bg_color = Theme::CARD;
    visuals.widgets.noninteractive.bg_fill = Theme::CARD;
    visuals.widgets.inactive.bg_fill = Theme::CARD;
    visuals.widgets.hovered.bg_fill = Theme::CARD_HOVER;
    visuals.widgets.active.bg_fill = Theme::ACCENT_DIM;
    visuals.selection.bg_fill = Theme::ACCENT.linear_multiply(0.35);
    visuals.widgets.noninteractive.fg_stroke.color = Theme::TEXT;
    visuals.widgets.inactive.fg_stroke.color = Theme::TEXT;
    visuals.widgets.hovered.fg_stroke.color = Theme::TEXT;
    visuals.override_text_color = Some(Theme::TEXT);
    visuals.window_corner_radius = egui::CornerRadius::same(10);
    visuals.widgets.noninteractive.corner_radius = egui::CornerRadius::same(8);
    visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(8);
    visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(8);
    visuals.widgets.active.corner_radius = egui::CornerRadius::same(8);
    ctx.set_visuals(visuals);

    ctx.style_mut(|style| {
        style.spacing.item_spacing = egui::vec2(10.0, 8.0);
        style.spacing.button_padding = egui::vec2(14.0, 8.0);
        style.spacing.window_margin = egui::Margin::same(16);
    });
}

fn card_frame(fill: egui::Color32) -> egui::Frame {
    egui::Frame::new()
        .fill(fill)
        .stroke(egui::Stroke::new(1.0, Theme::BORDER))
        .corner_radius(10)
        .inner_margin(14)
}

fn section_frame(ui: &mut egui::Ui, title: &str, add_body: impl FnOnce(&mut egui::Ui)) {
    card_frame(Theme::PANEL).show(ui, |ui| {
        ui.label(
            egui::RichText::new(title)
                .size(14.0)
                .strong()
                .color(Theme::TEXT),
        );
        ui.add_space(8.0);
        add_body(ui);
    });
}

fn stat_card(ui: &mut egui::Ui, label: &str, value: usize, color: egui::Color32) {
    card_frame(Theme::CARD).show(ui, |ui| {
        ui.set_min_width((ui.available_width() - 30.0) / 4.0);
        ui.vertical(|ui| {
            ui.label(
                egui::RichText::new(label)
                    .size(12.0)
                    .color(Theme::MUTED),
            );
            ui.label(
                egui::RichText::new(value.to_string())
                    .size(22.0)
                    .strong()
                    .color(color),
            );
        });
    });
}

fn badge(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    egui::Frame::new()
        .fill(color.linear_multiply(0.18))
        .stroke(egui::Stroke::new(1.0, color.linear_multiply(0.55)))
        .corner_radius(20)
        .inner_margin(egui::vec2(10.0, 4.0))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).size(12.0).color(color));
        });
}

fn render_device_card(ui: &mut egui::Ui, device: &DeviceDriver) {
    let (status_color, status_label) = status_badge(&device.status);

    card_frame(Theme::CARD).show(ui, |ui| {
        ui.horizontal_top(|ui| {
            egui::Frame::new()
                .fill(status_color.linear_multiply(0.18))
                .stroke(egui::Stroke::new(1.0, status_color.linear_multiply(0.5)))
                .corner_radius(6)
                .inner_margin(egui::vec2(8.0, 4.0))
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(status_label)
                            .size(11.0)
                            .strong()
                            .color(status_color),
                    );
                });

            ui.add_space(8.0);

            ui.vertical(|ui| {
                ui.label(
                    egui::RichText::new(&device.name)
                        .size(14.0)
                        .strong()
                        .color(Theme::TEXT),
                );

                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 12.0;
                    if !device.manufacturer.is_empty() {
                        detail_chip(ui, "Fabrikant", &device.manufacturer);
                    }
                    if !device.driver_version.is_empty() {
                        detail_chip(ui, "Driver", &format!("v{}", device.driver_version));
                    }
                    if let Some(date) = device.driver_date {
                        detail_chip(ui, "Datum", &date.format("%d-%m-%Y").to_string());
                    }
                    if !device.inf_name.is_empty() {
                        detail_chip(ui, "INF", &device.inf_name);
                    }
                });

                if let Some(title) = &device.update_title {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(format!("Update: {title}"))
                            .size(12.0)
                            .color(Theme::INFO),
                    );
                }
            });
        });
    });
}

fn detail_chip(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.label(
        egui::RichText::new(format!("{label}: "))
            .size(11.0)
            .color(Theme::MUTED),
    );
    ui.label(
        egui::RichText::new(value)
            .size(11.0)
            .color(Theme::TEXT),
    );
}

fn status_badge(status: &DeviceStatus) -> (egui::Color32, &str) {
    match status {
        DeviceStatus::Unknown => (Theme::MUTED, "Onbekend"),
        DeviceStatus::UpToDate => (Theme::SUCCESS, "Up-to-date"),
        DeviceStatus::UpdateAvailable => (Theme::INFO, "Update beschikbaar"),
        DeviceStatus::Installed => (Theme::SUCCESS, "Geïnstalleerd"),
    }
}

impl eframe::App for DriverUpdaterApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.sync_filter_cache();
        self.poll_worker(ctx);

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(Theme::BG).inner_margin(16))
            .show(ctx, |ui| {
                self.render_header(ui);
                ui.add_space(12.0);
                self.render_status_banner(ui);
                ui.add_space(12.0);
                self.render_stats(ui);
                ui.add_space(12.0);
                self.render_toolbar(ui, ctx);
                ui.add_space(12.0);
                self.render_main_content(ui, ctx);
            });

        self.render_confirm_dialog(ctx);
        self.render_restart_dialog(ctx);
    }
}
