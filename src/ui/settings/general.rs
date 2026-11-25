//! The settings sections with no worker jobs: receive, images, priority, voice, font, appearance, about (AS §1.2-1.6).
use super::{commit, row_title, save};
use crate::{
    app::{theme_preference, MimirDesktop},
    i18n::{tr, trf, Key, Lang},
    model::chat_list::build_chat_list,
    settings::FONT_SIZES,
    ui::{bubble, theme::{field_label, palette, primary_button}, welcome},
};
use eframe::egui::{self, vec2, Button, ComboBox, CornerRadius, FontFamily, FontId, Frame, OpenUrl, Rect, RichText, Role, Sense, Shape, Slider, Stroke, TextEdit, WidgetInfo};

/// Auto-download caps (AS §1.2): 0 never, -1 always, else up to that many bytes.
const CAPS: [(i64, Key); 5] = [
    (0, Key::DownloadNever),
    (1 << 20, Key::DownloadUpTo1Mb),
    (5 << 20, Key::DownloadUpTo5Mb),
    (10 << 20, Key::DownloadUpTo10Mb),
    (-1, Key::DownloadAlways),
];

const IMAGE_SIZES: [Key; 4] = [Key::ImageSizeNoResize, Key::ImageSize4k, Key::ImageSizeFullHd, Key::ImageSizeSmall];

const VOICE: [(u32, Key); 3] = [(32000, Key::VoiceQuality32), (64000, Key::VoiceQuality64), (128000, Key::VoiceQuality128)];

const FONT_NAMES: [Key; 5] = [Key::FontVerySmall, Key::FontSmall, Key::FontMedium, Key::FontLarge, Key::FontVeryLarge];

const SITE: &str = "https://mimir.im";

fn pal(ui: &egui::Ui) -> crate::ui::theme::Palette {
    palette(ui.visuals().dark_mode)
}

pub fn receive(app: &mut MimirDesktop, ui: &mut egui::Ui) {
    let (lang, p) = (app.lang, pal(ui));
    let mut changed = false;
    row_title(ui, &p, lang, Key::ContactRequestsFrom, Some(Key::ContactRequestsFromDesc));
    ui.horizontal(|ui| {
        for (v, k) in [(false, Key::ContactRequestsEveryone), (true, Key::ContactRequestsNobody)] {
            changed |= ui.radio_value(&mut app.settings.block_strangers, v, tr(lang, k)).changed();
        }
    });
    let m = &mut app.settings.media;
    for (title, desc, cap) in [
        (Key::AutoDownloadContacts, Key::AutoDownloadContactsDesc, &mut m.auto_contacts),
        (Key::AutoDownloadOthers, Key::AutoDownloadOthersDesc, &mut m.auto_others),
        (Key::AutoDownloadGroups, Key::AutoDownloadGroupsDesc, &mut m.auto_groups),
    ] {
        ui.separator();
        row_title(ui, &p, lang, title, Some(desc));
        ui.horizontal_wrapped(|ui| {
            for (v, k) in CAPS {
                changed |= ui.radio_value(cap, v, tr(lang, k)).changed();
            }
        });
    }
    if changed {
        commit(app);
    }
}

pub fn images(app: &mut MimirDesktop, ui: &mut egui::Ui) {
    let (lang, p) = (app.lang, pal(ui));
    let m = &mut app.settings.media;
    let mut changed = false;
    let size_label = field_label(ui, &p, tr(lang, Key::SelectResizeSize));
    let current = tr(lang, IMAGE_SIZES[(m.image_format as usize).min(3)]);
    let combo = ComboBox::from_id_salt("image_size").selected_text(current).width(240.).show_ui(ui, |ui| {
        for (i, k) in IMAGE_SIZES.into_iter().enumerate() {
            changed |= ui.selectable_value(&mut m.image_format, i as u8, tr(lang, k)).changed();
        }
    });
    combo.response.labelled_by(size_label.id);
    ui.add_space(8.);
    let quality_label = field_label(ui, &p, tr(lang, Key::SelectResizeQuality));
    ui.spacing_mut().slider_width = (ui.available_width() - 80.).max(120.);
    changed |= ui.add(Slider::new(&mut m.image_quality, 60..=99).suffix("%")).labelled_by(quality_label.id).changed();
    if changed {
        save(app);
    }
}

pub fn priority(app: &mut MimirDesktop, ui: &mut egui::Ui) {
    let (lang, p) = (app.lang, pal(ui));
    let mut apply = false;
    // Android's number dialog is titled with the row title.
    let label = field_label(ui, &p, tr(lang, Key::ClientPriority));
    ui.horizontal(|ui| {
        let field = ui.add(TextEdit::singleline(&mut app.state.settings_ui.priority).desired_width(120.)).labelled_by(label.id);
        apply = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        apply |= ui.add(primary_button(&p, tr(lang, Key::Save))).clicked();
    });
    if apply {
        // Android: trim, toIntOrNull, coerceIn(1, 100), else 1 (AS §1.4).
        let v = app.state.settings_ui.priority.trim().parse::<i64>().map_or(1, |v| v.clamp(1, 100)) as u8;
        app.settings.priority = v;
        app.state.settings_ui.priority = v.to_string();
        commit(app);
    }
}

pub fn voice(app: &mut MimirDesktop, ui: &mut egui::Ui) {
    let lang = app.lang;
    let mut changed = false;
    for (v, k) in VOICE {
        changed |= ui.radio_value(&mut app.settings.media.voice_bitrate, v, tr(lang, k)).changed();
    }
    if changed {
        save(app);
    }
}

pub fn font(app: &mut MimirDesktop, ui: &mut egui::Ui) {
    let (lang, p) = (app.lang, pal(ui));
    let mut step = FONT_SIZES.iter().position(|&s| s as f32 == app.settings.message_font).unwrap_or(2);
    let caption = format!("{}  {}", tr(lang, Key::MessageTextSizeLabel), tr(lang, FONT_NAMES[step])).to_uppercase();
    let label = ui.label(RichText::new(caption).size(14.).color(p.text));
    ui.spacing_mut().slider_width = ui.available_width();
    if ui.add(Slider::new(&mut step, 0..=4).show_value(false)).labelled_by(label.id).changed() {
        app.settings.message_font = FONT_SIZES[step] as f32;
        if let Some(chat) = app.state.open_chat.as_mut() {
            chat.timeline.invalidate_heights();
        }
        save(app);
    }
    ui.add_space(12.);
    ui.label(RichText::new(tr(lang, Key::FontPreviewLabel).to_uppercase()).size(14.).color(p.text));
    // An incoming bubble on the chat canvas, as the chat draws it (bubble::geom, radius 2, tail bottom-left).
    Frame::NONE.fill(p.canvas).inner_margin(8).corner_radius(2).show(ui, |ui| {
        let text = tr(lang, Key::FontPreviewText);
        let width = ui.available_width();
        let galley = ui.painter().layout(text.to_string(), FontId::proportional(app.settings.message_font), p.text, width - 40.);
        let (rect, resp) = ui.allocate_exact_size(vec2(width, galley.size().y + 12.), Sense::hover());
        resp.widget_info(|| WidgetInfo::labeled(Role::Label, true, text));
        let content = Rect::from_min_size(rect.min + vec2(24., 6.), galley.size());
        let g = bubble::geom(content, false);
        let r = bubble::RADIUS as u8;
        ui.painter().rect_filled(g.body, CornerRadius { nw: r, ne: r, sw: 0, se: r }, p.bubble_in);
        ui.painter().add(Shape::convex_polygon(g.tail.to_vec(), p.bubble_in, Stroke::NONE));
        ui.painter().galley(content.min, galley, p.text);
    });
}

pub fn appearance(app: &mut MimirDesktop, ui: &mut egui::Ui) {
    let (lang, p) = (app.lang, pal(ui));
    row_title(ui, &p, lang, Key::Theme, None);
    let mut dark = app.settings.dark;
    ui.horizontal(|ui| {
        for (v, k) in [(None, Key::ThemeSystem), (Some(false), Key::ThemeLight), (Some(true), Key::ThemeDark)] {
            ui.radio_value(&mut dark, v, tr(lang, k));
        }
    });
    ui.separator();
    row_title(ui, &p, lang, Key::Language, None);
    let mut choice = app.settings.lang;
    ui.horizontal(|ui| {
        // Each language under its own name, whatever the UI language.
        for (v, name) in [(None, tr(lang, Key::LanguageSystem)), (Some(Lang::Ru), "Русский"), (Some(Lang::En), "English")] {
            ui.radio_value(&mut choice, v, name);
        }
    });
    if dark != app.settings.dark {
        app.settings.dark = dark;
        ui.ctx().set_theme(theme_preference(dark));
        save(app);
    }
    if choice != app.settings.lang {
        app.settings.lang = choice;
        app.lang = choice.unwrap_or_else(Lang::from_system);
        // The chat list holds localized text («Избранное», previews).
        // ponytail: the audio player keeps the language it started with until restart (its error texts only).
        app.state.chats = build_chat_list(&app.core, app.lang);
        save(app);
    }
}

pub fn about(app: &mut MimirDesktop, ui: &mut egui::Ui) {
    let (lang, p) = (app.lang, pal(ui));
    ui.vertical_centered(|ui| {
        welcome::rune(ui, &p);
        ui.add_space(8.);
        ui.label(RichText::new("Mimir Desktop").size(18.).family(FontFamily::Name("bold".into())).color(p.text));
        // ponytail: the core (mimir crate) exposes no version, so only ours is shown.
        ui.label(RichText::new(trf(lang, Key::AboutVersion, &[env!("CARGO_PKG_VERSION")])).size(14.).color(p.text));
    });
    ui.add_space(16.);
    ui.label(RichText::new(tr(lang, Key::Links)).size(14.).family(FontFamily::Name("bold".into())).color(p.text));
    if ui.add_sized([180., 36.], Button::new("mimir.im").fill(p.field)).on_hover_text(SITE).clicked() {
        ui.ctx().open_url(OpenUrl::new_tab(SITE));
    }
}
