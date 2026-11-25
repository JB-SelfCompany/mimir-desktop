//! QR code texture for the contact link: white background, quiet zone 4 modules.
use eframe::egui::{self, Color32, ColorImage, TextureHandle, TextureOptions};
use qrcode::{Color, QrCode};

pub const SCALE: usize = 6;
const QUIET: usize = 4;

pub fn texture(ctx: &egui::Context, data: &str) -> TextureHandle {
    // ponytail: data longer than QR capacity (never for a contact link) panics here.
    let code = QrCode::new(data.as_bytes()).expect("contact link fits a QR code");
    let w = code.width();
    let side = (w + 2 * QUIET) * SCALE;
    let mut pixels = vec![Color32::WHITE; side * side];
    for (i, c) in code.to_colors().into_iter().enumerate() {
        if c == Color::Dark {
            let (x0, y0) = ((i % w + QUIET) * SCALE, (i / w + QUIET) * SCALE);
            for y in y0..y0 + SCALE {
                pixels[y * side..][x0..x0 + SCALE].fill(Color32::BLACK);
            }
        }
    }
    ctx.load_texture("my_qr", ColorImage::new([side, side], pixels), TextureOptions::NEAREST)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qr_texture_size() {
        let data = "mimir://mm/u/f9a4e8d77c34ce192aebc553d9815709c28d35c0a773160508de91a0dac43c22/Alice";
        let modules = QrCode::new(data.as_bytes()).unwrap().width();
        let t = texture(&egui::Context::default(), data);
        assert_eq!(t.size(), [(modules + 8) * SCALE; 2]);
    }
}
