//! The embedded NearDesk logo, decoded for the window icon and the UI.

use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};

const BYTES: &[u8] = include_bytes!("../../assets/logo.png");

fn decode() -> (Vec<u8>, u32, u32) {
    let img = image::load_from_memory(BYTES)
        .expect("embedded logo.png is valid")
        .to_rgba8();
    let (w, h) = img.dimensions();
    (img.into_raw(), w, h)
}

/// Load the logo as an egui texture (call once at startup).
pub fn texture(ctx: &egui::Context) -> TextureHandle {
    let (rgba, w, h) = decode();
    let image = ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
    ctx.load_texture("neardesk-logo", image, TextureOptions::LINEAR)
}

/// Decode the logo as a window / taskbar icon.
pub fn icon() -> egui::IconData {
    let (rgba, width, height) = decode();
    egui::IconData {
        rgba,
        width,
        height,
    }
}

/// A fixed-size image widget for the loaded logo texture.
pub fn image(texture: &TextureHandle, side: f32) -> egui::Image<'static> {
    let sized = egui::load::SizedTexture::new(texture.id(), egui::vec2(side, side));
    egui::Image::new(sized)
}
