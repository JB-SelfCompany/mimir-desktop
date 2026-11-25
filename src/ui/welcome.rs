//! First-run screen (Android activity_introduction).
use crate::{i18n::{tr, Key, Lang}, ui::theme::Palette};
use eframe::egui::{self, vec2, Button, CentralPanel, Frame, Pos2, RichText, Sense, Stroke, TextEdit};

/// Mannaz rune: two verticals joined at the top by an X.
pub fn rune(ui: &mut egui::Ui, p: &Palette) {
    let (rect, _) = ui.allocate_exact_size(vec2(96., 96.), Sense::hover());
    let (l, r) = (rect.left() + 20., rect.right() - 20.);
    let mid = rect.center().y - 8.;
    let s = Stroke::new(6., p.text);
    let g = ui.painter();
    g.line_segment([Pos2::new(l, rect.top()), Pos2::new(l, rect.bottom())], s);
    g.line_segment([Pos2::new(r, rect.top()), Pos2::new(r, rect.bottom())], s);
    g.line_segment([Pos2::new(l, rect.top()), Pos2::new(r, mid)], s);
    g.line_segment([Pos2::new(r, rect.top()), Pos2::new(l, mid)], s);
}

/// The startup screen while `backup::restore` runs (I2): the rune, a spinner and `text`; with `close` (another
/// instance restoring) a «Закрыть» button instead of the spinner, true once clicked.
pub fn progress(ui: &mut egui::Ui, lang: Lang, p: &Palette, text: Key, close: bool) -> bool {
    let mut clicked = false;
    CentralPanel::default().frame(Frame::NONE.fill(p.list_bg)).show(ui, |ui| {
        ui.vertical_centered(|ui| {
            ui.add_space((ui.available_height() - 200.).max(0.) / 2.);
            rune(ui, p);
            ui.add_space(24.);
            ui.label(RichText::new(tr(lang, text)).size(16.).color(p.text));
            ui.add_space(16.);
            if close {
                clicked = ui.add(crate::ui::theme::primary_button(p, tr(lang, Key::Close))).clicked();
            } else {
                ui.spinner();
            }
        });
    });
    clicked
}

/// What the user chose on the welcome screen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Choice {
    /// «Начать» (button or Enter in the name field).
    Start,
    /// «Импортировать с телефона»: an Android (or desktop) backup instead of a new account.
    Import,
}

/// `name` is the optional «What should we call you?» field. `import`: `None` idle, `Some(None)` an import is being
/// staged (spinner), `Some(Some(text))` it failed.
pub fn show(ui: &mut egui::Ui, lang: Lang, p: &Palette, name: &mut String, import: Option<Option<&str>>) -> Option<Choice> {
    let mut start = false;
    let mut choice = None;
    CentralPanel::default().frame(Frame::NONE.fill(p.list_bg)).show(ui, |ui| {
        ui.vertical_centered(|ui| {
            // Centred with last frame's content height (it depends on language and wrapping).
            let id = ui.scope_id().with("welcome_height");
            let content_h = ui.data(|d| d.get_temp::<f32>(id)).unwrap_or(380.);
            ui.add_space((ui.available_height() - content_h).max(0.) / 2.);
            let top = ui.cursor().top();
            rune(ui, p);
            ui.add_space(24.);
            ui.label(RichText::new(tr(lang, Key::WelcomeTitle)).size(24.).family(egui::FontFamily::Name("bold".into())).color(p.text));
            ui.add_space(12.);
            ui.scope(|ui| {
                ui.set_max_width(420.);
                ui.label(RichText::new(tr(lang, Key::WelcomeBody)).size(14.).color(p.text_dim));
            });
            ui.add_space(20.);
            let field = ui.add(TextEdit::singleline(name).hint_text(tr(lang, Key::WhatToCallYou)).desired_width(260.));
            start |= field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            // Focused on first show only, so typing goes straight in without grabbing focus back later.
            let focused_once = ui.scope_id().with("welcome_name_focused");
            if !ui.data(|d| d.get_temp::<bool>(focused_once).unwrap_or(false)) {
                field.request_focus();
                ui.data_mut(|d| d.insert_temp(focused_once, true));
            }
            ui.add_space(16.);
            let b = Button::new(RichText::new(tr(lang, Key::GetStarted)).size(15.).color(egui::Color32::BLACK))
                .fill(p.accent)
                .min_size(vec2(180., 38.));
            let button = ui.add(b);
            start |= button.clicked();
            ui.add_space(8.);
            // A text button (Material TextButton) under the filled one: the secondary way in.
            let import_button = Button::new(RichText::new(tr(lang, Key::ImportFromPhone)).size(14.).color(p.accent)).frame(false).min_size(vec2(180., 32.));
            let import_button = ui.add_enabled(import != Some(None), import_button).on_hover_cursor(egui::CursorIcon::PointingHand);
            if import_button.clicked() {
                choice = Some(Choice::Import);
            }
            let mut bottom = import_button.rect.bottom();
            match import {
                Some(None) => {
                    bottom = ui.horizontal(|ui| {
                        ui.add_space((ui.available_width() - 180.).max(0.) / 2.);
                        ui.spinner();
                        ui.label(RichText::new(tr(lang, Key::BackupImporting)).size(12.).color(p.text_dim));
                    }).response.rect.bottom();
                }
                Some(Some(text)) => bottom = ui.label(RichText::new(text).size(12.).color(p.danger)).rect.bottom(),
                None => {}
            }
            let h = bottom - top;
            if (h - content_h).abs() > 0.5 {
                ui.data_mut(|d| d.insert_temp(id, h));
                ui.ctx().request_repaint();
            }
        });
    });
    if start { Some(Choice::Start) } else { choice }
}
