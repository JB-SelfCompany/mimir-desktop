//! Chat list rows (Android MainActivity.getChatList): Saved Messages first, then contacts and groups by last message time.
use crate::i18n::{tr, Key, Lang};
use crate::model::{chat_key::ChatKey, group::{self, group_to_stored, Members}, message::{decode, preview}};
use mimir::{MimirApp, StoredDraft, StoredGroupChat};

pub const SAVED_ID: i64 = -100;

#[derive(Debug, Clone, PartialEq)]
pub struct ChatRow {
    pub key: ChatKey,
    /// Contact public key (our own for Saved Messages); empty for a group.
    pub pubkey: Vec<u8>,
    pub name: String,
    /// Stored avatar path (relative to the core's data dir; empty = none), resolved with `media::avatar::resolve`.
    pub avatar: String,
    pub preview: String,
    pub time: i64,
    pub unread: i32,
    pub has_draft: bool,
    /// State of the contact request we sent (Android ChatListItem.requestStatus).
    pub request: Request,
    /// Set for group and channel rows.
    pub group: Option<GroupRowInfo>,
}

/// What a group row shows besides the common fields (from `StoredGroupChat`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupRowInfo {
    pub muted: bool,
    pub read_only: bool,
    pub is_channel: bool,
}

/// Our outgoing contact request, as `MimirApp::get_outgoing_request_status` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Request {
    #[default]
    None,
    /// Not answered yet: «Waiting…» and a «?» badge.
    Pending,
    /// Declined: «Request declined» and a «!» badge.
    Declined,
}

impl Request {
    pub fn from_status(status: i32) -> Request {
        match status {
            1 => Request::Pending,
            2 => Request::Declined,
            _ => Request::None,
        }
    }

    /// Row preview for a request; it replaces the last message (a draft still wins).
    pub fn preview(self, lang: Lang) -> Option<&'static str> {
        match self {
            Request::None => None,
            Request::Pending => Some(tr(lang, Key::RequestPending)),
            Request::Declined => Some(tr(lang, Key::RequestDeclined)),
        }
    }
}

/// Name to show for a contact: its stored name, or the first 8 hex digits of its key when that is blank
/// (the core stores "" when the peer's profile has no name; Android shows `Hex.toHexString(pubkey).take(8)`).
pub fn display_name(name: &str, pubkey: &[u8]) -> String {
    if name.trim().is_empty() {
        hex::encode(&pubkey[..pubkey.len().min(4)])
    } else {
        name.to_string()
    }
}

/// Android ContactsAdapter.buildDraftText: «Черновик: » then the text, with an attachment's kind before it (or
/// instead of it when there is no text). A draft with neither is never stored: the core drops it on save.
fn draft_preview(d: &StoredDraft, lang: Lang) -> String {
    let text = d.text.as_deref().unwrap_or_default();
    let media = d.media_uri.as_deref().is_some_and(|u| !u.is_empty());
    let body = match (media, d.media_type, text.is_empty()) {
        (true, 1, false) => format!("🖼️ {text}"),
        (true, 3, false) => format!("📄 {text}"),
        (true, 1, true) => format!("🖼️ {}", tr(lang, Key::Image)),
        (true, 3, true) => format!("📄 {}", tr(lang, Key::DraftFile)),
        (true, _, true) => tr(lang, Key::File).to_string(),
        _ => text.to_string(),
    };
    format!("{}{body}", tr(lang, Key::Draft))
}

fn row(app: &MimirApp, lang: Lang, contact_id: i64, name: String, pubkey: Vec<u8>, avatar: String) -> ChatRow {
    let last = app.get_last_message(contact_id);
    let draft = app.get_draft(0, contact_id);
    // Android ContactsAdapter: draft > our pending/declined request > last message.
    let request = if contact_id == SAVED_ID { Request::None } else { Request::from_status(app.get_outgoing_request_status(pubkey.clone())) };
    let preview = match (&draft, &last, request.preview(lang)) {
        (Some(d), _, _) => draft_preview(d, lang),
        (None, _, Some(r)) => r.to_string(),
        (None, Some(m), None) => preview(&decode(m.msg_type, m.data.as_deref()), lang),
        (None, None, None) => tr(lang, Key::EmptyChat).to_string(),
    };
    ChatRow {
        key: ChatKey::Contact(contact_id),
        pubkey,
        name,
        avatar,
        preview,
        time: last.map_or(0, |m| m.send_time),
        unread: app.get_unread_count(contact_id),
        has_draft: draft.is_some(),
        request,
        group: None,
    }
}

/// Android ContactsAdapter for a group: draft > last message > description > «[ empty chat ]».
fn group_row(app: &MimirApp, lang: Lang, g: StoredGroupChat) -> ChatRow {
    let key = ChatKey::Group(g.chat_id);
    let draft = app.get_draft(key.draft_type(), g.chat_id);
    // The newest row a timeline shows (not a reaction or deletion notice); get_last_group_message would skip
    // system rows, which preview as their text, never as raw bytes.
    // ponytail: a newest page of only reactions falls back to get_last_group_message.
    let last = app.get_group_messages(g.chat_id, 0, 50).iter().map(group_to_stored).find(|m| !group::hidden(m.msg_type, m.data.as_deref()))
        .or_else(|| app.get_last_group_message(g.chat_id).filter(|m| !group::hidden(m.msg_type, m.data.as_deref())));
    let preview = match (&draft, last) {
        (Some(d), _) => draft_preview(d, lang),
        (None, Some(m)) if m.msg_type == group::SYSTEM => {
            let mut members = Members::load(app, g.chat_id);
            members.learn(app, std::slice::from_ref(&m));
            group::system_text(m.data.as_deref().unwrap_or_default(), &members, lang).unwrap_or_else(|| tr(lang, Key::SystemMessage).to_string())
        }
        (None, Some(m)) => preview(&decode(m.msg_type, m.data.as_deref()), lang),
        (None, None) if !g.description.trim().is_empty() => g.description.clone(),
        (None, None) => tr(lang, Key::EmptyChat).to_string(),
    };
    ChatRow {
        key,
        pubkey: vec![],
        name: if g.name.trim().is_empty() { tr(lang, Key::UnknownNickname).to_string() } else { g.name },
        avatar: g.avatar,
        preview,
        time: g.last_message_time,
        unread: g.unread_count,
        has_draft: draft.is_some(),
        request: Request::None,
        group: Some(GroupRowInfo { muted: g.muted, read_only: g.read_only, is_channel: g.is_channel }),
    }
}

pub fn build_chat_list(app: &MimirApp, lang: Lang) -> Vec<ChatRow> {
    let me = app.public_key();
    let mut rows: Vec<ChatRow> = app
        .get_contacts()
        .into_iter()
        .filter(|c| c.pubkey != me)
        .map(|c| row(app, lang, c.id, display_name(&c.name, &c.pubkey), c.pubkey, c.avatar))
        .collect();
    rows.extend(app.get_group_chats().into_iter().map(|g| group_row(app, lang, g)));
    rows.sort_by(|a, b| b.time.cmp(&a.time));
    rows.insert(0, row(app, lang, SAVED_ID, tr(lang, Key::SavedMessages).to_string(), me, String::new()));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{model::{chat_key::ChatKey, message::reaction_payload}, test_support::test_app};

    #[test] fn saved_first_then_by_time_desc() {
        let (_t, app) = test_app();
        let a = app.add_contact(vec![1;32], "Аня".into()).unwrap();
        let b = app.add_contact(vec![2;32], "Митя".into()).unwrap();
        app.add_local_message(a, 1, 0, true, true, 1000, 0, 0, b"old".to_vec()).unwrap();
        app.add_local_message(b, 2, 0, true, true, 2000, 0, 0, b"new".to_vec()).unwrap();
        let rows = build_chat_list(&app, Lang::Ru);
        assert_eq!(rows[0].key, ChatKey::SAVED);
        assert_eq!(rows[0].pubkey, app.public_key());
        assert_eq!(rows.iter().skip(1).map(|r| r.name.as_str()).collect::<Vec<_>>(), ["Митя", "Аня"]);
        assert_eq!(rows[1].preview, "new");
    }
    #[test] fn own_pubkey_contact_hidden() {
        let (_t, app) = test_app();
        app.add_contact(app.public_key(), "Unknown".into()).unwrap();
        assert!(build_chat_list(&app, Lang::Ru).iter().all(|r| r.name != "Unknown"));
    }
    #[test] fn draft_overrides_preview() {
        let (_t, app) = test_app();
        let a = app.add_contact(vec![1;32], "Аня".into()).unwrap();
        app.save_draft(0, a, Some("пишу…".into()), None, 0).unwrap();
        let rows = build_chat_list(&app, Lang::Ru);
        assert_eq!(rows[1].preview, "Черновик: пишу…");
        assert!(rows[1].has_draft);
    }
    #[test] fn media_draft_previews_like_android() {
        let (_t, app) = test_app();
        let a = app.add_contact(vec![1;32], "Аня".into()).unwrap();
        let preview = |text: Option<&str>, uri: Option<&str>, kind| {
            app.save_draft(0, a, text.map(Into::into), uri.map(Into::into), kind).unwrap();
            build_chat_list(&app, Lang::Ru)[1].preview.clone()
        };
        assert_eq!(preview(Some("смотри"), Some("content://x"), 1), "Черновик: 🖼️ смотри");
        assert_eq!(preview(Some("вот"), Some("content://x"), 3), "Черновик: 📄 вот");
        assert_eq!(preview(None, Some("content://x"), 1), "Черновик: 🖼️ Изображение");
        assert_eq!(preview(Some(""), Some("content://x"), 3), "Черновик: 📄 Файл");
        assert_eq!(preview(None, Some("content://x"), 4), "Черновик: Вложение");
        assert_eq!(preview(Some("текст"), Some("content://x"), 4), "Черновик: текст");
        assert_eq!(preview(Some(""), None, 0), "[ пустой чат ]", "the core drops a draft with nothing in it");
    }
    #[test] fn reaction_does_not_blank_preview() {
        let (_t, app) = test_app();
        let a = app.add_contact(vec![1;32], "Аня".into()).unwrap();
        app.add_local_message(a, 5, 0, true, true, 1000, 0, 0, b"hi".to_vec()).unwrap();
        app.add_local_message(a, 6, 5, true, true, 2000, 0, 10, reaction_payload("👍", 5)).unwrap();
        assert_eq!(build_chat_list(&app, Lang::Ru)[1].preview, "hi");
    }
    #[test] fn pending_outgoing_request_shows_waiting() {
        let (_t, app) = test_app();
        let a = app.add_contact(vec![1;32], "Аня".into()).unwrap();
        app.send_contact_request(vec![1;32], String::new()).unwrap();
        let rows = build_chat_list(&app, Lang::Ru);
        assert_eq!(rows[1].preview, "Ожидание…");
        assert_eq!(rows[1].request, Request::Pending);
        assert_eq!(build_chat_list(&app, Lang::En)[1].preview, "Waiting…");
        app.save_draft(0, a, Some("пишу".into()), None, 0).unwrap();
        let rows = build_chat_list(&app, Lang::Ru);
        assert_eq!(rows[1].preview, "Черновик: пишу", "draft still wins, like Android");
        assert_eq!(rows[1].request, Request::Pending);
        assert_eq!(rows[0].request, Request::None, "Saved Messages");
    }
    #[test] fn request_status_maps_like_android() {
        // MimirApp::get_outgoing_request_status: 0 none, 1 pending, 2 declined (Android ChatListItem.REQUEST_*).
        assert_eq!(Request::from_status(0), Request::None);
        assert_eq!(Request::from_status(1), Request::Pending);
        assert_eq!(Request::from_status(2), Request::Declined);
        assert_eq!(Request::Declined.preview(Lang::Ru), Some("Запрос отклонён"));
        assert_eq!(Request::Declined.preview(Lang::En), Some("Request declined"));
        assert_eq!(Request::Pending.preview(Lang::Ru), Some("Ожидание…"));
        assert_eq!(Request::None.preview(Lang::Ru), None);
    }
    #[test] fn empty_name_falls_back_to_short_key() {
        // The core renames a contact to "" when the peer's profile name is empty (info exchange).
        let (_t, app) = test_app();
        let mut key = vec![0x35, 0xE1, 0xFD, 0xE1];
        key.resize(32, 7);
        app.add_contact(key.clone(), String::new()).unwrap();
        app.add_contact(vec![2; 32], "  ".into()).unwrap();
        let rows = build_chat_list(&app, Lang::Ru);
        assert!(rows.iter().any(|r| r.name == "35e1fde1"), "{:?}", rows.iter().map(|r| &r.name).collect::<Vec<_>>());
        assert!(rows.iter().any(|r| r.name == "02020202"));
        assert_eq!(display_name("Аня", &key), "Аня");
    }
    fn group(app: &MimirApp, id: i64, name: &str, description: &str) {
        app.save_group_chat(id, name.into(), description.into(), None, vec![9; 32], vec![], vec![3; 32], 0).unwrap();
    }
    #[test] fn groups_listed_with_contacts_by_time() {
        let (_t, app) = test_app();
        let a = app.add_contact(vec![1;32], "Аня".into()).unwrap();
        app.add_local_message(a, 1, 0, true, true, 2000, 0, 0, b"old".to_vec()).unwrap();
        group(&app, 7, "Семья", "");
        app.add_group_message(7, 1, 11, vec![5; 32], 3000, 0, false, b"hi".to_vec(), 0, false).unwrap();
        let rows = build_chat_list(&app, Lang::Ru);
        assert_eq!(rows.iter().map(|r| r.key).collect::<Vec<_>>(), [ChatKey::SAVED, ChatKey::Group(7), ChatKey::Contact(a)]);
        assert_eq!((rows[1].name.as_str(), rows[1].preview.as_str(), rows[1].unread, rows[1].time), ("Семья", "hi", 1, 3000));
        assert!(rows[1].pubkey.is_empty());
        assert_eq!(rows[1].group, Some(GroupRowInfo { muted: false, read_only: false, is_channel: false }));
        assert_eq!(rows[2].group, None);
    }
    #[test] fn group_and_contact_with_same_id_stay_separate() {
        let (_t, app) = test_app();
        let n = app.add_contact(vec![1;32], "Аня".into()).unwrap();
        group(&app, n, "Семья", "");
        app.save_draft(1, n, Some("пишу".into()), None, 0).unwrap();
        let rows = build_chat_list(&app, Lang::Ru);
        let find = |k| rows.iter().find(|r| r.key == k).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(find(ChatKey::Group(n)).preview, "Черновик: пишу");
        assert!(find(ChatKey::Group(n)).has_draft);
        assert!(!find(ChatKey::Contact(n)).preview.starts_with("Черновик"));
        assert!(!find(ChatKey::Contact(n)).has_draft);
    }
    #[test] fn group_system_message_preview_is_text() {
        let (_t, app) = test_app();
        group(&app, 7, "Семья", "");
        app.add_group_message(7, 1, 11, vec![5; 32], 1000, 0, false, b"hi".to_vec(), 0, false).unwrap();
        let mut left = vec![0x03];
        left.extend([4; 32]);
        left.extend([0; 32]);
        app.add_group_message(7, 2, 12, vec![9; 32], 2000, 1000, true, left, 0, false).unwrap();
        let row = build_chat_list(&app, Lang::Ru).into_iter().find(|r| r.key == ChatKey::Group(7)).unwrap();
        assert_eq!(row.preview, "04040404 покинул(а) чат");
        assert_eq!(build_chat_list(&app, Lang::En)[1].preview, "04040404 left the chat");
    }
    #[test] fn group_preview_skips_reactions() {
        let (_t, app) = test_app();
        group(&app, 7, "Семья", "");
        app.add_group_message(7, 1, 11, vec![5; 32], 1000, 0, false, b"hi".to_vec(), 0, false).unwrap();
        let mut left = vec![0x03];
        left.extend([4; 32]);
        left.extend([0; 32]);
        app.add_group_message(7, 2, 12, vec![9; 32], 2000, 1000, true, left, 0, false).unwrap();
        app.add_group_message(7, 3, 13, vec![5; 32], 3000, 10, false, reaction_payload("👍", 11), 11, false).unwrap();
        let row = build_chat_list(&app, Lang::Ru).into_iter().find(|r| r.key == ChatKey::Group(7)).unwrap();
        assert_eq!(row.preview, "04040404 покинул(а) чат", "the reaction on top is skipped");
    }
    #[test] fn group_without_messages_shows_description() {
        let (_t, app) = test_app();
        group(&app, 7, "", "Про дачу");
        let rows = build_chat_list(&app, Lang::Ru);
        assert_eq!((rows[1].name.as_str(), rows[1].preview.as_str()), ("Неизвестный", "Про дачу"));
        group(&app, 8, "Пусто", "");
        assert!(build_chat_list(&app, Lang::Ru).iter().any(|r| r.key == ChatKey::Group(8) && r.preview == "[ пустой чат ]"));
    }
    #[test] fn empty_chat_placeholder() {
        let (_t, app) = test_app();
        app.add_contact(vec![1;32], "Аня".into()).unwrap();
        assert_eq!(build_chat_list(&app, Lang::En)[1].preview, "[ empty chat ]");
    }
}
