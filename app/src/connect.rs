//! "Connect" view: discover Windows PCs on the LAN and open Remote Desktop.

use crate::widgets::{self, ACCENT};
use eframe::egui::{self, Color32, Layout, RichText, Ui};
use neardesk_core as nd;
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

const DEFAULT_PORT: u16 = 3389;
const REPAINT_WHILE_SCANNING: Duration = Duration::from_millis(150);

pub struct Connect {
    /// Optional "find by name" hint (advanced).
    name: String,
    username: String,
    password: String,
    port: String,
    fullscreen: bool,
    status: String,
    /// Editable role label for the selected machine (e.g. "Backend agent").
    role: String,
    /// Remembered username per computer name, so only the password is needed.
    host_users: HashMap<String, String>,
    /// Remembered role label per computer name (the "agent board").
    host_roles: HashMap<String, String>,
    last_used: Option<String>,
    auto_started: bool,
    pending: Option<Receiver<nd::Discovery>>,
    hits: Vec<Ipv4Addr>,
    names: HashMap<Ipv4Addr, String>,
    name_match: Option<Ipv4Addr>,
    selected: Option<Ipv4Addr>,
}

impl Connect {
    pub fn new() -> Self {
        let cfg = nd::load_config();
        // Drop the deprecated "neardesk" account so the real one is entered instead.
        let stale = |u: &str| u.eq_ignore_ascii_case("neardesk");
        let mut host_users = HashMap::new();
        let mut host_roles = HashMap::new();
        for (k, v) in &cfg {
            if let Some(host) = k.strip_prefix("user.") {
                if !stale(v) {
                    host_users.insert(host.to_owned(), v.clone());
                }
            } else if let Some(host) = k.strip_prefix("role.") {
                host_roles.insert(host.to_owned(), v.clone());
            }
        }
        // Pre-fill the remembered username, else guess this machine's own account.
        let username = cfg
            .get("username")
            .cloned()
            .filter(|u| !stale(u))
            .unwrap_or_else(nd::current_username);
        Self {
            name: String::new(),
            username,
            password: String::new(),
            port: cfg
                .get("port")
                .cloned()
                .unwrap_or_else(|| DEFAULT_PORT.to_string()),
            fullscreen: true,
            status: String::new(),
            role: String::new(),
            host_users,
            host_roles,
            last_used: cfg.get("hostname").cloned().filter(|s| !s.is_empty()),
            auto_started: false,
            pending: None,
            hits: Vec::new(),
            names: HashMap::new(),
            name_match: None,
            selected: None,
        }
    }

    fn is_scanning(&self) -> bool {
        self.pending.is_some()
    }

    fn parsed_port(&self) -> u16 {
        self.port.trim().parse().unwrap_or(DEFAULT_PORT)
    }

    /// Computer name if known, else the IP.
    fn host_label(&self, ip: Ipv4Addr) -> String {
        self.names
            .get(&ip)
            .cloned()
            .unwrap_or_else(|| ip.to_string())
    }

    /// The discovered IP whose name matches the last-used computer, if present.
    fn last_used_ip(&self) -> Option<Ipv4Addr> {
        let last = self.last_used.as_deref()?;
        self.names
            .iter()
            .find(|(_, name)| name.as_str() == last)
            .map(|(ip, _)| *ip)
    }

    /// Select a host and load its remembered username + role.
    fn select(&mut self, ip: Ipv4Addr) {
        self.selected = Some(ip);
        let host = self.host_label(ip);
        if let Some(user) = self.host_users.get(&host) {
            self.username = user.clone();
        }
        self.role = self.host_roles.get(&host).cloned().unwrap_or_default();
    }

    /// Save the selected machine's username, role and port to disk.
    fn persist(&mut self) {
        let Some(ip) = self.selected else { return };
        let host = self.host_label(ip);
        let user = self.username.trim().to_owned();
        let role = self.role.trim().to_owned();
        self.host_users.insert(host.clone(), user.clone());

        let mut cfg = nd::load_config();
        cfg.insert("hostname".to_owned(), host.clone());
        cfg.insert("username".to_owned(), user.clone());
        cfg.insert("port".to_owned(), self.port.trim().to_owned());
        cfg.insert(format!("user.{host}"), user);
        if role.is_empty() {
            self.host_roles.remove(&host);
            cfg.remove(&format!("role.{host}"));
        } else {
            self.host_roles.insert(host.clone(), role.clone());
            cfg.insert(format!("role.{host}"), role);
        }
        nd::save_config(&cfg);
        self.last_used = Some(host);
    }

    /// Auto-scan on first show, then collect a finished scan.
    pub fn poll(&mut self, ctx: &egui::Context) {
        if !self.auto_started {
            self.auto_started = true;
            self.start_discovery();
        }
        if let Some(rx) = &self.pending {
            if let Ok(found) = rx.try_recv() {
                let target = nd::pick_target(&found);
                let nd::Discovery {
                    hits,
                    name_match,
                    names,
                } = found;
                self.hits = hits;
                self.names = names;
                self.name_match = name_match;
                self.selected = None;
                if let Some(ip) = target.or_else(|| self.last_used_ip()) {
                    self.select(ip);
                }
                self.status = self.describe();
                self.pending = None;
            }
        }
        if self.is_scanning() {
            ctx.request_repaint_after(REPAINT_WHILE_SCANNING);
        }
    }

    fn start_discovery(&mut self) {
        let name = self.name.trim().to_owned();
        let port = self.parsed_port();
        self.port = port.to_string();
        self.status = String::new();
        self.hits.clear();
        self.names.clear();
        self.name_match = None;
        self.selected = None;

        let (tx, rx) = channel();
        self.pending = Some(rx);
        std::thread::spawn(move || {
            let _ = tx.send(nd::discover(&name, port));
        });
    }

    fn describe(&self) -> String {
        match self.hits.len() {
            0 => String::new(),
            1 => format!("Found {}.", self.host_label(self.hits[0])),
            n => format!("Found {n} computers."),
        }
    }

    fn connect(&mut self, ip: Ipv4Addr) {
        self.persist();
        self.status = match nd::launch_rdp(
            ip,
            self.parsed_port(),
            &self.username,
            &self.password,
            self.fullscreen,
        ) {
            Ok(()) => format!("Opening {ip}\u{2026}"),
            Err(e) => format!("Failed to launch Remote Desktop: {e}"),
        };
    }

    pub fn ui(&mut self, ui: &mut Ui) {
        ui.heading("Connect to a PC");
        widgets::caption(
            ui,
            "Pick a computer on your network and open Remote Desktop.",
        );
        ui.add_space(10.0);

        self.computers(ui);
        ui.add_space(10.0);
        self.sign_in(ui);
        ui.add_space(12.0);
        self.connect_bar(ui);
    }

    /// The live list of discovered computers, with a rescan control.
    fn computers(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.strong("Computers on your network");
            ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                if self.is_scanning() {
                    ui.add(egui::Spinner::new().size(16.0));
                    ui.label(
                        RichText::new("Scanning\u{2026}")
                            .color(widgets::MUTED)
                            .small(),
                    );
                } else if ui
                    .add(egui::Button::new(RichText::new("Rescan").color(ACCENT)))
                    .clicked()
                {
                    self.start_discovery();
                }
            });
        });
        ui.add_space(4.0);

        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            if self.hits.is_empty() {
                let msg = if self.is_scanning() {
                    "Searching the network\u{2026}".to_owned()
                } else {
                    "No computers found. Turn on sharing on the other PC under \u{201C}This \
                     PC\u{201D}, then Rescan."
                        .to_owned()
                };
                ui.label(RichText::new(msg).color(widgets::MUTED));
            } else {
                egui::ScrollArea::vertical()
                    .max_height(180.0)
                    .show(ui, |ui| {
                        egui::Grid::new("computers")
                            .num_columns(4)
                            .striped(true)
                            .spacing([14.0, 8.0])
                            .show(ui, |ui| {
                                for ip in self.hits.clone() {
                                    self.computer_row(ui, ip);
                                }
                            });
                    });
            }
        });
    }

    fn computer_row(&mut self, ui: &mut Ui, ip: Ipv4Addr) {
        let name = self.names.get(&ip).cloned();
        let selected = self.selected == Some(ip);
        let title = name.clone().unwrap_or_else(|| ip.to_string());

        if ui
            .selectable_label(selected, RichText::new(title).size(15.0))
            .clicked()
        {
            self.select(ip);
        }
        // Role column — the "agent board" label.
        let role = name.as_ref().and_then(|n| self.host_roles.get(n));
        match role {
            Some(r) => ui.label(RichText::new(r).color(ACCENT)),
            None => ui.label(RichText::new("—").color(widgets::MUTED)),
        };
        let ip_text = if name.is_some() {
            ip.to_string()
        } else {
            String::new()
        };
        ui.label(RichText::new(ip_text).color(widgets::MUTED).small());
        ui.horizontal(|ui| {
            if name.is_some() && self.last_used.as_deref() == name.as_deref() {
                widgets::badge(ui, "last used", ACCENT);
            }
            if Some(ip) == self.name_match {
                widgets::badge(ui, "name match", ACCENT);
            }
        });
        ui.end_row();
    }

    /// Credentials for the selected computer, plus an advanced section.
    fn sign_in(&mut self, ui: &mut Ui) {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let header = match self.selected {
                Some(ip) => format!("Sign in to {}", self.host_label(ip)),
                None => "Sign in".to_owned(),
            };
            ui.strong(header);
            widgets::caption(
                ui,
                "Use the account set on that PC. After the first connect only the password \
                 is needed.",
            );
            ui.add_space(6.0);
            let mut changed = false;
            egui::Grid::new("creds")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Role");
                    changed |= ui
                        .add(
                            egui::TextEdit::singleline(&mut self.role)
                                .hint_text("e.g. Backend agent, Test runner"),
                        )
                        .lost_focus();
                    ui.end_row();
                    ui.label("Username");
                    changed |= ui
                        .add(
                            egui::TextEdit::singleline(&mut self.username)
                                .hint_text("the account on that PC"),
                        )
                        .lost_focus();
                    ui.end_row();
                    ui.label("Password");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.password)
                            .password(true)
                            .hint_text("connects without a prompt"),
                    );
                    ui.end_row();
                });
            if changed && self.selected.is_some() {
                self.persist();
            }
            egui::CollapsingHeader::new("Advanced").show(ui, |ui| {
                egui::Grid::new("adv")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Find by name");
                        ui.add(egui::TextEdit::singleline(&mut self.name).hint_text("optional"));
                        ui.end_row();
                        ui.label("Port");
                        ui.add(egui::TextEdit::singleline(&mut self.port).desired_width(80.0));
                        ui.end_row();
                        ui.label("Full screen");
                        ui.checkbox(&mut self.fullscreen, "");
                        ui.end_row();
                    });
            });
        });
    }

    fn connect_bar(&mut self, ui: &mut Ui) {
        let target = self.selected;
        let label = match target {
            Some(ip) => format!("Connect to {}", self.host_label(ip)),
            None => "Connect".to_owned(),
        };
        let button = egui::Button::new(RichText::new(label).color(Color32::WHITE).strong())
            .fill(ACCENT)
            .min_size(egui::vec2(240.0, 40.0));
        if ui.add_enabled(target.is_some(), button).clicked() {
            if let Some(ip) = target {
                self.connect(ip);
            }
        }
        if !self.status.is_empty() {
            ui.add_space(6.0);
            widgets::caption(ui, &self.status);
        }
    }
}
