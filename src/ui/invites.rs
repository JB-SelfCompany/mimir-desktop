//! Group invites dialog (Android InviteListActivity + GroupInviteActivity): accept or decline each invite.
//! Accepting syncs history and joins on the mediator, so it runs on the worker; declining only deletes the row.
use crate::{
    app::MimirDesktop,
    core::worker::WorkResult,
    i18n::{tr, trf, Key},
    model::{chat_key::ChatKey, chat_list::display_name},
    media::avatar::resolve,
    ui::{avatar::{self, avatar_seed_color}, theme::{dialog, dialog_title, palette, primary_button}},
};
use eframe::egui::{self, Align, Button, Id, Layout, RichText, ScrollArea, Spinner};
use std::collections::HashMap;

#[derive(Default)]
pub struct InvitesUi {
    /// Invite being accepted; the whole list is locked meanwhile.
    pub busy: Option<i64>,
    pub error: Option<String>,
    /// Inviter name per pubkey (contact name, else 8 hex), read when the list is refreshed.
    inviters: HashMap<Vec<u8>, String>,
}

/// Re-reads the pending invites and mediator invitations (storage-only) and their inviters' names.
pub fn refresh(app: &mut MimirDesktop) {
    crate::ui::mediators::refresh_invitations(app);
    app.state.invites = app.core.get_pending_invites();
    let ui = &mut app.state.invites_ui;
    ui.inviters.clear();
    for i in &app.state.invites {
        let name = app.core.get_contact_by_pubkey(i.from_pubkey.clone()).map(|c| c.name).unwrap_or_default();
        ui.inviters.insert(i.from_pubkey.clone(), display_name(&name, &i.from_pubkey));
    }
}

pub fn open(app: &mut MimirDesktop) {
    refresh(app);
    app.state.invites_ui.error = None;
    app.state.show_invites = true;
}

/// Group invites plus invitations to mediators we do not use yet (Android notifies those; not done here).
pub fn count(app: &MimirDesktop) -> usize {
    app.state.invites.len() + app.state.mediators_ui.invitations.len()
}

/// The banner: the invites dialog, or straight to the mediators dialog when only mediator invitations wait.
pub fn open_banner(app: &mut MimirDesktop) {
    if app.state.invites.is_empty() && !app.state.mediators_ui.invitations.is_empty() {
        crate::ui::mediators::open(app);
    } else {
        open(app);
    }
}

/// A mediator invitation picked in the invites dialog: its dialog in the mediators registry.
fn open_mediator_invitation(app: &mut MimirDesktop, pubkey: Vec<u8>) {
    app.state.show_invites = false;
    crate::ui::mediators::open(app);
    app.state.mediators_ui.invitation = Some(pubkey);
}

fn accept(app: &mut MimirDesktop, id: i64, chat_id: i64) {
    app.state.invites_ui.busy = Some(id);
    app.state.invites_ui.error = None;
    app.group_worker.submit(Box::new(move |core| match core.accept_group_invite(id) {
        Ok(()) => WorkResult::InviteAccepted { chat_id },
        Err(e) => WorkResult::Failed { what: "accept_group_invite", err: e.to_string() },
    }));
}

/// Local only: nothing is sent to the inviter or the mediator.
fn decline(app: &mut MimirDesktop, id: i64) {
    match app.core.delete_invite(id) {
        Ok(()) => {
            refresh(app);
            app.notice(Key::InviteRejected);
        }
        Err(e) => app.state.invites_ui.error = Some(trf(app.lang, Key::ErrorRejectingInvite, &[&e.to_string()])),
    }
}

pub fn accepted(app: &mut MimirDesktop, chat_id: i64) {
    app.state.invites_ui.busy = None;
    app.state.show_invites = false;
    refresh(app);
    app.state.selected = Some(ChatKey::Group(chat_id));
}

pub fn accept_failed(app: &mut MimirDesktop, err: &str) {
    app.state.invites_ui.busy = None;
    app.state.invites_ui.error = Some(trf(app.lang, Key::ErrorAcceptingInvite, &[err]));
    refresh(app); // the core may have dropped the invite
}

pub fn show(app: &mut MimirDesktop, ctx: &egui::Context) {
    if !app.state.show_invites {
        return;
    }
    let (lang, started) = (app.lang, app.state.core_started);
    let mut chosen = None;
    let mut mediator = None;
    let mut close = false;
    let r = dialog(ctx, Id::unique("group_invites")).show(ctx, |ui| {
        ui.set_width(400.);
        let p = palette(ui.visuals().dark_mode);
        let busy = app.state.invites_ui.busy;
        dialog_title(ui, tr(lang, Key::GroupInvites));
        if count(app) == 0 {
            ui.label(tr(lang, Key::NoInvites));
        }
        if !started {
            ui.weak(tr(lang, Key::NotStarted));
        }
        ScrollArea::vertical().max_height(360.).show(ui, |ui| {
            for (inv, name) in &app.state.mediators_ui.invitations {
                ui.separator();
                ui.label(RichText::new(tr(lang, Key::MediatorInvitation)).size(16.).strong());
                let row = Button::new(RichText::new(trf(lang, Key::MediatorInvitationRow, &[name])).color(p.accent)).frame(false);
                if ui.add_enabled(busy.is_none(), row).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    mediator = Some(inv.mediator_pubkey.clone());
                }
            }
            for i in &app.state.invites {
                ui.separator();
                ui.horizontal(|ui| {
                    // 64 px, corner 8, in the invites list.
                    let base = avatar_seed_color(i.chat_id.to_string().as_bytes(), ui.visuals().dark_mode);
                    avatar::show(ui, &mut app.state.thumbs, resolve(&app.paths.core_dir, &i.chat_avatar).as_deref(), base, 64., 4.);
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&i.chat_name).size(16.).strong());
                        if !i.chat_description.is_empty() {
                            ui.label(&i.chat_description);
                        }
                        let from = app.state.invites_ui.inviters.get(&i.from_pubkey).map_or("", String::as_str);
                        ui.label(RichText::new(trf(lang, Key::InvitedBy, &[from])).size(12.).color(p.text_dim));
                    });
                });
                if busy == Some(i.id) {
                    ui.horizontal(|ui| {
                        ui.add(Spinner::new());
                        ui.label(tr(lang, Key::JoiningGroup));
                    });
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add_enabled(started && busy.is_none(), primary_button(&p, tr(lang, Key::EnterChat))).clicked() {
                        chosen = Some((i.id, i.chat_id, true));
                    }
                    if ui.add_enabled(busy.is_none(), Button::new(tr(lang, Key::Decline))).clicked() {
                        chosen = Some((i.id, i.chat_id, false));
                    }
                });
            }
        });
        if let Some(e) = &app.state.invites_ui.error {
            ui.colored_label(p.danger, e);
        }
        ui.add_space(8.);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            close = ui.add_enabled(busy.is_none(), Button::new(tr(lang, Key::Close))).clicked();
        });
    });
    let busy = app.state.invites_ui.busy.is_some();
    if let Some(pk) = mediator {
        return open_mediator_invitation(app, pk);
    }
    match chosen {
        Some((id, chat_id, true)) => accept(app, id, chat_id),
        Some((id, _, false)) => decline(app, id),
        None if close || (r.should_close() && !busy) => app.state.show_invites = false,
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{core::{events::Event, worker::WorkResult}, model::chat_key::ChatKey, settings::Settings, test_support::{start_app, test_harness}};
    use egui_kittest::{kittest::{NodeT as _, Queryable as _}, Harness};
    use eframe::egui::Role;
    use std::sync::mpsc::Sender;

    type H = Harness<'static, MimirDesktop>;

    /// A few frames: a spinner keeps repainting, so `Harness::run` would never settle.
    fn frames(h: &mut H) {
        for _ in 0..3 {
            h.step();
        }
    }
    const FROM: [u8; 32] = [9; 32];

    fn save(h: &H, id: i64, chat_id: i64, name: &str) {
        h.state().core.save_group_invite(chat_id, id, FROM.to_vec(), 100 + id, name.into(), format!("о {name}"), None, vec![1; 8], vec![7; 32], 0).unwrap();
    }

    fn started() -> (H, Sender<Event>) {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        let ctx = h.ctx.clone();
        start_app(h.state_mut(), &ctx);
        h.state_mut().state.core_started = true;
        let (tx, rx) = std::sync::mpsc::channel();
        h.state_mut().rx = rx;
        (h, tx)
    }

    fn received(h: &mut H, tx: &Sender<Event>, invite_id: i64, chat_id: i64) {
        tx.send(Event::GroupInviteReceived { invite_id, chat_id, from_pubkey: FROM.to_vec() }).unwrap();
        h.step();
        frames(h);
    }

    fn open_list(h: &mut H, n: usize) {
        h.get_by_label(&format!("Приглашения в группы: {n}")).click();
        frames(h);
    }

    #[test]
    fn icon_only_while_invites_exist_and_list_refreshes_live() {
        let (mut h, tx) = started();
        frames(&mut h);
        assert!(h.query_by_label_contains("Приглашения в группы").is_none());
        save(&h, 1, 10, "Дача");
        received(&mut h, &tx, 1, 10);
        open_list(&mut h, 1);
        assert!(h.query_by_label("Дача").is_some());
        assert!(h.query_by_label("о Дача").is_some());
        // A second invite arrives while the dialog is open: it shows up without reopening.
        save(&h, 2, 11, "Баня");
        received(&mut h, &tx, 2, 11);
        assert!(h.query_by_label("Баня").is_some());
        assert!(h.query_by_label("Приглашения в группы: 2").is_some());
    }

    #[test]
    fn invited_by_contact_name_or_short_key() {
        let (mut h, tx) = started();
        save(&h, 1, 10, "Дача");
        received(&mut h, &tx, 1, 10);
        open_list(&mut h, 1);
        assert!(h.query_by_label("Приглашение от: 09090909").is_some());
        h.state().core.add_contact(FROM.to_vec(), "Боб".into()).unwrap();
        open(h.state_mut());
        frames(&mut h);
        assert!(h.query_by_label("Приглашение от: Боб").is_some());
    }

    #[test]
    fn mediator_invitation_in_the_banner() {
        const MED: [u8; 32] = [0xAB; 32];
        let (mut h, tx) = started();
        frames(&mut h);
        // The core stores it (peer_handler) before the event; MimirApp has no public writer for it.
        let dir = h.state().paths.core_dir.to_string_lossy().into_owned();
        mimir::storage::MimirStorage::new(dir).unwrap().add_mediator_invitation(MED.to_vec(), FROM.to_vec()).unwrap();
        tx.send(Event::MediatorInvitation { from_pubkey: FROM.to_vec(), mediator_pubkey: MED.to_vec() }).unwrap();
        h.step();
        frames(&mut h);
        h.get_by_label("Приглашения в группы: 1").click();
        frames(&mut h);
        let row = "Приглашение от 09090909 · нажмите, чтобы подключиться";
        assert!(h.state().state.show_mediators && !h.state().state.show_invites, "only a mediator invitation: the mediators dialog");
        assert!(h.query_by_label(row).is_some());
        h.state_mut().state.show_mediators = false;
        // With a group invite as well, both are in the invites dialog; the mediator row opens its own dialog.
        save(&h, 1, 10, "Дача");
        received(&mut h, &tx, 1, 10);
        open_list(&mut h, 2);
        assert!(h.query_by_label("Дача").is_some() && h.query_by_label("Ожидает приглашение в группу").is_some());
        // The dialog is still re-centring on its grown content: let it settle before clicking.
        for _ in 0..10 {
            frames(&mut h);
        }
        h.get_by_label(row).click();
        frames(&mut h);
        assert!(!h.state().state.show_invites);
        assert_eq!(h.state().state.mediators_ui.invitation.as_deref(), Some(&MED[..]));
        assert!(h.query_by_role_and_label(Role::Button, "Подключиться").is_some());
    }

    #[test]
    fn empty_list_message() {
        let (mut h, _tx) = started();
        h.state_mut().state.show_invites = true;
        frames(&mut h);
        assert!(h.query_by_label("Нет ожидающих приглашений").is_some());
    }

    #[test]
    fn decline_deletes_locally() {
        let (mut h, tx) = started();
        save(&h, 1, 10, "Дача");
        save(&h, 2, 11, "Баня");
        received(&mut h, &tx, 1, 10);
        open_list(&mut h, 2);
        h.get_all_by_role_and_label(Role::Button, "Отклонить").next().unwrap().click();
        frames(&mut h);
        let left = h.state().core.get_pending_invites();
        assert_eq!(left.len(), 1);
        assert_eq!(h.state().state.invites.len(), 1);
        assert!(h.query_by_label(&left[0].chat_name).is_some());
        assert_eq!(h.get_all_by_role_and_label(Role::Button, "Отклонить").count(), 1);
    }

    #[test]
    fn accept_shows_progress_then_opens_chat() {
        let (mut h, tx) = started();
        save(&h, 1, 10, "Дача");
        received(&mut h, &tx, 1, 10);
        open_list(&mut h, 1);
        h.get_by_role_and_label(Role::Button, "Войти в чат").click();
        h.step();
        frames(&mut h);
        assert!(h.query_by_label("Синхронизируем сообщения и входим в группу…").is_some());
        assert!(h.get_by_role_and_label(Role::Button, "Отклонить").accesskit_node().is_disabled());
        h.state().core.save_group_chat(10, "Дача".into(), String::new(), None, vec![7; 32], vec![], vec![3; 32], 0).unwrap();
        h.state().core.delete_invite(1).unwrap();
        tx.send(Event::WorkDone(WorkResult::InviteAccepted { chat_id: 10 })).unwrap();
        h.step();
        frames(&mut h);
        assert!(!h.state().state.show_invites);
        assert_eq!(h.state().state.selected, Some(ChatKey::Group(10)));
        assert!(h.state().state.invites.is_empty());
        assert!(h.query_by_label_contains("Приглашения в группы").is_none());
    }

    #[test]
    fn accept_error_reenables_buttons() {
        let (mut h, tx) = started();
        save(&h, 1, 10, "Дача");
        received(&mut h, &tx, 1, 10);
        open_list(&mut h, 1);
        h.get_by_role_and_label(Role::Button, "Войти в чат").click();
        h.step();
        tx.send(Event::WorkDone(WorkResult::Failed { what: "accept_group_invite", err: "boom".into() })).unwrap();
        h.step();
        frames(&mut h);
        assert!(h.query_by_label_contains("Ошибка принятия приглашения: boom").is_some());
        assert!(h.query_by_label("Синхронизируем сообщения и входим в группу…").is_none());
        assert!(!h.get_by_role_and_label(Role::Button, "Войти в чат").accesskit_node().is_disabled());
        assert!(!h.get_by_role_and_label(Role::Button, "Отклонить").accesskit_node().is_disabled());
        assert_eq!(h.state().state.invites.len(), 1);
    }
}
