//! OS notifications for new messages, group invites and incoming calls, with Android's `message_alert` sound.
//! Decided on the core's callback thread (`core::events::Bridge`): a hidden window may run no frames.
use crate::{
    core::events::Event,
    i18n::{tr, Key, Lang},
    model::{chat_key::ChatKey, chat_list::display_name, group::SYSTEM, message::{decode, preview}, timeline::REACTION},
};
use eframe::egui;
use mimir::MimirApp;
use notify_rust::NotificationResponse;
use std::{sync::{atomic::{AtomicU64, Ordering}, mpsc::Sender}, time::{Duration, Instant, SystemTime, UNIX_EPOCH}};

/// Android's message alert (PCM WAV).
const ALERT: &[u8] = include_bytes!("../../assets/sounds/message_alert.wav");

/// Our Windows AppUserModelID (`platform::register_app_id` gives it the name «Mimir» and the icon).
pub const APP_ID: &str = "Mimir.Desktop";

/// A burst of notices (a reconnect delivering queued messages) plays the alert once.
const ALERT_GAP_MS: u64 = 1500;
/// A caller who keeps ringing is announced once a minute.
const RING_GAP: Duration = Duration::from_secs(60);

/// When the alert last played (ms since the epoch).
static LAST_ALERT: AtomicU64 = AtomicU64::new(0);

/// What the window shows, written by the app on each frame (and `focused = false` when it hides).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Focus {
    pub chat: Option<ChatKey>,
    pub focused: bool,
    /// The UI language, for the notification texts.
    pub lang: Lang,
}

/// Title and body of the notification `ev` calls for, if any (Android rules: not for the open chat while the window
/// has focus, muted chats, our own or already read messages (a sibling's catch-up brings rows read on the phone),
/// reactions, call records or system rows). A call ignores mute: it is urgent.
pub fn decide(core: &MimirApp, ev: &Event, focus: &Focus) -> Option<(String, String)> {
    let lang = focus.lang;
    let open = |key: ChatKey| focus.focused && focus.chat == Some(key);
    match ev {
        Event::MessageReceived { contact_id, message_id, msg_type, .. } => {
            // 2: a call record.
            if matches!(*msg_type, 2 | REACTION) || open(ChatKey::Contact(*contact_id)) {
                return None;
            }
            let c = core.get_contact(*contact_id).filter(|c| !c.muted)?;
            let m = core.get_message(*message_id).filter(|m| m.incoming && !m.read)?;
            Some((display_name(&c.name, &c.pubkey), preview(&decode(m.msg_type, m.data.as_deref()), lang)))
        }
        Event::GroupMessageReceived { chat_id, message_id, msg_type, .. } => {
            if matches!(*msg_type, REACTION | SYSTEM) || open(ChatKey::Group(*chat_id)) {
                return None;
            }
            let g = core.get_group_chat(*chat_id).filter(|g| !g.muted)?;
            let m = core.get_group_message(*chat_id, *message_id).filter(|m| m.incoming && !m.read)?;
            // The mediator echoes our own messages back.
            if core.get_member_pubkey(*chat_id, m.contact_id) == Some(core.public_key()) {
                return None;
            }
            let name = if g.name.trim().is_empty() { tr(lang, Key::UnknownNickname).to_string() } else { g.name };
            Some((name, preview(&decode(m.msg_type, m.data.as_deref()), lang)))
        }
        Event::GroupInviteReceived { invite_id, .. } => {
            let inv = core.get_pending_invites().into_iter().find(|i| i.id == *invite_id)?;
            let inviter = contact_name(core, &inv.from_pubkey);
            Some((tr(lang, Key::GroupJoinTitle).to_string(), format!("{} — {inviter}", inv.chat_name)))
        }
        Event::IncomingCall { pubkey } => {
            let contact = core.get_contact_by_pubkey(pubkey.clone());
            if contact.as_ref().is_some_and(|c| open(ChatKey::Contact(c.id))) {
                return None;
            }
            Some((contact_name(core, pubkey), tr(lang, Key::IncomingCall).to_string()))
        }
        _ => None,
    }
}

fn contact_name(core: &MimirApp, pubkey: &[u8]) -> String {
    let name = core.get_contact_by_pubkey(pubkey.to_vec()).map(|c| c.name).unwrap_or_default();
    display_name(&name, pubkey)
}

/// Whether `pubkey` ringing at `now` is news: not the same caller as `last` within `RING_GAP` (then it becomes `last`).
pub fn new_ring(last: &mut Option<(Vec<u8>, Instant)>, pubkey: &[u8], now: Instant) -> bool {
    let repeat = last.as_ref().is_some_and(|(k, at)| k == pubkey && now.duration_since(*at) < RING_GAP);
    if !repeat {
        *last = Some((pubkey.to_vec(), now));
    }
    !repeat
}

/// Whether the alert may play at `now_ms`: none played in the last `ALERT_GAP_MS` (then `now_ms` is recorded).
fn claim_alert(last: &AtomicU64, now_ms: u64) -> bool {
    let prev = last.load(Ordering::Relaxed);
    now_ms.saturating_sub(prev) >= ALERT_GAP_MS && last.compare_exchange(prev, now_ms, Ordering::Relaxed, Ordering::Relaxed).is_ok()
}

/// The toast and the alert sound, on a thread of their own (both can block). The thread then waits for the toast to be
/// clicked (the window comes back) or closed. ponytail: a toast the OS never reports closed keeps its thread.
pub fn show(title: String, body: String, tx: Sender<Event>, ctx: egui::Context) {
    std::thread::spawn(move || {
        let mut n = notify_rust::Notification::new();
        n.appname("Mimir").summary(&title).body(&body);
        #[cfg(windows)]
        n.app_id(APP_ID);
        // XDG servers report a body click only for a notification with a "default" action (on Windows it is a button).
        #[cfg(target_os = "linux")]
        n.action("default", "default");
        let shown = n.show();
        if let Err(e) = &shown {
            tracing::warn!("notification failed: {e}");
        }
        let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
        if claim_alert(&LAST_ALERT, now_ms) {
            play_alert();
        }
        if let Ok(handle) = shown {
            let _ = handle.wait_for_response(|r: &NotificationResponse| clicked(r, &tx, &ctx));
        }
    });
}

/// A click on the toast (its body or a button) brings the window back, as the tray icon does.
fn clicked(r: &NotificationResponse, tx: &Sender<Event>, ctx: &egui::Context) {
    if matches!(r, NotificationResponse::Default | NotificationResponse::Action(_)) {
        crate::platform::tray::show_from_tray(tx, ctx);
    }
}

/// Plays `ALERT` on a short-lived output stream (the player's stream lives on its own thread).
fn play_alert() {
    let Ok((_stream, out)) = rodio::OutputStream::try_default() else { return };
    let Ok(sink) = rodio::Sink::try_new(&out) else { return };
    match crate::media::player::embedded(ALERT, "wav") {
        Ok(sound) => {
            sink.append(sound);
            sink.sleep_until_end();
        }
        Err(e) => tracing::warn!("alert sound: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{model::message::reaction_payload, test_support::test_app};

    const FOCUSED: Focus = Focus { chat: None, focused: true, lang: Lang::Ru };

    fn msg(contact_id: i64, message_id: i64, msg_type: i32) -> Event {
        Event::MessageReceived { contact_id, message_id, msg_type, reply_to: 0 }
    }

    #[test]
    fn alert_throttled_in_bursts() {
        let last = AtomicU64::new(0);
        assert!(claim_alert(&last, 10_000));
        assert!(!claim_alert(&last, 11_000), "within 1.5 s");
        assert!(claim_alert(&last, 11_500));
    }

    #[test]
    fn repeated_ring_announced_once_a_minute() {
        let (mut last, t) = (None, Instant::now());
        assert!(new_ring(&mut last, &[7; 32], t));
        assert!(!new_ring(&mut last, &[7; 32], t + Duration::from_secs(30)), "same caller");
        assert!(new_ring(&mut last, &[8; 32], t + Duration::from_secs(31)), "another caller");
        assert!(new_ring(&mut last, &[8; 32], t + Duration::from_secs(92)), "a minute later");
    }

    #[test]
    fn click_brings_the_window_back() {
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = egui::Context::default();
        clicked(&NotificationResponse::Closed(notify_rust::CloseReason::Expired), &tx, &ctx);
        assert!(rx.try_recv().is_err(), "an expired toast does nothing");
        clicked(&NotificationResponse::Default, &tx, &ctx);
        assert!(matches!(rx.try_recv(), Ok(Event::External(crate::platform::Ext::Show))));
    }

    #[test]
    fn alert_sound_decodes() {
        use rodio::Source as _;
        let sound = crate::media::player::embedded(ALERT, "wav").unwrap();
        assert_eq!((sound.channels(), sound.sample_rate()), (1, 44100));
    }

    #[test]
    fn notification_rules() {
        let (_t, core) = test_app();
        let c = core.add_contact(vec![7; 32], "Аня".into()).unwrap();
        let hi = core.add_local_message(c, 1, 0, true, false, 1000, 0, 0, "привет".as_bytes().to_vec()).unwrap();
        let ev = msg(c, hi, 0);
        let open = |focused| Focus { chat: Some(ChatKey::Contact(c)), focused, lang: Lang::Ru };
        let some = |t: &str, b: &str| Some((t.to_string(), b.to_string()));
        assert_eq!(decide(&core, &ev, &open(true)), None, "open chat, focused window");
        assert_eq!(decide(&core, &ev, &open(false)), some("Аня", "привет"), "open chat, window away");
        assert_eq!(decide(&core, &ev, &FOCUSED), some("Аня", "привет"), "another chat open");
        let mine = core.add_local_message(c, 2, 0, false, false, 1001, 0, 0, b"my".to_vec()).unwrap();
        assert_eq!(decide(&core, &msg(c, mine, 0), &FOCUSED), None, "our own message");
        let r = core.add_local_message(c, 3, 1, true, false, 1002, 0, REACTION, reaction_payload("👍", 1)).unwrap();
        assert_eq!(decide(&core, &msg(c, r, REACTION), &FOCUSED), None, "reaction");
        let call = core.add_local_message(c, 4, 0, true, false, 1003, 0, 2, b"{}".to_vec()).unwrap();
        assert_eq!(decide(&core, &msg(c, call, 2), &FOCUSED), None, "call record");
        let pic = core.add_local_message(c, 5, 0, true, false, 1004, 0, 1, r#"{"name":"h","text":"кот"}"#.as_bytes().to_vec()).unwrap();
        assert_eq!(decide(&core, &msg(c, pic, 1), &FOCUSED), some("Аня", "🖼️ кот"), "the chat list's preview");
        core.set_contact_muted(c, true).unwrap();
        assert_eq!(decide(&core, &ev, &FOCUSED), None, "muted contact");
        core.set_contact_muted(c, false).unwrap();
        core.mark_all_read(c).unwrap();
        assert_eq!(decide(&core, &ev, &FOCUSED), None, "already read (on the phone)");

        core.save_group_chat(7, "Семья".into(), String::new(), None, vec![9; 32], vec![], vec![3; 32], 0).unwrap();
        let theirs = core.add_group_message(7, 1, 11, vec![5; 32], 2000, 0, false, b"hi".to_vec(), 0, false).unwrap();
        let echo = core.add_group_message(7, 2, 12, core.public_key(), 2001, 0, false, b"me".to_vec(), 0, false).unwrap();
        let group = |message_id, msg_type| Event::GroupMessageReceived { chat_id: 7, message_id, msg_type, reply_to: 0 };
        assert_eq!(decide(&core, &group(theirs, 0), &FOCUSED), some("Семья", "hi"));
        assert_eq!(decide(&core, &group(echo, 0), &FOCUSED), None, "own group echo");
        assert_eq!(decide(&core, &group(theirs, SYSTEM), &FOCUSED), None, "system row");
        assert_eq!(decide(&core, &group(theirs, 0), &Focus { chat: Some(ChatKey::Group(7)), ..FOCUSED }), None, "open group");
        core.set_group_chat_muted(7, true).unwrap();
        assert_eq!(decide(&core, &group(theirs, 0), &FOCUSED), None, "muted group");
        core.set_group_chat_muted(7, false).unwrap();
        core.mark_all_group_read(7).unwrap();
        assert_eq!(decide(&core, &group(theirs, 0), &FOCUSED), None, "group message already read");

        core.save_group_invite(8, 42, vec![7; 32], 3000, "Дача".into(), String::new(), None, vec![], vec![9; 32], 0).unwrap();
        let invite = Event::GroupInviteReceived { invite_id: 42, chat_id: 8, from_pubkey: vec![7; 32] };
        assert_eq!(decide(&core, &invite, &FOCUSED), some("Приглашение в группу", "Дача — Аня"));
        let call = Event::IncomingCall { pubkey: vec![7; 32] };
        assert_eq!(decide(&core, &call, &FOCUSED), some("Аня", "Входящий звонок — ответьте на телефоне"));
        assert_eq!(decide(&core, &call, &open(true)), None, "the chat shows the banner instead");
        core.set_contact_muted(c, true).unwrap();
        assert_eq!(decide(&core, &call, &FOCUSED), some("Аня", "Входящий звонок — ответьте на телефоне"), "a call ignores mute");
        let stranger = Event::IncomingCall { pubkey: vec![0xAB; 32] };
        assert_eq!(decide(&core, &stranger, &Focus { lang: Lang::En, ..FOCUSED }), some("abababab", "Incoming call — answer on your phone"));
        assert_eq!(decide(&core, &Event::ContactConnected { pubkey: vec![7; 32] }, &FOCUSED), None);
    }
}
