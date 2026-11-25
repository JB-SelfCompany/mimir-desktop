//! Message actions: reaction strip + menu, opened by right click or the hover toolbar.
use crate::{i18n::{tr, Key, Lang}, model::{Menu, MsgRef}};
use eframe::egui::{self, Area, Button, Frame, Id, Key as K, Order, RichText, Ui};

/// Android BaseChatActivity.REACTION_EMOJIS, verbatim.
pub const REACTION_EMOJIS: [&str; 26] = [
    "👍", "❤\u{FE0F}", "🔥", "😂", "😮", "😢", "😡", "💩", "😱", "🤷\u{200D}♂\u{FE0F}", "🤷\u{200D}♀\u{FE0F}", "🤗", "😘",
    "😍", "💪", "🥴", "🤔", "🤮", "👌", "🍆", "🍌", "🍑", "💊", "❤\u{FE0F}\u{200D}🔥", "💯", "✅",
];
const STRIP: usize = 7;

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    React(String),
    Reply(i64),   // guid
    Copy,
    Forward(i64), // message id
    Delete(i64),  // message id
    /// Fetch the attachment (P3-R9).
    Download,
    /// Open the attachment with the system app (P3-R8).
    Open,
    /// Show the picture in the viewer.
    View,
    SaveFile,
    /// Delete the attachment's bytes, keeping the message (Android menu_delete_file).
    DeleteFile,
    /// Play/pause a voice message or an audio file inline.
    Play,
    /// Move the playing voice message to this fraction of its length.
    Seek(f32),
}

fn item(ui: &mut Ui, label: &str, shortcut: &str) -> bool {
    ui.add(Button::new(label).shortcut_text(shortcut).frame(false).min_size(egui::vec2(170., 0.))).clicked()
}

/// Draws the open menu; returns the chosen action with its message. Closes on action, Esc or a click outside.
pub fn show(ctx: &egui::Context, menu: &mut Option<Menu>, lang: Lang) -> Option<(Action, MsgRef)> {
    let m = menu.as_mut()?;
    let mut picked = None;
    let area = Area::new(Id::unique("message_menu")).order(Order::Foreground).fixed_pos(m.pos).constrain(true).show(ctx, |ui| {
        Frame::popup(ui.style()).show(ui, |ui| {
            if m.expanded {
                ui.set_max_width(9. * 30.);
                ui.horizontal_wrapped(|ui| {
                    for e in REACTION_EMOJIS {
                        if ui.add(Button::new(RichText::new(e).size(17.)).frame(false)).clicked() {
                            picked = Some(Action::React(e.to_string()));
                        }
                    }
                });
            } else {
                ui.horizontal(|ui| {
                    for e in &REACTION_EMOJIS[..STRIP] {
                        if ui.add(Button::new(RichText::new(*e).size(17.)).frame(false)).clicked() {
                            picked = Some(Action::React(e.to_string()));
                        }
                    }
                    if ui.add(Button::new("+").frame(false)).clicked() {
                        m.expanded = true;
                    }
                });
            }
            ui.separator();
            if item(ui, tr(lang, Key::Reply), "R") { picked = Some(Action::Reply(m.msg.guid)); }
            if item(ui, tr(lang, Key::Copy), "Ctrl+C") { picked = Some(Action::Copy); }
            if m.msg.forward.is_some() && item(ui, tr(lang, Key::Forward), "") { picked = Some(Action::Forward(m.msg.id)); }
            if m.msg.file.is_some() {
                if item(ui, tr(lang, Key::SaveFile), "") { picked = Some(Action::SaveFile); }
                if item(ui, tr(lang, Key::DeleteFile), "") { picked = Some(Action::DeleteFile); }
            }
            if item(ui, tr(lang, Key::Delete), "Del") { picked = Some(Action::Delete(m.msg.id)); }
        });
    });
    // ponytail: hotkeys work only while the menu is open, not for a merely hovered message.
    let typing = ctx.egui_wants_keyboard_input();
    ctx.input(|i| {
        if !typing && i.key_pressed(K::R) { picked = Some(Action::Reply(m.msg.guid)); }
        if !typing && i.key_pressed(K::Delete) { picked = Some(Action::Delete(m.msg.id)); }
        if i.events.iter().any(|e| matches!(e, egui::Event::Copy)) { picked = Some(Action::Copy); }
    });
    let outside = ctx.input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| !area.response.rect.contains(p)));
    let msg = m.msg.clone();
    if picked.is_some() || outside || ctx.input(|i| i.key_pressed(K::Escape)) {
        *menu = None;
    }
    picked.map(|a| (a, msg))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn emoji_list() {
        assert_eq!(REACTION_EMOJIS.len(), 26);
        assert_eq!(REACTION_EMOJIS[1], "❤\u{FE0F}");
        assert_eq!(REACTION_EMOJIS[23], "❤️‍🔥");
    }
}
