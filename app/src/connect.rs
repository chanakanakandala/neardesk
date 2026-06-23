//! "Connect" view: discover machines on the LAN and open a remote session.

use crate::widgets::{self, ACCENT};
use eframe::egui::{self, Color32, Layout, RichText, Ui};
use neardesk_core as nd;
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

const REPAINT_WHILE_SCANNING: Duration = Duration::from_millis(150);
const VNC_COLOR: Color32 = Color32::from_rgb(150, 110, 220);

pub struct Connect {
    /// Optional "find by name" hint (advanced).
    name: String,
    username: String,
    password: String,
    fullscreen: bool,
    status: String,
    /// Editable role label for the selected machine.
    role: String,
    /// Remembered username per machine label.
    host_users: HashMap<String, String>,
    /// Remembered role per machine label (the "agent board").
    host_roles: HashMap<String, String>,
    last_used: Option<String>,
    auto_started: bool,
    pending: Option<Receiver<nd::Discovery>>,
    hosts: Vec<nd::Host>,
    name_match: Option<Ipv4Addr>,
    selected: Option<Ipv4Addr>,
}

impl Connect {
    pub fn new() -> Self {
        let cfg = nd::load_config();
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
        let username = cfg
            .get("username")
            .cloned()
            .filter(|u| !stale(u))
            .unwrap_or_else(nd::current_username);
        Self {
            name: String::new(),
            username,
            password: String::new(),
            fullscreen: true,
            status: String::new(),
            role: String::new(),
            host_users,
            host_roles,
            last_used: cfg.get("hostname").cloned().filter(|s| !s.is_empty()),
            auto_started: false,
            pending: None,
            hosts: Vec::new(),
            name_match: None,
            selected: None,
        }
    }

    fn is_scanning(&self) -> bool {
        self.pending.is_some()
    }

    fn host(&self, ip: Ipv4Addr) -> Option<&nd::Host> {
        self.hosts.iter().find(|h| h.ip == ip)
    }

    /// Machine name if known, else the IP.
    fn host_label(&self, ip: Ipv4Addr) -> String {
        self.host(ip)
            .map(|h| h.label())
            .unwrap_or_else(|| ip.to_string())
    }

    fn last_used_ip(&self) -> Option<Ipv4Addr> {
        let last = self.last_used.as_deref()?;
        self.hosts.iter().find(|h| h.label() == last).map(|h| h.ip)
    }

    /// Select a machine and load its remembered username + role.
    fn select(&mut self, ip: Ipv4Addr) {
        self.selected = Some(ip);
        let key = self.host_label(ip);
        if let Some(user) = self.host_users.get(&key) {
            self.username = user.clone();
        }
        self.role = self.host_roles.get(&key).cloned().unwrap_or_default();
    }

    /// Save the selected machine's username and role to disk.
    fn persist(&mut self) {
        let Some(ip) = self.selected else { return };
        let key = self.host_label(ip);
        let user = self.username.trim().to_owned();
        let role = self.role.trim().to_owned();
        self.host_users.insert(key.clone(), user.clone());

        let mut cfg = nd::load_config();
        cfg.insert("hostname".to_owned(), key.clone());
        cfg.insert("username".to_owned(), user.clone());
        cfg.insert(format!("user.{key}"), user);
        if role.is_empty() {
            self.host_roles.remove(&key);
            cfg.remove(&format!("role.{key}"));
        } else {
            self.host_roles.insert(key.clone(), role.clone());
            cfg.insert(format!("role.{key}"), role);
        }
        nd::save_config(&cfg);
        self.last_used = Some(key);
    }

    pub fn poll(&mut self, ctx: &egui::Context) {
        if !self.auto_started {
            self.auto_started = true;
            self.start_discovery();
        }
        if let Some(rx) = &self.pending {
            if let Ok(found) = rx.try_recv() {
                let target = nd::pick_target(&found);
                let nd::Discovery { hosts, name_match } = found;
                self.hosts = hosts;
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
        self.status = String::new();
        self.hosts.clear();
        self.name_match = None;
        self.selected = None;

        let (tx, rx) = channel();
        self.pending = Some(rx);
        std::thread::spawn(move || {
            let _ = tx.send(nd::discover(&name));
        });
    }

    fn describe(&self) -> String {
        match self.hosts.len() {
            0 => String::new(),
            1 => format!("Found {}.", self.hosts[0].label()),
            n => format!("Found {n} computers."),
        }
    }

    fn connect(&mut self, ip: Ipv4Addr) {
        self.persist();
        let Some(host) = self.host(ip).cloned() else {
            return;
        };
        self.status = match nd::launch(
            ip,
            host.protocol.port(),
            &self.username,
            &self.password,
            self.fullscreen,
            host.protocol,
        ) {
            Ok(()) => format!(
                "Opening {} ({})\u{2026}",
                host.label(),
                host.protocol.label()
            ),
            Err(e) => format!("Couldn't connect: {e}"),
        };
    }

    pub fn ui(&mut self, ui: &mut Ui) {
        ui.heading("Connect to a PC");
        widgets::caption(
            ui,
            "Pick a computer on your network and open a remote session.",
        );
        ui.add_space(10.0);

        self.computers(ui);
        ui.add_space(10.0);
        self.sign_in(ui);
        ui.add_space(12.0);
        self.connect_bar(ui);
    }

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
            if self.hosts.is_empty() {
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
                                for host in self.hosts.clone() {
                                    self.computer_row(ui, &host);
                                }
                            });
                    });
            }
        });
    }

    fn computer_row(&mut self, ui: &mut Ui, host: &nd::Host) {
        let ip = host.ip;
        let key = host.label();
        let selected = self.selected == Some(ip);

        if ui
            .selectable_label(selected, RichText::new(&key).size(15.0))
            .clicked()
        {
            self.select(ip);
        }
        // Role column.
        match self.host_roles.get(&key) {
            Some(r) => ui.label(RichText::new(r).color(ACCENT)),
            None => ui.label(RichText::new("\u{2014}").color(widgets::MUTED)),
        };
        // Protocol chip.
        let color = if host.protocol == nd::Protocol::Vnc {
            VNC_COLOR
        } else {
            ACCENT
        };
        widgets::badge(ui, host.protocol.label(), color);
        // IP + tags.
        ui.horizontal(|ui| {
            if host.name.is_some() {
                ui.label(RichText::new(ip.to_string()).color(widgets::MUTED).small());
            }
            if self.last_used.as_deref() == Some(key.as_str()) {
                widgets::badge(ui, "last used", ACCENT);
            }
            if Some(ip) == self.name_match {
                widgets::badge(ui, "name match", ACCENT);
            }
        });
        ui.end_row();
    }

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
                "Use the account set on that machine. After the first connect only the \
                 password is needed.",
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
                                .hint_text("the account on that machine"),
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
