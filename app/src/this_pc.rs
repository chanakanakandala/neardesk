//! "This PC" view: show this machine's details and share it via Remote Desktop.

use crate::widgets::{self, BAD, OK, WARN};
use eframe::egui::{self, Layout, Ui};
use neardesk_core as nd;

/// One applied setting in the enable-Remote-Desktop checklist.
struct Step {
    name: String,
    ok: bool,
    detail: String,
}

pub struct ThisPc {
    info: nd::SystemInfo,
    steps: Vec<Step>,
}

impl ThisPc {
    pub fn new() -> Self {
        Self {
            info: nd::system_info(),
            steps: Vec::new(),
        }
    }

    pub fn info(&self) -> &nd::SystemInfo {
        &self.info
    }

    fn refresh(&mut self) {
        self.info = nd::system_info();
    }

    fn run_setup(&mut self) {
        self.steps = nd::enable_remote_desktop()
            .into_iter()
            .map(|(name, result)| match result {
                Ok(()) => Step {
                    name,
                    ok: true,
                    detail: String::new(),
                },
                Err(detail) => Step {
                    name,
                    ok: false,
                    detail,
                },
            })
            .collect();
        self.refresh();
    }

    pub fn ui(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.heading("This PC");
            ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Refresh").clicked() {
                    self.refresh();
                }
            });
        });
        widgets::caption(ui, "How this computer appears to others on the network.");
        ui.add_space(10.0);

        self.info_card(ui);
        ui.add_space(12.0);
        self.action(ui);

        if !self.steps.is_empty() {
            ui.add_space(10.0);
            self.checklist(ui);
        }
    }

    fn info_card(&self, ui: &mut Ui) {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            egui::Grid::new("sysinfo")
                .num_columns(2)
                .spacing([16.0, 8.0])
                .show(ui, |ui| {
                    widgets::info_row(ui, "Computer name", &self.info.hostname);
                    widgets::info_row(ui, "Signed in as", &self.info.username);
                    widgets::info_row(ui, "Operating system", &self.info.os_name);
                    widgets::info_row(ui, "Version", &self.info.os_build);
                    widgets::info_row(ui, "Architecture", &self.info.arch);
                    widgets::info_row(ui, "IP address", &self.info.ip);

                    ui.label(egui::RichText::new("Remote Desktop").color(widgets::MUTED));
                    if self.info.rdp_enabled {
                        widgets::badge(ui, "Enabled", OK);
                    } else {
                        widgets::badge(ui, "Disabled", BAD);
                    }
                    ui.end_row();
                });
        });
    }

    fn action(&mut self, ui: &mut Ui) {
        if self.info.rdp_enabled {
            ui.horizontal(|ui| {
                widgets::badge(ui, "Ready", OK);
                ui.label(format!(
                    "Others can connect to this PC using \u{201C}{}\u{201D}.",
                    self.info.hostname
                ));
            });
            return;
        }

        if !self.info.elevated {
            ui.colored_label(
                WARN,
                "Administrator rights are needed to enable Remote Desktop.",
            );
            ui.add_space(4.0);
            if ui
                .add_sized([240.0, 34.0], egui::Button::new("Restart as Administrator"))
                .clicked()
            {
                let _ = nd::relaunch_elevated();
                std::process::exit(0);
            }
            return;
        }

        if ui
            .add_sized([240.0, 36.0], egui::Button::new("Enable Remote Desktop"))
            .clicked()
        {
            self.run_setup();
        }
    }

    fn checklist(&self, ui: &mut Ui) {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            for step in &self.steps {
                ui.horizontal(|ui| {
                    let (color, mark) = if step.ok {
                        (OK, "\u{2713}")
                    } else {
                        (BAD, "\u{2717}")
                    };
                    ui.colored_label(color, mark);
                    ui.label(&step.name);
                });
                if !step.ok && !step.detail.is_empty() {
                    ui.colored_label(widgets::MUTED, format!("      {}", step.detail));
                }
            }
        });
    }
}
