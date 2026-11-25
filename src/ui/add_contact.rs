//! Add-contact dialog (Android AddContactActivity): name, key or link, optional message.
use crate::{
    app::MimirDesktop,
    core::worker::WorkResult,
    i18n::{tr, Key},
    model::{group_link::{parse_group_link, LinkKind}, links::{validate_new_contact, AddContactError}, AddContactDialog},
    ui::join_group,
};
use crate::ui::theme::{dialog, dialog_title, field_label, palette, primary_button};
use eframe::egui::{self, Align, Color32, Id, Layout, TextEdit};

/// A group link in the key field goes to the join dialog, a channel link gets the «next version» notice (AG §1.3).
/// True when the input was a group or channel link.
fn route_group_link(app: &mut MimirDesktop) -> bool {
    let Some(link) = parse_group_link(&app.state.add_dialog.key) else { return false };
    match link.kind {
        LinkKind::Channel => app.state.add_dialog.error = Some(Key::ChannelsNextVersion),
        LinkKind::Group => {
            app.state.add_dialog = AddContactDialog::default();
            app.state.show_add_contact = false;
            join_group::open(app, link);
        }
    }
    true
}

fn submit(app: &mut MimirDesktop) {
    if route_group_link(app) {
        return;
    }
    let d = &mut app.state.add_dialog;
    let (key, link_name) = match validate_new_contact(&app.core, &d.key) {
        Ok(v) => v,
        Err(e) => {
            d.error = Some(match e {
                AddContactError::WrongKey => Key::WrongKey,
                AddContactError::Yourself => Key::Yourself,
                AddContactError::AlreadyAdded => Key::AlreadyAdded,
            });
            return;
        }
    };
    let name = if d.name.trim().is_empty() { link_name.unwrap_or_default() } else { d.name.trim().to_string() };
    if name.is_empty() {
        d.error = Some(Key::EmptyName);
        return;
    }
    let message = d.message.trim().to_string();
    let opened_from = app.state.selected;
    app.state.add_dialog = AddContactDialog::default();
    app.state.show_add_contact = false;
    app.worker.submit(Box::new(move |core| {
        let sent = core.add_contact(key.clone(), name).and_then(|id| core.send_contact_request(key, message).map(|_| id));
        match sent {
            Ok(contact_id) => WorkResult::ContactAdded { contact_id, opened_from },
            Err(e) => WorkResult::Failed { what: "add contact", err: e.to_string() },
        }
    }));
}

pub fn show(app: &mut MimirDesktop, ctx: &egui::Context) {
    if !app.state.show_add_contact {
        return;
    }
    let (lang, started) = (app.lang, app.state.core_started);
    let (mut go, mut cancel, mut edited) = (false, false, false);
    let r = dialog(ctx, Id::unique("add_contact")).show(ctx, |ui| {
        ui.set_width(380.);
        let p = palette(ui.visuals().dark_mode);
        let d = &mut app.state.add_dialog;
        dialog_title(ui, tr(lang, Key::AddContact));
        let l = field_label(ui, &p, tr(lang, Key::ContactName));
        ui.add(TextEdit::singleline(&mut d.name).hint_text(tr(lang, Key::NameHint)).desired_width(f32::INFINITY)).labelled_by(l.id);
        let l = field_label(ui, &p, tr(lang, Key::PublicKeyOrLink));
        let key = ui.add(TextEdit::singleline(&mut d.key).desired_width(f32::INFINITY)).labelled_by(l.id);
        if key.changed() {
            d.error = None;
            edited = true;
        }
        let l = field_label(ui, &p, tr(lang, Key::RequestMessage));
        ui.add(TextEdit::multiline(&mut d.message).hint_text(tr(lang, Key::RequestMessageHint)).desired_rows(3).desired_width(f32::INFINITY))
            .labelled_by(l.id);
        if let Some(e) = d.error {
            ui.colored_label(Color32::from_rgb(0xF4, 0x43, 0x36), tr(lang, e));
        }
        if !started {
            ui.weak(tr(lang, Key::NotStarted));
        }
        ui.add_space(8.);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            go = ui.add_enabled(started, primary_button(&p, tr(lang, Key::AddAndSend))).clicked();
            cancel = ui.button(tr(lang, Key::Cancel)).clicked();
        });
    });
    if go {
        submit(app);
    } else if edited && started {
        route_group_link(app); // a pasted link needs no further click
    } else if cancel || r.should_close() {
        app.state.add_dialog = AddContactDialog::default();
        app.state.show_add_contact = false;
    }
}

#[cfg(test)]
mod tests {
    use crate::model::chat_key::ChatKey;
    use crate::{
        settings::Settings,
        test_support::{start_app, test_harness},
    };
    use egui_kittest::{kittest::Queryable as _, Harness};
    use crate::app::MimirDesktop;
    use std::time::{Duration, Instant};

    const HEX: &str = "f9a4e8d77c34ce192aebc553d9815709c28d35c0a773160508de91a0dac43c22";

    fn open_dialog(started: bool) -> Harness<'static, MimirDesktop> {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        if started {
            let ctx = h.ctx.clone();
            start_app(h.state_mut(), &ctx);
            h.state_mut().state.core_started = true;
        }
        h.state_mut().state.show_add_contact = true;
        h.run();
        h
    }

    fn fill(h: &mut Harness<'static, MimirDesktop>, name: &str, key: &str, msg: &str) {
        let d = &mut h.state_mut().state.add_dialog;
        (d.name, d.key, d.message) = (name.into(), key.into(), msg.into());
        h.run();
    }

    #[test] fn adds_contact_and_sends_request() {
        let mut h = open_dialog(true);
        fill(&mut h, "Боб", &format!("mimir://mm/u/{HEX}/Alice"), "привет");
        h.get_by_label("Добавить контакт и послать запрос").click();
        h.run();
        assert!(!h.state().state.show_add_contact);
        let pk = hex::decode(HEX).unwrap();
        let end = Instant::now() + Duration::from_secs(5);
        while !h.state().state.chats.iter().any(|c| c.name == "Боб") && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
            h.run();
        }
        assert_eq!(h.state().core.get_outgoing_request_status(pk.clone()), 1);
        assert_eq!(h.state().core.get_contact_by_pubkey(pk).unwrap().name, "Боб");
        assert!(h.state().state.chats.iter().any(|c| c.name == "Боб" && c.key != ChatKey::SAVED));
        let id = h.state().core.get_contact_by_pubkey(hex::decode(HEX).unwrap()).unwrap().id;
        assert_eq!(h.state().state.selected, Some(ChatKey::Contact(id)), "the new chat opens");
        h.run();
        assert_eq!(h.state().state.open_chat.as_ref().map(|c| c.key), Some(ChatKey::Contact(id)));
        assert!(h.state().state.chats.iter().any(|c| c.key == ChatKey::Contact(id) && c.request == crate::model::chat_list::Request::Pending && c.preview == "Ожидание…"));
    }

    #[test] fn switching_chat_meanwhile_keeps_the_users_choice() {
        let mut h = open_dialog(true);
        fill(&mut h, "Боб", HEX, "");
        h.get_by_label("Добавить контакт и послать запрос").click();
        h.step(); // the click submits the job; its result is drained no earlier than the next frame
        assert!(!h.state().state.show_add_contact);
        h.state_mut().state.selected = Some(ChatKey::SAVED); // the user moves on while the add is in flight
        let end = Instant::now() + Duration::from_secs(5);
        while !h.state().state.chats.iter().any(|c| c.name == "Боб") && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
            h.run();
        }
        assert!(h.state().state.chats.iter().any(|c| c.name == "Боб"));
        assert_eq!(h.state().state.selected, Some(ChatKey::SAVED), "not taken over by the new chat");
    }

    #[test] fn inline_errors() {
        let mut h = open_dialog(true);
        fill(&mut h, "Боб", "zz", "");
        h.get_by_label("Добавить контакт и послать запрос").click();
        h.run();
        assert!(h.query_by_label("Неверный публичный ключ").is_some());
        let me = hex::encode(h.state().core.public_key());
        fill(&mut h, "Боб", &me, "");
        h.get_by_label("Добавить контакт и послать запрос").click();
        h.run();
        assert!(h.query_by_label("Вы не можете добавить сами себя").is_some());
        assert!(h.state().state.show_add_contact);
    }

    const MED: &str = "000096390fde19f30d3ba042d77f5f5ce0e6c23bd62ff0afc2e6c29a6fe96b33";
    const KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8";

    #[test] fn group_link_closes_dialog_and_opens_join() {
        let mut h = open_dialog(true);
        let (_tx, rx) = std::sync::mpsc::channel();
        h.state_mut().rx = rx; // the lookup job reports to a channel nobody reads
        fill(&mut h, "", &format!("mimir://g/{MED}/123456#{KEY}"), "");
        h.get_by_label("Добавить контакт и послать запрос").click();
        h.step();
        h.step();
        assert!(!h.state().state.show_add_contact);
        let j = h.state().state.join.as_ref().expect("join dialog");
        assert_eq!(j.link.chat_id, 123456);
        assert!(h.query_by_label("Приглашение в группу").is_some());
        assert!(h.state().core.get_contact_by_pubkey(vec![0; 32]).is_none());
    }

    #[test] fn pasted_group_link_routes_without_pressing_add() {
        let mut h = open_dialog(true);
        let (_tx, rx) = std::sync::mpsc::channel();
        h.state_mut().rx = rx;
        h.get_by_label("Публичный ключ или ссылка:").click();
        h.get_by_label("Публичный ключ или ссылка:").type_text(&format!("https://go.mimir.im/g/{MED}/5#{KEY}"));
        h.step();
        h.step();
        assert!(!h.state().state.show_add_contact);
        assert!(h.state().state.join.is_some());
    }

    #[test] fn channel_link_shows_notice() {
        let mut h = open_dialog(true);
        fill(&mut h, "", &format!("mimir://c/{MED}/123456#{KEY}"), "");
        h.get_by_label("Добавить контакт и послать запрос").click();
        h.run();
        assert!(h.state().state.show_add_contact, "stays open");
        assert!(h.state().state.join.is_none());
        assert!(h.query_by_label("Каналы — в следующей версии").is_some());
    }

    #[test] fn disabled_until_core_started() {
        let mut h = open_dialog(false);
        fill(&mut h, "Боб", HEX, "");
        h.get_by_label("Добавить контакт и послать запрос").click();
        h.run();
        assert!(h.state().state.show_add_contact, "stays open");
        assert!(h.query_by_label("Сеть ещё не запущена, подождите").is_some());
        assert!(h.state().core.get_contact_by_pubkey(hex::decode(HEX).unwrap()).is_none());
    }
}
