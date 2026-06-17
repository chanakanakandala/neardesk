//! "About" view: app identity, purpose, and credits.

use crate::logo;
use crate::widgets;
use eframe::egui::{self, RichText, TextureHandle, Ui};

const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn ui(ui: &mut Ui, logo: &TextureHandle) {
    ui.heading("About");
    widgets::caption(ui, "What NearDesk is for.");
    ui.add_space(10.0);

    ui.horizontal(|ui| {
        ui.add(logo::image(logo, 72.0));
        ui.add_space(8.0);
        ui.vertical(|ui| {
            ui.add_space(8.0);
            ui.label(
                RichText::new("NearDesk")
                    .size(26.0)
                    .strong()
                    .color(widgets::ACCENT),
            );
            ui.label(RichText::new(format!("Version {VERSION}")).color(widgets::MUTED));
        });
    });
    ui.add_space(12.0);

    ui.label(
        "NearDesk reaches the other Windows PCs on your local network and opens a Remote \
         Desktop session in a couple of clicks \u{2014} no setup files, accounts, or cloud \
         services.",
    );
    ui.add_space(8.0);
    ui.label(
        "It exists for one workflow in particular: running AI coding agents \u{2014} Claude \
         Code, OpenAI Codex, GitHub Copilot CLI, and friends \u{2014} across more than one \
         machine. Instead of piling every task onto the computer in front of you, delegate \
         work to the right agent on the right box (a spare mini PC, a build server, a GPU \
         rig) and hop onto it to drive and review what it is doing.",
    );
    ui.add_space(14.0);

    egui::Frame::group(ui.style()).show(ui, |ui| {
        egui::Grid::new("about-info")
            .num_columns(2)
            .spacing([16.0, 8.0])
            .show(ui, |ui| {
                widgets::info_row(ui, "Built with", "Rust + egui");
                widgets::info_row(ui, "Protocol", "Windows Remote Desktop (RDP)");
                widgets::info_row(ui, "License", "MIT");
            });
    });

    ui.add_space(10.0);
    widgets::caption(ui, "\u{00A9} 2026 NearDesk contributors");
}
