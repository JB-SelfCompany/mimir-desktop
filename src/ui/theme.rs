use eframe::egui::{self, vec2, Button, Color32, Context, CornerRadius, FontData, FontDefinitions, FontFamily, Frame, Id, Modal, Role, RichText, Sense, Stroke, Theme, WidgetInfo};
use std::sync::Arc;

#[derive(Clone, Copy)]
pub struct Palette {
    pub toolbar: Color32,
    pub list_bg: Color32,
    pub divider: Color32,
    pub canvas: Color32,
    pub bubble_out: Color32,
    pub bubble_in: Color32,
    pub field: Color32,
    pub accent: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    /// Destructive confirms (delete, clear history, remove contact).
    pub danger: Color32,
    /// File bubble icon box (Android file_icon_background: semi_gray / light_gray).
    pub file_icon: Color32,
}

const fn rgb(c: u32) -> Color32 {
    Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

pub fn palette(dark: bool) -> Palette {
    let pick = |d: u32, l: u32| rgb(if dark { d } else { l });
    Palette {
        toolbar: rgb(0x404040),
        list_bg: pick(0x121212, 0xFFFFFF),
        divider: pick(0x222222, 0xF2F2F2),
        canvas: pick(0x000000, 0xE0E0E0),
        bubble_out: pick(0x305090, 0xA0C0FF),
        bubble_in: pick(0x303030, 0xFFFFFF),
        field: pick(0x303030, 0xE0E0E0),
        accent: rgb(0x49C29C),
        text: pick(0xFFFFFF, 0x000000),
        text_dim: pick(0xA0A0A0, 0x606060),
        danger: rgb(0xF44336),
        file_icon: pick(0x404040, 0xE0E0E0),
    }
}

/// Dialog shell shared by the add-contact, profile and requests modals: 16 px inner margin.
pub fn dialog(ctx: &Context, id: Id) -> Modal {
    Modal::new(id).frame(Frame::popup(&ctx.global_style()).inner_margin(16))
}

/// Dialog title (toolbar title size) with an 8 px gap below.
pub fn dialog_title(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(16.).family(FontFamily::Name("bold".into())));
    ui.add_space(8.);
}

/// Muted caption above a dialog field; the response is for `labelled_by`.
pub fn field_label(ui: &mut egui::Ui, p: &Palette, text: &str) -> egui::Response {
    ui.label(RichText::new(text).size(12.).color(p.text_dim))
}

/// A 22 px ✕ (dim strokes) named `label` for assistive tech, as in the info panel's header.
pub fn close_button(ui: &mut egui::Ui, p: &Palette, label: &str) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(22., 22.), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, label));
    let s = Stroke::new(1.5, p.text_dim);
    let c = r.center();
    ui.painter().line_segment([c + vec2(-5., -5.), c + vec2(5., 5.)], s);
    ui.painter().line_segment([c + vec2(-5., 5.), c + vec2(5., -5.)], s);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A clickable row (a `Sense::click()` scope) with the chat list's hover fill under its content.
pub fn click_row<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> egui::InnerResponse<R> {
    let bg = ui.painter().add(egui::Shape::Noop);
    let r = ui.scope_builder(egui::UiBuilder::new().sense(egui::Sense::click()), add);
    if r.response.hovered() {
        ui.painter().set(bg, egui::epaint::RectShape::filled(r.response.rect, 0., palette(ui.visuals().dark_mode).divider));
    }
    r
}

/// Filled with the theme accent, like the welcome «Начать».
pub fn primary_button(p: &Palette, text: &str) -> Button<'static> {
    Button::new(RichText::new(text.to_owned()).color(Color32::BLACK)).fill(p.accent)
}

/// `primary_button` for destructive confirms: red fill, white text. Dismiss buttons stay plain `Button`s.
pub fn danger_button(p: &Palette, text: &str) -> Button<'static> {
    Button::new(RichText::new(text.to_owned()).color(Color32::WHITE)).fill(p.danger)
}

pub fn install(ctx: &Context) {
    let mut fonts = FontDefinitions::default();
    let mut add = |name: &str, bytes: &'static [u8]| {
        fonts.font_data.insert(name.into(), Arc::new(FontData::from_static(bytes)));
    };
    add("noto-sans", include_bytes!("../../assets/fonts/NotoSans-Regular.ttf"));
    add("noto-sans-bold", include_bytes!("../../assets/fonts/NotoSans-Bold.ttf"));
    // Noto Sans Mono (OFL), google/fonts ofl/notosansmono/NotoSansMono[wdth,wght].ttf (variable, default = Regular)
    add("noto-mono", include_bytes!("../../assets/fonts/NotoSansMono.ttf"));
    // Pinned: googlefonts/noto-emoji@1ffdd21391dd1f25c081fa93a9dea0c7c029442b 2D/fonts/Noto-COLRv1.ttf (OFL, see OFL.txt)
    add("noto-emoji", include_bytes!("../../assets/fonts/Noto-COLRv1.ttf"));
    let fam = &mut fonts.families;
    fam.insert(FontFamily::Proportional, vec!["noto-sans".into(), "noto-emoji".into()]);
    fam.insert(FontFamily::Monospace, vec!["noto-mono".into(), "noto-emoji".into()]);
    fam.insert(FontFamily::Name("bold".into()), vec!["noto-sans-bold".into(), "noto-emoji".into()]);
    ctx.set_fonts(fonts);

    for (theme, dark) in [(Theme::Dark, true), (Theme::Light, false)] {
        let p = palette(dark);
        let mut style = (*ctx.style_of(theme)).clone();
        let v = &mut style.visuals;
        v.panel_fill = p.list_bg;
        v.window_fill = p.list_bg;
        v.selection.bg_fill = p.accent;
        let r = CornerRadius::same(3);
        v.window_corner_radius = r;
        v.menu_corner_radius = r;
        for w in [
            &mut v.widgets.noninteractive,
            &mut v.widgets.inactive,
            &mut v.widgets.hovered,
            &mut v.widgets.active,
            &mut v.widgets.open,
        ] {
            w.corner_radius = r;
        }
        ctx.set_style_of(theme, style);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Color32;
    #[test]
    fn install_sets_styles() {
        let ctx = Context::default();
        install(&ctx);
        assert_eq!(ctx.style_of(Theme::Dark).visuals.selection.bg_fill, palette(true).accent);
        assert_eq!(ctx.style_of(Theme::Light).visuals.panel_fill, palette(false).list_bg);
        let v = &ctx.style_of(Theme::Light).visuals;
        let r = CornerRadius::same(3);
        assert_eq!((v.widgets.inactive.corner_radius, v.window_corner_radius, v.menu_corner_radius), (r, r, r), "half egui's 6");
    }
    #[test]
    fn palette_matches_spec() {
        let d = palette(true);
        let l = palette(false);
        assert_eq!(d.bubble_out, Color32::from_rgb(0x30, 0x50, 0x90));
        assert_eq!(l.bubble_out, Color32::from_rgb(0xA0, 0xC0, 0xFF));
        assert_eq!(d.canvas, Color32::BLACK);
        assert_eq!(l.canvas, Color32::from_rgb(0xE0, 0xE0, 0xE0));
        assert_eq!(d.toolbar, l.toolbar);
        assert_eq!(d.accent, Color32::from_rgb(0x49, 0xC2, 0x9C));
    }
}
