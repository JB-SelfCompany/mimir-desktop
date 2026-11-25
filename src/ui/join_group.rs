//! Join a public group by link (Android ChannelJoinActivity): look the group up, show it, join on confirm.
//! Both calls block on the network, so they run on the worker; a failed lookup still allows a blind join.
use crate::{
    app::MimirDesktop,
    core::worker::WorkResult,
    i18n::{tr, trf, Key},
    model::{chat_key::ChatKey, group_link::GroupLink},
    ui::theme::{dialog, dialog_title, palette, primary_button},
};
use eframe::egui::{self, Align, Id, Layout, RichText, Spinner};
use mimir::MimirError;
use std::time::Duration;

/// Android retries 6 times; desktop needs fewer (P2 plan).
const LOOKUP_ATTEMPTS: u32 = 3;
const LOOKUP_PAUSE: Duration = Duration::from_secs(2);

pub struct JoinDialog {
    pub link: GroupLink,
    /// What the mediator told us about the group; `None` until the lookup succeeds.
    pub info: Option<mimir::ChannelInfo>,
    /// A lookup job is in flight.
    pub looking: bool,
    pub joining: bool,
    pub error: Option<String>,
}

/// Opens the dialog and starts the lookup; a group we are already in opens its chat instead.
pub fn open(app: &mut MimirDesktop, link: GroupLink) {
    if let Some(g) = app.core.get_group_chat(link.chat_id) {
        // The core keys groups by chat id alone: the same id on another mediator is not this group.
        if g.mediator_pubkey == link.mediator {
            app.state.selected = Some(ChatKey::Group(link.chat_id));
        } else {
            app.error_text(tr(app.lang, Key::GroupJoinFailed).to_string());
        }
        return;
    }
    app.state.join = Some(JoinDialog { link, info: None, looking: false, joining: false, error: None });
    lookup(app);
}

fn lookup(app: &mut MimirDesktop) {
    let Some(d) = app.state.join.as_mut() else { return };
    d.looking = true;
    let (mediator, chat_id) = (d.link.mediator.to_vec(), d.link.chat_id);
    app.group_worker.submit(Box::new(move |core| {
        for attempt in 1..=LOOKUP_ATTEMPTS {
            match core.preview_public_group(mediator.clone(), chat_id) {
                Ok(info) => return WorkResult::Preview(Some(info)),
                // The mediator may still be connecting.
                Err(MimirError::Connection(_)) if attempt < LOOKUP_ATTEMPTS => std::thread::sleep(LOOKUP_PAUSE),
                Err(_) => break,
            }
        }
        WorkResult::Preview(None)
    }));
}

/// The mediator came up: look again if the lookup had given up.
pub fn retry_lookup(app: &mut MimirDesktop) {
    if app.state.join.as_ref().is_some_and(|d| d.info.is_none() && !d.looking && !d.joining) {
        lookup(app);
    }
}

pub fn preview_done(app: &mut MimirDesktop, info: Option<mimir::ChannelInfo>) {
    let Some(d) = app.state.join.as_mut() else { return };
    if info.as_ref().is_some_and(|i| i.chat_id != d.link.chat_id) {
        return; // a lookup for a dialog that has since been replaced
    }
    d.looking = false;
    d.info = info;
}

pub fn joined(app: &mut MimirDesktop, chat_id: i64) {
    app.state.join = None;
    app.state.selected = Some(ChatKey::Group(chat_id));
}

pub fn join_failed(app: &mut MimirDesktop, err: &str) {
    let lang = app.lang;
    if let Some(d) = app.state.join.as_mut() {
        d.joining = false;
        d.error = Some(format!("{}: {err}", tr(lang, Key::GroupJoinFailed)));
    }
}

fn join(app: &mut MimirDesktop) {
    let Some(d) = app.state.join.as_mut() else { return };
    d.joining = true;
    d.error = None;
    let (mediator, chat_id, key) = (d.link.mediator.to_vec(), d.link.chat_id, d.link.key.to_vec());
    app.group_worker.submit(Box::new(move |core| match core.join_public_group(mediator, chat_id, key) {
        Ok(chat_id) => WorkResult::GroupJoined { chat_id },
        Err(e) => WorkResult::Failed { what: "join_public_group", err: e.to_string() },
    }));
}

pub fn show(app: &mut MimirDesktop, ctx: &egui::Context) {
    if app.state.join.is_none() {
        return;
    }
    let (lang, started) = (app.lang, app.state.core_started);
    let (mut go, mut cancel) = (false, false);
    let r = dialog(ctx, Id::unique("join_group")).show(ctx, |ui| {
        ui.set_width(420.);
        let p = palette(ui.visuals().dark_mode);
        let Some(d) = app.state.join.as_ref() else { return };
        dialog_title(ui, tr(lang, Key::GroupJoinTitle));
        if d.looking {
            ui.horizontal(|ui| {
                ui.add(Spinner::new());
                ui.label(tr(lang, Key::GroupLookingUp));
            });
        }
        if let Some(i) = &d.info {
            ui.label(RichText::new(&i.name).size(18.).strong());
            if !i.description.is_empty() {
                ui.label(&i.description);
            }
            ui.label(RichText::new(trf(lang, Key::MembersOnly, &[&i.subscribers.to_string()])).size(12.).color(p.text_dim));
        } else if !d.looking {
            ui.colored_label(p.danger, tr(lang, Key::GroupLookupFailed));
        }
        ui.add_space(6.);
        let mediator: String = hex::encode_upper(d.link.mediator).chars().take(8).collect();
        ui.label(RichText::new(trf(lang, Key::ChannelJoinMediator, &[&mediator])).size(12.).color(p.text_dim));
        ui.add_space(6.);
        ui.label(tr(lang, Key::GroupJoinPrompt));
        ui.label(RichText::new(tr(lang, Key::GroupJoinWarning)).size(12.).color(p.text_dim));
        if let Some(e) = &d.error {
            ui.colored_label(p.danger, e);
        }
        ui.add_space(8.);
        if d.joining {
            ui.horizontal(|ui| {
                ui.add(Spinner::new());
                ui.label(tr(lang, Key::GroupJoining));
            });
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            go = ui.add_enabled(started && !d.joining, primary_button(&p, tr(lang, Key::GroupJoinAction))).clicked();
            cancel = ui.add_enabled(!d.joining, egui::Button::new(tr(lang, Key::Cancel))).clicked();
        });
    });
    let joining = app.state.join.as_ref().is_some_and(|d| d.joining);
    if go {
        join(app);
    } else if cancel || (r.should_close() && !joining) {
        app.state.join = None;
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn same_id_on_another_mediator_is_not_opened() {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        let core = h.state().core.clone();
        core.save_group_chat(42, "Чужая".into(), String::new(), None, vec![0xCD; 32], vec![], vec![3; 32], 0).unwrap();
        open(h.state_mut(), link());
        let s = &h.state().state;
        assert_eq!((s.selected, s.join.is_none(), s.status_ok), (None, true, false));
        assert_eq!(s.status.as_ref().map(|s| s.0.as_str()), Some("Не удалось войти в группу"));
        core.save_group_chat(43, "Дача".into(), String::new(), None, MED.to_vec(), vec![], vec![3; 32], 0).unwrap();
        open(h.state_mut(), GroupLink { chat_id: 43, ..link() });
        assert_eq!(h.state().state.selected, Some(ChatKey::Group(43)), "ours: opened");
    }

    use super::*;
    use crate::{core::events::Event, model::{chat_key::ChatKey, group_link::LinkKind}, settings::Settings, test_support::{start_app, test_harness}};
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
    const MED: [u8; 32] = [0xAB; 32];

    fn link() -> GroupLink {
        GroupLink { kind: LinkKind::Group, mediator: MED, chat_id: 42, key: [5; 32] }
    }

    fn info() -> mimir::ChannelInfo {
        mimir::ChannelInfo { chat_id: 42, name: "Дача".into(), description: "Про дачу".into(), avatar: None, owner_pubkey: vec![1; 32], flags: 0, subscribers: 5 }
    }

    /// Opens the dialog on a started app; core jobs report to a channel nobody reads, `tx` plays the worker.
    fn open_dialog() -> (H, Sender<Event>) {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        let ctx = h.ctx.clone();
        start_app(h.state_mut(), &ctx);
        h.state_mut().state.core_started = true;
        let (tx, rx) = std::sync::mpsc::channel();
        h.state_mut().rx = rx;
        open(h.state_mut(), link());
        frames(&mut h);
        (h, tx)
    }

    fn done(h: &mut H, tx: &Sender<Event>, r: WorkResult) {
        tx.send(Event::WorkDone(r)).unwrap();
        h.step();
        frames(h);
    }

    #[test]
    fn looks_up_then_shows_the_group() {
        let (mut h, tx) = open_dialog();
        assert!(h.query_by_label("Приглашение в группу").is_some());
        assert!(h.query_by_label("Ищем группу…").is_some());
        assert!(h.query_by_label("Медиатор: ABABABAB").is_some());
        assert!(h.query_by_label("Войти в эту группу?").is_some());
        assert!(h.query_by_label_contains("При входе вы регистрируетесь на медиаторе этой группы").is_some());
        done(&mut h, &tx, WorkResult::Preview(Some(info())));
        assert!(h.query_by_label("Ищем группу…").is_none());
        assert!(h.query_by_label("Дача").is_some());
        assert!(h.query_by_label("Про дачу").is_some());
        assert!(h.query_by_label("5 участников").is_some());
    }

    #[test]
    fn lookup_failure_still_allows_joining() {
        let (mut h, tx) = open_dialog();
        done(&mut h, &tx, WorkResult::Preview(None));
        assert!(h.query_by_label("Ищем группу…").is_none());
        assert!(h.query_by_label("Не удалось связаться с медиатором, чтобы посмотреть группу. Войти всё равно можно.").is_some());
        assert!(!h.get_by_role_and_label(Role::Button, "Войти").accesskit_node().is_disabled());
    }

    #[test]
    fn join_shows_progress_then_opens_chat() {
        let (mut h, tx) = open_dialog();
        h.get_by_role_and_label(Role::Button, "Войти").click();
        h.step();
        frames(&mut h);
        assert!(h.state().state.join.as_ref().unwrap().joining);
        assert!(h.query_by_label("Входим…").is_some());
        assert!(h.get_by_role_and_label(Role::Button, "Войти").accesskit_node().is_disabled());
        h.state().core.save_group_chat(42, "Дача".into(), String::new(), None, MED.to_vec(), vec![], vec![5; 32], 0).unwrap();
        done(&mut h, &tx, WorkResult::GroupJoined { chat_id: 42 });
        assert!(h.state().state.join.is_none());
        assert_eq!(h.state().state.selected, Some(ChatKey::Group(42)));
        assert!(h.state().state.chats.iter().any(|c| c.key == ChatKey::Group(42)));
    }

    #[test]
    fn join_failure_shows_error_and_reenables() {
        let (mut h, tx) = open_dialog();
        h.get_by_role_and_label(Role::Button, "Войти").click();
        h.step();
        done(&mut h, &tx, WorkResult::Failed { what: "join_public_group", err: "no route".into() });
        assert!(h.query_by_label_contains("Не удалось войти в группу: no route").is_some());
        assert!(!h.state().state.join.as_ref().unwrap().joining);
        assert!(h.query_by_label("Входим…").is_none());
        assert!(!h.get_by_role_and_label(Role::Button, "Войти").accesskit_node().is_disabled());
    }

    #[test]
    fn already_joined_opens_chat_directly() {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        h.state().core.save_group_chat(42, "Дача".into(), String::new(), None, MED.to_vec(), vec![], vec![5; 32], 0).unwrap();
        let lang = h.state().lang;
        h.state_mut().state.chats = crate::model::chat_list::build_chat_list(&h.state().core, lang);
        open(h.state_mut(), link());
        frames(&mut h);
        assert!(h.state().state.join.is_none());
        assert_eq!(h.state().state.selected, Some(ChatKey::Group(42)));
    }

    #[test]
    fn cancel_closes() {
        let (mut h, _tx) = open_dialog();
        h.get_by_role_and_label(Role::Button, "Отмена").click();
        frames(&mut h);
        assert!(h.state().state.join.is_none());
    }
}
