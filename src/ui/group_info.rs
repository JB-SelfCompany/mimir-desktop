//! Group info panel (Android GroupInfoActivity): header, actions by role, description, invite link, and the members
//! with their roles and the owner/admin member menu; plus the group's ⋮ menu and its dialogs.
//! Mediator calls (leave, delete, invite, role, edit) run on the worker as a `GroupOp`; only their real results are
//! reported. Clear history and the local delete of a read-only chat write many rows, so they run there too;
//! mute is a single-row write and stays inline.
use crate::{
    app::MimirDesktop,
    core::worker::WorkResult,
    i18n::{tr, trf, Key, Lang},
    model::{
        chat_key::ChatKey,
        chat_list::{build_chat_list, display_name, GroupRowInfo},
        group::{self, role_name, role_of, GroupStatus, Role},
        group_link::{encode_group_link, GroupLink, LinkKind},
        timeline::Timeline,
    },
    ui::{
        avatar::{self, avatar_color, avatar_seed_color, pick_button, AvatarPick},
        chat_view::last_seen_text,
        contact_picker::{ContactPicker, Pick},
        info_panel::{panel_frame, row},
        new_group::{counted, MAX_DESCRIPTION, MAX_NAME},
        qr,
        theme::{danger_button, dialog, dialog_title, field_label, palette, primary_button, Palette},
    },
};
use eframe::egui::{
    self, vec2, Align, Button, Color32, FontFamily, FontId, Id, Label, Layout, Popup, Response, RichText, Sense, SetOpenCommand,
    Spinner, TextureHandle, Ui, UiBuilder,
};
use crate::media::{avatar::{resolve, GROUP_MAX}, thumbs::ThumbCache};
use mimir::{MemberUpdate, MimirApp, StoredGroupChat, StoredMember};
use std::path::Path;

/// A mediator call, run on the worker.
#[derive(Clone, Debug, PartialEq)]
pub enum GroupOp {
    Leave(i64),
    Delete(i64),
    /// The local-only delete of a read-only chat.
    DeleteLocal(i64),
    /// Deletes the chat's messages and their attachment files (a bare file name inside `files_dir`).
    ClearHistory { chat_id: i64, files_dir: std::path::PathBuf },
    Invite { chat_id: i64, pubkey: Vec<u8>, name: String },
    /// `perms`: 0x10 user, 0x20 moderator, 0x40 admin, 0x01 banned (a ban is a role change).
    SetRole { chat_id: i64, pubkey: Vec<u8>, perms: u8, name: String },
    /// Only the fields that changed are `Some`; `avatar` is a newly picked one.
    UpdateInfo { chat_id: i64, name: Option<String>, description: Option<String>, avatar: Option<Vec<u8>> },
}

impl GroupOp {
    fn run(self, core: &MimirApp) -> WorkResult {
        let failed = |what| move |e: mimir::MimirError| WorkResult::Failed { what, err: e.to_string() };
        let r = match self {
            GroupOp::Leave(chat_id) => core.leave_group(chat_id).map(|_| WorkResult::Left { chat_id }).map_err(failed("leave_group")),
            GroupOp::Delete(chat_id) => core.delete_group(chat_id).map(|_| WorkResult::GroupDeleted { chat_id }).map_err(failed("delete_group")),
            GroupOp::DeleteLocal(chat_id) => core.delete_group_chat(chat_id).map(|_| WorkResult::GroupDeleted { chat_id }).map_err(failed("delete_group_chat")),
            GroupOp::ClearHistory { chat_id, files_dir } => {
                crate::media::remove_attachment_files(&files_dir, &core.clear_group_chat_history(chat_id));
                Ok(WorkResult::HistoryCleared { chat_id })
            }
            GroupOp::Invite { chat_id, pubkey, name } => {
                core.invite_to_group(chat_id, pubkey).map(|_| WorkResult::Invited { name }).map_err(failed("invite_to_group"))
            }
            GroupOp::SetRole { chat_id, pubkey, perms, name } => core
                .change_member_role(chat_id, pubkey.clone(), perms)
                .map(|_| if perms == 0x01 { WorkResult::Banned { chat_id, pubkey, name } } else { WorkResult::RoleChanged { chat_id, pubkey, perms, name } })
                .map_err(failed("change_member_role")),
            GroupOp::UpdateInfo { chat_id, name, description, avatar } => core
                .update_group_info(chat_id, name, description, avatar)
                .map(|_| WorkResult::GroupInfoUpdated { chat_id })
                .map_err(failed("update_group_info")),
        };
        r.unwrap_or_else(|e| e)
    }
}

fn submit(app: &mut MimirDesktop, op: GroupOp) {
    #[cfg(test)]
    app.state.group_ui.submitted.push(op.clone());
    // Storage-only jobs must not queue behind mediator calls (dial and request timeouts).
    let worker = if matches!(op, GroupOp::DeleteLocal(_) | GroupOp::ClearHistory { .. }) { &app.worker } else { &app.group_worker };
    worker.submit(Box::new(move |core| op.run(core)));
}

/// The stored chat and its members as the panel shows them; re-read when the chat or its members change.
struct Cache {
    chat_id: i64,
    chat: StoredGroupChat,
    /// Current members, in display order.
    members: Vec<StoredMember>,
    me: Role,
}

pub struct EditDialog {
    name: String,
    description: String,
    /// The stored (name, description) the dialog opened with.
    stored: (String, String),
    busy: bool,
    error: Option<String>,
    /// A newly picked avatar; the stored one (`stored_avatar`) shows until then.
    avatar: AvatarPick,
    stored_avatar: Option<std::path::PathBuf>,
}

pub enum Dialog {
    Leave,
    Delete,
    /// A read-only chat (removed, banned, deleted): local delete only.
    DeleteLocal,
    Clear,
    Ban { pubkey: Vec<u8>, name: String },
    Role { pubkey: Vec<u8>, name: String, current: Role, pick: Role },
    Edit(EditDialog),
    Invite(ContactPicker),
}

#[derive(Default)]
pub struct GroupUi {
    cache: Option<Cache>,
    /// (chat id, dialog); dropped when another chat is selected.
    dialog: Option<(i64, Dialog)>,
    /// The confirmed dialog (leave, delete, ban, role) waits for its mediator call: it stays open, disabled.
    busy: bool,
    qr: Option<TextureHandle>,
    /// Every `GroupOp` handed to the worker, for the tests.
    #[cfg(test)]
    pub submitted: Vec<GroupOp>,
}

impl GroupUi {
    /// Members or chat info changed: re-read them next frame.
    pub fn invalidate(&mut self) {
        self.cache = None;
    }
}

#[derive(Clone, Copy)]
enum Act {
    Message,
    Edit,
    Mute,
    Leave,
    Delete,
    DeleteLocal,
    Clear,
    Invite,
}

/// Our role from our own member row; before the first member sync, the stored owner key.
fn my_role(members: &[StoredMember], me: &[u8], owner: &[u8]) -> Role {
    match members.iter().find(|m| m.pubkey == me) {
        Some(m) => role_of(m.permissions),
        None if owner == me => Role::Owner,
        None => Role::User,
    }
}

/// Online first, then the owner, then admins and moderators, then by name.
fn sort_members(members: &mut [StoredMember]) {
    let rank = |m: &StoredMember| match role_of(m.permissions) {
        Role::Owner => 0,
        Role::Admin | Role::Mod => 1,
        _ => 2,
    };
    members.sort_by_cached_key(|m| (!m.online, rank(m), display_name(&m.nickname, &m.pubkey).to_lowercase()));
}

/// The cache for chat `id`, taken out of the state (put it back with `restore`); `None` if the chat is gone.
fn take_cache(app: &mut MimirDesktop, id: i64) -> Option<Cache> {
    match app.state.group_ui.cache.take() {
        Some(c) if c.chat_id == id => Some(c),
        _ => {
            // ponytail: storage reads on the UI thread, once per chat or member change.
            let chat = app.core.get_group_chat(id)?;
            let mut members = app.core.get_members(id);
            // A channel has no member list to read permissions from: ownership is the stored owner key (Android).
            let me = match chat.is_channel {
                true if chat.owner_pubkey == app.core.public_key() => Role::Owner,
                true => Role::User,
                false => my_role(&members, &app.core.public_key(), &chat.owner_pubkey),
            };
            sort_members(&mut members);
            Some(Cache { chat_id: id, chat, members, me })
        }
    }
}

/// The chat row's flags and whether the chat is read-only (stored flag or subscription state). Somebody else's
/// channel never is here: its subscribers are read-only by design and still mute or unsubscribe (Android isChannel
/// && !isOwner). Our own channel is, once deleted elsewhere: the mediator would refuse «Удалить канал».
/// ponytail: so a channel its owner deleted offers «Отписаться», which the mediator refuses with an error.
fn row_info(app: &MimirDesktop, id: i64) -> Option<(String, GroupRowInfo, bool)> {
    let r = app.state.chats.iter().find(|r| r.key == ChatKey::Group(id))?;
    let g = r.group?;
    let status_ro = app.state.open_chat.as_ref().is_some_and(|c| c.key == ChatKey::Group(id) && c.status == GroupStatus::ReadOnly);
    Some((r.name.clone(), g, (g.read_only || status_ro) && (!g.is_channel || g.is_owner)))
}

/// The invite link of a public chat; `None` without a 32-byte mediator key and shared key.
fn invite_link(chat: &StoredGroupChat) -> Option<String> {
    let kind = if chat.is_channel { LinkKind::Channel } else { LinkKind::Group };
    let l = GroupLink { kind, mediator: chat.mediator_pubkey.clone().try_into().ok()?, chat_id: chat.chat_id, key: chat.shared_key.clone().try_into().ok()? };
    chat.is_public.then(|| encode_group_link(&l))
}

/// Leave (or unsubscribe) for members, delete for the owner; delete-locally for a read-only chat.
fn exit_action(me: Role, info: GroupRowInfo, read_only: bool) -> (Act, Key) {
    match (read_only, me, info.is_channel) {
        (true, ..) => (Act::DeleteLocal, Key::Delete),
        (_, Role::Owner, true) => (Act::Delete, Key::DeleteChannel),
        (_, Role::Owner, false) => (Act::Delete, Key::DeleteGroup),
        (_, _, true) => (Act::Leave, Key::LeaveChannel),
        _ => (Act::Leave, Key::LeaveGroup),
    }
}

pub fn contents(app: &mut MimirDesktop, ui: &mut Ui, p: &Palette, id: i64) {
    let lang = app.lang;
    let Some((name, info, read_only)) = row_info(app, id) else { return };
    let Some(c) = take_cache(app, id) else { return };
    let counts = app.state.open_chat.as_ref().filter(|o| o.key == ChatKey::Group(id)).map_or_else(|| group::counts(&app.core, id), |o| o.counts);
    let manage = matches!(c.me, Role::Owner | Role::Admin) && !read_only;
    let link = invite_link(&c.chat);
    let me_key = app.core.public_key();
    let (mut picked, mut member_dialog, mut copy, mut show_qr) = (None, None, false, false);
    let (thumbs, data_dir) = (&mut app.state.thumbs, app.paths.core_dir.as_path());
    let close = panel_frame(ui, p, lang, |ui| {
        ui.vertical_centered(|ui| {
            // 128 px, corner 8, in group info.
            let path = resolve(data_dir, &c.chat.avatar);
            avatar::show(ui, thumbs, path.as_deref(), avatar_seed_color(id.to_string().as_bytes(), ui.visuals().dark_mode), 128., 4.);
            ui.label(RichText::new(&name).font(FontId::new(15., FontFamily::Name("bold".into()))).color(p.text));
            let (n, on) = (counts.0.to_string(), counts.1.to_string());
            let line = match (info.is_channel, counts.1 > 0) {
                (true, _) => trf(lang, Key::ChannelSubscribers, &[&n, &on]),
                (false, true) => trf(lang, Key::MemberCount, &[&n, &on]),
                (false, false) => trf(lang, Key::MembersOnly, &[&n]),
            };
            ui.label(RichText::new(line).size(11.).color(p.text_dim));
        });
        let w = ui.available_width();
        let mut actions = vec![];
        if !read_only {
            actions.push(if manage { (Act::Edit, Key::Edit) } else { (Act::Message, Key::MessageAction) });
            actions.push((Act::Mute, if info.muted { Key::Unmute } else { Key::Mute }));
        }
        actions.push(exit_action(c.me, info, read_only));
        for (a, key) in actions {
            if ui.add_sized([w, 28.], Button::new(tr(lang, key))).clicked() {
                picked = Some(a);
            }
        }
        if !c.chat.description.trim().is_empty() {
            ui.separator();
            ui.label(RichText::new(tr(lang, Key::DescriptionTitle)).size(11.).color(p.text_dim));
            ui.label(&c.chat.description);
        }
        if let Some(link) = &link {
            ui.separator();
            match row(ui, p, tr(lang, Key::InviteLink), link, &[tr(lang, Key::QrButton), tr(lang, Key::Copy)]) {
                Some(0) => show_qr = true,
                Some(_) => copy = true,
                None => {}
            }
        }
        // A public channel is joined by link, so there is nobody to add; a private one grows by invite. A channel
        // discloses counts, never a roster.
        let invite = manage && !(info.is_channel && c.chat.is_public);
        if invite || !info.is_channel {
            ui.separator();
        }
        if invite && ui.add_sized([w, 28.], Button::new(tr(lang, Key::AddMembers))).clicked() {
            picked = Some(Act::Invite);
        }
        if !info.is_channel {
            ui.label(RichText::new(tr(lang, Key::Members)).size(11.).color(p.text_dim));
            for m in &c.members {
                if let Some(d) = member_row(ui, p, lang, m, manage && m.pubkey != me_key, thumbs, data_dir) {
                    member_dialog = Some(d);
                }
            }
        }
    });
    let members: Vec<Vec<u8>> = c.members.iter().map(|m| m.pubkey.clone()).collect();
    let (stored, me) = ((c.chat.name.clone(), c.chat.description.clone()), c.me);
    app.state.group_ui.cache = Some(c);
    if copy {
        if let Some(link) = link.clone() {
            ui.ctx().copy_text(link);
            app.notice(if info.is_channel { Key::ChannelLinkCopied } else { Key::GroupLinkCopied });
        }
    }
    if show_qr {
        app.state.group_ui.qr = link.map(|l| qr::texture(ui.ctx(), &l));
    }
    if let Some(d) = member_dialog {
        app.state.group_ui.dialog = Some((id, d));
    }
    if let Some(a) = picked {
        act(app, id, a, info, stored, &members, me);
    }
    if close {
        app.state.info_open = false;
    }
}

/// One member: avatar, name and role badge, 8-hex key, online or last seen. With `manage`, a click (either button)
/// opens the member menu; returns the dialog it picked.
fn member_row(ui: &mut Ui, p: &Palette, lang: Lang, m: &StoredMember, manage: bool, thumbs: &mut ThumbCache, data_dir: &Path) -> Option<Dialog> {
    let name = display_name(&m.nickname, &m.pubkey);
    let role = role_of(m.permissions);
    let resp = ui
        .push_id(&m.pubkey, |ui| {
            ui.scope_builder(UiBuilder::new().sense(if manage { Sense::click() } else { Sense::hover() }), |ui| {
                ui.horizontal(|ui| {
                    // 48 px in the member list.
                    let path = resolve(data_dir, &m.avatar);
                    avatar::show(ui, thumbs, path.as_deref(), avatar_color(&m.pubkey), 48., 3.);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 1.;
                        ui.horizontal(|ui| {
                            ui.add(Label::new(RichText::new(&name).font(FontId::new(13., FontFamily::Name("bold".into()))).color(p.text)).selectable(false));
                            if !matches!(role, Role::User | Role::Banned) {
                                ui.add(Label::new(RichText::new(role_name(role, lang)).size(10.).color(p.accent)).selectable(false));
                            }
                        });
                        let hex8: String = hex::encode(&m.pubkey).chars().take(8).collect();
                        ui.add(Label::new(RichText::new(hex8).font(FontId::monospace(10.)).color(p.text_dim)).selectable(false));
                        let (status, color) = if m.online {
                            (tr(lang, Key::MemberOnline).to_string(), Color32::from_rgb(0x00, 0xAA, 0x00))
                        } else {
                            (last_seen_text(m.last_seen, lang).unwrap_or_else(|| tr(lang, Key::LastSeenNever).to_string()), p.text_dim)
                        };
                        ui.add(Label::new(RichText::new(status).size(10.).color(color)).selectable(false));
                    });
                });
            })
            .response
        })
        .inner;
    if !manage {
        return None;
    }
    let mut out = None;
    let toggle = (resp.clicked() || resp.secondary_clicked()).then_some(SetOpenCommand::Toggle);
    Popup::menu(&resp).open_memory(toggle).show(|ui| {
        ui.set_min_width(170.);
        if role == Role::Owner {
            ui.label(tr(lang, Key::CannotActOnOwner));
            return;
        }
        let (pubkey, name) = (m.pubkey.clone(), name.clone());
        if ui.button(tr(lang, Key::MenuChangeRole)).clicked() {
            // The actual role, so a read-only member (no radio picked) can be set back to user.
            let current = role;
            out = Some(Dialog::Role { pubkey: pubkey.clone(), name: name.clone(), current, pick: current });
        }
        if ui.button(tr(lang, Key::MenuBanUser)).clicked() {
            out = Some(Dialog::Ban { pubkey, name });
        }
    });
    out
}

fn act(app: &mut MimirDesktop, id: i64, a: Act, info: GroupRowInfo, stored: (String, String), members: &[Vec<u8>], me: Role) {
    let dialog = match a {
        Act::Message => {
            // ponytail: like the contact panel, "Message" only closes the panel; the chat is already open.
            app.state.info_open = false;
            return;
        }
        Act::Mute => {
            // Storage-only single-row write; an error leaves the toggle as it was.
            if let Err(e) = app.core.set_group_chat_muted(id, !info.muted) {
                app.error_text(format!("set_group_chat_muted: {e}"));
            }
            app.state.chats = build_chat_list(&app.core, app.lang);
            return;
        }
        Act::Edit => {
            let stored_avatar = app.core.get_group_chat(id).and_then(|g| resolve(&app.paths.core_dir, &g.avatar));
            Dialog::Edit(EditDialog { name: stored.0.clone(), description: stored.1.clone(), stored, busy: false, error: None, avatar: Default::default(), stored_avatar })
        }
        Act::Invite => {
            let mut exclude = members.to_vec();
            exclude.push(app.core.public_key());
            Dialog::Invite(ContactPicker::new(&app.core, &exclude))
        }
        Act::Leave => Dialog::Leave,
        Act::Delete if me == Role::Owner => Dialog::Delete,
        Act::Delete => return,
        Act::DeleteLocal => Dialog::DeleteLocal,
        Act::Clear => Dialog::Clear,
    };
    app.state.group_ui.dialog = Some((id, dialog));
}

/// The ⋮ menu of a group's header: mute, clear history, then leave or delete as our role allows.
pub fn menu(app: &mut MimirDesktop, more: &Response, id: i64) {
    let lang = app.lang;
    let Some((_, info, read_only)) = row_info(app, id) else { return };
    let Some(c) = take_cache(app, id) else { return };
    let (me, stored, members) = (c.me, (c.chat.name.clone(), c.chat.description.clone()), c.members.iter().map(|m| m.pubkey.clone()).collect::<Vec<_>>());
    app.state.group_ui.cache = Some(c);
    let mut picked = None;
    Popup::menu(more).show(|ui| {
        ui.set_min_width(170.);
        let mut items = vec![];
        if !read_only {
            items.push((Act::Mute, if info.muted { Key::Unmute } else { Key::Mute }));
        }
        items.push((Act::Clear, Key::ClearHistory));
        items.push(exit_action(me, info, read_only));
        for (a, key) in items {
            if ui.button(tr(lang, key)).clicked() {
                picked = Some(a);
            }
        }
    });
    if let Some(a) = picked {
        act(app, id, a, info, stored, &members, me);
    }
}

/// The chat is gone (left, deleted here or elsewhere): close it and its panel.
pub fn closed(app: &mut MimirDesktop, id: i64) {
    if app.state.selected == Some(ChatKey::Group(id)) {
        app.state.selected = None;
        app.state.open_chat = None;
        app.state.info_open = false;
    }
    let g = &mut app.state.group_ui;
    g.cache = None;
    g.qr = None;
    if g.dialog.as_ref().is_some_and(|d| d.0 == id) {
        g.dialog = None;
        g.busy = false;
    }
}

/// Our own role change or ban went through. The mediator does not echo it to the actor (upstream
/// `change_member_role` writes nothing locally), so the member row is written here: a storage-only single-row write.
/// A ban is the soft "gone" of the core's own `ban_member`: the member leaves `get_members` but still
/// names their old messages. Then the members are re-read.
pub fn member_changed(app: &mut MimirDesktop, chat_id: i64, pubkey: &[u8], perms: u8) {
    // ponytail: a failed local write only leaves the old role shown until the next member sync.
    let _ = if perms == 0x01 {
        app.core.delete_member(chat_id, pubkey.to_vec())
    } else {
        let (online, last_seen) = app.core.get_member_info(chat_id, pubkey.to_vec()).map_or((false, 0), |m| (m.online, m.last_seen));
        app.core.update_members_bulk(chat_id, vec![MemberUpdate { pubkey: pubkey.to_vec(), permissions: perms as u32, online, last_seen }])
    };
    op_done(app);
    app.state.group_ui.invalidate();
    if let Some(chat) = app.state.open_chat.as_mut().filter(|c| c.key == ChatKey::Group(chat_id)) {
        crate::app::reload_members(&app.core, chat, chat_id);
    }
}

/// A failed mediator call of a confirmed dialog closes it (the status line says why).
pub fn op_failed(app: &mut MimirDesktop, what: &str) {
    if matches!(what, "leave_group" | "delete_group" | "delete_group_chat" | "change_member_role") {
        op_done(app);
    }
}

/// The confirmed dialog's mediator call answered: it closes.
fn op_done(app: &mut MimirDesktop) {
    let g = &mut app.state.group_ui;
    if g.busy {
        g.busy = false;
        g.dialog = None;
    }
}

/// True while the edit dialog waits for `update_group_info`.
pub fn editing(app: &MimirDesktop) -> bool {
    matches!(&app.state.group_ui.dialog, Some((_, Dialog::Edit(d))) if d.busy)
}

/// `update_group_info` failed: the dialog shows why and can be used again.
pub fn edit_failed(app: &mut MimirDesktop, err: &str) {
    if let Some((_, Dialog::Edit(d))) = &mut app.state.group_ui.dialog {
        d.busy = false;
        d.error = Some(format!("{} {err}", tr(app.lang, Key::FailedToUpdateChatInfo)));
    }
}

/// `update_group_info` succeeded (the core has written the fields locally): close the dialog, and mark the stored
/// ChatInfoChanged rows as applied so an older rename is not re-applied over ours after a restart (we get no echo).
pub fn edit_done(app: &mut MimirDesktop, id: i64) {
    // A new avatar may be written over the old file within the same mtime tick.
    if let Some(p) = app.core.get_group_chat(id).and_then(|g| resolve(&app.paths.core_dir, &g.avatar)) {
        app.state.thumbs.invalidate(&p);
    }
    if matches!(&app.state.group_ui.dialog, Some((d_id, Dialog::Edit(_))) if *d_id == id) {
        app.state.group_ui.dialog = None;
    }
    if let Some(row) = group::newest_chat_info(&app.core, id) {
        app.mark_chat_info_applied(id, row);
    }
    app.state.group_ui.invalidate();
    app.notice(Key::ChatInfoUpdated);
}

/// The worker cleared the chat's history: reload what the open chat shows and close the dialog.
pub fn history_cleared(app: &mut MimirDesktop, id: i64) {
    if let Some(chat) = app.state.open_chat.as_mut().filter(|c| c.key == ChatKey::Group(id)) {
        chat.timeline = Timeline::open(&app.core, chat.key);
        chat.reactions.clear();
        chat.day_key = (usize::MAX, 0, 0);
    }
    app.state.chats = build_chat_list(&app.core, app.lang);
    op_done(app);
}

/// Runs a confirmed dialog: the job to hand to the worker, if the confirmation has one.
fn apply(app: &mut MimirDesktop, id: i64, d: &Dialog) -> Option<GroupOp> {
    let op = match d {
        Dialog::Leave => Some(GroupOp::Leave(id)),
        Dialog::Delete => Some(GroupOp::Delete(id)),
        Dialog::DeleteLocal => Some(GroupOp::DeleteLocal(id)),
        Dialog::Clear => Some(GroupOp::ClearHistory { chat_id: id, files_dir: app.paths.files_dir.clone() }),
        Dialog::Ban { pubkey, name } => Some(GroupOp::SetRole { chat_id: id, pubkey: pubkey.clone(), perms: 0x01, name: name.clone() }),
        Dialog::Role { pubkey, name, current, pick } => (pick != current).then(|| {
            let perms = match pick {
                Role::Admin => 0x40,
                Role::Mod => 0x20,
                _ => 0x10,
            };
            GroupOp::SetRole { chat_id: id, pubkey: pubkey.clone(), perms, name: name.clone() }
        }),
        Dialog::Edit(_) | Dialog::Invite(_) => None,
    };
    app.state.chats = build_chat_list(&app.core, app.lang);
    op
}

/// The invite-link QR, then the open dialog.
pub fn dialogs(app: &mut MimirDesktop, ctx: &egui::Context, id: i64) {
    let lang = app.lang;
    if let Some(tex) = app.state.group_ui.qr.clone() {
        let r = dialog(ctx, Id::NULL.with("group_qr")).show(ctx, |ui| {
            ui.set_width(260.);
            let p = palette(ui.visuals().dark_mode);
            dialog_title(ui, tr(lang, Key::InviteLink));
            ui.image((tex.id(), vec2(260., 260.)));
            ui.add_space(8.);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.add(primary_button(&p, tr(lang, Key::Ok))).clicked()).inner
        });
        if r.inner || r.should_close() {
            app.state.group_ui.qr = None;
        }
        return;
    }
    if app.state.group_ui.dialog.as_ref().is_some_and(|d| d.0 != id) {
        app.state.group_ui.dialog = None;
        app.state.group_ui.busy = false;
    }
    let channel = row_info(app, id).is_some_and(|r| r.1.is_channel);
    let busy = app.state.group_ui.busy;
    let Some((_, d)) = &mut app.state.group_ui.dialog else { return };
    match d {
        Dialog::Invite(picker) => {
            if let Some(pick) = picker.show(ctx, &mut app.state.thumbs, &app.paths.core_dir, "group_invite", tr(lang, Key::AddMember), lang) {
                app.state.group_ui.dialog = None;
                if let Pick::Contact { pubkey, name } = pick {
                    submit(app, GroupOp::Invite { chat_id: id, pubkey, name });
                }
            }
            return;
        }
        Dialog::Edit(_) => return edit_dialog(app, ctx, id, channel),
        _ => {}
    }
    let (mut ok, mut cancel) = (false, false);
    let r = dialog(ctx, Id::NULL.with("group_dialog")).show(ctx, |ui| {
        ui.set_width(300.);
        let p = palette(ui.visuals().dark_mode);
        let (title, confirm, red) = match d {
            Dialog::Leave => (tr(lang, if channel { Key::LeaveChannel } else { Key::LeaveGroup }).to_string(), Key::Leave, true),
            Dialog::Delete => (tr(lang, if channel { Key::DeleteChannel } else { Key::DeleteGroup }).to_string(), Key::Delete, true),
            Dialog::DeleteLocal => (tr(lang, Key::Delete).to_string(), Key::Delete, true),
            Dialog::Clear => (tr(lang, Key::ClearHistory).to_string(), Key::Ok, true),
            Dialog::Ban { .. } => (tr(lang, Key::BanUserTitle).to_string(), Key::Ban, true),
            Dialog::Role { name, .. } => (trf(lang, Key::ChangeRoleTitle, &[name]), Key::Ok, false),
            Dialog::Edit(_) | Dialog::Invite(_) => return,
        };
        dialog_title(ui, &title);
        match d {
            Dialog::Leave => {
                ui.label(tr(lang, Key::ConfirmLeaveGroup));
            }
            Dialog::Delete => {
                ui.label(tr(lang, Key::ConfirmDeleteGroup));
            }
            Dialog::DeleteLocal => {
                ui.label(tr(lang, Key::DeleteLocalChatQuestion));
            }
            Dialog::Clear => {
                ui.label(tr(lang, Key::ClearHistoryQuestion));
            }
            Dialog::Ban { name, .. } => {
                ui.label(trf(lang, Key::BanUserConfirmation, &[name]));
            }
            Dialog::Role { pick, .. } => {
                ui.add_enabled_ui(!busy, |ui| {
                    for role in [Role::User, Role::Mod, Role::Admin] {
                        ui.radio_value(pick, role, role_name(role, lang));
                    }
                });
            }
            Dialog::Edit(_) | Dialog::Invite(_) => {}
        }
        ui.add_space(8.);
        if busy {
            ui.add(Spinner::new());
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let button = if red { danger_button } else { primary_button };
            // Disabled while the mediator call runs: a second click must not submit it again.
            ok = ui.add_enabled(!busy, button(&p, tr(lang, confirm))).clicked();
            cancel = ui.add_enabled(!busy, Button::new(tr(lang, Key::Cancel))).clicked();
        });
    });
    if ok || cancel || (r.should_close() && !busy) {
        if let Some((_, d)) = app.state.group_ui.dialog.take() {
            if let Some(op) = ok.then(|| apply(app, id, &d)).flatten() {
                submit(app, op);
                app.state.group_ui.dialog = Some((id, d));
                app.state.group_ui.busy = true;
            }
        }
    }
}

/// Edit name and description (Android GroupChatEditActivity, edit mode); stays open with a spinner until the
/// worker's result closes it or shows its error.
fn edit_dialog(app: &mut MimirDesktop, ctx: &egui::Context, id: i64, channel: bool) {
    let (lang, started) = (app.lang, app.state.core_started);
    if let Some((_, Dialog::Edit(d))) = &mut app.state.group_ui.dialog {
        if let Some(e) = d.avatar.poll(ctx) {
            d.error = Some(avatar::error_text(e, lang).to_string());
        }
    }
    let (mut save, mut cancel) = (false, false);
    let r = dialog(ctx, Id::NULL.with("group_edit")).show(ctx, |ui| {
        ui.set_width(420.);
        let p = palette(ui.visuals().dark_mode);
        let Some((_, Dialog::Edit(d))) = &mut app.state.group_ui.dialog else { return };
        dialog_title(ui, tr(lang, if channel { Key::EditChannelInfo } else { Key::EditGroupInfo }));
        ui.label(RichText::new(tr(lang, Key::EditChatInfoHint)).size(12.).color(p.text_dim));
        ui.add_enabled_ui(!d.busy, |ui| {
            ui.vertical_centered(|ui| {
                let base = avatar_seed_color(id.to_string().as_bytes(), ui.visuals().dark_mode);
                if pick_button(ui, &mut app.state.thumbs, &d.avatar, d.stored_avatar.as_deref(), base, 128., tr(lang, Key::ChangeAvatar)) {
                    d.avatar.start(ui.ctx(), tr(lang, Key::Image), GROUP_MAX);
                }
            });
            let l = field_label(ui, &p, tr(lang, Key::GroupName));
            if counted(ui, &mut d.name, tr(lang, Key::GroupNameHint), false, MAX_NAME, &l) {
                d.error = None;
            }
            let l = field_label(ui, &p, tr(lang, Key::GroupDescription));
            counted(ui, &mut d.description, tr(lang, Key::GroupDescriptionHint), true, MAX_DESCRIPTION, &l);
        });
        if let Some(e) = &d.error {
            ui.colored_label(p.danger, e);
        }
        if !started {
            ui.weak(tr(lang, Key::NotStarted));
        }
        ui.add_space(8.);
        if d.busy {
            ui.horizontal(|ui| {
                ui.add(Spinner::new());
                ui.label(tr(lang, Key::SavingChanges));
            });
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            save = ui.add_enabled(!d.busy, primary_button(&p, tr(lang, Key::SaveChanges))).clicked();
            cancel = ui.add_enabled(!d.busy, Button::new(tr(lang, Key::Cancel))).clicked();
        });
    });
    let Some((_, Dialog::Edit(d))) = &mut app.state.group_ui.dialog else { return };
    if save {
        let (name, description) = (d.name.trim().to_string(), d.description.trim().to_string());
        if name.is_empty() {
            d.error = Some(tr(lang, Key::GroupNameRequired).to_string());
            return;
        }
        let name = (name != d.stored.0.trim()).then_some(name);
        let description = (description != d.stored.1.trim()).then_some(description);
        let avatar = d.avatar.bytes.clone();
        if name.is_none() && description.is_none() && avatar.is_none() {
            app.state.group_ui.dialog = None;
            app.notice(Key::NoChangesMade);
            return;
        }
        d.busy = true;
        d.error = None;
        submit(app, GroupOp::UpdateInfo { chat_id: id, name, description, avatar });
    } else if cancel || (r.should_close() && !d.busy) {
        app.state.group_ui.dialog = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{core::events::Event, model::chat_list::build_chat_list, settings::Settings, test_support::{button_fill, test_harness}};
    use egui_kittest::{kittest::{NodeT as _, Queryable as _}, Harness};
    use std::sync::mpsc::{channel, Sender};

    type H = Harness<'static, MimirDesktop>;
    const OWNER: u32 = 0x80;
    const ADMIN: u32 = 0x40;
    const USER: u32 = 0x10;
    const ANYA: [u8; 32] = [1; 32];
    const BORIS: [u8; 32] = [2; 32];
    const MEDIATOR: [u8; 32] = [0xA1; 32];
    const KEY: [u8; 32] = [5; 32];
    const PUBLIC: i32 = 2;
    const CHANNEL: i32 = 1;

    fn now_ms() -> i64 {
        time::OffsetDateTime::now_utc().unix_timestamp() * 1000
    }

    /// Group 7 «Семья» open with its info panel: we (perms `me`, «Я»), Аня (user, online), Борис (admin; the owner
    /// when we are not). The app reads a fresh event channel: worker results are injected through the sender.
    fn group(me: u32, flags: i32, description: &str) -> (H, Sender<Event>) {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        h.set_size(vec2(1200., 900.));
        let core = h.state().core.clone();
        let my = core.public_key();
        let we_own = me & 0x80 != 0;
        let owner = if we_own { my.clone() } else { BORIS.to_vec() };
        core.save_group_chat(7, "Семья".into(), description.into(), None, MEDIATOR.to_vec(), owner, KEY.to_vec(), flags).unwrap();
        core.update_members_bulk(7, vec![
            MemberUpdate { pubkey: my.clone(), permissions: me, online: false, last_seen: 0 },
            MemberUpdate { pubkey: ANYA.to_vec(), permissions: USER, online: true, last_seen: 0 },
            MemberUpdate { pubkey: BORIS.to_vec(), permissions: if we_own { ADMIN } else { OWNER }, online: false, last_seen: now_ms() - 60_000 },
        ])
        .unwrap();
        for (k, n) in [(my, "Я"), (ANYA.to_vec(), "Аня"), (BORIS.to_vec(), "Борис")] {
            core.update_member_info(7, k, Some(n.into()), None, None).unwrap();
        }
        let (tx, rx) = channel();
        h.state_mut().rx = rx;
        h.state_mut().state.chats = build_chat_list(&core, Lang::Ru);
        h.state_mut().state.selected = Some(ChatKey::Group(7));
        h.state_mut().state.info_open = true;
        h.run();
        (h, tx)
    }

    fn panel_nodes<'a>(h: &'a H, label: &'a str) -> impl Iterator<Item = egui::Rect> + 'a {
        h.query_all_by_label(label).map(|n| n.rect()).filter(|r| r.left() >= 1200. - crate::ui::info_panel::WIDTH)
    }

    fn in_panel(h: &H, label: &str) -> bool {
        panel_nodes(h, label).next().is_some()
    }

    fn y(h: &H, label: &str) -> f32 {
        panel_nodes(h, label).next().unwrap_or_else(|| panic!("{label} not in the panel")).center().y
    }

    fn submitted(h: &H) -> Vec<GroupOp> {
        h.state().state.group_ui.submitted.clone()
    }

    fn status(h: &H) -> Option<String> {
        h.state().state.status.as_ref().map(|s| s.0.clone())
    }

    fn member_menu(h: &mut H, name: &str) {
        let r = panel_nodes(h, name).next().unwrap();
        h.query_all_by_label(name).find(|n| n.rect() == r).unwrap().click_secondary();
        h.run();
    }

    #[test]
    fn header_and_counts() {
        let (h, _) = group(OWNER, 0, "Наша семья");
        assert!(in_panel(&h, "Семья"));
        assert!(in_panel(&h, "3 участников, 1 в сети"));
        assert!(in_panel(&h, "Описание") && in_panel(&h, "Наша семья"));
        let (mut h, tx) = group(OWNER, 0, "");
        assert!(!in_panel(&h, "Описание"), "no description section when it is empty");
        h.state().core.update_member_online(7, ANYA.to_vec(), false, 0).unwrap();
        tx.send(Event::GroupMemberOnline { chat_id: 7, pubkey: ANYA.to_vec(), online: false }).unwrap();
        h.run();
        assert!(in_panel(&h, "3 участников"), "nobody online: members only");
    }

    #[test]
    fn members_sorted_and_badged() {
        let (mut h, tx) = group(OWNER, 0, "");
        let core = h.state().core.clone();
        core.update_members_bulk(7, vec![
            MemberUpdate { pubkey: vec![4; 32], permissions: 0x20, online: false, last_seen: 0 },
            MemberUpdate { pubkey: vec![6; 32], permissions: 0x08, online: false, last_seen: 0 },
        ])
        .unwrap();
        core.update_member_info(7, vec![4; 32], Some("Оля".into()), None, None).unwrap();
        tx.send(Event::GroupMembersChanged { chat_id: 7 }).unwrap();
        h.run();
        // Online first, then the owner, then admins and moderators, then by name.
        let order = ["Аня", "Я", "Борис", "Оля", "06060606"].map(|n| y(&h, n));
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}");
        let badge = |h: &H, name: &str| {
            let at = y(h, name);
            ["Владелец", "Админ", "Модератор", "Только чтение"].into_iter().find(|b| panel_nodes(h, b).any(|r| (r.center().y - at).abs() < 4.))
        };
        assert_eq!(badge(&h, "Аня"), None, "users have no badge");
        assert_eq!(badge(&h, "Я"), Some("Владелец"));
        assert_eq!(badge(&h, "Борис"), Some("Админ"));
        assert_eq!(badge(&h, "Оля"), Some("Модератор"));
        assert_eq!(badge(&h, "06060606"), Some("Только чтение"));
        let between = |label: &str, a: &str, b: &str| panel_nodes(&h, label).any(|r| r.center().y > y(&h, a) && r.center().y < y(&h, b));
        assert!(between("В сети", "Аня", "Я"), "online status under Аня");
        assert!(h.get_all_by_label_contains("был(а) сегодня").any(|n| n.rect().center().y > y(&h, "Борис") && n.rect().center().y < y(&h, "Оля")));
        assert!(between("Не был онлайн", "Оля", "06060606"));
        assert!(in_panel(&h, "01010101"), "8-hex key");
        assert_eq!(panel_nodes(&h, "В сети").count(), 1);
    }

    #[test]
    fn actions_by_role() {
        let (mut h, _) = group(OWNER, 0, "");
        assert!(in_panel(&h, "Изменить") && !in_panel(&h, "Покинуть группу"));
        h.get_by_label("Удалить группу").click();
        h.run();
        assert!(h.query_by_label("Вы уверены, что хотите удалить группу? Это безвозвратно, и все участники будут изгнаны из неё.").is_some());
        assert_eq!(button_fill(&h, "Удалить"), Some(crate::ui::theme::palette(true).danger));
        h.get_by_label("Удалить").click();
        h.run_steps(3); // the spinner keeps repainting while the call runs
        assert_eq!(submitted(&h), [GroupOp::Delete(7)]);

        let (mut h, _) = group(USER, 0, "");
        assert!(!in_panel(&h, "Изменить") && !in_panel(&h, "Удалить группу"));
        member_menu(&mut h, "Аня");
        assert!(h.query_by_label("Изменить роль").is_none(), "no member actions for a plain member");
        h.get_by_label("Покинуть группу").click();
        h.run();
        assert!(h.query_by_label("Вы уверены, что хотите выйти из группы? Без приглашения вы не сможете в неё перезайти.").is_some());
        assert_eq!(button_fill(&h, "Выйти"), Some(crate::ui::theme::palette(true).danger));
        h.get_by_label("Выйти").click();
        h.run_steps(3);
        assert_eq!(submitted(&h), [GroupOp::Leave(7)]);

        let (mut h, _) = group(ADMIN, 0, "");
        assert!(in_panel(&h, "Изменить") && in_panel(&h, "Покинуть группу"));
        member_menu(&mut h, "Борис");
        assert!(h.query_by_label("Нельзя изменить права владельца чата").is_some());
        assert!(h.query_by_label("Изменить роль").is_none(), "not on the owner");
        h.key_press(egui::Key::Escape);
        h.run();
        member_menu(&mut h, "Аня");
        assert!(h.query_by_label("Изменить роль").is_some() && h.query_by_label("Забанить пользователя").is_some());
        h.key_press(egui::Key::Escape);
        h.run();
        member_menu(&mut h, "Я");
        assert!(h.query_by_label("Изменить роль").is_none(), "not on ourselves");
    }

    #[test]
    fn change_role_and_ban() {
        let (mut h, tx) = group(OWNER, 0, "");
        let core = h.state().core.clone();
        member_menu(&mut h, "Борис");
        h.get_by_label("Изменить роль").click();
        h.run();
        assert!(h.query_by_label("Изменить роль Борис").is_some());
        h.get_by_label("OK").click();
        h.run();
        assert!(submitted(&h).is_empty(), "the current role (admin) is preselected: OK changes nothing");

        member_menu(&mut h, "Аня");
        h.get_by_label("Изменить роль").click();
        h.run();
        h.get_by_role_and_label(egui::accesskit::Role::RadioButton, "Модератор").click();
        h.run();
        h.get_by_label("OK").click();
        h.run_steps(3); // the spinner keeps repainting
        let role = GroupOp::SetRole { chat_id: 7, pubkey: ANYA.to_vec(), perms: 0x20, name: "Аня".into() };
        assert_eq!(submitted(&h), [role.clone()]);
        assert_eq!(status(&h), None, "nothing reported before the worker answers");
        assert!(h.query_by_label("Изменить роль Аня").is_some(), "stays open while the mediator answers");
        assert!(h.get_by_label("OK").accesskit_node().is_disabled(), "no second submit");
        h.get_by_label("OK").click();
        h.run_steps(3);
        assert_eq!(submitted(&h).len(), 1);

        // No echo from the mediator: the result itself updates the member.
        tx.send(Event::WorkDone(WorkResult::RoleChanged { chat_id: 7, pubkey: ANYA.to_vec(), perms: 0x20, name: "Аня".into() })).unwrap();
        h.run();
        assert!(h.query_by_label("Изменить роль Аня").is_none(), "the result closes the dialog");
        assert_eq!(status(&h).as_deref(), Some("Роль Аня изменена на Модератор"));
        let anya = core.get_member_info(7, ANYA.to_vec()).unwrap();
        assert_eq!((anya.permissions, anya.online), (0x20, true), "written locally, presence kept");
        let at = y(&h, "Аня");
        assert!(panel_nodes(&h, "Модератор").any(|r| (r.center().y - at).abs() < 4.), "badge shown");
        member_menu(&mut h, "Аня");
        h.get_by_label("Изменить роль").click();
        h.run();
        h.get_by_label("OK").click();
        h.run();
        assert_eq!(submitted(&h).len(), 1, "the new role is preselected");

        member_menu(&mut h, "Аня");
        h.get_by_label("Забанить пользователя").click();
        h.run();
        assert!(h.query_by_label("Вы уверены, что хотите забанить Аня?").is_some());
        assert_eq!(button_fill(&h, "Забанить"), Some(crate::ui::theme::palette(true).danger));
        h.get_by_label("Забанить").click();
        h.run_steps(3);
        let ban = GroupOp::SetRole { chat_id: 7, pubkey: ANYA.to_vec(), perms: 0x01, name: "Аня".into() };
        assert_eq!(submitted(&h), [role, ban]);
        assert!(h.get_by_label("Забанить").accesskit_node().is_disabled());
        tx.send(Event::WorkDone(WorkResult::Banned { chat_id: 7, pubkey: ANYA.to_vec(), name: "Аня".into() })).unwrap();
        h.run();
        assert_eq!(status(&h).as_deref(), Some("Пользователь забанен: Аня"));
        assert!(!in_panel(&h, "Аня"), "gone from the members");
        assert!(in_panel(&h, "2 участников"), "and from the count");
        assert_eq!(core.get_members(7).len(), 2);
        assert!(core.get_member_info(7, ANYA.to_vec()).is_some(), "still names her old messages");

        member_menu(&mut h, "Борис");
        h.get_by_label("Изменить роль").click();
        h.run();
        h.get_by_role_and_label(egui::accesskit::Role::RadioButton, "Пользователь").click();
        h.run();
        h.get_by_label("OK").click();
        h.run_steps(3);
        tx.send(Event::WorkDone(WorkResult::Failed { what: "change_member_role", err: "denied".into() })).unwrap();
        h.run();
        assert!(h.query_by_label("Изменить роль Борис").is_none(), "a failure closes it too");
        assert_eq!(status(&h).as_deref(), Some("change_member_role: denied"));
    }

    #[test]
    fn read_only_member_can_be_set_back_to_user() {
        let (mut h, tx) = group(OWNER, 0, "");
        h.state().core.update_members_bulk(7, vec![MemberUpdate { pubkey: vec![6; 32], permissions: 0x08, online: false, last_seen: 0 }]).unwrap();
        tx.send(Event::GroupMembersChanged { chat_id: 7 }).unwrap();
        h.run();
        member_menu(&mut h, "06060606");
        h.get_by_label("Изменить роль").click();
        h.run();
        h.get_by_role_and_label(egui::accesskit::Role::RadioButton, "Пользователь").click();
        h.run();
        h.get_by_label("OK").click();
        h.run_steps(3);
        assert_eq!(submitted(&h), [GroupOp::SetRole { chat_id: 7, pubkey: vec![6; 32], perms: 0x10, name: "06060606".into() }]);
    }

    #[test]
    fn leave_confirm_waits_for_the_result() {
        let (mut h, tx) = group(USER, 0, "");
        h.get_by_label("Покинуть группу").click();
        h.run();
        h.get_by_label("Выйти").click();
        h.run_steps(3);
        assert!(h.get_by_label("Выйти").accesskit_node().is_disabled() && h.get_by_label("Отмена").accesskit_node().is_disabled());
        h.key_press(egui::Key::Escape);
        h.run_steps(3);
        assert!(h.query_by_label("Выйти").is_some(), "Escape does not close it while busy");
        assert_eq!(submitted(&h), [GroupOp::Leave(7)]);
        tx.send(Event::WorkDone(WorkResult::Failed { what: "leave_group", err: "timeout".into() })).unwrap();
        h.run();
        assert!(h.query_by_label("Выйти").is_none());
    }

    #[test]
    fn group_avatar_in_create_and_edit() {
        use crate::{media::avatar::{set_picked, AvatarError, GROUP_MAX}, test_support::{png_bytes, run_until, start_app}};
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("big.png");
        std::fs::write(&src, png_bytes(3000, 1000, [90, 160, 60])).unwrap();
        // Create: the prepared avatar goes to create_group.
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        let ctx = h.ctx.clone();
        start_app(h.state_mut(), &ctx);
        h.state_mut().state.core_started = true;
        h.state().core.add_mediator(MEDIATOR.to_vec(), "Семья".into()).unwrap();
        crate::ui::new_group::open(h.state_mut(), false);
        h.run();
        set_picked(src.clone());
        h.get_by_label("Изменить аватар").click();
        run_until(&mut h, "the picked avatar", |h| h.state().state.new_group.as_ref().is_some_and(|d| d.avatar.bytes.is_some()));
        h.get_by_label("Имя группы:").click();
        h.get_by_label("Имя группы:").type_text("Дача");
        h.run();
        let (_tx, rx) = channel();
        h.state_mut().rx = rx; // the real job reaches no mediator; its result is not read
        h.get_by_role_and_label(egui::Role::Button, "Создать группу").click();
        h.run_steps(3);
        let d = h.state().state.new_group.as_ref().unwrap();
        let sent = d.submitted_avatar.clone().expect("create_group got an avatar");
        assert!(sent.len() <= GROUP_MAX && Some(&sent) == d.avatar.bytes.as_ref());
        assert_eq!(image::load_from_memory(&sent).unwrap().width(), 256);
        // A picture no quality can fit: Android's message, nothing kept.
        let (tx, rx) = channel();
        h.state_mut().state.new_group.as_mut().unwrap().busy = false;
        h.state_mut().state.new_group.as_mut().unwrap().avatar = Default::default();
        h.state_mut().state.new_group.as_mut().unwrap().avatar.picking = Some(rx);
        tx.send(Err(AvatarError::TooLarge)).unwrap();
        h.run();
        assert!(h.query_by_label("Аватар слишком велик!").is_some());
        assert!(h.state().state.new_group.as_ref().unwrap().avatar.bytes.is_none());

        // Edit: only the avatar changed, so only it goes to update_group_info.
        let (mut h, _tx) = group(OWNER, 0, "Наша семья");
        h.get_by_label("Изменить").click();
        h.run();
        set_picked(src);
        h.get_by_label("Изменить аватар").click();
        run_until(&mut h, "the picked avatar", |h| matches!(&h.state().state.group_ui.dialog, Some((_, Dialog::Edit(d))) if d.avatar.bytes.is_some()));
        h.get_by_label("Сохранить изменения").click();
        h.run_steps(3);
        let [GroupOp::UpdateInfo { chat_id: 7, name: None, description: None, avatar: Some(sent) }] = &submitted(&h)[..] else { panic!("{:?}", submitted(&h)) };
        assert!(sent.len() <= GROUP_MAX);
        // Undecodable: Android's error_loading_image.
        let (tx, rx) = channel();
        if let Some((_, Dialog::Edit(d))) = &mut h.state_mut().state.group_ui.dialog {
            d.busy = false;
            d.avatar.picking = Some(rx);
        }
        tx.send(Err(AvatarError::Unreadable)).unwrap();
        h.run();
        assert!(h.query_by_label("Ошибка загрузки изображения!").is_some());
    }

    #[test]
    fn edit_group_info() {
        let (mut h, tx) = group(OWNER, 0, "Наша семья");
        h.get_by_label("Изменить").click();
        h.run();
        assert!(h.query_by_label("Изменить данные группы").is_some());
        assert_eq!(h.get_by_label("Имя группы:").value().as_deref(), Some("Семья"));
        h.get_by_label("Сохранить изменения").click();
        h.run();
        assert!(h.query_by_label("Изменить данные группы").is_none(), "nothing changed: closed");
        assert_eq!(status(&h).as_deref(), Some("Данные не изменены"));
        assert!(submitted(&h).is_empty());

        h.get_by_label("Изменить").click();
        h.run();
        h.get_by_label("Имя группы:").click();
        h.get_by_label("Имя группы:").type_text(&"ы".repeat(30));
        h.run();
        assert!(h.query_by_label("25/25").is_some(), "same limit as the create dialog");
        h.get_by_label("Отмена").click();
        h.run();
        assert!(submitted(&h).is_empty());
        h.get_by_label("Изменить").click();
        h.run();
        h.get_by_label("Описание группы:").click();
        h.get_by_label("Описание группы:").type_text("!");
        h.run();
        h.get_by_label("Сохранить изменения").click();
        h.run_steps(3); // the spinner keeps repainting
        assert_eq!(submitted(&h), [GroupOp::UpdateInfo { chat_id: 7, name: None, description: Some("Наша семья!".into()), avatar: None }], "only the changed field");
        assert!(h.query_by_label("Сохранение изменений…").is_some());

        tx.send(Event::WorkDone(WorkResult::Failed { what: "update_group_info", err: "timeout".into() })).unwrap();
        h.run_steps(3);
        assert!(h.query_by_label("Ошибка изменения данных чата timeout").is_some(), "error in the dialog, which stays open");
        assert!(h.query_by_label("Сохранение изменений…").is_none());

        h.get_by_label("Сохранить изменения").click();
        h.run_steps(3);
        assert_eq!(submitted(&h).len(), 2);
        h.state().core.update_group_chat_description(7, "Наша семья!".into()).unwrap(); // what the core writes on success
        tx.send(Event::WorkDone(WorkResult::GroupInfoUpdated { chat_id: 7 })).unwrap();
        h.run();
        assert!(h.query_by_label("Изменить данные группы").is_none(), "the result closes the dialog");
        assert_eq!(status(&h).as_deref(), Some("Данные чата изменены!"));
        assert!(in_panel(&h, "Наша семья!"));

        h.get_by_label("Изменить").click();
        h.run();
        h.get_by_label("Имя группы:").click();
        for _ in 0..5 {
            h.key_press(egui::Key::Backspace);
        }
        h.run();
        h.get_by_label("Сохранить изменения").click();
        h.run();
        assert!(h.query_by_label("Название группы обязательно!").is_some());
        assert_eq!(submitted(&h).len(), 2);
        h.get_by_label("Имя группы:").click();
        h.get_by_label("Имя группы:").type_text("Д");
        h.run();
        assert!(h.query_by_label("Название группы обязательно!").is_none(), "cleared once the name is edited");
    }

    #[test]
    fn own_rename_survives_restart() {
        // Our own update_group_info is not echoed, so after a restart the older ChatInfoChanged
        // of another member must not be applied again over it.
        let (mut h, tx) = group(OWNER, 0, "");
        let core = h.state().core.clone();
        let mut info = vec![0x06, 0x01, 32];
        info.extend(BORIS);
        info.extend([0x20, "Старое".len() as u8]);
        info.extend("Старое".as_bytes());
        let old = core.add_group_message(7, 1, 11, MEDIATOR.to_vec(), 1000, group::SYSTEM, true, info, 0, false).unwrap();
        core.update_group_chat_name(7, "Новое".into()).unwrap(); // what the core writes on success
        tx.send(Event::WorkDone(WorkResult::GroupInfoUpdated { chat_id: 7 })).unwrap();
        h.run();
        let saved = Settings::load(&h.state().paths.settings_file);
        assert_eq!(saved.chat_info_applied.get(&7), Some(&old), "watermark persisted");
        h.state_mut().settings = saved; // as after a restart
        tx.send(Event::GroupMembersChanged { chat_id: 7 }).unwrap();
        h.run();
        assert_eq!(core.get_group_chat(7).unwrap().name, "Новое");
    }

    #[test]
    fn picker_rows_show_avatars() {
        let (mut h, _tx) = group(OWNER, 0, "");
        let core = h.state().core.clone();
        let vera = core.add_contact(vec![8; 32], "Вера".into()).unwrap();
        core.update_contact_avatar(vera, crate::test_support::png_bytes(64, 64, [30, 30, 200])).unwrap();
        core.add_contact(vec![9; 32], "Глеб".into()).unwrap(); // no picture: the placeholder, no panic
        h.get_by_label("Добавить участников").click();
        h.run();
        crate::test_support::run_until(&mut h, "the contact's picture", |h| crate::test_support::textured(h).contains(&vec2(32., 32.)));
        assert!(h.query_by_label("Глеб").is_some());
    }

    #[test]
    fn invite_contact() {
        let (mut h, tx) = group(OWNER, 0, "");
        let core = h.state().core.clone();
        core.add_contact(ANYA.to_vec(), "Аня".into()).unwrap();
        core.add_contact(vec![8; 32], "Вера".into()).unwrap();
        h.get_by_label("Добавить участников").click();
        h.run();
        assert!(h.query_by_label("Добавить участника").is_some());
        let Some((_, Dialog::Invite(picker))) = &h.state().state.group_ui.dialog else { panic!("picker open") };
        assert_eq!(picker.contacts.iter().map(|c| c.1.as_str()).collect::<Vec<_>>(), ["Вера"], "current members are not offered");
        h.get_by_label("Вера").click();
        h.run();
        assert!(h.query_by_label("Добавить участника").is_none());
        assert_eq!(submitted(&h), [GroupOp::Invite { chat_id: 7, pubkey: vec![8; 32], name: "Вера".into() }]);
        assert_eq!(status(&h), None, "not optimistic");
        tx.send(Event::WorkDone(WorkResult::Invited { name: "Вера".into() })).unwrap();
        h.run();
        assert_eq!(status(&h).as_deref(), Some("Приглашение отправлено: Вера"));

        let (h, _) = group(USER, 0, "");
        assert!(!in_panel(&h, "Добавить участников"), "owner or admin only");
        let (h, _) = group(OWNER, CHANNEL | PUBLIC, "");
        assert!(!in_panel(&h, "Добавить участников"), "a public channel is joined by link");
        assert!(!in_panel(&h, "Участники"), "a channel has no member list");
        assert!(in_panel(&h, "Удалить канал"));
        let (h, _) = group(OWNER, CHANNEL, "");
        assert!(in_panel(&h, "Добавить участников"), "a private channel grows by invite");
        assert!(!in_panel(&h, "Участники") && !in_panel(&h, "Аня"), "no roster");
    }

    #[test]
    fn invite_link_for_public_only() {
        let (mut h, _) = group(OWNER, PUBLIC, "");
        let link = encode_group_link(&GroupLink { kind: LinkKind::Group, mediator: MEDIATOR, chat_id: 7, key: KEY });
        assert!(in_panel(&h, "Ссылка приглашения") && in_panel(&h, &link));
        h.get_by_label("Копировать").click();
        h.run();
        assert_eq!(status(&h).as_deref(), Some("Ссылка на группу скопирована"));
        h.get_by_label("QR").click();
        h.run();
        assert!(h.state().state.group_ui.qr.is_some());
        h.get_by_label("OK").click();
        h.run();
        assert!(h.state().state.group_ui.qr.is_none());

        let (h, _) = group(OWNER, 0, "");
        assert!(!in_panel(&h, "Ссылка приглашения"), "a private group grows by invite only");
    }

    #[test]
    fn channel_subscriber_panel() {
        // Somebody else's public channel, read-only by design (join_public_channel sets the flag); a stray admin
        // member row does not make us one: a channel's owner is its stored key.
        let (mut h, _) = group(ADMIN, CHANNEL | PUBLIC, "");
        h.state().core.set_group_chat_read_only(7, true).unwrap();
        h.state_mut().state.chats = build_chat_list(&h.state().core, Lang::Ru);
        h.state_mut().state.group_ui.invalidate();
        h.run();
        for label in ["Сообщение", "Без звука", "Отписаться", "0 подписчиков, 0 в сети"] {
            assert!(in_panel(&h, label), "{label}");
        }
        for label in ["Изменить", "Удалить канал", "Удалить", "Добавить участников", "Участники"] {
            assert!(!in_panel(&h, label), "{label}");
        }
        let link = encode_group_link(&GroupLink { kind: LinkKind::Channel, mediator: MEDIATOR, chat_id: 7, key: KEY });
        assert!(link.starts_with("mimir://c/") && in_panel(&h, &link), "the channel link, Android's encoding");
        h.get_by_label("Копировать").click();
        h.run();
        assert_eq!(status(&h).as_deref(), Some("Ссылка на канал скопирована"));
    }

    #[test]
    fn leave_and_delete_results() {
        for (me, result) in [(USER, WorkResult::Left { chat_id: 7 }), (OWNER, WorkResult::GroupDeleted { chat_id: 7 })] {
            let (mut h, tx) = group(me, 0, "");
            h.state().core.delete_group_chat(7).unwrap(); // the core's local delete
            tx.send(Event::WorkDone(result)).unwrap();
            h.run();
            let s = &h.state().state;
            assert_eq!((s.selected, s.open_chat.is_none(), s.info_open), (None, true, false));
            assert!(!s.chats.iter().any(|r| r.key == ChatKey::Group(7)));
        }
        // leave_group deletes locally even when the mediator call fails.
        let (mut h, tx) = group(USER, 0, "");
        h.state().core.delete_group_chat(7).unwrap();
        tx.send(Event::WorkDone(WorkResult::Failed { what: "leave_group", err: "no mediator".into() })).unwrap();
        h.run();
        assert_eq!(h.state().state.selected, None);
        assert_eq!(status(&h).as_deref(), Some("leave_group: no mediator"));
    }

    #[test]
    fn read_only_delete_is_local() {
        let (mut h, tx) = group(USER, 0, "");
        let core = h.state().core.clone();
        core.set_group_chat_read_only(7, true).unwrap();
        h.state_mut().state.chats = build_chat_list(&core, Lang::Ru);
        h.run();
        for gone in ["Покинуть группу", "Изменить", "Без звука", "Добавить участников"] {
            assert!(!in_panel(&h, gone), "{gone}");
        }
        h.get_by_label("Удалить").click();
        h.run();
        assert!(h.query_by_label("Удалить этот чат с устройства?").is_some());
        h.get_all_by_label("Удалить").last().unwrap().click();
        // The job runs on the real worker (its result goes to the replaced channel): wait for the write, then deliver it.
        crate::test_support::run_until(&mut h, "local delete", |_| core.get_group_chat(7).is_none());
        assert!(h.query_by_label("Удалить этот чат с устройства?").is_some(), "the dialog waits for the result");
        tx.send(Event::WorkDone(WorkResult::GroupDeleted { chat_id: 7 })).unwrap();
        h.run();
        assert_eq!(h.state().state.selected, None);
        assert_eq!(submitted(&h), [GroupOp::DeleteLocal(7)], "a local job only: no mediator call");
    }

    #[test]
    fn owners_channel_deleted_elsewhere_is_deleted_locally() {
        let (mut h, _tx) = group(OWNER, CHANNEL, "");
        let core = h.state().core.clone();
        core.set_group_chat_read_only(7, true).unwrap(); // ChatDeleted from our other device
        h.state_mut().state.chats = build_chat_list(&core, Lang::Ru);
        h.run();
        for gone in ["Удалить канал", "Изменить"] {
            assert!(!in_panel(&h, gone), "{gone}");
        }
        h.get_by_label("Удалить").click();
        h.run();
        assert!(h.query_by_label("Удалить этот чат с устройства?").is_some(), "a local delete, no mediator call");
    }

    #[test]
    fn local_group_jobs_do_not_wait_behind_mediator_calls() {
        let (mut h, _tx) = group(OWNER, 0, "");
        let core = h.state().core.clone();
        let (release, gate) = std::sync::mpsc::channel::<()>();
        // A slow mediator call holds the group lane.
        h.state().group_worker.submit(Box::new(move |_| {
            let _ = gate.recv();
            WorkResult::Done
        }));
        core.add_group_message(7, 2, 12, ANYA.to_vec(), now_ms(), 0, false, b"hello".to_vec(), 0, false).unwrap();
        submit(h.state_mut(), GroupOp::ClearHistory { chat_id: 7, files_dir: std::env::temp_dir() });
        crate::test_support::run_until(&mut h, "history cleared", |_| core.get_group_messages(7, 0, 10).is_empty());
        let _ = release.send(());
    }

    #[test]
    fn mute_and_clear() {
        let (mut h, tx) = group(OWNER, 0, "");
        let core = h.state().core.clone();
        h.get_by_label("Без звука").click();
        h.run();
        assert!(core.get_group_chat(7).unwrap().muted);
        assert!(in_panel(&h, "Включить звук"));

        core.add_group_message(7, 1, 11, ANYA.to_vec(), now_ms(), 1, false, br#"{"name":"a.jpg"}"#.to_vec(), 0, false).unwrap();
        core.add_group_message(7, 2, 12, ANYA.to_vec(), now_ms(), 0, false, b"hello".to_vec(), 0, false).unwrap();
        let files = h.state().paths.files_dir.clone();
        std::fs::create_dir_all(&files).unwrap();
        std::fs::write(files.join("a.jpg"), b"x").unwrap();
        h.state_mut().state.open_chat = None;
        h.run();
        assert!(h.query_by_label("hello").is_some());
        h.get_by_label("⋮").click();
        h.run();
        assert_eq!(h.query_all_by_label("Включить звук").count(), 2, "the menu has the toggle too");
        h.get_by_label("Очистить историю").click();
        h.run();
        assert!(h.query_by_label("hello").is_some(), "not before confirm");
        assert_eq!(button_fill(&h, "OK"), Some(crate::ui::theme::palette(true).danger));
        h.get_by_label("OK").click();
        crate::test_support::run_until(&mut h, "history cleared", |_| core.get_group_messages(7, 0, 10).is_empty() && !files.join("a.jpg").exists());
        assert!(h.query_by_label("hello").is_some(), "the view reloads on the result");
        tx.send(Event::WorkDone(WorkResult::HistoryCleared { chat_id: 7 })).unwrap();
        h.run();
        assert!(h.query_by_label("hello").is_none());
        assert!(h.query_by_label("Очистить историю").is_none(), "dialog closed");
    }
}
