//! Group chats: members and their names, roles, system-message text, group rows as `StoredMessage`.
use crate::{
    i18n::{tr, trf, Key, Lang},
    model::{chat_list::display_name, timeline::REACTION},
};
use mimir::{parse_system_message, MimirApp, StoredGroupMessage, StoredMember, StoredMessage, SystemMessage};
use std::collections::HashMap;

/// `msg_type` of a mediator system row.
pub const SYSTEM: i32 = 1000;
/// Rows scanned for ChatInfoChanged on a members/history event.
const INFO_SCAN: i32 = 50;

/// Members of one group by row id (= a message's `sender_id`) and by public key.
#[derive(Default)]
pub struct Members {
    chat_id: i64,
    by_id: HashMap<i64, StoredMember>,
    by_key: HashMap<Vec<u8>, i64>,
}

impl Members {
    /// Current members (`get_members`: without those who left or were banned; `learn` adds them).
    pub fn load(app: &MimirApp, chat_id: i64) -> Members {
        let mut m = Members { chat_id, ..Default::default() };
        for s in app.get_members(chat_id) {
            m.insert(s);
        }
        m
    }

    fn insert(&mut self, s: StoredMember) {
        self.by_key.insert(s.pubkey.clone(), s.id);
        self.by_id.insert(s.id, s);
    }

    /// Adds members who left or were banned but still author rows of `msgs` or appear in their system rows
    /// (`get_member_info` keeps them).
    pub fn learn(&mut self, app: &MimirApp, msgs: &[StoredMessage]) {
        for m in msgs {
            if m.msg_type == SYSTEM {
                for pk in system_pubkeys(m.data.as_deref().unwrap_or_default()) {
                    if !self.by_key.contains_key(&pk) {
                        if let Some(s) = app.get_member_info(self.chat_id, pk) {
                            self.insert(s);
                        }
                    }
                }
            } else if m.incoming && m.contact_id >= 0 && !self.by_id.contains_key(&m.contact_id) {
                if let Some(s) = app.get_member_pubkey(self.chat_id, m.contact_id).and_then(|pk| app.get_member_info(self.chat_id, pk)) {
                    self.insert(s);
                }
            }
        }
    }

    /// Nickname, else the first 8 hex digits of the key, else «Неизвестный» (no member row, e.g. `sender_id` -1).
    pub fn name(&self, sender_id: i64, lang: Lang) -> String {
        match self.by_id.get(&sender_id) {
            Some(m) => display_name(&m.nickname, &m.pubkey),
            None => tr(lang, Key::UnknownNickname).to_string(),
        }
    }

    /// Like `name`, for a key named in a system row: without a member row it is the key's first 8 hex digits.
    pub fn name_for_key(&self, pubkey: &[u8], lang: Lang) -> String {
        match self.by_key.get(pubkey).and_then(|id| self.by_id.get(id)) {
            Some(m) => display_name(&m.nickname, &m.pubkey),
            None if pubkey.is_empty() => tr(lang, Key::UnknownNickname).to_string(),
            None => display_name("", pubkey),
        }
    }

    /// The sender's stored avatar path (relative to the core's data dir), if they have one.
    pub fn avatar(&self, sender_id: i64) -> Option<&str> {
        self.by_id.get(&sender_id).map(|m| m.avatar.as_str()).filter(|a| !a.is_empty())
    }

    pub fn pubkey(&self, sender_id: i64) -> Option<&[u8]> {
        self.by_id.get(&sender_id).map(|m| m.pubkey.as_slice())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Owner,
    Admin,
    Mod,
    User,
    ReadOnly,
    Banned,
}

/// Role from the permission bits, the highest of owner > admin > mod first.
pub fn role_of(perms: i32) -> Role {
    match perms {
        p if p & 0x80 != 0 => Role::Owner,
        p if p & 0x40 != 0 => Role::Admin,
        p if p & 0x20 != 0 => Role::Mod,
        p if p & 0x01 != 0 => Role::Banned,
        p if p & 0x08 != 0 => Role::ReadOnly,
        _ => Role::User,
    }
}

pub fn role_name(role: Role, lang: Lang) -> &'static str {
    tr(lang, match role {
        Role::Owner => Key::Owner,
        Role::Admin => Key::Admin,
        Role::Mod => Key::Moderator,
        Role::User => Key::RoleUser,
        Role::ReadOnly => Key::ReadOnly,
        Role::Banned => Key::RoleBanned,
    })
}

/// Role name in a PermsChanged row: banned is checked first, then admin and moderator by bit;
/// everything else reads «User» (not `role_of`, which also knows owner and read-only).
fn perms_change_name(perms: i32, lang: Lang) -> &'static str {
    let role = if perms & 0x01 != 0 { Role::Banned } else if perms & 0x40 != 0 { Role::Admin } else if perms & 0x20 != 0 { Role::Mod } else { Role::User };
    role_name(role, lang)
}

/// Keys a system row names (targets and actors).
fn system_pubkeys(data: &[u8]) -> Vec<Vec<u8>> {
    match parse_system_message(data.to_vec()) {
        Ok(SystemMessage::UserAdded { target, actor } | SystemMessage::UserBanned { target, actor } | SystemMessage::PermsChanged { target, actor, .. }) => vec![target, actor],
        Ok(SystemMessage::UserLeft { user: k } | SystemMessage::ChatDeleted { actor: k } | SystemMessage::ChatInfoChanged { actor: k, .. }) => vec![k],
        _ => vec![],
    }
}

/// Text of a system row; `None` for `MessageDeleted`, which is never shown.
pub fn system_text(data: &[u8], members: &Members, lang: Lang) -> Option<String> {
    let n = |k: &[u8]| members.name_for_key(k, lang);
    let f = |key, args: &[&str]| trf(lang, key, args);
    Some(match parse_system_message(data.to_vec()) {
        Ok(SystemMessage::UserAdded { target, actor }) if target == actor => f(Key::SystemMsgUserJoinedByLink, &[&n(&target)]),
        Ok(SystemMessage::UserAdded { target, actor }) => f(Key::SystemMsgUserAcceptedInvite, &[&n(&target), &n(&actor)]),
        Ok(SystemMessage::UserLeft { user }) => f(Key::SystemMsgUserLeft, &[&n(&user)]),
        Ok(SystemMessage::UserBanned { target, actor }) => f(Key::SystemMsgUserBanned, &[&n(&actor), &n(&target)]),
        Ok(SystemMessage::ChatDeleted { actor }) => f(Key::SystemMsgChatDeleted, &[&n(&actor)]),
        Ok(SystemMessage::ChatInfoChanged { actor, .. }) => f(Key::SystemMsgChatInfoChanged, &[&n(&actor)]),
        Ok(SystemMessage::PermsChanged { target, new_permissions, actor }) => {
            f(Key::SystemMsgRoleChanged, &[&n(&actor), &n(&target), perms_change_name(new_permissions as i32, lang)])
        }
        Ok(SystemMessage::MessageDeleted { .. }) => return None,
        Ok(SystemMessage::Unknown { event_code, .. }) => f(Key::SystemMsgUnknown, &[&format!("0x{event_code:02x}")]),
        Err(_) => f(Key::SystemMsgUnknown, &[&format!("0x{}", hex::encode(&data[..data.len().min(1)]))]),
    })
}

/// The mediator's «chat deleted» system row.
pub fn deletes_chat(msg_type: i32, data: Option<&[u8]>) -> bool {
    msg_type == SYSTEM && matches!(parse_system_message(data.unwrap_or_default().to_vec()), Ok(SystemMessage::ChatDeleted { .. }))
}

/// Rows a timeline or preview skips: reactions, and `MessageDeleted` notices.
pub fn hidden(msg_type: i32, data: Option<&[u8]>) -> bool {
    msg_type == REACTION
        || msg_type == SYSTEM && matches!(parse_system_message(data.unwrap_or_default().to_vec()), Ok(SystemMessage::MessageDeleted { .. }))
}

/// A group row in the 1:1 shape, so `Timeline` keeps one message type: `contact_id` holds `sender_id`
/// (as `get_group_message` does), and a system row is `msg_type` `SYSTEM`.
pub fn group_to_stored(m: &StoredGroupMessage) -> StoredMessage {
    StoredMessage {
        id: m.id,
        contact_id: m.sender_id,
        guid: m.guid,
        reply_to: m.reply_to,
        incoming: m.incoming,
        delivered: m.delivered,
        read: m.read,
        send_time: m.send_time,
        edit_time: 0,
        msg_type: if m.system { SYSTEM } else { m.msg_type },
        data: m.data.clone(),
    }
}

/// Mediator subscription of a group, from `get_group_chat_status` / `GroupChatStatus`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum GroupStatus {
    #[default]
    Connecting,
    Subscribed,
    Disconnected,
    /// Removed, banned, chat deleted, or left.
    ReadOnly,
}

impl GroupStatus {
    pub fn parse(s: &str) -> GroupStatus {
        match s {
            "SUBSCRIBED" => GroupStatus::Subscribed,
            "DISCONNECTED" => GroupStatus::Disconnected,
            "READ_ONLY" | "LEFT" => GroupStatus::ReadOnly,
            _ => GroupStatus::Connecting,
        }
    }
}

/// (members, online) for the header: a channel has no member list, so it shows the mediator's last counts.
pub fn counts(app: &MimirApp, chat_id: i64) -> (i32, i32) {
    match app.get_group_chat(chat_id) {
        Some(g) if g.is_channel => (g.subscriber_count, g.online_count),
        _ => (app.get_member_count(chat_id), app.get_online_member_count(chat_id)),
    }
}

/// The core stores other members' ChatInfoChanged rows without applying them. Writes the name,
/// description and avatar of those newer than row `after` into the local chat, oldest first, and returns the newest
/// one's id.
// ponytail: only the newest INFO_SCAN rows are looked at; a rename buried under more rows than that is missed.
pub fn apply_chat_info(app: &MimirApp, chat_id: i64, after: i64) -> Option<i64> {
    let mut newest = None;
    for m in app.get_group_messages(chat_id, 0, INFO_SCAN).iter().rev().filter(|m| m.system && m.id > after) {
        if let Ok(SystemMessage::ChatInfoChanged { name, description, avatar, .. }) = parse_system_message(m.data.clone().unwrap_or_default()) {
            // ponytail: storage-only single-row writes on the UI thread; a failure keeps the old name.
            if let Some(name) = name.filter(|n| !n.trim().is_empty()) {
                let _ = app.update_group_chat_name(chat_id, name);
            }
            if let Some(d) = description {
                let _ = app.update_group_chat_description(chat_id, d);
            }
            // ponytail: empty avatar bytes are skipped, not taken as «removed».
            if let Some(a) = avatar.filter(|a| !a.is_empty()) {
                let _ = app.update_group_chat_avatar(chat_id, a);
            }
            newest = Some(m.id);
        }
    }
    newest
}

/// Id of the newest stored ChatInfoChanged row (within the rows `apply_chat_info` looks at).
pub fn newest_chat_info(app: &MimirApp, chat_id: i64) -> Option<i64> {
    app.get_group_messages(chat_id, 0, INFO_SCAN)
        .iter()
        .filter(|m| m.system && matches!(parse_system_message(m.data.clone().unwrap_or_default()), Ok(SystemMessage::ChatInfoChanged { .. })))
        .map(|m| m.id)
        .max()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_app;

    fn pk(b: u8) -> Vec<u8> {
        vec![b; 32]
    }

    /// System body: code, the parts, then the mediator's 32 random bytes.
    pub fn body(code: u8, parts: &[&[u8]]) -> Vec<u8> {
        let mut v = vec![code];
        for p in parts {
            v.extend_from_slice(p);
        }
        v.extend([0; 32]);
        v
    }

    /// ChatInfoChanged: 0x06, TLV actor (0x01) and, when given, name (0x20).
    pub fn info_body(actor: &[u8], name: Option<&str>) -> Vec<u8> {
        let mut v = vec![0x06, 0x01, actor.len() as u8];
        v.extend_from_slice(actor);
        if let Some(n) = name {
            v.extend([0x20, n.len() as u8]);
            v.extend_from_slice(n.as_bytes());
        }
        v
    }

    fn members() -> (tempfile::TempDir, std::sync::Arc<MimirApp>, Members) {
        let (t, app) = test_app();
        app.save_group_chat(7, "Семья".into(), String::new(), None, pk(9), vec![], pk(3), 0).unwrap();
        app.update_member_info(7, pk(1), Some("Аня".into()), None, None).unwrap();
        app.update_member_info(7, pk(2), Some("Митя".into()), None, None).unwrap();
        let m = Members::load(&app, 7);
        (t, app, m)
    }

    #[test]
    fn role_of_bits() {
        assert_eq!(role_of(0x80), Role::Owner);
        assert_eq!(role_of(0xC0), Role::Owner);
        assert_eq!(role_of(0x40), Role::Admin);
        assert_eq!(role_of(0x20), Role::Mod);
        assert_eq!(role_of(0x10), Role::User);
        assert_eq!(role_of(0x08), Role::ReadOnly);
        assert_eq!(role_of(0x01), Role::Banned);
    }

    #[test]
    fn system_texts_ru() {
        let (_t, _app, m) = members();
        let ru = |d: Vec<u8>| system_text(&d, &m, Lang::Ru);
        assert_eq!(ru(body(0x01, &[&pk(1), &pk(1)])).as_deref(), Some("Аня вошёл(ла) по ссылке"));
        assert_eq!(ru(body(0x01, &[&pk(1), &pk(2)])).as_deref(), Some("Аня принял(а) приглашение от Митя"));
        assert_eq!(ru(body(0x03, &[&pk(1)])).as_deref(), Some("Аня покинул(а) чат"));
        assert_eq!(ru(body(0x03, &[&pk(0xab)])).as_deref(), Some("abababab покинул(а) чат"), "no member row: short key");
        assert_eq!(ru(body(0x04, &[&pk(1), &pk(2)])).as_deref(), Some("Митя забанил(а) Аня в чате"), "actor first");
        assert_eq!(ru(body(0x05, &[&pk(2)])).as_deref(), Some("Митя удалил(а) чат"));
        assert_eq!(ru(body(0x07, &[&pk(1), &[0x40], &pk(2)])).as_deref(), Some("Митя изменил(а) роль Аня на Админ"));
        assert_eq!(ru(body(0x07, &[&pk(1), &[0x01], &pk(2)])).as_deref(), Some("Митя изменил(а) роль Аня на забанен"));
        // Banned wins, then admin, moderator; owner/read-only/anything else reads «User».
        let role = |perms: u8| ru(body(0x07, &[&pk(1), &[perms], &pk(2)])).unwrap();
        assert!(role(0x41).ends_with("на забанен"), "ban bit first");
        assert!(role(0x20).ends_with("на Модератор"));
        assert!(role(0x10).ends_with("на Пользователь"));
        assert!(role(0x80).ends_with("на Пользователь"), "owner bit is not special here");
        assert!(role(0x08).ends_with("на Пользователь"), "read-only reads as user");
        assert_eq!(ru(info_body(&pk(2), Some("Дача"))).as_deref(), Some("Митя обновил(а) данные чата"));
        assert_eq!(ru(vec![0x02, 1, 2, 3]).as_deref(), Some("Системное событие: 0x02"), "unknown code");
        assert_eq!(ru(vec![0x01, 1, 2]).as_deref(), Some("Системное событие: 0x01"), "too short to parse");
        assert_eq!(ru(vec![]).as_deref(), Some("Системное событие: 0x"));
        let mut deleted = vec![0x08];
        deleted.extend(5i64.to_be_bytes());
        deleted.extend(pk(2));
        assert_eq!(ru(deleted), None, "MessageDeleted is never shown");
        assert_eq!(system_text(&body(0x03, &[&pk(1)]), &m, Lang::En).as_deref(), Some("Аня left the chat"));
    }

    #[test]
    fn unknown_sender_shows_unknown() {
        let (_t, app, m) = members();
        assert_eq!(m.name(-1, Lang::Ru), "Неизвестный");
        assert_eq!(m.name(-1, Lang::En), "Unknown");
        assert!(m.pubkey(-1).is_none());
        app.update_member_info(7, pk(0xcd), Some(" ".into()), None, None).unwrap();
        let m = Members::load(&app, 7);
        let id = app.get_member_info(7, pk(0xcd)).unwrap().id;
        assert_eq!(m.name(id, Lang::Ru), "cdcdcdcd", "blank nickname: short key");
        assert_eq!(m.pubkey(id), Some(&pk(0xcd)[..]));
    }

    #[test]
    fn members_who_left_are_still_named() {
        let (_t, app, _) = members();
        app.update_member_info(7, pk(4), Some("Оля".into()), None, None).unwrap();
        let olya = app.get_member_info(7, pk(4)).unwrap().id;
        let from_olya = app.add_group_message(7, 1, 11, pk(4), 1000, 0, false, b"hi".to_vec(), 0, false).unwrap();
        app.delete_member(7, pk(4)).unwrap();
        let left = app.add_group_message(7, 2, 12, pk(9), 2000, SYSTEM, true, body(0x03, &[&pk(4)]), 0, false).unwrap();
        let mut m = Members::load(&app, 7);
        assert_eq!(m.name(olya, Lang::Ru), "Неизвестный", "get_members skips members who left");
        let msgs: Vec<StoredMessage> = app.get_group_messages(7, 0, 10).iter().map(group_to_stored).collect();
        m.learn(&app, &msgs);
        assert_eq!(m.name(olya, Lang::Ru), "Оля");
        assert_eq!(m.name_for_key(&pk(4), Lang::Ru), "Оля");
        assert_eq!(msgs.iter().map(|m| m.id).collect::<Vec<_>>(), [left, from_olya]);
    }

    #[test]
    fn group_rows_become_stored_messages() {
        let (_t, app, _) = members();
        app.add_group_message(7, 1, 11, pk(1), 1000, 0, false, b"hi".to_vec(), 5, false).unwrap();
        app.add_group_message(7, 2, 12, pk(9), 2000, SYSTEM, true, body(0x03, &[&pk(1)]), 0, false).unwrap();
        let rows = app.get_group_messages(7, 0, 10);
        let (sys, msg) = (group_to_stored(&rows[0]), group_to_stored(&rows[1]));
        assert_eq!((msg.contact_id, msg.guid, msg.reply_to, msg.edit_time, msg.incoming), (rows[1].sender_id, 11, 5, 0, true));
        assert_eq!(msg.data.as_deref(), Some(&b"hi"[..]));
        assert_eq!((sys.msg_type, sys.contact_id), (SYSTEM, -1));
    }

    #[test]
    fn status_strings() {
        assert_eq!(GroupStatus::parse("SUBSCRIBED"), GroupStatus::Subscribed);
        assert_eq!(GroupStatus::parse("CONNECTING"), GroupStatus::Connecting);
        assert_eq!(GroupStatus::parse("DISCONNECTED"), GroupStatus::Disconnected);
        assert_eq!(GroupStatus::parse("READ_ONLY"), GroupStatus::ReadOnly);
        assert_eq!(GroupStatus::parse("LEFT"), GroupStatus::ReadOnly);
        assert_eq!(GroupStatus::parse("?"), GroupStatus::Connecting);
    }

    #[test]
    fn others_avatar_change_is_applied() {
        // A member's ChatInfoChanged with an avatar (TLV 0x22) updates our copy of the group avatar.
        let (_t, app, _) = members();
        let png = crate::test_support::png_bytes(2, 2, [1, 2, 3]);
        assert!(png.len() < 128, "one-byte TLV length");
        let mut body = info_body(&pk(2), None);
        body.extend([0x22, png.len() as u8]);
        body.extend_from_slice(&png);
        let id = app.add_group_message(7, 1, 11, pk(9), 1000, SYSTEM, true, body, 0, false).unwrap();
        assert_eq!(apply_chat_info(&app, 7, 0), Some(id));
        let stored = app.get_group_chat(7).unwrap().avatar;
        assert!(!stored.is_empty(), "avatar written");
    }

    #[test]
    fn newest_chat_info_is_applied_once() {
        let (_t, app, _) = members();
        app.add_group_message(7, 1, 11, pk(9), 1000, SYSTEM, true, info_body(&pk(2), Some("Дача")), 0, false).unwrap();
        let last = app.add_group_message(7, 2, 12, pk(9), 2000, SYSTEM, true, info_body(&pk(2), Some("Дача 2")), 0, false).unwrap();
        assert_eq!(apply_chat_info(&app, 7, 0), Some(last));
        assert_eq!(app.get_group_chat(7).unwrap().name, "Дача 2", "newest wins");
        app.update_group_chat_name(7, "Своё".into()).unwrap();
        assert_eq!(apply_chat_info(&app, 7, last), None, "nothing newer");
        assert_eq!(app.get_group_chat(7).unwrap().name, "Своё");
        let desc_only = app.add_group_message(7, 3, 13, pk(9), 3000, SYSTEM, true, info_body(&pk(2), None), 0, false).unwrap();
        assert_eq!(apply_chat_info(&app, 7, last), Some(desc_only));
        assert_eq!(app.get_group_chat(7).unwrap().name, "Своё", "no name in the row: name kept");
        assert_eq!(newest_chat_info(&app, 7), Some(desc_only));
        app.add_group_message(7, 4, 14, pk(9), 4000, SYSTEM, true, body(0x03, &[&pk(1)]), 0, false).unwrap();
        assert_eq!(newest_chat_info(&app, 7), Some(desc_only), "other system rows are not renames");
        assert_eq!(newest_chat_info(&app, 8), None);
    }
}
