//! Info panel (right side, or an overlay on narrow windows) and the chat menu with its dialogs: contacts here,
//! groups in `group_info`.
use crate::{
    app::MimirDesktop,
    core::worker::WorkResult,
    i18n::{tr, Key, Lang},
    model::{chat_key::ChatKey, chat_list::{build_chat_list, SAVED_ID}, links::contact_link, timeline::Timeline},
    ui::{avatar::{self, avatar_color}, group_info, qr, theme::{danger_button, dialog_title, field_label, palette, primary_button, Palette}},
};
use eframe::egui::{
    self, vec2, Area, Button, FontFamily, FontId, Frame, Id, Label, Layout, Order, Popup, Response, RichText,
    ScrollArea, TextEdit, TextureHandle, Ui,
};

pub const WIDTH: f32 = 230.;

pub enum Act {
    Mute,
    Block,
    Rename,
    Clear,
    Remove,
}

enum Dialog {
    /// (stored name, edited text)
    Rename(String, String),
    Clear,
    Remove,
}

#[derive(Default)]
pub struct ContactUi {
    /// (contact id, muted, blocked): read from storage when the panel or menu first needs it for a chat, then kept in step with the toggles.
    flags: Option<(i64, bool, bool)>,
    dialog: Option<Dialog>,
    qr: Option<TextureHandle>,
}

/// (contact id, public key, name) of the selected chat; `None` for a group (`group_info` has its own).
fn current(app: &MimirDesktop) -> Option<(i64, Vec<u8>, String)> {
    let Some(ChatKey::Contact(id)) = app.state.selected else { return None };
    let r = app.state.chats.iter().find(|r| r.key == ChatKey::Contact(id))?;
    Some((id, r.pubkey.clone(), r.name.clone()))
}

/// True when the info panel should be drawn (never for Saved Messages).
pub fn visible(app: &MimirDesktop) -> bool {
    app.state.info_open
        && match app.state.selected {
            Some(key @ ChatKey::Group(_)) => app.state.chats.iter().any(|r| r.key == key),
            _ => current(app).is_some_and(|c| c.0 != SAVED_ID),
        }
}

fn flags(app: &mut MimirDesktop, id: i64, pubkey: &[u8]) -> (bool, bool) {
    if app.state.contact_ui.flags.map(|f| f.0) != Some(id) {
        let muted = app.core.get_contact(id).is_some_and(|c| c.muted);
        app.state.contact_ui.flags = Some((id, muted, app.core.is_contact_blocked(pubkey.to_vec())));
    }
    let f = app.state.contact_ui.flags.unwrap();
    (f.1, f.2)
}

fn act(app: &mut MimirDesktop, a: Act) {
    let Some((id, pubkey, _)) = current(app) else { return };
    let (muted, blocked) = flags(app, id, &pubkey);
    match a {
        Act::Mute => match app.core.set_contact_muted(id, !muted) {
            Ok(()) => app.state.contact_ui.flags = Some((id, !muted, blocked)),
            Err(e) => failed(app, if muted { Key::Unmute } else { Key::Mute }, e),
        },
        Act::Block => match if blocked { app.core.unblock_contact(pubkey) } else { app.core.block_contact(pubkey) } {
            Ok(()) => app.state.contact_ui.flags = Some((id, muted, !blocked)),
            Err(e) => failed(app, if blocked { Key::Unblock } else { Key::Block }, e),
        },
        Act::Rename => {
            // The stored name, not the key-prefix fallback the list shows (empty stays empty).
            let stored = app.core.get_contact(id).map(|c| c.name).unwrap_or_default();
            app.state.contact_ui.dialog = Some(Dialog::Rename(stored.clone(), stored));
        }
        Act::Clear => app.state.contact_ui.dialog = Some(Dialog::Clear),
        Act::Remove => app.state.contact_ui.dialog = Some(Dialog::Remove),
    }
}

/// A storage call behind menu item `what` failed: said in the status line.
fn failed(app: &mut MimirDesktop, what: Key, e: mimir::MimirError) {
    app.error_text(format!("{}: {e}", tr(app.lang, what)));
}

/// The ⋮ menu of the chat header, a popup under the button `more`.
pub fn menu(app: &mut MimirDesktop, more: &Response) {
    if let Some(ChatKey::Group(id)) = app.state.selected {
        return group_info::menu(app, more, id);
    }
    let Some((id, pubkey, _)) = current(app) else { return };
    let (lang, saved) = (app.lang, id == SAVED_ID);
    let mut picked = None;
    Popup::menu(more).show(|ui| {
        ui.set_min_width(170.);
        if !saved {
            let (muted, blocked) = flags(app, id, &pubkey);
            let items = [
                (Act::Rename, tr(lang, Key::Rename)),
                (Act::Mute, tr(lang, if muted { Key::Unmute } else { Key::Mute })),
                (Act::Block, tr(lang, if blocked { Key::Unblock } else { Key::Block })),
            ];
            for (a, label) in items {
                if ui.button(label).clicked() {
                    picked = Some(a);
                }
            }
        }
        if ui.button(tr(lang, Key::ClearHistory)).clicked() {
            picked = Some(Act::Clear);
        }
        if !saved && ui.button(tr(lang, Key::RemoveContact)).clicked() {
            picked = Some(Act::Remove);
        }
    });
    if let Some(a) = picked {
        act(app, a);
    }
}

/// Title, value and small buttons; returns the index of the clicked button.
pub(crate) fn row(ui: &mut Ui, p: &Palette, title: &str, value: &str, buttons: &[&str]) -> Option<usize> {
    ui.label(RichText::new(title).size(11.).color(p.text_dim));
    ui.add(Label::new(RichText::new(value).font(FontId::monospace(11.))));
    let mut clicked = None;
    ui.horizontal(|ui| {
        for (i, b) in buttons.iter().enumerate() {
            if ui.add(Button::new(*b)).clicked() {
                clicked = Some(i);
            }
        }
    });
    clicked
}

/// The panel's frame: the close button, then `body` in a scroll area. Returns whether close was clicked.
pub(crate) fn panel_frame(ui: &mut Ui, p: &Palette, lang: Lang, body: impl FnOnce(&mut Ui)) -> bool {
    let mut close = false;
    Frame::NONE.inner_margin(12).show(ui, |ui| {
        ui.set_width(WIDTH - 24.);
        ui.spacing_mut().item_spacing.y = 6.;
        ui.with_layout(Layout::right_to_left(egui::Align::Min), |ui| {
            close = crate::ui::theme::close_button(ui, p, tr(lang, Key::Close)).clicked();
        });
        // Below the close button the panel scrolls, so short windows still reach the last link.
        // A solid bar takes its own strip; the default floating one covered the key's last column (F17).
        ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
        ScrollArea::vertical().auto_shrink([false, true]).show(ui, body);
    });
    close
}

pub fn contents(app: &mut MimirDesktop, ui: &mut Ui, p: &Palette) {
    if let Some(ChatKey::Group(id)) = app.state.selected {
        return group_info::contents(app, ui, p, id);
    }
    let Some((id, pubkey, name)) = current(app) else { return };
    let lang = app.lang;
    let (muted, blocked) = flags(app, id, &pubkey);
    let hex = hex::encode(&pubkey);
    let (mimir_link, web_link) = (contact_link(&pubkey, &name), format!("https://go.mimir.im/u/{hex}"));
    let (mut picked, mut close, mut show_qr, mut copy) = (None, false, false, None);
    let path = app.state.chats.iter().find(|r| r.key == ChatKey::Contact(id)).and_then(|r| crate::media::avatar::resolve(&app.paths.core_dir, &r.avatar));
    let thumbs = &mut app.state.thumbs;
    let close_clicked = panel_frame(ui, p, lang, |ui| {
        ui.vertical_centered(|ui| {
            // 128 px, corner 8, in contact info.
            avatar::show(ui, thumbs, path.as_deref(), avatar_color(&pubkey), 128., 4.);
            ui.label(RichText::new(&name).font(FontId::new(15., FontFamily::Name("bold".into()))).color(p.text));
        });
        let w = ui.available_width();
        let actions = [
            (None, tr(lang, Key::MessageAction)),
            (Some(Act::Mute), tr(lang, if muted { Key::Unmute } else { Key::Mute })),
            (Some(Act::Block), tr(lang, if blocked { Key::Unblock } else { Key::Block })),
        ];
        for (a, label) in actions {
            if ui.add_sized([w, 28.], Button::new(label)).clicked() {
                match a {
                    Some(a) => picked = Some(a),
                    // ponytail: "Message" has no other job while the chat is already open; it only closes the panel.
                    None => close = true,
                }
            }
        }
        ui.separator();
        if row(ui, p, tr(lang, Key::PublicKey), &hex, &[tr(lang, Key::Copy)]).is_some() {
            copy = Some(hex.clone());
        }
        match row(ui, p, tr(lang, Key::MimirLink), &mimir_link, &[tr(lang, Key::QrButton), tr(lang, Key::Copy)]) {
            Some(0) => show_qr = true,
            Some(_) => copy = Some(mimir_link.clone()),
            None => {}
        }
        if row(ui, p, tr(lang, Key::WebLink), &web_link, &[tr(lang, Key::Copy)]).is_some() {
            copy = Some(web_link.clone());
        }
    });
    close |= close_clicked;
    if let Some(t) = copy {
        ui.ctx().copy_text(t);
    }
    if show_qr {
        app.state.contact_ui.qr = Some(qr::texture(ui.ctx(), &mimir_link));
    }
    if let Some(a) = picked {
        act(app, a);
    }
    if close {
        app.state.info_open = false;
    }
}

/// Narrow windows: the panel floats over the chat at the right edge, below its header, instead of taking its width.
/// A click on the chat or the list closes it (not one on the header, whose name toggles it, nor on a dialog or menu).
pub fn overlay(app: &mut MimirDesktop, ctx: &egui::Context, p: &Palette) {
    let screen = ctx.content_rect();
    let top = screen.top() + crate::ui::chat_view::HEADER_H;
    let area = Area::new(Id::NULL.with("contact_info_overlay")).order(Order::Foreground).fixed_pos(egui::pos2(screen.right() - WIDTH, top)).show(ctx, |ui| {
        Frame::popup(ui.style()).fill(p.list_bg).inner_margin(0).corner_radius(0).show(ui, |ui| {
            ui.set_min_size(vec2(WIDTH, screen.bottom() - top));
            contents(app, ui, p);
        });
    });
    let rect = area.response.rect;
    let press = ctx.input(|i| i.pointer.interact_pos().filter(|_| i.pointer.any_pressed()));
    // layer_id_at locks the context: not inside the input closure.
    if press.is_some_and(|pos| pos.y > top && !rect.contains(pos) && ctx.layer_id_at(pos).is_none_or(|l| l.order == Order::Background)) {
        app.state.info_open = false;
    }
}

fn apply(app: &mut MimirDesktop, id: i64, d: Dialog) {
    let lang = app.lang;
    match d {
        Dialog::Rename(stored, n) => {
            let n = n.trim();
            // An unchanged name is not a local rename: that would stop the peer's own name from applying.
            if !n.is_empty() && n != stored.trim() {
                if let Err(e) = app.core.rename_contact(id, n.to_string(), true) {
                    failed(app, Key::Rename, e);
                }
            }
        }
        Dialog::Clear => {
            let names = app.core.clear_contact_history(id);
            let files_dir = app.paths.files_dir.clone();
            app.worker.submit(Box::new(move |_| {
                crate::media::remove_attachment_files(&files_dir, &names);
                WorkResult::Done
            }));
            if let Some(chat) = app.state.open_chat.as_mut().filter(|c| c.key == ChatKey::Contact(id)) {
                chat.timeline = Timeline::open(&app.core, chat.key);
                chat.reactions.clear();
                chat.day_key = (usize::MAX, 0, 0);
            }
        }
        Dialog::Remove => match app.core.remove_contact(id) {
            Ok(()) => {
                app.state.selected = None;
                app.state.open_chat = None;
                app.state.info_open = false;
            }
            Err(e) => failed(app, Key::RemoveContact, e),
        },
    }
    app.state.chats = build_chat_list(&app.core, lang);
}

/// Contact QR, then the rename / clear history / remove dialog (a group's come from `group_info`).
pub fn dialogs(app: &mut MimirDesktop, ctx: &egui::Context) {
    if let Some(ChatKey::Group(id)) = app.state.selected {
        app.state.contact_ui.dialog = None;
        return group_info::dialogs(app, ctx, id);
    }
    let lang = app.lang;
    if let Some(tex) = app.state.contact_ui.qr.clone() {
        let (mut ok, r) = (false, crate::ui::theme::dialog(ctx, Id::NULL.with("contact_qr")).show(ctx, |ui| {
            ui.set_width(260.); // the code's width; OK sits under its right edge
            let p = palette(ui.visuals().dark_mode);
            dialog_title(ui, tr(lang, Key::ContactQr));
            ui.image((tex.id(), vec2(260., 260.)));
            ui.add_space(8.);
            ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| ui.add(primary_button(&p, tr(lang, Key::Ok))).clicked()).inner
        }));
        ok |= r.inner;
        if ok || r.should_close() {
            app.state.contact_ui.qr = None;
        }
        return;
    }
    let Some((id, _, _)) = current(app) else {
        app.state.contact_ui.dialog = None;
        return;
    };
    let Some(dialog) = &mut app.state.contact_ui.dialog else { return };
    let (mut ok, mut cancel) = (false, false);
    let r = crate::ui::theme::dialog(ctx, Id::NULL.with("contact_dialog")).show(ctx, |ui| {
        ui.set_width(280.);
        let p = palette(ui.visuals().dark_mode);
        dialog_title(ui, tr(lang, match dialog {
            Dialog::Rename(..) => Key::Rename,
            Dialog::Clear => Key::ClearHistory,
            Dialog::Remove => Key::RemoveContact,
        }));
        match dialog {
            Dialog::Rename(_, name) => {
                let l = field_label(ui, &p, tr(lang, Key::NewName));
                let e = ui.add(TextEdit::singleline(name).desired_width(f32::INFINITY)).labelled_by(l.id);
                if ui.memory(|m| m.focused().is_none()) {
                    e.request_focus();
                    // Typing replaces the old name (desktop rename convention; Android leaves the cursor at the end).
                    if let Some(mut s) = egui::text_edit::TextEditState::load(ui.ctx(), e.id) {
                        s.cursor.set_char_range(Some(egui::text::CCursorRange::select_all_str(name)));
                        s.store(ui.ctx(), e.id);
                    }
                }
                ok = e.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            }
            Dialog::Clear => {
                ui.label(tr(lang, Key::ClearHistoryQuestion));
            }
            Dialog::Remove => {
                ui.label(tr(lang, Key::RemoveContactQuestion));
            }
        }
        ui.add_space(8.);
        ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
            let confirm = if matches!(dialog, Dialog::Rename(..)) { primary_button } else { danger_button };
            ok |= ui.add(confirm(&p, tr(lang, Key::Ok))).clicked();
            cancel = ui.button(tr(lang, Key::Cancel)).clicked();
        });
    });
    if ok || cancel || r.should_close() {
        let d = app.state.contact_ui.dialog.take().unwrap();
        if ok {
            apply(app, id, d);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::model::chat_key::ChatKey;
    use crate::{
        app::MimirDesktop,
        model::chat_list::build_chat_list,
        settings::Settings,
        test_support::test_harness,
    };
    use egui_kittest::{kittest::Queryable as _, Harness};
    use eframe::egui::vec2;

    fn contact_chat(size: (f32, f32)) -> (Harness<'static, MimirDesktop>, i64) {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        h.set_size(vec2(size.0, size.1));
        let core = h.state().core.clone();
        let c = core.add_contact(vec![7; 32], "Аня".into()).unwrap();
        h.state_mut().state.chats = build_chat_list(&core, crate::i18n::Lang::Ru);
        h.state_mut().state.selected = Some(ChatKey::Contact(c));
        h.run();
        (h, c)
    }

    #[test]
    fn links_are_labelled() {
        let (mut h, _) = contact_chat((1200., 700.));
        h.get_all_by_label("Аня").last().unwrap().click();
        h.run();
        assert!(h.query_by_label("Ссылка mimir://").is_some());
        assert!(h.query_by_label("Веб-ссылка").is_some());
    }

    #[test]
    fn contact_qr_dialog_is_as_wide_as_the_code() {
        let (mut h, _) = contact_chat((1200., 800.));
        h.state_mut().state.info_open = true;
        h.run();
        h.get_by_label("QR").click();
        h.run();
        let (title, ok) = (h.get_by_label("QR-код контакта").rect(), h.get_by_label("OK").rect());
        let w = ok.right() - title.left();
        assert!((250. ..=270.).contains(&w), "OK sits under the 260 px code: {w}");
    }

    #[test]
    fn info_panel_toggles() {
        let (mut h, _) = contact_chat((1200., 700.));
        assert!(h.query_by_label("Публичный ключ").is_none());
        h.get_all_by_label("Аня").last().unwrap().click();
        h.run();
        assert!(h.state().state.info_open);
        assert!(h.query_by_label("Публичный ключ").is_some());
        h.get_by_label("Закрыть").click();
        h.run();
        assert!(h.query_by_label("Публичный ключ").is_none());
    }

    #[test]
    fn panel_scrolls_to_its_last_button() {
        // Wide (side panel) and narrow (overlay) windows, 700 px high and lower.
        for (w, hgt) in [(1200., 700.), (900., 700.), (1200., 480.), (900., 480.)] {
            let (mut h, _) = contact_chat((w, hgt));
            h.state_mut().state.info_open = true;
            h.run();
            h.get_all_by_label("Копировать").last().unwrap().scroll_to_me();
            h.run();
            let last = h.get_all_by_label("Копировать").last().unwrap().rect();
            assert!(last.top() >= 0. && last.bottom() <= hgt, "{w}x{hgt}: last button {last:?} off-screen");
        }
    }

    #[test]
    fn scroll_bar_does_not_cover_the_key() {
        // F17: a short window needs the scroll bar; it gets its own strip instead of floating over the text.
        let (mut h, _) = contact_chat((1200., 480.));
        h.state_mut().state.info_open = true;
        h.run();
        let key = h.get_by_label(&"07".repeat(32)).rect();
        let content_right = 1200. - 12.; // panel right edge minus its inner margin
        let bar = h.ctx.global_style().spacing.scroll.bar_width;
        assert!(key.right() <= content_right - bar, "key ends at {} under a {bar} px bar at {content_right}", key.right());
    }

    #[test]
    fn narrow_window_overlays_info() {
        let right = |h: &Harness<'static, MimirDesktop>| h.get_by_label("⋮").rect().right();
        for (w, shrinks) in [(900., false), (1200., true)] {
            let (mut h, _) = contact_chat((w, 700.));
            let before = right(&h);
            h.state_mut().state.info_open = true;
            h.run();
            assert!(h.query_by_label("Публичный ключ").is_some(), "width {w}");
            assert_eq!(right(&h) < before, shrinks, "width {w}");
        }
    }

    #[test]
    fn narrow_overlay_leaves_the_header_and_closes_on_a_chat_click() {
        let (mut h, _) = contact_chat((900., 700.));
        h.state_mut().state.info_open = true;
        h.run();
        let more = h.get_by_label("⋮").rect();
        assert!(h.get_by_label("Публичный ключ").rect().top() > more.bottom(), "the overlay starts below the header");
        h.get_by_label("⋮").click();
        h.run();
        assert!(h.query_by_label("Переименовать").is_some(), "⋮ is not covered");
        h.key_press(eframe::egui::Key::Escape);
        h.run();
        assert!(h.state().state.info_open, "the menu closing leaves the panel");
        // A press on the chat itself (left of the overlay, below the header).
        let at = eframe::egui::pos2(900. - super::WIDTH - 40., 400.);
        h.hover_at(at);
        for pressed in [true, false] {
            h.event(eframe::egui::Event::PointerButton { pos: at, button: eframe::egui::PointerButton::Primary, pressed, modifiers: Default::default() });
            h.run();
        }
        assert!(!h.state().state.info_open);
    }

    #[test]
    fn copy_buttons_copy_key_and_links() {
        let (mut h, _) = contact_chat((1200., 800.));
        h.state_mut().state.info_open = true;
        h.run();
        let copied = |h: &mut Harness<'static, MimirDesktop>, i: usize| {
            h.get_all_by_label("Копировать").nth(i).unwrap().click();
            h.step(); // the copy command is in this frame's output
            h.output().platform_output.commands.iter().find_map(|c| match c {
                eframe::egui::OutputCommand::CopyText(t) => Some(t.clone()),
                _ => None,
            })
        };
        let hex = "07".repeat(32);
        assert_eq!(copied(&mut h, 0), Some(hex.clone()));
        assert!(copied(&mut h, 1).is_some_and(|t| t.starts_with("mimir://") && t.contains(&hex)));
        assert_eq!(copied(&mut h, 2), Some(format!("https://go.mimir.im/u/{hex}")));
    }

    #[test]
    fn saved_has_only_clear_history() {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        h.state_mut().state.selected = Some(ChatKey::SAVED);
        h.state_mut().state.info_open = true;
        h.run();
        assert!(h.query_by_label("Публичный ключ").is_none());
        h.get_by_label("⋮").click();
        h.run();
        assert!(h.query_by_label("Очистить историю").is_some());
        for gone in ["Переименовать", "Без звука", "Заблокировать", "Удалить контакт"] {
            assert!(h.query_by_label(gone).is_none(), "{gone}");
        }
    }

    #[test]
    fn mute_and_block_persist() {
        let (mut h, c) = contact_chat((1200., 700.));
        h.state_mut().state.info_open = true;
        h.run();
        h.get_by_label("Без звука").click();
        h.run();
        assert!(h.state().core.get_contact(c).unwrap().muted);
        assert!(h.query_by_label("Включить звук").is_some());
        h.get_by_label("Заблокировать").click();
        h.run();
        assert!(h.state().core.is_contact_blocked(vec![7; 32]));
        h.get_by_label("Разблокировать").click();
        h.run();
        assert!(!h.state().core.is_contact_blocked(vec![7; 32]));
    }

    #[test]
    fn menu_rename_updates_list() {
        let (mut h, c) = contact_chat((1200., 700.));
        h.get_by_label("⋮").click();
        h.run();
        h.get_by_label("Переименовать").click();
        h.run();
        assert!(h.get_by_label("Новое имя:").is_focused());
        h.get_by_label("Новое имя:").type_text("Боря");
        h.run();
        h.get_by_label("OK").click();
        h.run();
        assert_eq!(h.state().core.get_contact(c).unwrap().name, "Боря", "the selected old name is replaced");
        assert!(h.state().state.chats.iter().any(|r| r.name == "Боря"));
    }

    #[test]
    fn unchanged_rename_keeps_the_peer_name_in_charge() {
        // A nameless contact shows its key prefix, but the field starts from the stored (empty) name,
        // and OK without a change must not mark the contact as locally renamed.
        for stored in ["", "Аня"] {
            let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
            let core = h.state().core.clone();
            let c = core.add_contact(vec![7; 32], stored.into()).unwrap();
            h.state_mut().state.chats = build_chat_list(&core, crate::i18n::Lang::Ru);
            h.state_mut().state.selected = Some(ChatKey::Contact(c));
            h.run();
            h.get_by_label("⋮").click();
            h.run();
            h.get_by_label("Переименовать").click();
            h.run();
            assert_eq!(h.get_by_label("Новое имя:").value().as_deref(), Some(stored), "prefill");
            h.get_by_label("OK").click();
            h.run();
            let after = core.get_contact(c).unwrap();
            assert!(!after.renamed, "{stored:?}: not pinned by an unchanged OK");
            assert_eq!(after.name, stored);
        }
    }

    #[test]
    fn clear_history_confirms_then_empties() {
        let (mut h, c) = contact_chat((1200., 700.));
        h.state().core.add_local_message(c, 1, 0, true, true, 1_700_000_000_000, 0, 0, b"hello".to_vec()).unwrap();
        h.state_mut().state.open_chat = None;
        h.run();
        assert!(h.query_by_label("hello").is_some());
        h.get_by_label("⋮").click();
        h.run();
        h.get_by_label("Очистить историю").click();
        h.run();
        assert!(h.query_by_label("hello").is_some(), "not before confirm");
        let p = crate::ui::theme::palette(true);
        assert_eq!(crate::test_support::button_fill(&h, "OK"), Some(p.danger), "destructive confirm is red");
        assert_ne!(crate::test_support::button_fill(&h, "Отмена"), Some(p.accent), "dismiss is plain");
        h.get_by_label("OK").click();
        h.run();
        assert!(h.query_by_label("hello").is_none());
        assert_eq!(h.state().state.selected, Some(ChatKey::Contact(c)));
    }

    #[test]
    fn clear_history_deletes_attachment_files_and_posters() {
        let (mut h, c) = contact_chat((1200., 700.));
        let files = h.state().paths.files_dir.clone();
        std::fs::create_dir_all(files.join(".thumbs")).unwrap();
        let (file, poster) = (files.join("v.mp4"), files.join(".thumbs/v.mp4.jpg"));
        std::fs::write(&file, b"x").unwrap();
        std::fs::write(&poster, b"x").unwrap();
        h.state().core.add_local_message(c, 1, 0, true, true, 1_700_000_000_000, 0, 1, br#"{"name":"v.mp4"}"#.to_vec()).unwrap();
        h.get_by_label("⋮").click();
        h.run();
        h.get_by_label("Очистить историю").click();
        h.run();
        h.get_by_label("OK").click();
        crate::test_support::run_until(&mut h, "files deleted", |_| !file.exists() && !poster.exists());
    }

    #[test]
    fn remove_contact_closes_chat() {
        let (mut h, c) = contact_chat((1200., 700.));
        h.get_by_label("⋮").click();
        h.run();
        h.get_by_label("Удалить контакт").click();
        h.run();
        assert!(h.state().core.get_contact(c).is_some(), "not before confirm");
        assert_eq!(crate::test_support::button_fill(&h, "OK"), Some(crate::ui::theme::palette(true).danger));
        h.get_by_label("OK").click();
        h.run();
        assert!(h.state().core.get_contact(c).is_none());
        assert_eq!(h.state().state.selected, None);
        assert!(h.state().state.open_chat.is_none());
    }

    #[test]
    fn qr_dialog_opens_and_closes() {
        let (mut h, _) = contact_chat((1200., 700.));
        h.state_mut().state.info_open = true;
        h.run();
        h.get_by_label("QR").click();
        h.run();
        assert!(h.query_by_label("QR-код контакта").is_some());
        h.get_by_label("OK").click();
        h.run();
        assert!(h.query_by_label("QR-код контакта").is_none());
    }
}
