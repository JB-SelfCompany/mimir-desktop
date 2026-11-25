//! Outgoing messages and reactions.
use crate::{
    app::MimirDesktop,
    core::worker::{WorkResult, Worker},
    media::stage::{self, Staged},
    model::{chat_key::ChatKey, chat_list::SAVED_ID, message::reaction_payload},
};
use mimir::{AttachmentSpec, MimirApp, MimirError};
#[cfg(test)]
use std::sync::{mpsc::Receiver, Arc};
use time::OffsetDateTime;

#[derive(Clone, Debug)]
pub enum Target {
    Saved { own_pubkey: Vec<u8> },
    Contact { id: i64, pubkey: Vec<u8> },
    Group { chat_id: i64 },
}

impl Target {
    /// Saved Messages is the chat row whose pubkey is our own.
    /// `pubkey` is the chat row's key; a group has none.
    pub fn for_key(key: ChatKey, pubkey: Vec<u8>) -> Target {
        match key {
            ChatKey::Group(chat_id) => Target::Group { chat_id },
            ChatKey::Contact(SAVED_ID) => Target::Saved { own_pubkey: pubkey },
            ChatKey::Contact(id) => Target::Contact { id, pubkey },
        }
    }
    fn key(&self) -> ChatKey {
        match self {
            Target::Saved { .. } => ChatKey::SAVED,
            Target::Contact { id, .. } => ChatKey::Contact(*id),
            Target::Group { chat_id } => ChatKey::Group(*chat_id),
        }
    }
    fn id(&self) -> i64 {
        self.key().id()
    }
    /// A group has no peer key to validate or dial.
    fn pubkey(&self) -> &[u8] {
        match self { Target::Saved { own_pubkey: k } | Target::Contact { pubkey: k, .. } => k, Target::Group { .. } => &[] }
    }
    fn has_valid_key(&self, what: &str) -> bool {
        matches!(self, Target::Group { .. }) || crate::core::valid_key(self.pubkey(), what)
    }
}

fn now_ms() -> i64 {
    // Milliseconds: the newest reaction per sender wins, so a quick toggle must not share a timestamp.
    (OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}

/// Sends a text message off the UI thread; the row shows up via Event::OutgoingMessageStored.
/// The core routes our own pubkey to Saved Messages (-100) without creating a contact.
pub fn send_text(w: &Worker, to: Target, reply_to: i64, text: String) {
    if !to.has_valid_key("send a message") {
        return;
    }
    if let Target::Group { chat_id } = to {
        // Stores the row (OutgoingMessageStored), sends to the mediator, then GroupMessageDelivered.
        // A connection error leaves the row undelivered; the core's resend_undelivered retries it (CG §5).
        w.submit(Box::new(move |app| match app.send_group_chat_message(chat_id, reply_to, text, None, None) {
            Ok(message_id) => WorkResult::Sent { chat: ChatKey::Group(chat_id), message_id },
            Err(MimirError::Connection(_)) => WorkResult::Done,
            Err(e) => WorkResult::Failed { what: "send_group_message", err: e.to_string() },
        }));
        return;
    }
    w.submit(Box::new(move |app| match app.send_contact_message(to.pubkey().to_vec(), reply_to, text, None) {
        Ok(message_id) => WorkResult::Sent { chat: to.key(), message_id },
        // Stored, but nobody to hand it to yet: queued; the core's retry loop re-dials and sends it.
        Err(MimirError::Connection(_)) => {
            let _ = app.connect_to_contact(to.pubkey().to_vec());
            WorkResult::Done
        }
        Err(e) => {
            // Same as Android ChatActivity.sendMessage: a failed send triggers a reconnect attempt.
            let _ = app.connect_to_contact(to.pubkey().to_vec());
            WorkResult::Failed { what: "send_message", err: e.to_string() }
        }
    }));
}

/// Sends a staged file with `caption` off the UI thread (CA §2.2-2.3): 1:1 and Saved Messages on `worker`, a group
/// (file copy, hash, upload, mediator) on `group_worker`. The row shows up via OutgoingMessageStored; the job ends
/// with `WorkResult::AttachmentSent`, which ends the row's upload state. A temp staged file is deleted afterwards.
pub fn send_attachment(app: &mut MimirDesktop, to: Target, reply_to: i64, caption: String, staged: Staged) {
    send_attachment_via(app, to, reply_to, caption, staged, MimirApp::send_group_chat_message);
}

/// The core's group send, a parameter so tests can route a group send without a network.
type GroupSend = fn(&MimirApp, i64, i64, String, Option<AttachmentSpec>, Option<Vec<u8>>) -> Result<i64, MimirError>;

fn send_attachment_via(app: &mut MimirDesktop, to: Target, reply_to: i64, caption: String, staged: Staged, group_send: GroupSend) {
    if !to.has_valid_key("send a file") {
        stage::discard(staged);
        return;
    }
    let chat = to.key();
    *app.state.pending_sends.entry(chat).or_default() += 1;
    let spec = AttachmentSpec {
        file_path: staged.path.to_string_lossy().into_owned(),
        original_name: staged.original_name.clone(),
        mime_type: staged.mime.clone(),
        msg_type: staged.msg_type,
    };
    if let Target::Group { chat_id } = to {
        // An unparsable key is passed as none: the core then refuses the send and says why.
        let server = hex::decode(&app.settings.media.file_server).ok();
        app.group_worker.submit(Box::new(move |core| {
            let r = group_send(core, chat_id, reply_to, caption, Some(spec), server);
            stage::discard(staged);
            WorkResult::AttachmentSent { chat, err: r.err() }
        }));
        return;
    }
    app.worker.submit(Box::new(move |core| {
        let r = core.send_contact_message(to.pubkey().to_vec(), reply_to, caption, Some(spec));
        stage::discard(staged);
        if r.is_err() {
            let _ = core.connect_to_contact(to.pubkey().to_vec()); // queued or failed: redial, as for text
        }
        WorkResult::AttachmentSent { chat, err: r.err() }
    }));
}

/// Guid and payload of a group reaction (type 10); an empty emoji removes ours.
pub fn group_reaction(app: &MimirApp, target_guid: i64, emoji: &str, now: i64) -> (i64, Vec<u8>) {
    let payload = reaction_payload(emoji, target_guid);
    (app.generate_guid(now, payload.clone()), payload)
}

/// Stores the reaction locally (a DB row, done inline) and, for contacts, sends it from the worker.
/// A group reaction is stored and sent by one worker call (`send_group_message`).
// ponytail: a failed local store is dropped silently; the chip just does not appear.
pub fn send_reaction(app: &MimirApp, w: &Worker, to: Target, target_guid: i64, emoji: &str) {
    if !to.has_valid_key("send a reaction") {
        return;
    }
    let now = now_ms();
    if let Target::Group { chat_id } = to {
        let (guid, payload) = group_reaction(app, target_guid, emoji, now);
        w.submit(Box::new(move |app| match app.send_group_message(chat_id, guid, target_guid, now, 10, payload) {
            Ok(_) => WorkResult::Done,
            // Stored before the mediator call, so resend_undelivered retries it; no mediator at all stores nothing.
            Err(MimirError::Connection(_)) if app.get_group_message_by_guid(chat_id, guid).is_some() => WorkResult::Done,
            Err(e) => WorkResult::Failed { what: "send_group_reaction", err: e.to_string() },
        }));
        return;
    }
    let payload = reaction_payload(emoji, target_guid);
    let guid = app.generate_guid(now, payload.clone());
    let saved = matches!(to, Target::Saved { .. });
    if app.add_local_message(to.id(), guid, target_guid, false, saved, now, 0, 10, payload.clone()).is_err() || saved {
        return;
    }
    w.submit(Box::new(move |app| match app.send_message(to.pubkey().to_vec(), guid, target_guid, now, 10, payload) {
        Ok(()) | Err(MimirError::Connection(_)) => WorkResult::Done, // stored; queued like a text message
        Err(e) => WorkResult::Failed { what: "send_reaction", err: e.to_string() },
    }));
}

/// Android parity (AG §1.5): the server delete from the worker, the local delete at once.
/// Returns the row's attachment file name, for the caller to remove from files_dir.
// ponytail: the local row goes even when the server call fails; a failure shows in the status line.
pub fn delete_group_message(app: &MimirApp, w: &Worker, chat_id: i64, guid: i64) -> Option<String> {
    // The server call reads only the chat row (upstream app/mod.rs delete_group_message, ~L788-794), not the message row.
    let file = app.delete_group_message_by_guid(chat_id, guid);
    w.submit(Box::new(move |app| match app.delete_group_message(chat_id, guid) {
        Ok(()) => WorkResult::Done,
        Err(e) => WorkResult::Failed { what: "delete_group_message", err: e.to_string() },
    }));
    file
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{core::events::Event, test_support::{start_offline, test_app}};
    use std::time::{Duration, Instant};

    fn test_worker(app: &Arc<MimirApp>) -> (Worker, Receiver<Event>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = eframe::egui::Context::default();
        start_offline(app, tx.clone(), &ctx);
        (Worker::spawn(app.clone(), tx, ctx), rx)
    }

    /// Next worker result, skipping core events (e.g. OutgoingMessageStored).
    fn work_done(rx: &Receiver<Event>) -> WorkResult {
        work_done_within(rx, Duration::from_secs(5))
    }

    /// A call to a mediator nobody can reach takes the dial timeout (about 10 s) to fail.
    fn mediator_call(rx: &Receiver<Event>) -> WorkResult {
        work_done_within(rx, Duration::from_secs(60))
    }

    fn work_done_within(rx: &Receiver<Event>, limit: Duration) -> WorkResult {
        loop {
            if let Event::WorkDone(r) = rx.recv_timeout(limit).unwrap() { return r; }
        }
    }

    #[test] fn saved_reaction_stays_local() {
        let (_t, app) = test_app();
        let before = app.get_contacts().len();
        let (w, _rx) = test_worker(&app);
        send_reaction(&app, &w, Target::Saved { own_pubkey: app.public_key() }, 1, "👍");
        assert_eq!(app.get_contacts().len(), before);
        assert_eq!(app.get_user_reaction(SAVED_ID, 1).as_deref(), Some("👍"));
    }

    #[test] fn contact_reaction_is_stored_and_sent() {
        let (_t, app) = test_app();
        let c = app.add_contact(vec![7; 32], "Аня".into()).unwrap();
        let (w, rx) = test_worker(&app);
        send_reaction(&app, &w, Target::Contact { id: c, pubkey: vec![7; 32] }, 5, "🔥");
        assert_eq!(app.get_user_reaction(c, 5).as_deref(), Some("🔥"));
        work_done(&rx);
    }

    #[test] fn saved_text_goes_to_saved_chat() {
        let (_t, app) = test_app();
        let (w, rx) = test_worker(&app);
        send_text(&w, Target::Saved { own_pubkey: app.public_key() }, 0, "заметка".into());
        let WorkResult::Sent { chat, message_id } = work_done(&rx) else { panic!() };
        assert_eq!(chat, ChatKey::SAVED);
        assert_eq!(app.get_message(message_id).unwrap().data.as_deref(), Some("заметка".as_bytes()));
        assert!(app.get_contacts().iter().all(|c| c.pubkey != app.public_key()));
    }

    #[test] fn invalid_key_is_never_sent_or_dialled() {
        let (_t, app) = test_app();
        let c = app.add_contact(vec![7; 32], "Аня".into()).unwrap();
        let (w, rx) = test_worker(&app);
        send_text(&w, Target::Contact { id: c, pubkey: vec![] }, 0, "в никуда".into());
        send_reaction(&app, &w, Target::Contact { id: c, pubkey: vec![1; 5] }, 5, "🔥");
        crate::core::net::connect(&w, vec![]);
        let end = Instant::now() + Duration::from_millis(1500);
        while let Some(left) = end.checked_duration_since(Instant::now()) {
            if let Ok(Event::WorkDone(r)) = rx.recv_timeout(left) {
                panic!("a job ran for an invalid key: {r:?}");
            }
        }
        assert_eq!(app.get_user_reaction(c, 5), None);
        assert!(app.get_messages(c, i64::MAX, 10).is_empty());
    }

    fn group(app: &MimirApp) {
        app.save_group_chat(7, "Семья".into(), String::new(), None, vec![9; 32], vec![], vec![3; 32], 0).unwrap();
    }

    #[test] fn group_text_goes_through_send_group_chat_message() {
        let (_t, app) = test_app();
        group(&app);
        let (w, rx) = test_worker(&app);
        send_text(&w, Target::Group { chat_id: 7 }, 0, "привет".into());
        match mediator_call(&rx) {
            WorkResult::Done | WorkResult::Sent { chat: ChatKey::Group(7), .. } => {}
            WorkResult::Failed { what, .. } => assert_eq!(what, "send_group_message"),
            r => panic!("{r:?}"),
        }
    }

    #[test] fn target_for_key() {
        let own = vec![1; 32];
        assert!(matches!(Target::for_key(ChatKey::SAVED, own.clone()), Target::Saved { .. }));
        assert!(matches!(Target::for_key(ChatKey::Contact(3), own.clone()), Target::Contact { id: 3, .. }));
        assert!(matches!(Target::for_key(ChatKey::Group(7), vec![]), Target::Group { chat_id: 7 }));
    }

    #[test] fn group_reaction_payload() {
        let (_t, app) = test_app();
        let (guid, data) = group_reaction(&app, 42, "👍", 1000);
        assert_eq!(data, r#"{"emoji":"👍","replyTo":42}"#.as_bytes());
        assert_eq!(guid, app.generate_guid(1000, data));
        assert_eq!(group_reaction(&app, 42, "", 1000).1, r#"{"emoji":"","replyTo":42}"#.as_bytes(), "empty emoji removes");
    }

    #[test] fn group_reaction_is_sent_from_the_worker() {
        let (_t, app) = test_app();
        group(&app);
        let (w, rx) = test_worker(&app);
        send_reaction(&app, &w, Target::Group { chat_id: 7 }, 42, "👍");
        match mediator_call(&rx) {
            WorkResult::Done => {}
            WorkResult::Failed { what, .. } => assert_eq!(what, "send_group_reaction"),
            r => panic!("{r:?}"),
        }
        send_reaction(&app, &w, Target::Group { chat_id: 7 }, 42, "");
        assert!(matches!(mediator_call(&rx), WorkResult::Done | WorkResult::Failed { what: "send_group_reaction", .. }));
    }

    #[test] fn group_delete_calls_server_and_local() {
        let (_t, app) = test_app();
        group(&app);
        app.add_group_message(7, 1, 11, app.public_key(), 1000, 0, false, "мой".as_bytes().to_vec(), 0, false).unwrap();
        let (w, rx) = test_worker(&app);
        delete_group_message(&app, &w, 7, 11);
        assert!(app.get_group_message_by_guid(7, 11).is_none(), "removed locally");
        // Offline there is no mediator: the server call fails, and says so.
        assert!(matches!(mediator_call(&rx), WorkResult::Failed { what: "delete_group_message", .. }));
    }

    #[test] fn send_offline_does_not_block() {
        let (_t, app) = test_app();
        let c = app.add_contact(vec![7; 32], "Аня".into()).unwrap();
        let (w, rx) = test_worker(&app);
        let t = Instant::now();
        send_text(&w, Target::Contact { id: c, pubkey: vec![7; 32] }, 0, "привет".into());
        assert!(t.elapsed() < Duration::from_millis(50));
        work_done(&rx);
    }

    #[test] fn group_attachment_runs_on_the_group_lane() {
        use crate::{settings::Settings, test_support::test_harness};
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        let tmp = tempfile::tempdir().unwrap();
        let staged = |n: &str| {
            let path = tmp.path().join(n);
            std::fs::write(&path, b"png").unwrap();
            Staged { path, original_name: "t.png".into(), mime: "image/png".into(), msg_type: 1, size: 3, temp: true, resized: false }
        };
        // Stands in for send_group_chat_message: reports what it was given (a fn pointer cannot capture).
        fn fake(_: &MimirApp, chat_id: i64, _: i64, caption: String, spec: Option<AttachmentSpec>, server: Option<Vec<u8>>) -> Result<i64, MimirError> {
            let spec = spec.unwrap();
            let present = std::path::Path::new(&spec.file_path).exists();
            Err(MimirError::Io(format!("{chat_id} {caption} {} {} {present}", spec.original_name, server.map_or("none".into(), hex::encode))))
        }
        let next = |rx: &Receiver<Event>| loop {
            if let Event::WorkDone(r) = rx.recv_timeout(Duration::from_secs(1)).expect("not queued behind the 1:1 lane") { return r; }
        };
        let app = h.state_mut();
        // A slow 1:1 job: the group send must not wait for it.
        app.worker.submit(Box::new(|_| {
            std::thread::sleep(Duration::from_secs(3));
            WorkResult::Done
        }));
        let a = staged("a.png");
        send_attachment_via(app, Target::Group { chat_id: 7 }, 0, "подпись".into(), a.clone(), fake);
        assert_eq!(app.state.pending_sends.get(&ChatKey::Group(7)), Some(&1));
        let server = app.settings.media.file_server.clone();
        match next(&app.rx) {
            WorkResult::AttachmentSent { chat: ChatKey::Group(7), err: Some(MimirError::Io(seen)) } => {
                assert_eq!(seen, format!("7 подпись t.png {server} true"), "settings' file server as bytes; the file still there during the call");
            }
            r => panic!("{r:?}"),
        }
        assert!(!a.path.exists(), "our temp file is deleted after the call");
        // An unparsable file server key is passed as none (the core then refuses: CA §2.3).
        app.settings.media.file_server = "not hex".into();
        let b = staged("b.png");
        send_attachment_via(app, Target::Group { chat_id: 7 }, 0, String::new(), b.clone(), fake);
        match next(&app.rx) {
            WorkResult::AttachmentSent { err: Some(MimirError::Io(seen)), .. } => assert!(seen.ends_with(" t.png none true"), "{seen}"),
            r => panic!("{r:?}"),
        }
        assert!(!b.path.exists());
    }
}
