//! Small shared UI helpers and the colour palette.

use eframe::egui::{Color32, RichText, Ui};

pub const ACCENT: Color32 = Color32::from_rgb(80, 160, 255);
pub const OK: Color32 = Color32::from_rgb(64, 192, 100);
pub const BAD: Color32 = Color32::from_rgb(224, 84, 84);
pub const WARN: Color32 = Color32::from_rgb(230, 168, 50);
pub const MUTED: Color32 = Color32::from_gray(150);

/// A small filled "pill" badge, e.g. `Enabled` / `Disabled`.
pub fn badge(ui: &mut Ui, text: &str, color: Color32) {
    ui.label(
        RichText::new(format!(" {text} "))
            .color(Color32::WHITE)
            .background_color(color)
            .strong(),
    );
}

/// A muted `label` followed by a monospace `value`, as one grid row.
pub fn info_row(ui: &mut Ui, label: &str, value: &str) {
    ui.label(RichText::new(label).color(MUTED));
    ui.label(RichText::new(value).monospace());
    ui.end_row();
}

/// Caption text under a heading.
pub fn caption(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).color(MUTED));
}
