//! Join a public group or subscribe to a public channel by link (Android ChannelJoinActivity): look the chat up, show
//! it, join on confirm; the link's kind picks the core calls and the wording.
//! Both calls block on the network, so they run on the worker; a failed lookup still allows a blind join.
use crate::{
    app::MimirDesktop,
    core::worker::WorkResult,
    i18n::{tr, trf, Key},
    model::{chat_key::ChatKey, group_link::{GroupLink, LinkKind}},
    ui::theme::{dialog, dialog_title, palette, primary_button},
};
use eframe::egui::{self, Align, Id, Layout, RichText, Spinner};
use mimir::MimirError;
use std::{sync::atomic::{AtomicU64, Ordering}, time::Duration};

/// Android retries 6 times; desktop needs fewer.
const LOOKUP_ATTEMPTS: u32 = 3;
const LOOKUP_PAUSE: Duration = Duration::from_secs(2);

/// The channel bit of a chat's flags (the core's mediator::protocol::CHAT_FLAG_CHANNEL, not exported).
pub const CHAT_FLAG_CHANNEL: u8 = 0x01;

/// Source of the lookup tokens: a result belongs to the dialog that asked only if the tokens match.
static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

pub struct JoinDialog {
    pub link: GroupLink,
    /// The lookup whose result this dialog accepts (a closed or replaced dialog's lookup may still be running).
    pub token: u64,
    /// What the mediator told us about the group; `None` until the lookup succeeds.
    pub info: Option<mimir::ChannelInfo>,
    /// A lookup job is in flight.
    pub looking: bool,
    pub joining: bool,
    pub error: Option<String>,
}

/// The channel wording for a channel link, the group one otherwise.
fn pick(kind: LinkKind, group: Key, channel: Key) -> Key {
    if kind == LinkKind::Channel { channel } else { group }
}

/// Opens the dialog and starts the lookup; a group or channel we are already in opens its chat instead.
pub fn open(app: &mut MimirDesktop, link: GroupLink) {
    if let Some(g) = app.core.get_group_chat(link.chat_id) {
        // The core keys groups by chat id alone: the same id on another mediator is not this group.
        if g.mediator_pubkey == link.mediator {
            app.state.selected = Some(ChatKey::Group(link.chat_id));
        } else {
            app.error_text(tr(app.lang, pick(link.kind, Key::GroupJoinFailed, Key::ChannelJoinFailed)).to_string());
        }
        return;
    }
    app.state.join = Some(JoinDialog { link, token: 0, info: None, looking: false, joining: false, error: None });
    lookup(app);
}

/// Whether a lookup found what the link promised: a channel link's id must be a channel (join_public_channel refuses a
/// group too); a group link takes what preview_public_group gives.
fn as_linked(channel: bool, info: &mimir::ChannelInfo) -> bool {
    !channel || info.flags & CHAT_FLAG_CHANNEL != 0
}

fn lookup(app: &mut MimirDesktop) {
    let Some(d) = app.state.join.as_mut() else { return };
    d.looking = true;
    d.token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
    let token = d.token;
    let (mediator, chat_id, channel) = (d.link.mediator.to_vec(), d.link.chat_id, d.link.kind == LinkKind::Channel);
    app.group_worker.submit(Box::new(move |core| {
        for attempt in 1..=LOOKUP_ATTEMPTS {
            // preview_public_group refuses a channel; a channel link's id must be a channel too (CHAT_FLAG_CHANNEL).
            let found = if channel { core.preview_public_channel(mediator.clone(), chat_id) } else { core.preview_public_group(mediator.clone(), chat_id) };
            match found {
                Ok(info) if as_linked(channel, &info) => return WorkResult::Preview { token, info: Some(info) },
                Ok(_) => break, // a group behind a channel link: no preview (join_public_channel refuses it too)
                // The mediator may still be connecting.
                Err(MimirError::Connection(_)) if attempt < LOOKUP_ATTEMPTS => std::thread::sleep(LOOKUP_PAUSE),
                Err(_) => break,
            }
        }
        WorkResult::Preview { token, info: None }
    }));
}

/// The mediator came up: look again if the lookup had given up.
pub fn retry_lookup(app: &mut MimirDesktop) {
    if app.state.join.as_ref().is_some_and(|d| d.info.is_none() && !d.looking && !d.joining) {
        lookup(app);
    }
}

pub fn preview_done(app: &mut MimirDesktop, token: u64, info: Option<mimir::ChannelInfo>) {
    let Some(d) = app.state.join.as_mut().filter(|d| d.token == token) else { return }; // a closed or replaced dialog's lookup
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
        d.error = Some(format!("{}: {err}", tr(lang, pick(d.link.kind, Key::GroupJoinFailed, Key::ChannelJoinFailed))));
    }
}

fn join(app: &mut MimirDesktop) {
    let Some(d) = app.state.join.as_mut() else { return };
    d.joining = true;
    d.error = None;
    let (mediator, chat_id, key) = (d.link.mediator.to_vec(), d.link.chat_id, d.link.key.to_vec());
    let channel = d.link.kind == LinkKind::Channel;
    app.group_worker.submit(Box::new(move |core| {
        let (joined, what) = if channel {
            (core.join_public_channel(mediator, chat_id, key), "join_public_channel")
        } else {
            (core.join_public_group(mediator, chat_id, key), "join_public_group")
        };
        match joined {
            Ok(chat_id) => WorkResult::GroupJoined { chat_id },
            Err(e) => WorkResult::Failed { what, err: e.to_string() },
        }
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
        let kind = d.link.kind;
        let k = |group, channel| tr(lang, pick(kind, group, channel));
        dialog_title(ui, k(Key::GroupJoinTitle, Key::ChannelJoinTitle));
        if d.looking {
            ui.horizontal(|ui| {
                ui.add(Spinner::new());
                ui.label(k(Key::GroupLookingUp, Key::ChannelLookingUp));
            });
        }
        if let Some(i) = &d.info {
            ui.label(RichText::new(&i.name).size(18.).strong());
            if !i.description.is_empty() {
                ui.label(&i.description);
            }
            let count = pick(kind, Key::MembersOnly, Key::ChannelSubscriberCount);
            ui.label(RichText::new(trf(lang, count, &[&i.subscribers.to_string()])).size(12.).color(p.text_dim));
        } else if !d.looking {
            ui.colored_label(p.danger, k(Key::GroupLookupFailed, Key::ChannelLookupFailed));
        }
        ui.add_space(6.);
        let mediator: String = hex::encode_upper(d.link.mediator).chars().take(8).collect();
        ui.label(RichText::new(trf(lang, Key::ChannelJoinMediator, &[&mediator])).size(12.).color(p.text_dim));
        ui.add_space(6.);
        ui.label(k(Key::GroupJoinPrompt, Key::ChannelJoinPrompt));
        ui.label(RichText::new(k(Key::GroupJoinWarning, Key::ChannelJoinWarning)).size(12.).color(p.text_dim));
        if let Some(e) = &d.error {
            ui.colored_label(p.danger, e);
        }
        ui.add_space(8.);
        if d.joining {
            ui.horizontal(|ui| {
                ui.add(Spinner::new());
                ui.label(k(Key::GroupJoining, Key::ChannelJoining));
            });
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            go = ui.add_enabled(started && !d.joining, primary_button(&p, k(Key::GroupJoinAction, Key::ChannelJoinAction))).clicked();
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

    fn token(h: &H) -> u64 {
        h.state().state.join.as_ref().unwrap().token
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
        let t = token(&h);
        done(&mut h, &tx, WorkResult::Preview { token: t, info: Some(info()) });
        assert!(h.query_by_label("Ищем группу…").is_none());
        assert!(h.query_by_label("Дача").is_some());
        assert!(h.query_by_label("Про дачу").is_some());
        assert!(h.query_by_label("5 участников").is_some());
    }

    #[test]
    fn a_stale_lookup_result_is_ignored() {
        let (mut h, tx) = open_dialog();
        let stale = token(&h) - 1;
        done(&mut h, &tx, WorkResult::Preview { token: stale, info: None });
        assert!(h.state().state.join.as_ref().unwrap().looking, "still waiting for its own lookup");
    }

    #[test]
    fn lookup_failure_still_allows_joining() {
        let (mut h, tx) = open_dialog();
        let t = token(&h);
        done(&mut h, &tx, WorkResult::Preview { token: t, info: None });
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
    fn channel_link_subscribes() {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        let ctx = h.ctx.clone();
        start_app(h.state_mut(), &ctx);
        h.state_mut().state.core_started = true;
        let (tx, rx) = std::sync::mpsc::channel();
        h.state_mut().rx = rx;
        open(h.state_mut(), GroupLink { kind: LinkKind::Channel, ..link() });
        frames(&mut h);
        for text in ["Приглашение в канал", "Ищем канал…", "Подписаться на этот канал?"] {
            assert!(h.query_by_label(text).is_some(), "{text}");
        }
        assert!(h.query_by_label_contains("При подписке вы регистрируетесь на медиаторе этого канала").is_some());
        let t = token(&h);
        done(&mut h, &tx, WorkResult::Preview { token: t, info: Some(mimir::ChannelInfo { flags: 1, subscribers: 40, ..info() }) });
        assert!(h.query_by_label("40 подписчиков").is_some());
        h.get_by_role_and_label(Role::Button, "Подписаться").click();
        h.step();
        frames(&mut h);
        assert!(h.query_by_label("Подписываемся…").is_some());
        done(&mut h, &tx, WorkResult::Failed { what: "join_public_channel", err: "no route".into() });
        assert!(h.query_by_label_contains("Не удалось подписаться на канал: no route").is_some());
        assert!(!h.state().state.join.as_ref().unwrap().joining);
    }

    #[test]
    fn channel_link_to_a_group_is_no_preview() {
        assert!(as_linked(true, &mimir::ChannelInfo { flags: CHAT_FLAG_CHANNEL, ..info() }));
        assert!(as_linked(true, &mimir::ChannelInfo { flags: CHAT_FLAG_CHANNEL | 0x02, ..info() }), "a public channel");
        assert!(!as_linked(true, &info()), "a group behind a channel link");
        assert!(as_linked(false, &info()));
    }

    #[test]
    fn channel_lookup_failure_still_allows_subscribing() {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        let ctx = h.ctx.clone();
        start_app(h.state_mut(), &ctx);
        h.state_mut().state.core_started = true;
        let (tx, rx) = std::sync::mpsc::channel();
        h.state_mut().rx = rx;
        open(h.state_mut(), GroupLink { kind: LinkKind::Channel, ..link() });
        frames(&mut h);
        let t = token(&h);
        done(&mut h, &tx, WorkResult::Preview { token: t, info: None });
        assert!(h.query_by_label("Не удалось связаться с медиатором, чтобы посмотреть канал. Подписаться всё равно можно.").is_some());
        assert!(!h.get_by_role_and_label(Role::Button, "Подписаться").accesskit_node().is_disabled());
    }

    #[test]
    fn cancel_closes() {
        let (mut h, _tx) = open_dialog();
        h.get_by_role_and_label(Role::Button, "Отмена").click();
        frames(&mut h);
        assert!(h.state().state.join.is_none());
    }
}
