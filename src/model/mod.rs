pub mod attachment;
pub mod chat_key;
pub mod chat_list;
pub mod group;
pub mod group_link;
pub mod links;
pub mod message;
pub mod prefs_xml;
pub mod reactions;
pub mod send;
pub mod timeline;

use chat_key::ChatKey;
use chat_list::ChatRow;
use eframe::egui::Pos2;
use crate::core::{events::Event, worker::WorkResult};
use std::collections::{HashMap, HashSet};
use std::time::Instant;
use timeline::Timeline;

/// Message menu (reaction strip + actions) open over one message.
/// What an action needs to know about its message.
#[derive(Clone)]
pub struct MsgRef {
    pub id: i64,
    pub guid: i64,
    pub text: String,
    /// The attachment's file and the name to save it under, when it is on disk.
    pub file: Option<(std::path::PathBuf, String)>,
    /// What «Переслать» sends; `None` for an attachment whose file is not on disk (nothing to forward).
    pub forward: Option<Forward>,
}

/// A forwarded message: its text, or an attachment's file sent again (same type, mime, name) with its caption.
#[derive(Clone, Debug, PartialEq)]
pub enum Forward {
    Text(String),
    File(crate::media::stage::Staged, String),
}

pub struct Menu {
    pub msg: MsgRef,
    pub pos: Pos2,
    pub expanded: bool,
}

pub struct OpenChat {
    pub key: ChatKey,
    /// Contact public key (our own for Saved Messages); empty for a group.
    pub pubkey: Vec<u8>,
    pub timeline: Timeline,
    /// Reaction chips per message guid, filled when the row is first drawn (cleared on events).
    pub reactions: HashMap<i64, Vec<(String, u32, bool)>>,
    pub menu: Option<Menu>,
    /// Contact's last_seen, read when the chat opens and on connect/disconnect events.
    pub last_seen: i64,
    /// Ids of messages that start a new day, with the (len, first id, last id) it was computed for.
    pub day_starts: HashSet<i64>,
    pub day_key: (usize, i64, i64),
    /// Reply targets by guid (incoming, sender id, text), recomputed with `day_starts`; `None` = not found.
    /// The sender id is the row's `contact_id`: a contact id, or a group `sender_id`.
    pub quotes: HashMap<i64, Option<(bool, i64, String)>>,
    /// A message arrived while the window was unfocused; marked read when focus returns.
    pub unread_while_away: bool,
    /// First unread message when the chat opened; the «Unread messages» separator sits above it.
    pub unread_from: Option<i64>,
    /// Group members (sender names and colours); `None` for a 1:1 chat.
    pub members: Option<group::Members>,
    /// Group subscription state (header badge, read-only); unused for a 1:1 chat.
    pub status: group::GroupStatus,
    /// Group (members, online), or a channel's (subscribers, online), for the header subtitle.
    pub counts: (i32, i32),
}

/// Forward dialog: what is being forwarded and the search box.
pub struct ForwardDialog {
    pub forward: Forward,
    pub query: String,
}

/// Add-contact dialog fields; `error` is the inline validation message.
#[derive(Default)]
pub struct AddContactDialog {
    pub name: String,
    pub key: String,
    pub message: String,
    pub error: Option<crate::i18n::Key>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Dot {
    #[default]
    Red,
    Yellow,
    Green,
}

/// Toolbar connection indicators: red before start, yellow while unknown after it, then fed by core events.
#[derive(Default)]
pub struct NetDots {
    pub ygg: Dot,
    pub tracker: Dot,
    pub mediator: Dot,
}

impl NetDots {
    /// Ygg and tracker dots, and Red -> Yellow at start; the mediator dot follows `AppState::connected_mediators`.
    pub fn apply(&mut self, e: &Event) {
        let on = |ok: bool| if ok { Dot::Green } else { Dot::Red };
        match e {
            // Start reports connectivity (and may announce) before its WorkDone(Started) arrives: keep what is known.
            Event::WorkDone(WorkResult::Started) => {
                for d in [&mut self.ygg, &mut self.tracker, &mut self.mediator] {
                    if *d == Dot::Red {
                        *d = Dot::Yellow;
                    }
                }
            }
            Event::ConnectivityChanged { is_online } => self.ygg = on(*is_online),
            Event::TrackerAnnounce { ok, .. } => self.tracker = on(*ok),
            _ => {}
        }
    }
}

impl AppState {
    /// Feeds a core event to the toolbar dots; the mediator dot is green while any mediator is connected.
    pub fn apply_net(&mut self, e: &Event) {
        self.net.apply(e);
        match e {
            Event::MediatorAuthenticated { mediator_pubkey } => {
                self.connected_mediators.insert(mediator_pubkey.clone());
            }
            Event::MediatorDisconnected { mediator_pubkey } => {
                self.connected_mediators.remove(mediator_pubkey);
            }
            _ => return,
        }
        self.net.mediator = if self.connected_mediators.is_empty() { Dot::Red } else { Dot::Green };
    }
}

#[derive(Default)]
pub struct AppState {
    pub chats: Vec<ChatRow>,
    pub selected: Option<ChatKey>,
    pub info_open: bool,
    pub contact_ui: crate::ui::info_panel::ContactUi,
    pub group_ui: crate::ui::group_info::GroupUi,
    pub net: NetDots,
    pub requests_count: usize,
    /// Pending contact requests (same source as `requests_count`).
    pub requests: Vec<mimir::StoredContactRequest>,
    pub show_requests: bool,
    /// Pending group invites (storage), shown in the invites dialog and counted in the banner.
    pub invites: Vec<mimir::StoredInvite>,
    pub show_invites: bool,
    pub invites_ui: crate::ui::invites::InvitesUi,
    /// Join-by-link dialog, open while Some.
    pub join: Option<crate::ui::join_group::JoinDialog>,
    /// Mediators currently authenticated (MediatorAuthenticated / MediatorDisconnected).
    pub connected_mediators: HashSet<Vec<u8>>,
    pub show_mediators: bool,
    pub mediators_ui: crate::ui::mediators::MediatorsUi,
    pub settings_ui: crate::ui::settings::SettingsUi,
    /// Chat list width last chosen from the window size (the panel keeps a dragged width instead).
    pub list_auto: f32,
    pub add_dialog: AddContactDialog,
    /// Create-group dialog, open while Some.
    pub new_group: Option<crate::ui::new_group::NewGroupDialog>,
    /// Set by toolbar buttons; opens the add-contact dialog.
    pub show_add_contact: bool,
    pub show_profile: bool,
    /// Our stored account avatar path for the main toolbar; `None` until first read (empty = none).
    pub own_avatar: Option<String>,
    /// Name typed on the welcome screen, saved as the account name on «Get started».
    pub welcome_name: String,
    pub profile: crate::ui::profile::Profile,
    pub open_chat: Option<OpenChat>,
    /// Pubkeys of contacts currently connected (ContactConnected / ContactDisconnected).
    pub online: HashSet<Vec<u8>>,
    /// (guid, preview) of the message being replied to; the composer sends it with the next message.
    pub reply_to: Option<(i64, String)>,
    /// (target guid, emoji) chosen in the UI; the open chat sends it next frame.
    pub pending_reaction: Option<(i64, String)>,
    /// (target chat, what) chosen in the forward dialog; sent next frame.
    pub pending_forward: Option<(ChatKey, Forward)>,
    /// Message id awaiting delete confirmation.
    pub confirm_delete: Option<i64>,
    pub forward: Option<ForwardDialog>,
    /// True once the core is started (set on WorkResult::Started); the composer sends nothing before.
    pub core_started: bool,
    /// `MimirDesktop::apply_live` calls so far (tests: settings are re-applied after every start).
    pub live_applied: u32,
    pub composer: crate::ui::composer::Composer,
    /// Last failed job as a status line: (text, hide-after); `None` deadline = stays (core start failure).
    pub status: Option<(String, Option<Instant>)>,
    /// `status` reports a success (shown in the accent colour, not red).
    pub status_ok: bool,
    /// File transfers in progress by message guid (FileProgress); no entry = `Transfer::Idle`.
    pub transfers: HashMap<i64, attachment::Transfer>,
    /// Attachment rows stored by a send job still running, oldest first: (chat, guid). One lane per chat runs its
    /// jobs in order, so the first entry of a chat belongs to the job whose `AttachmentSent` comes next.
    pub sending: Vec<(ChatKey, i64)>,
    /// Attachment send jobs submitted and not yet ended, per chat. Only while one runs is a stored attachment row
    /// ours to mark uploading; rows synced from a sibling device fire the same store event.
    pub pending_sends: HashMap<ChatKey, usize>,
    /// Image textures for bubbles and the viewer, decoded off the UI thread.
    pub thumbs: crate::media::thumbs::ThumbCache,
    /// Full-window image viewer: (file, name to save it under), open while Some.
    pub viewer: Option<(std::path::PathBuf, String)>,
    /// The ffmpeg found at startup; without it the mic is disabled and videos get no poster.
    pub ffmpeg: Option<std::path::PathBuf>,
    /// Video poster frames, made with `ffmpeg` off the UI thread.
    pub posters: crate::media::ffmpeg::Posters,
    /// True while the tray icon is up (`platform::tray::spawn`): closing the window then only hides it.
    pub tray: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Set by an explicit exit (tray, settings): the window close that follows is not cancelled.
    pub quitting: bool,
    /// The window was closed to the tray (or minimized on Wayland) and not shown since: it has no focus.
    pub hidden: bool,
    /// The 1:1 call ringing, being dialled or running (one at a time, as in the core).
    pub call: Option<crate::ui::call::Call>,
    /// The group call being joined or running (never together with `call`).
    pub group_call: Option<crate::ui::call::GroupCall>,
    /// A `mimir://gc/…` link waiting for «Войти» in the join dialog.
    pub join_group_call: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dots_follow_events() {
        use Dot::*;
        let mut s = AppState::default();
        let mut step = |e: Event| {
            s.apply_net(&e);
            (s.net.ygg, s.net.tracker, s.net.mediator)
        };
        assert_eq!(step(Event::GroupCallDisconnected), (Red, Red, Red), "before start");
        assert_eq!(step(Event::WorkDone(WorkResult::Started)), (Yellow, Yellow, Yellow));
        assert_eq!(step(Event::ConnectivityChanged { is_online: true }), (Green, Yellow, Yellow));
        assert_eq!(step(Event::TrackerAnnounce { ok: true, ttl: 60 }), (Green, Green, Yellow));
        assert_eq!(step(Event::MediatorAuthenticated { mediator_pubkey: vec![1] }), (Green, Green, Green));
        assert_eq!(step(Event::MediatorAuthenticated { mediator_pubkey: vec![2] }), (Green, Green, Green));
        assert_eq!(step(Event::MediatorDisconnected { mediator_pubkey: vec![1] }), (Green, Green, Green), "another is still connected");
        assert_eq!(step(Event::MediatorDisconnected { mediator_pubkey: vec![2] }), (Green, Green, Red));
        assert_eq!(step(Event::TrackerAnnounce { ok: false, ttl: 0 }), (Green, Red, Red));
        assert_eq!(step(Event::ConnectivityChanged { is_online: false }), (Red, Red, Red));
    }

    #[test]
    fn events_before_started_are_kept() {
        use Dot::*;
        let mut n = AppState::default();
        // MimirApp::start reports connectivity (and may announce) before its WorkDone(Started) is queued.
        n.apply_net(&Event::ConnectivityChanged { is_online: true });
        n.apply_net(&Event::TrackerAnnounce { ok: true, ttl: 60 });
        n.apply_net(&Event::WorkDone(WorkResult::Started));
        let n = n.net;
        assert_eq!((n.ygg, n.tracker, n.mediator), (Green, Green, Yellow));
        let mut m = AppState::default();
        m.apply_net(&Event::MediatorAuthenticated { mediator_pubkey: vec![1] });
        m.apply_net(&Event::WorkDone(WorkResult::Started));
        let m = m.net;
        assert_eq!((m.ygg, m.tracker, m.mediator), (Yellow, Yellow, Green));
    }
}
