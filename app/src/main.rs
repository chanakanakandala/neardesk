// NearDesk — one app to discover, connect to, and share Windows PCs over the LAN.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod about;
mod connect;
mod logo;
mod this_pc;
mod widgets;

use connect::Connect;
use eframe::egui;
use std::sync::Arc;
use this_pc::ThisPc;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([720.0, 500.0])
            .with_min_inner_size([620.0, 440.0])
            .with_icon(Arc::new(logo::icon())),
        ..Default::default()
    };
    eframe::run_native("NearDesk", options, Box::new(|cc| Box::new(App::new(cc))))
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum View {
    Connect,
    ThisPc,
    About,
}

struct App {
    view: View,
    connect: Connect,
    this_pc: ThisPc,
    logo: egui::TextureHandle,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        configure_style(&cc.egui_ctx);
        Self {
            view: View::Connect,
            connect: Connect::new(),
            this_pc: ThisPc::new(),
            logo: logo::texture(&cc.egui_ctx),
        }
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        let host = self.this_pc.info().hostname.clone();
        ui.add_space(12.0);
        ui.vertical_centered(|ui| {
            ui.add(logo::image(&self.logo, 104.0));
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("LAN Remote Desktop")
                    .color(widgets::MUTED)
                    .small(),
            );
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new(format!("This PC: {host}"))
                    .color(widgets::ACCENT)
                    .small()
                    .strong(),
            );
        });
        ui.add_space(16.0);

        nav(ui, &mut self.view, View::Connect, "Connect");
        nav(ui, &mut self.view, View::ThisPc, "This PC");
        nav(ui, &mut self.view, View::About, "About");

        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            ui.add_space(10.0);
            let info = self.this_pc.info();
            ui.horizontal(|ui| {
                if info.elevated {
                    widgets::badge(ui, "Admin", widgets::OK);
                } else {
                    widgets::badge(ui, "Standard", widgets::MUTED);
                }
            });
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Remote Desktop")
                        .color(widgets::MUTED)
                        .small(),
                );
                if info.rdp_enabled {
                    widgets::badge(ui, "on", widgets::OK);
                } else {
                    widgets::badge(ui, "off", widgets::BAD);
                }
            });
        });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.connect.poll(ctx);

        egui::SidePanel::left("nav")
            .exact_width(170.0)
            .resizable(false)
            .show(ctx, |ui| self.sidebar(ui));

        egui::CentralPanel::default().show(ctx, |ui| match self.view {
            View::Connect => self.connect.ui(ui),
            View::ThisPc => self.this_pc.ui(ui),
            View::About => about::ui(ui, &self.logo),
        });
    }
}

/// A full-width selectable navigation entry.
fn nav(ui: &mut egui::Ui, current: &mut View, target: View, label: &str) {
    let selected = *current == target;
    let item = egui::SelectableLabel::new(selected, egui::RichText::new(label).size(15.0));
    if ui.add_sized([ui.available_width(), 34.0], item).clicked() {
        *current = target;
    }
}

fn configure_style(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(10.0, 6.0);
    ctx.set_style(style);
}
