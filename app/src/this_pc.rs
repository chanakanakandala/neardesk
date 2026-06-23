//! "This PC" view: show this machine's details and share it via Remote Desktop.

use crate::widgets::{self, BAD, OK};
use eframe::egui::{self, Layout, RichText, Ui};
use neardesk_core as nd;

/// One applied setting in the share checklist.
struct Step {
    name: String,
    ok: bool,
    detail: String,
}

fn to_steps(raw: Vec<(String, Result<(), String>)>) -> Vec<Step> {
    raw.into_iter()
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
        .collect()
}

pub struct ThisPc {
    info: nd::SystemInfo,
    password: String,
    steps: Vec<Step>,
}

impl ThisPc {
    pub fn new() -> Self {
        Self {
            info: nd::system_info(),
            password: String::new(),
            steps: Vec::new(),
        }
    }

    pub fn info(&self) -> &nd::SystemInfo {
        &self.info
    }

    fn refresh(&mut self) {
        self.info = nd::system_info();
    }

    /// Enable Remote Desktop, set the password, grant admin + RDP access.
    fn turn_on(&mut self) {
        let user = self.info.username.clone();
        self.steps = to_steps(nd::share(&user, &self.password));
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
        self.remote_access(ui);

        if !self.steps.is_empty() {
            ui.add_space(10.0);
            self.checklist(ui);
        }
    }

    fn info_card(&self, ui: &mut Ui) {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
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

                    ui.label(egui::RichText::new("Remote access").color(widgets::MUTED));
                    if self.info.sharing_enabled {
                        widgets::badge(ui, "Enabled", OK);
                    } else {
                        widgets::badge(ui, "Disabled", BAD);
                    }
                    ui.end_row();
                });
        });
    }

    fn remote_access(&mut self, ui: &mut Ui) {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.strong("Remote access");

            if !self.info.elevated {
                widgets::caption(
                    ui,
                    "Sharing changes Windows settings, so it needs Administrator access.",
                );
                ui.add_space(6.0);
                ui.label(
                    "Click below and confirm the Windows prompt. NearDesk reopens with the \
                     rights it needs, then you can finish here.",
                );
                ui.add_space(8.0);
                if ui
                    .add_sized([260.0, 38.0], egui::Button::new("Restart as Administrator"))
                    .clicked()
                {
                    let _ = nd::relaunch_elevated();
                    std::process::exit(0);
                }
                return;
            }

            ui.add_space(2.0);
            ui.horizontal(|ui| {
                if self.info.sharing_enabled {
                    widgets::badge(ui, "On", OK);
                    ui.label("Other PCs on your network can connect to this computer.");
                } else {
                    widgets::badge(ui, "Off", BAD);
                    ui.label("Turn on access, then connect with your Windows password.");
                }
            });
            ui.add_space(8.0);
            widgets::caption(
                ui,
                "Others sign in with this computer's name and your normal Windows password \
                 \u{2014} nothing extra to set.",
            );
            ui.add_space(8.0);

            let account = format!("{}\\{}", self.info.hostname, self.info.username);
            egui::Grid::new("share")
                .num_columns(2)
                .spacing([16.0, 8.0])
                .show(ui, |ui| {
                    widgets::info_row(ui, "Your account", &account);
                });
            ui.add_space(10.0);
            let label = if self.info.sharing_enabled {
                "Re-apply remote access"
            } else {
                "Turn on remote access"
            };
            if ui
                .add_sized([260.0, 40.0], egui::Button::new(label))
                .clicked()
            {
                self.turn_on();
            }

            egui::CollapsingHeader::new("Account has no password?").show(ui, |ui| {
                widgets::caption(
                    ui,
                    "Only if you sign in with a PIN and never set a password: set one here, \
                     then use it to connect.",
                );
                ui.add_space(4.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.password)
                        .password(true)
                        .hint_text("new password (optional)"),
                );
            });

            if self.info.sharing_enabled {
                ui.add_space(12.0);
                ui.separator();
                ui.add_space(6.0);
                ui.label(RichText::new("To connect from another PC").strong());
                ui.add_space(4.0);
                egui::Grid::new("howto")
                    .num_columns(2)
                    .spacing([16.0, 8.0])
                    .show(ui, |ui| {
                        widgets::info_row(ui, "Computer name", &self.info.hostname);
                        widgets::info_row(ui, "Username", &self.info.username);
                        ui.label(RichText::new("Password").color(widgets::MUTED));
                        ui.label("your Windows password");
                        ui.end_row();
                    });
            }
        });
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
