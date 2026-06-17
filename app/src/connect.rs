//! "Connect" view: discover Windows PCs on the LAN and open Remote Desktop.

use crate::widgets::{self, ACCENT};
use eframe::egui::{self, Color32, RichText, Ui};
use neardesk_core as nd;
use std::net::Ipv4Addr;
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

const DEFAULT_PORT: u16 = 3389;
const REPAINT_WHILE_SCANNING: Duration = Duration::from_millis(150);

pub struct Connect {
    name: String,
    port: String,
    fullscreen: bool,
    status: String,
    /// Channel to the background discovery thread; `Some` while a scan runs.
    pending: Option<Receiver<nd::Discovery>>,
    hits: Vec<Ipv4Addr>,
    name_match: Option<Ipv4Addr>,
    selected: Option<Ipv4Addr>,
}

impl Connect {
    pub fn new() -> Self {
        let cfg = nd::load_config();
        Self {
            name: cfg.get("hostname").cloned().unwrap_or_default(),
            port: cfg
                .get("port")
                .cloned()
                .unwrap_or_else(|| DEFAULT_PORT.to_string()),
            fullscreen: true,
            status: "Enter a PC name, or just press Discover to scan the network.".to_owned(),
            pending: None,
            hits: Vec::new(),
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

    /// Drive the background scan: collect its result and keep repainting.
    pub fn poll(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.pending {
            if let Ok(found) = rx.try_recv() {
                self.selected = nd::pick_target(&found);
                let nd::Discovery { hits, name_match } = found;
                self.hits = hits;
                self.name_match = name_match;
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
        self.status = "Searching the local network\u{2026}".to_owned();
        self.hits.clear();
        self.name_match = None;
        self.selected = None;

        let (tx, rx) = channel();
        self.pending = Some(rx);
        std::thread::spawn(move || {
            let _ = tx.send(nd::discover(&name, port));
        });
    }

    fn describe(&self) -> String {
        match (self.hits.as_slice(), self.name_match) {
            ([], _) => {
                "No PCs with Remote Desktop found. Is the other PC on and shared?".to_owned()
            }
            (_, Some(ip)) => format!("Found \u{201C}{}\u{201D} at {ip}.", self.name.trim()),
            ([only], None) => format!("Found one PC at {only}."),
            (many, None) => format!("Found {} PCs \u{2014} pick one below.", many.len()),
        }
    }

    fn connect(&mut self, ip: Ipv4Addr) {
        let mut cfg = nd::load_config();
        cfg.insert("hostname".to_owned(), self.name.trim().to_owned());
        cfg.insert("port".to_owned(), self.port.trim().to_owned());
        nd::save_config(&cfg);

        self.status = match nd::launch_mstsc(ip, self.fullscreen) {
            Ok(()) => format!("Connecting to {ip}\u{2026}"),
            Err(e) => format!("Failed to launch Remote Desktop: {e}"),
        };
    }

    pub fn ui(&mut self, ui: &mut Ui) {
        ui.heading("Connect to a PC");
        widgets::caption(
            ui,
            "Find a Windows PC on your network and open Remote Desktop.",
        );
        ui.add_space(10.0);

        egui::Frame::group(ui.style()).show(ui, |ui| {
            egui::Grid::new("connect-form")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.label("PC name");
                    ui.text_edit_singleline(&mut self.name);
                    ui.end_row();
                    ui.label("Port");
                    ui.add(egui::TextEdit::singleline(&mut self.port).desired_width(80.0));
                    ui.end_row();
                });
            ui.checkbox(&mut self.fullscreen, "Open full screen");
        });

        ui.add_space(10.0);
        self.buttons(ui);
        ui.add_space(8.0);
        ui.separator();
        widgets::caption(ui, &self.status);

        if !self.hits.is_empty() {
            self.results(ui);
        }
    }

    fn buttons(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            let busy = self.is_scanning();
            let label = if busy {
                "Discovering\u{2026}"
            } else {
                "Discover"
            };
            if ui
                .add_enabled(
                    !busy,
                    egui::Button::new(label).min_size(egui::vec2(120.0, 30.0)),
                )
                .clicked()
            {
                self.start_discovery();
            }
            if let Some(ip) = self.selected {
                let text = RichText::new(format!("Connect to {ip}"))
                    .color(Color32::WHITE)
                    .strong();
                let button = egui::Button::new(text)
                    .fill(ACCENT)
                    .min_size(egui::vec2(170.0, 30.0));
                if ui.add(button).clicked() {
                    self.connect(ip);
                }
            }
        });
    }

    fn results(&mut self, ui: &mut Ui) {
        ui.add_space(8.0);
        ui.label(RichText::new(format!("{} found", self.hits.len())).strong());
        egui::ScrollArea::vertical()
            .max_height(180.0)
            .show(ui, |ui| {
                for ip in self.hits.clone() {
                    ui.horizontal(|ui| {
                        let chosen = self.selected == Some(ip);
                        if ui
                            .selectable_label(chosen, RichText::new(ip.to_string()).monospace())
                            .clicked()
                        {
                            self.selected = Some(ip);
                        }
                        if Some(ip) == self.name_match {
                            widgets::badge(ui, "name match", ACCENT);
                        }
                    });
                }
            });
    }
}
