use crate::{
    core::{self, events::{Bridge, Event}, net, worker::{WorkResult, Worker}},
    i18n::{tr, trf, Key, Lang},
    model::{attachment::{is_present, AttachmentMeta, Transfer}, chat_key::ChatKey, chat_list::build_chat_list, group::{self, GroupStatus, Members}, timeline::{Timeline, REACTION}, AppState, OpenChat},
    paths::DataPaths,
    settings::Settings,
    ui::{group_info, invites, join_group, shell, theme},
};
use eframe::egui;
use mimir::MimirApp;
use crate::platform::notify::{self, Focus};
use std::{sync::{mpsc::{Receiver, Sender, TryRecvError}, Arc, Mutex}, time::{Duration, Instant}};

pub struct MimirDesktop {
    pub core: Arc<MimirApp>,
    /// 1:1 and contact jobs (sends, dials, requests) and the core start.
    pub worker: Worker,
    /// Mediator and group jobs (create, join, invites, leave/delete, roles, group sends, mediator registry): a slow or
    /// unreachable mediator (dial and request timeouts, proof-of-work) must not hold up 1:1 jobs. Same result channel.
    pub group_worker: Worker,
    pub rx: Receiver<Event>,
    /// The sender of `rx`, for events from outside the core (links from other launches).
    pub events: Sender<Event>,
    pub state: AppState,
    pub settings: Settings,
    pub paths: DataPaths,
    pub lang: Lang,
    /// Voice and audio playback (its own thread).
    pub player: crate::media::player::Player,
    /// Call audio (its own threads); the core's Bridge feeds it the received frames.
    pub call_audio: crate::media::call_audio::CallAudio,
    /// What the window shows, for the notifications the core's callback thread decides (`core::events::Bridge`).
    pub focus: Arc<Mutex<Focus>>,
    /// Test data dir, dropped (deleted) with the app.
    #[cfg(test)]
    pub test_tmp: Option<tempfile::TempDir>,
}

/// The window: `backup::restore` (recovery, and a staged import that can take minutes) runs on a thread behind a
/// progress screen, then the core opens. eframe shows the window only after its first frame, so nothing may block here.
pub enum Startup {
    /// `ext`: what other launches ask, handed to the app once it opens.
    /// `rx`: what `backup::restore` did; an error comes with whether the old data is still in restore_old.
    Restoring { lang: Lang, root: std::path::PathBuf, rx: Receiver<Option<Result<bool, (String, bool)>>>, ext: Option<Receiver<crate::platform::Ext>> },
    /// Another instance holds the restore lock: say so and close.
    Busy(Lang),
    /// The old data could not be put back (the text says why): the core is not opened, it would start an empty account.
    Failed(Lang, String),
    Ready(Box<MimirDesktop>),
}

impl Startup {
    /// `link`: this launch's own link, handled like a forwarded one once the app is up.
    pub fn new(cc: &eframe::CreationContext<'_>, instance: crate::platform::Instance, link: Option<String>) -> Self {
        let root = DataPaths::default_for_user().root;
        let paths_root = root.clone();
        // The settings as they were before a staged import replaces them: only the screen's language and theme.
        let settings = Settings::load(&DataPaths::under(&root).settings_file);
        theme::install(&cc.egui_ctx);
        cc.egui_ctx.set_theme(theme_preference(settings.dark));
        crate::platform::tray::remember_window(cc);
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = cc.egui_ctx.clone();
        std::thread::spawn(move || {
            let paths = DataPaths::under(&root);
            let restored = crate::backup::restore(&paths).map(|r| r.map_err(|e| (e, crate::backup::old_data_kept(&paths.root))));
            let _ = tx.send(restored);
            ctx.request_repaint();
            crate::media::stage::sweep_stale();
        });
        let (ext_tx, ext) = std::sync::mpsc::channel();
        if let Some(link) = link {
            let _ = ext_tx.send(crate::platform::Ext::Link(link));
        }
        let ctx = cc.egui_ctx.clone();
        instance.listen(move |msg| {
            let _ = ext_tx.send(msg.map_or(crate::platform::Ext::Show, crate::platform::Ext::Link));
            ctx.request_repaint();
        });
        Startup::Restoring { lang: settings.lang.unwrap_or_else(Lang::from_system), root: paths_root, rx, ext: Some(ext) }
    }
}

impl eframe::App for Startup {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        match self {
            Startup::Ready(app) => app.logic(ctx, frame),
            // The restore finishes first: a window closed mid-swap would leave it to the next start.
            Startup::Restoring { .. } if ctx.input(|i| i.viewport().close_requested()) => ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose),
            _ => {}
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        if let Startup::Restoring { lang, root, rx, ext } = self {
            let restored = match rx.try_recv() {
                Ok(r) => Some(r),
                // The thread died: the same recovery check as after an error, old data left behind must not meet a new core.
                Err(TryRecvError::Disconnected) => Some(Some(Err(("restore stopped".into(), crate::backup::old_data_kept(root))))),
                Err(TryRecvError::Empty) => None,
            };
            let next = restored.map(|r| match r {
                Some(Err((e, true))) => Startup::Failed(*lang, trf(*lang, Key::DataNotRestored, &[&e])),
                Some(restored) => {
                    let mut app = MimirDesktop::open(ui.ctx(), restored.map_err(|(e, _)| e));
                    if let Some(ext) = ext.take() {
                        crate::platform::relay(ext, app.events.clone(), ui.ctx().clone());
                    }
                    app.state.tray = crate::platform::tray::spawn(app.events.clone(), ui.ctx().clone(), app.lang);
                    Startup::Ready(Box::new(app))
                }
                None => Startup::Busy(*lang),
            });
            if let Some(next) = next {
                *self = next;
            }
        }
        let p = theme::palette(ui.visuals().dark_mode);
        match self {
            Startup::Ready(app) => app.ui(ui, frame),
            Startup::Restoring { lang, .. } => {
                crate::ui::welcome::progress(ui, *lang, &p, tr(*lang, Key::Restoring), false);
            }
            Startup::Busy(lang) => {
                if crate::ui::welcome::progress(ui, *lang, &p, tr(*lang, Key::RestoreRunning), true) {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            Startup::Failed(lang, text) => {
                if crate::ui::welcome::progress(ui, *lang, &p, text, true) {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
    }

    fn on_exit(&mut self) {
        if let Startup::Ready(app) = self {
            app.on_exit();
        }
    }
}

impl MimirDesktop {
    /// The app on the restored data (`restored`: what `backup::restore` did, shown to the user).
    pub fn open(ctx: &egui::Context, restored: Result<bool, String>) -> Self {
        let paths = DataPaths::default_for_user();
        // ponytail: a core that fails to open aborts startup with a panic, no error screen.
        let core = core::open(&paths).expect("failed to open Mimir core");
        // An import is always a sibling (the phone stays in use): this copy needs its own client id, before start.
        // Retried at every start until it succeeds.
        let mut regen_failed = None;
        if restored == Ok(true) || crate::backup::regenerate_pending(&paths.root) {
            match core.regenerate_client_id() {
                Ok(()) => crate::backup::client_id_regenerated(&paths.root),
                Err(e) => regen_failed = Some(e.to_string()),
            }
        }
        let settings = Settings::load(&paths.settings_file);
        let cfg = net::config_for(&paths, &settings);
        let mut app = Self::new_with(ctx, core, paths, settings, Some(cfg));
        match restored {
            Ok(true) => app.notice(Key::BackupImportSuccess),
            Ok(false) => {}
            Err(e) => {
                let text = trf(app.lang, Key::BackupImportFailed, &[&e]);
                crate::ui::settings::job_failed(&mut app, crate::ui::settings::Job::Import, text);
            }
        }
        if let Some(e) = regen_failed {
            let text = trf(app.lang, Key::BackupImportFailed, &[&e]);
            crate::ui::settings::job_failed(&mut app, crate::ui::settings::Job::Import, text);
        }
        app
    }

    /// Builds the app on `ctx`; with a config it also starts the core's networking (tests pass `None`).
    pub fn new_with(ctx: &egui::Context, core: Arc<MimirApp>, paths: DataPaths, settings: Settings, net_cfg: Option<mimir::AppConfig>) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = Worker::spawn(core.clone(), tx.clone(), ctx.clone());
        let group_worker = Worker::spawn(core.clone(), tx.clone(), ctx.clone());
        let lang = settings.lang.unwrap_or_else(Lang::from_system);
        ctx.set_theme(theme_preference(settings.dark));
        // Tests never use the machine's ffmpeg: they set a fake one where they need it.
        let ffmpeg = if cfg!(test) { None } else { crate::media::ffmpeg::locate() };
        let state = AppState { chats: build_chat_list(&core, lang), ffmpeg, ..Default::default() };
        let player = crate::media::player::Player::spawn(ctx.clone(), worker.clone(), lang);
        let focus = Arc::new(Mutex::new(Focus { chat: None, focused: false, lang, in_call: false }));
        let call_audio = crate::media::call_audio::CallAudio::new(core.clone(), state.ffmpeg.clone(), tx.clone(), ctx.clone());
        let mut app = MimirDesktop { core, worker, group_worker, rx, events: tx.clone(), state, settings, paths, lang, player, call_audio, focus, #[cfg(test)] test_tmp: None };
        app.refresh_requests();
        invites::refresh(&mut app);
        if let Some(cfg) = net_cfg {
            let (events, wake, focus) = (tx.clone(), ctx.clone(), app.focus.clone());
            let show = move |title, body, alert| notify::show(title, body, alert, focus.lock().unwrap_or_else(|p| p.into_inner()).lang, events.clone(), wake.clone());
            let bridge = Bridge::new(tx, ctx.clone()).notifying(&app.core, app.focus.clone(), show).with_call_audio(app.call_audio.sink());
            net::start(&app.worker, cfg, bridge);
        }
        app
    }

    /// Leaves for real: the close this sends is not turned into hiding to the tray.
    pub fn quit(&mut self, ctx: &egui::Context) {
        self.state.quitting = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// A success message in the status line for a few seconds.
    pub fn notice(&mut self, key: crate::i18n::Key) {
        self.notice_text(crate::i18n::tr(self.lang, key).to_string());
    }

    pub fn notice_text(&mut self, text: String) {
        self.state.status = Some((text, Some(Instant::now() + STATUS_TTL)));
        self.state.status_ok = true;
    }

    /// A failure in the status line (red) for a few seconds.
    pub fn error_text(&mut self, text: String) {
        self.state.status = Some((text, Some(Instant::now() + STATUS_TTL)));
        self.state.status_ok = false;
    }

    /// Hands the current settings to the core (after every start, and on each settings change).
    pub fn apply_live(&mut self) {
        net::apply_live(&self.core, &self.settings);
        self.state.live_applied += 1;
    }

    /// The worker lane for jobs of chat `key`: groups go to `group_worker`, contacts and Saved Messages to `worker`.
    pub fn worker_for(&self, key: ChatKey) -> &Worker {
        if key.is_group() { &self.group_worker } else { &self.worker }
    }

    pub fn refresh_requests(&mut self) {
        self.state.requests = self.core.get_contact_requests();
        self.state.requests_count = self.state.requests.len();
    }

    /// Applies pending core events; the chat list is rebuilt only when one of them can change it.
    /// Messages in the open chat count as read only while the window has focus.
    pub fn drain_events(&mut self, ctx: &egui::Context) {
        let focused = self.window_focused(ctx);
        let mut refresh = false;
        let mut requests = false;
        let mut mediators = false;
        while let Ok(e) = self.rx.try_recv() {
            self.state.apply_net(&e);
            match &e {
                Event::ContactConnected { pubkey } => {
                    self.state.online.insert(pubkey.clone());
                    // The peer's profile (and avatar) may have come with the connection; no event says so,
                    // and it is written over the same file. The chat list is rebuilt below, so a new path shows too.
                    // ponytail: info that lands after this event shows on the next connect (or once its mtime moves).
                    let avatar = self.core.get_contact_by_pubkey(pubkey.clone()).and_then(|c| crate::media::avatar::resolve(&self.paths.core_dir, &c.avatar));
                    if let Some(p) = avatar {
                        self.state.thumbs.invalidate(&p);
                    }
                    self.refresh_last_seen(pubkey);
                    crate::ui::call::on_event(self, ctx, &e);
                }
                Event::ContactDisconnected { pubkey, .. } => {
                    self.state.online.remove(pubkey);
                    self.refresh_last_seen(pubkey);
                }
                Event::OutgoingMessageStored { is_group, target_id, message_id } => {
                    let key = if *is_group { ChatKey::Group(*target_id) } else { ChatKey::Contact(*target_id) };
                    // A send job of ours runs for this chat and has not stored its row yet.
                    let pending = self.state.pending_sends.get(&key).copied().unwrap_or(0);
                    let ours = pending > self.state.sending.iter().filter(|(k, _)| *k == key).count();
                    // Rows are read only for our own send or the open chat; the list refreshes either way.
                    if ours || open_mut(&mut self.state, key).is_some() {
                        let stored = if *is_group { self.core.get_group_message(*target_id, *message_id) } else { self.core.get_message(*message_id) };
                        // Our attachment row is uploading until its job ends. A sibling's outgoing row (stored delivered,
                        // sibling sync) is not ours even while we send; Saved Messages rows are delivered at once.
                        let mine = |m: &&mimir::StoredMessage| ours && ATTACHMENT_TYPES.contains(&m.msg_type) && (!m.delivered || key == ChatKey::SAVED);
                        if let Some((m, meta)) = stored.as_ref().filter(mine).and_then(|m| Some((m, AttachmentMeta::parse(m.data.as_deref()?)?))) {
                            self.state.transfers.insert(m.guid, Transfer::Uploading { done: 0, total: meta.size });
                            self.state.sending.push((key, m.guid));
                        }
                        if let Some(chat) = open_mut(&mut self.state, key) {
                            // The row may already be loaded when the chat opened between the store and this event.
                            if chat.timeline.push_if_newer(stored) {
                                chat.timeline.at_bottom = true; // own message: follow it down
                            }
                        }
                    }
                }
                Event::GroupMessageReceived { chat_id, message_id, msg_type, reply_to } => {
                    if let Some(chat) = open_mut(&mut self.state, ChatKey::Group(*chat_id)) {
                        if *msg_type == REACTION {
                            chat.reactions.remove(reply_to);
                        } else {
                            chat.timeline.push_if_newer(self.core.get_group_message(*chat_id, *message_id));
                        }
                        chat.unread_while_away = true;
                    }
                }
                Event::GroupMessageDeleted { chat_id, guid } => {
                    // The core has already deleted the row.
                    if let Some(chat) = open_mut(&mut self.state, ChatKey::Group(*chat_id)) {
                        let t = &mut chat.timeline;
                        if let Some(i) = t.msgs.iter().position(|m| m.guid == *guid) {
                            t.msgs.remove(i);
                            if i < t.heights.len() { t.heights.remove(i); }
                        }
                        chat.reactions.remove(guid);
                    }
                }
                Event::GroupHistorySynced { chat_id } => {
                    self.state.group_ui.invalidate();
                    self.apply_chat_info(*chat_id);
                    if let Some(chat) = open_mut(&mut self.state, ChatKey::Group(*chat_id)) {
                        chat.timeline = Timeline::open(&self.core, chat.key);
                        chat.reactions.clear();
                        reload_members(&self.core, chat, *chat_id);
                        chat.unread_while_away = true; // synced rows are stored unread
                    }
                }
                Event::GroupMembersChanged { chat_id } => {
                    self.state.group_ui.invalidate();
                    self.apply_chat_info(*chat_id);
                    if let Some(chat) = open_mut(&mut self.state, ChatKey::Group(*chat_id)) {
                        reload_members(&self.core, chat, *chat_id);
                        // System rows (joins, leaves, roles, renames) are stored without a message event.
                        chat.timeline.load_newer(&self.core);
                    }
                }
                Event::GroupMemberOnline { chat_id, .. } => {
                    self.state.group_ui.invalidate();
                    if let Some(chat) = open_mut(&mut self.state, ChatKey::Group(*chat_id)) {
                        chat.counts = group::counts(&self.core, *chat_id);
                    }
                }
                Event::ChatStats { chat_id, subscribers, online } => {
                    if let Some(chat) = open_mut(&mut self.state, ChatKey::Group(*chat_id)) {
                        // A channel has no member list: the mediator's numbers are all there is.
                        let channel = self.core.get_group_chat(*chat_id).is_some_and(|g| g.is_channel);
                        chat.counts = if channel { (*subscribers as i32, *online as i32) } else { group::counts(&self.core, *chat_id) };
                    }
                }
                Event::GroupChatStatus { chat_id, status } => {
                    if let Some(chat) = open_mut(&mut self.state, ChatKey::Group(*chat_id)) {
                        // The core reports DISCONNECTED for every chat on a dropped mediator, read-only ones included:
                        // only a new subscription ends read-only.
                        let s = GroupStatus::parse(status);
                        if chat.status != GroupStatus::ReadOnly || s == GroupStatus::Subscribed {
                            chat.status = s;
                        }
                    }
                }
                Event::MessageReceived { contact_id, message_id, msg_type, reply_to } => {
                    if let Some(chat) = self.state.open_chat.as_mut().filter(|c| c.key == ChatKey::Contact(*contact_id)) {
                        if *msg_type == REACTION {
                            chat.reactions.remove(reply_to); // re-read when the target row is next drawn
                        } else {
                            chat.timeline.push_if_newer(self.core.get_message(*message_id));
                        }
                        chat.unread_while_away = true; // marked read below once the window has focus
                    }
                }
                Event::GroupMessageDelivered { chat_id, guid } => {
                    // The mediator accepted it: the single tick.
                    if let Some(chat) = open_mut(&mut self.state, ChatKey::Group(*chat_id)) {
                        if let Some(m) = chat.timeline.msgs.iter_mut().find(|m| m.guid == *guid) {
                            m.delivered = true;
                        }
                    }
                }
                Event::MessageDelivered { contact_pubkey, guid } => {
                    if let Some(chat) = self.state.open_chat.as_mut().filter(|c| c.pubkey == *contact_pubkey) {
                        if let Some(m) = chat.timeline.msgs.iter_mut().find(|m| m.guid == *guid) {
                            m.delivered = true;
                        }
                    }
                }
                Event::FileProgress { guid, bytes_done, total_bytes, is_upload } => {
                    let (done, total) = (*bytes_done, *total_bytes);
                    let t = if *is_upload { Transfer::Uploading { done, total } } else { Transfer::Downloading { done, total } };
                    self.state.transfers.insert(*guid, t);
                }
                // Neither event's file_path nor FileDownloaded's chat_id (always 0) can be trusted:
                // the row is found by guid and its file is at files_dir/name.
                Event::FileReceived { guid, .. } | Event::FileDownloaded { guid, .. } => {
                    self.state.transfers.remove(guid);
                    self.refresh_row(*guid);
                }
                Event::MessagesSynced { contact_id } => {
                    // A sibling blob landed (no guid, no progress): end the 1:1 downloads whose file is now on disk.
                    let (core, files) = (&self.core, &self.paths.files_dir);
                    // An upload's file is in files_dir from the start: only downloads end here.
                    self.state.transfers.retain(|g, t| {
                        if !matches!(t, Transfer::Downloading { .. }) {
                            return true;
                        }
                        let meta = core.get_message_by_guid(*g).and_then(|m| AttachmentMeta::parse(m.data.as_deref()?));
                        !meta.is_some_and(|m| is_present(files, &m))
                    });
                    if let Some(chat) = self.state.open_chat.as_mut().filter(|c| c.key == ChatKey::Contact(*contact_id)) {
                        chat.timeline = Timeline::open(&self.core, chat.key);
                        chat.reactions.clear();
                    }
                }
                Event::External(ext) => crate::platform::handle(self, ctx, ext),
                Event::Quit => self.quit(ctx),
                Event::WorkDone(WorkResult::Started) => {
                    self.state.core_started = true;
                    self.apply_live();
                }
                Event::WorkDone(WorkResult::Done) => {
                    // A group reaction is stored by its worker job (send_group_message): its chip can only be read now.
                    if let Some(chat) = self.state.open_chat.as_mut().filter(|c| c.key.is_group()) {
                        chat.reactions.clear();
                    }
                }
                Event::WorkDone(WorkResult::ContactAdded { contact_id, opened_from }) => {
                    if self.state.selected == *opened_from {
                        self.state.selected = Some(ChatKey::Contact(*contact_id));
                    }
                }
                Event::WorkDone(WorkResult::GroupCreated { chat_id }) => {
                    self.state.new_group = None;
                    self.state.selected = Some(ChatKey::Group(*chat_id));
                    self.notice(crate::i18n::Key::ChatCreated);
                }
                Event::WorkDone(WorkResult::GroupJoined { chat_id }) => join_group::joined(self, *chat_id),
                Event::WorkDone(WorkResult::InviteAccepted { chat_id }) => invites::accepted(self, *chat_id),
                Event::WorkDone(WorkResult::Preview { token, info }) => join_group::preview_done(self, *token, info.clone()),
                // Live: the list and the toolbar icon follow the event, unlike Android's onResume.
                Event::GroupInviteReceived { .. } => invites::refresh(self),
                Event::MediatorAuthenticated { .. } => join_group::retry_lookup(self),
                Event::WorkDone(WorkResult::Failed { what: "join_public_group", err }) if self.state.join.as_ref().is_some_and(|d| d.joining) => {
                    join_group::join_failed(self, err);
                }
                Event::WorkDone(WorkResult::Failed { what: "accept_group_invite", err }) if self.state.invites_ui.busy.is_some() => {
                    invites::accept_failed(self, err);
                }
                Event::WorkDone(WorkResult::Notice(key)) => self.notice(*key),
                Event::WorkDone(WorkResult::Error(text)) => self.error_text(text.clone()),
                Event::WorkDone(WorkResult::Fixed { fixed, scanned }) => {
                    let text = match fixed {
                        0 => crate::i18n::tr(self.lang, Key::NoMessagesToFix).to_string(),
                        _ => trf(self.lang, Key::MessagesFixed, &[&fixed.to_string(), &scanned.to_string()]),
                    };
                    crate::ui::settings::job_done(self, crate::ui::settings::Job::Fix, text);
                }
                Event::WorkDone(WorkResult::OrphansDeleted { count, bytes }) => {
                    let size = crate::ui::attachment_bubble::format_size(*bytes as i64, self.lang);
                    let text = match count {
                        0 => crate::i18n::tr(self.lang, Key::NoOrphanedMedia).to_string(),
                        _ => trf(self.lang, Key::OrphanedMediaDeleted, &[&count.to_string(), &size]),
                    };
                    crate::ui::settings::job_done(self, crate::ui::settings::Job::Orphans, text);
                }
                Event::WorkDone(WorkResult::Files(rows)) => crate::ui::settings::files_listed(self, rows.clone()),
                Event::WorkDone(WorkResult::FileDeleted { storage_name, err }) => crate::ui::settings::file_deleted(self, storage_name, err.as_ref()),
                Event::WorkDone(WorkResult::BackupSaved { path, err }) => crate::ui::settings::backup_saved(self, path.as_ref(), err.as_ref()),
                Event::WorkDone(WorkResult::BackupPicked { path, valid }) => crate::ui::settings::backup_picked(self, path.clone(), *valid),
                Event::WorkDone(WorkResult::BackupStaged { err }) => crate::ui::settings::import_staged(self, ctx, err.as_ref()),
                Event::WorkDone(WorkResult::AttachmentSent { chat, err }) => self.attachment_sent(*chat, err.as_ref()),
                Event::WorkDone(WorkResult::DownloadFailed { guid, err }) => {
                    self.state.transfers.remove(guid);
                    if let Some(err) = err {
                        self.error_text(trf(self.lang, Key::DownloadFailed, &[err]));
                    }
                }
                Event::WorkDone(WorkResult::Left { chat_id } | WorkResult::GroupDeleted { chat_id }) => group_info::closed(self, *chat_id),
                Event::WorkDone(WorkResult::HistoryCleared { chat_id }) => group_info::history_cleared(self, *chat_id),
                Event::WorkDone(WorkResult::Invited { name }) => self.notice_text(trf(self.lang, Key::InviteSentTo, &[name])),
                Event::WorkDone(WorkResult::Banned { chat_id, pubkey, name }) => {
                    group_info::member_changed(self, *chat_id, pubkey, 0x01);
                    self.notice_text(trf(self.lang, Key::UserBanned, &[name]));
                }
                Event::WorkDone(WorkResult::RoleChanged { chat_id, pubkey, perms, name }) => {
                    group_info::member_changed(self, *chat_id, pubkey, *perms);
                    let role = group::role_name(group::role_of(*perms as i32), self.lang);
                    self.notice_text(trf(self.lang, Key::RoleChanged, &[name, role]));
                }
                Event::WorkDone(WorkResult::GroupInfoUpdated { chat_id }) => group_info::edit_done(self, *chat_id),
                Event::WorkDone(WorkResult::AccountAvatar { path }) => crate::ui::profile::avatar_saved(self, path),
                Event::WorkDone(WorkResult::Failed { what: "update_group_info", err }) if group_info::editing(self) => group_info::edit_failed(self, err),
                Event::WorkDone(WorkResult::Failed { what: "create_group", err }) if self.state.new_group.is_some() => {
                    // Shown in the dialog (which stays open with its fields); no status line.
                    if let Some(d) = self.state.new_group.as_mut() {
                        d.busy = false;
                        d.error = Some(format!("{} {err}", crate::i18n::tr(self.lang, crate::i18n::Key::FailedToCreateChat)));
                    }
                }
                Event::WorkDone(WorkResult::Failed { what, err }) => {
                    group_info::op_failed(self, what);
                    if *what == "start" {
                        // A failed start stays on screen (nothing works without it); other failures fade.
                        self.state.status = Some((format!("{what}: {err}"), None));
                        self.state.status_ok = false;
                    } else {
                        self.error_text(format!("{what}: {err}"));
                    }
                }
                Event::IncomingCall { .. }
                | Event::CallStatusChanged { .. }
                | Event::CallAudio(_)
                | Event::GroupCallParticipants { .. }
                | Event::GroupCallParticipantEvent { .. }
                | Event::GroupCallError { .. }
                | Event::GroupCallDisconnected => crate::ui::call::on_event(self, ctx, &e),
                Event::WorkDone(WorkResult::GroupCallUp { token, link }) => crate::ui::call::group_up(self, *token, link),
                Event::WorkDone(WorkResult::GroupCallFailed { token, err }) => crate::ui::call::group_failed(self, *token, err),
                Event::DraftSynced { chat_type, chat_id } => {
                    // 0 = contact (or Saved Messages), 1 = group; anything else is not a chat we know.
                    let key = match chat_type {
                        0 => Some(ChatKey::Contact(*chat_id)),
                        1 => Some(ChatKey::Group(*chat_id)),
                        _ => None,
                    };
                    if let Some(key) = key.filter(|k| self.state.open_chat.as_ref().is_some_and(|c| c.key == *k)) {
                        if !self.state.composer.focused {
                            self.state.composer.load_draft(&self.core, key);
                        }
                    }
                }
                _ => {}
            }
            requests |= matches!(
                e,
                Event::ContactRequestReceived { .. }
                    | Event::WorkDone(WorkResult::RequestAccepted { .. } | WorkResult::RequestDeclined | WorkResult::Failed { .. })
            );
            mediators |= matches!(
                e,
                Event::MediatorInvitation { .. } | Event::MediatorAuthenticated { .. } | Event::MediatorDisconnected { .. } | Event::WorkDone(_)
            );
            refresh |= matches!(
                e,
                Event::MessageReceived { .. }
                    | Event::OutgoingMessageStored { .. }
                    | Event::GroupMessageReceived { .. }
                    | Event::GroupMessageDeleted { .. }
                    | Event::GroupHistorySynced { .. }
                    | Event::GroupMembersChanged { .. }
                    | Event::GroupChatStatus { .. }
                    | Event::MessagesSynced { .. }
                    | Event::DraftSynced { .. }
                    | Event::ContactResponse { .. }
                    // The core's auto-accept clears our outgoing request silently; the connect that follows refreshes «Waiting…».
                    | Event::ContactConnected { .. }
                    | Event::WorkDone(_)
            );
        }
        if let Some(chat) = self.state.open_chat.as_mut().filter(|c| focused && c.unread_while_away) {
            chat.unread_while_away = false;
            // ponytail: a failed mark-read only leaves the unread badge until the next open.
            let _ = match chat.key {
                ChatKey::Contact(id) => self.core.mark_all_read(id),
                // Storage-only, inline; not sibling-synced by the core.
                ChatKey::Group(id) => self.core.mark_all_group_read(id),
            };
            refresh = true;
        }
        if requests {
            self.refresh_requests();
        }
        if mediators {
            // Mediator invitations are counted in the invites banner, so it follows them too.
            invites::refresh(self);
            if self.state.show_mediators {
                crate::ui::mediators::refresh(self);
            }
        }
        if refresh {
            self.state.chats = build_chat_list(&self.core, self.lang);
            // A selected group that is gone (left despite a mediator error, deleted elsewhere) closes.
            if let Some(key @ ChatKey::Group(id)) = self.state.selected {
                if !self.state.chats.iter().any(|r| r.key == key) {
                    group_info::closed(self, id);
                }
            }
        }
    }

    /// The window is shown, focused and not minimised. Live input: while hidden, eframe runs `logic` alone and
    /// `i.focused` keeps the last shown frame's value.
    fn window_focused(&self, ctx: &egui::Context) -> bool {
        !self.state.hidden && ctx.input(|i| i.raw.focused && i.viewport().minimized != Some(true))
    }

    fn refresh_last_seen(&mut self, pubkey: &[u8]) {
        if let Some(chat) = self.state.open_chat.as_mut().filter(|c| c.pubkey == pubkey) {
            if let ChatKey::Contact(id) = chat.key {
                chat.last_seen = self.core.get_contact(id).map_or(0, |c| c.last_seen);
            }
        }
    }

    /// A send job of `chat` ended: its row stops uploading and is re-read (a group upload added key and server).
    /// A 1:1 `Connection` error is a queued send (the core resends on connect); so is a group one once the file is
    /// uploaded (`resend_undelivered`). A group row left without a key can never be fetched: reported.
    fn attachment_sent(&mut self, chat: ChatKey, err: Option<&mimir::MimirError>) {
        if let Some(n) = self.state.pending_sends.get_mut(&chat) {
            *n = n.saturating_sub(1);
        }
        let guid = self.state.sending.iter().position(|(k, _)| *k == chat).map(|i| self.state.sending.remove(i).1);
        if let Some(guid) = guid {
            self.state.transfers.remove(&guid);
            self.refresh_row(guid);
        }
        let uploaded = || {
            let row = guid.and_then(|g| match chat {
                ChatKey::Group(id) => self.core.get_group_message_by_guid(id, g),
                ChatKey::Contact(_) => None,
            });
            row.and_then(|m| AttachmentMeta::parse(m.data.as_deref()?)).is_some_and(|m| m.key.is_some())
        };
        let queued = match err {
            None => true,
            Some(mimir::MimirError::Connection(_)) => !chat.is_group() || uploaded(),
            Some(_) => false,
        };
        if let Some(e) = err.filter(|_| !queued) {
            self.error_text(trf(self.lang, Key::SendFileFailed, &[&e.to_string()]));
        }
    }

    /// Re-reads row `guid` of the open chat, if it is there (its file landed, or its upload rewrote the JSON).
    fn refresh_row(&mut self, guid: i64) {
        let Some(chat) = self.state.open_chat.as_mut() else { return };
        let Some(m) = chat.timeline.msgs.iter_mut().find(|m| m.guid == guid) else { return };
        let fresh = match chat.key {
            ChatKey::Contact(_) => self.core.get_message_by_guid(guid),
            ChatKey::Group(id) => self.core.get_group_message_by_guid(id, guid),
        };
        if let Some(fresh) = fresh {
            *m = fresh;
        }
    }

    /// Writes other members' renames and avatars into the local chat, once per stored ChatInfoChanged row.
    fn apply_chat_info(&mut self, chat_id: i64) {
        let after = self.settings.chat_info_applied.get(&chat_id).copied().unwrap_or(0);
        if let Some(id) = group::apply_chat_info(&self.core, chat_id, after) {
            self.mark_chat_info_applied(chat_id, id);
            // A new avatar may land in the same file within one mtime tick.
            if let Some(p) = self.core.get_group_chat(chat_id).and_then(|g| crate::media::avatar::resolve(&self.paths.core_dir, &g.avatar)) {
                self.state.thumbs.invalidate(&p);
            }
        }
    }

    /// Moves the group's ChatInfoChanged watermark to row `id` and saves it (settings.json).
    pub fn mark_chat_info_applied(&mut self, chat_id: i64, id: i64) {
        let w = self.settings.chat_info_applied.entry(chat_id).or_default();
        if id > *w {
            *w = id;
            // ponytail: a failed settings write only means the row may be re-applied after a restart.
            let _ = self.settings.save(&self.paths.settings_file);
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        shell::show(self, ui);
    }
}

/// Message types whose data is attachment JSON: image, file/voice, video.
const ATTACHMENT_TYPES: [i32; 3] = [1, 3, 4];

/// How long a status line notice or error stays.
const STATUS_TTL: Duration = Duration::from_secs(8);

fn open_mut(state: &mut AppState, key: ChatKey) -> Option<&mut OpenChat> {
    state.open_chat.as_mut().filter(|c| c.key == key)
}

/// Re-reads an open group's members and counts; the rows are re-laid out so names and quotes follow.
pub(crate) fn reload_members(core: &MimirApp, chat: &mut OpenChat, chat_id: i64) {
    chat.members = Some(Members::load(core, chat_id));
    chat.counts = group::counts(core, chat_id);
    chat.day_key = (usize::MAX, 0, 0); // recompute quotes and learn members who left
}

/// `Settings.dark` as egui's theme choice (None follows the system).
pub fn theme_preference(dark: Option<bool>) -> egui::ThemePreference {
    match dark {
        Some(true) => egui::ThemePreference::Dark,
        Some(false) => egui::ThemePreference::Light,
        None => egui::ThemePreference::System,
    }
}

impl eframe::App for MimirDesktop {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events(ctx);
        let tray_up = self.state.tray.load(std::sync::atomic::Ordering::Relaxed);
        if crate::platform::tray::on_close_request(ctx, tray_up, self.state.quitting) {
            self.state.hidden = true;
        } else if ctx.input(|i| i.raw.events.contains(&egui::Event::WindowFocused(true))) {
            self.state.hidden = false; // brought back by the OS (a Wayland taskbar, say), not by us
        }
        let chat = self.state.open_chat.as_ref().map(|c| c.key);
        let focused = self.window_focused(ctx);
        *self.focus.lock().unwrap_or_else(|p| p.into_inner()) = Focus { chat, focused, lang: self.lang, in_call: self.state.call.is_some() || self.state.group_call.is_some() };
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }

    fn on_exit(&mut self) {
        crate::ui::call::on_exit(self);
        crate::ui::composer::save_open_draft(self);
    }
}

#[cfg(test)]
mod startup_tests {
    use super::*;
    use egui::{ViewportCommand, ViewportEvent, ViewportId};
    use egui_kittest::{kittest::Queryable as _, Harness};

    type Restored = Option<Result<bool, (String, bool)>>;

    /// The startup screen, waiting for what `rx` says the restore did.
    fn startup(rx: Receiver<Restored>) -> Harness<'static, Startup> {
        Harness::builder().with_context_setup(theme::install).build_eframe(move |_| Startup::Restoring { lang: Lang::Ru, root: Default::default(), rx, ext: None })
    }

    fn commands<'h>(h: &'h Harness<'static, Startup>) -> &'h [ViewportCommand] {
        &h.output().viewport_output[&ViewportId::ROOT].commands
    }

    #[test]
    fn close_waits_for_the_restore() {
        let (_tx, rx) = std::sync::mpsc::channel();
        let mut h = startup(rx);
        h.step();
        h.input_mut().viewports.entry(ViewportId::ROOT).or_default().events.push(ViewportEvent::Close);
        h.step();
        assert!(commands(&h).contains(&ViewportCommand::CancelClose), "{:?}", commands(&h));
        assert!(matches!(h.state(), Startup::Restoring { .. }));
    }

    #[test]
    fn restore_thread_gone_with_old_data_left_never_opens_the_core() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("restore_old/mimir")).unwrap();
        let (tx, rx) = std::sync::mpsc::channel::<Restored>();
        drop(tx);
        let root = tmp.path().to_path_buf();
        let mut h = Harness::builder().with_context_setup(theme::install).build_eframe(move |_| Startup::Restoring { lang: Lang::Ru, root, rx, ext: None });
        h.step();
        h.step();
        assert!(matches!(h.state(), Startup::Failed(..)), "no app, so no new empty account");
    }

    #[test]
    fn old_data_not_back_never_opens_the_core() {
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(Some(Err(("locked".into(), true)))).unwrap();
        let mut h = startup(rx);
        h.step();
        h.step();
        assert!(matches!(h.state(), Startup::Failed(..)), "no app, so no new empty account");
        assert!(h.query_by_label_contains("Не удалось вернуть ваши данные: locked").is_some());
        h.get_by_label("Закрыть").click();
        h.step();
        assert!(commands(&h).contains(&ViewportCommand::Close));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{model::{message::reaction_payload, Dot}, test_support::test_harness};
    use egui_kittest::{kittest::Queryable as _, Harness};
    use std::{sync::mpsc::{channel, Sender}, time::Duration};

    const T: i64 = 1_700_000_000_000;

    /// App with the chat of contact «Аня» open; the sender feeds the app's event channel.
    fn open_contact_chat() -> (Harness<'static, MimirDesktop>, i64, Sender<Event>) {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        let core = h.state().core.clone();
        let c = core.add_contact(vec![7; 32], "Аня".into()).unwrap();
        let (tx, rx) = channel();
        h.state_mut().rx = rx;
        h.state_mut().state.chats = build_chat_list(&core, Lang::Ru);
        h.state_mut().state.selected = Some(ChatKey::Contact(c));
        h.run();
        (h, c, tx)
    }

    fn received(c: i64, id: i64, msg_type: i32, reply_to: i64) -> Event {
        Event::MessageReceived { contact_id: c, message_id: id, msg_type, reply_to }
    }

    fn preview(h: &Harness<'static, MimirDesktop>, c: i64) -> String {
        h.state().state.chats.iter().find(|r| r.key == ChatKey::Contact(c)).unwrap().preview.clone()
    }

    #[test]
    fn incoming_message_updates_open_chat() {
        let (mut h, c, tx) = open_contact_chat();
        let id = h.state().core.add_local_message(c, 1, 0, true, false, T, 0, 0, "привет".as_bytes().to_vec()).unwrap();
        tx.send(received(c, id, 0, 0)).unwrap();
        h.run();
        assert_eq!(h.state().state.open_chat.as_ref().unwrap().timeline.msgs.last().map(|m| m.id), Some(id));
        assert_eq!(preview(&h, c), "привет");
        assert_eq!(h.state().core.get_unread_count(c), 0, "read while on screen");
    }

    #[test]
    fn unfocused_window_leaves_open_chat_unread() {
        let (mut h, c, tx) = open_contact_chat();
        h.input_mut().focused = false;
        h.run();
        let id = h.state().core.add_local_message(c, 1, 0, true, false, T, 0, 0, "привет".as_bytes().to_vec()).unwrap();
        tx.send(received(c, id, 0, 0)).unwrap();
        h.run();
        assert_eq!(h.state().state.open_chat.as_ref().unwrap().timeline.msgs.len(), 1, "still shown");
        assert_eq!(h.state().core.get_unread_count(c), 1, "not read while the window is in the background");
        h.input_mut().focused = true;
        h.run();
        assert_eq!(h.state().core.get_unread_count(c), 0, "read once the window has focus again");
        assert_eq!(h.state().state.chats.iter().find(|r| r.key == ChatKey::Contact(c)).unwrap().unread, 0);
    }

    #[test]
    fn hidden_window_leaves_open_chat_unread() {
        // Hidden to the tray, egui's input still says focused (the last shown frame's): no read receipt.
        let (mut h, c, tx) = open_contact_chat();
        h.state_mut().state.hidden = true;
        let id = h.state().core.add_local_message(c, 1, 0, true, false, T, 0, 0, "привет".as_bytes().to_vec()).unwrap();
        tx.send(received(c, id, 0, 0)).unwrap();
        h.run();
        assert_eq!(h.state().core.get_unread_count(c), 1, "not read while hidden");
        h.state_mut().state.hidden = false;
        h.run();
        assert_eq!(h.state().core.get_unread_count(c), 0, "read once shown again");
    }

    #[test]
    fn reaction_refreshes_chips_not_rows() {
        let (mut h, c, tx) = open_contact_chat();
        let core = h.state().core.clone();
        let id = core.add_local_message(c, 5, 0, true, true, T, 0, 0, b"hi".to_vec()).unwrap();
        tx.send(received(c, id, 0, 0)).unwrap();
        h.run();
        assert!(h.query_by_label("👍").is_none());
        let r = core.add_local_message(c, 6, 5, true, true, T + 1, 0, 10, reaction_payload("👍", 5)).unwrap();
        tx.send(received(c, r, 10, 5)).unwrap();
        h.run();
        assert_eq!(h.state().state.open_chat.as_ref().unwrap().timeline.msgs.len(), 1);
        assert!(h.query_by_label("👍").is_some());
        assert_eq!(preview(&h, c), "hi");
    }

    #[test]
    fn contact_connected_rebuilds_rows() {
        // The core clears our outgoing request on auto-accept without an event; the connect that follows refreshes the row.
        let (mut h, c, tx) = open_contact_chat();
        h.state().core.add_local_message(c, 1, 0, true, true, T, 0, 0, "тихо".as_bytes().to_vec()).unwrap(); // no event
        h.run();
        assert_ne!(preview(&h, c), "тихо");
        tx.send(Event::ContactConnected { pubkey: vec![7; 32] }).unwrap();
        h.run();
        assert_eq!(preview(&h, c), "тихо");
    }

    #[test]
    fn delivered_event_sets_tick() {
        let (mut h, c, tx) = open_contact_chat();
        h.state().core.add_local_message(c, 9, 0, false, false, T, 0, 0, b"out".to_vec()).unwrap();
        h.state_mut().state.open_chat = None; // reopen with the row loaded
        h.run();
        tx.send(Event::MessageDelivered { contact_pubkey: vec![7; 32], guid: 9 }).unwrap();
        h.run();
        assert!(h.state().state.open_chat.as_ref().unwrap().timeline.msgs[0].delivered);
    }

    #[test]
    fn synced_messages_reload_open_chat() {
        let (mut h, c, tx) = open_contact_chat();
        h.state().core.add_local_message(c, 3, 0, true, true, T, 0, 0, b"synced".to_vec()).unwrap();
        tx.send(Event::MessagesSynced { contact_id: c }).unwrap();
        h.run();
        assert_eq!(h.state().state.open_chat.as_ref().unwrap().timeline.msgs.len(), 1);
    }

    #[test]
    fn start_result_sets_core_started_and_dots() {
        let (mut h, _, tx) = open_contact_chat();
        let dots = |h: &Harness<'static, MimirDesktop>| { let n = &h.state().state.net; (n.ygg, n.tracker, n.mediator) };
        assert_eq!(dots(&h), (Dot::Red, Dot::Red, Dot::Red));
        tx.send(Event::WorkDone(WorkResult::Failed { what: "start", err: "port busy".into() })).unwrap();
        h.run();
        assert!(!h.state().state.core_started);
        assert!(h.query_by_label_contains("port busy").is_some());
        tx.send(Event::WorkDone(WorkResult::Started)).unwrap();
        h.run();
        assert!(h.state().state.core_started);
        assert_eq!(dots(&h), (Dot::Yellow, Dot::Yellow, Dot::Yellow));
    }

    #[test]
    fn block_strangers_reapplied_after_start() {
        // The core has no getter for the block flag: what is tested is that every start re-applies the settings.
        let (mut h, _, tx) = open_contact_chat();
        h.state_mut().settings.block_strangers = true;
        h.run();
        assert_eq!(h.state().state.live_applied, 0);
        tx.send(Event::WorkDone(WorkResult::Started)).unwrap();
        tx.send(Event::WorkDone(WorkResult::Started)).unwrap();
        h.run();
        assert_eq!(h.state().state.live_applied, 2);
    }

    #[test]
    fn offline_send_is_queued_without_status_line() {
        let (mut h, c, _tx) = open_contact_chat();
        let ctx = h.ctx.clone();
        crate::test_support::start_app(h.state_mut(), &ctx);
        crate::model::send::send_text(&h.state().worker, crate::model::send::Target::for_key(ChatKey::Contact(c), vec![7; 32]), 0, "offline".into());
        let t = Instant::now();
        // Row via OutgoingMessageStored, then the job's own result.
        while t.elapsed() < Duration::from_secs(5) {
            h.run();
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(h.state().state.open_chat.as_ref().unwrap().timeline.msgs.len(), 1);
        assert_eq!(h.state().state.status, None);
    }

    /// App with group 7 «Семья» open (member Аня, pk 1); the sender feeds the app's event channel.
    fn open_group_chat() -> (Harness<'static, MimirDesktop>, Sender<Event>) {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        let core = h.state().core.clone();
        core.save_group_chat(7, "Семья".into(), String::new(), None, vec![9; 32], vec![], vec![3; 32], 0).unwrap();
        core.update_member_info(7, vec![1; 32], Some("Аня".into()), None, None).unwrap();
        let (tx, rx) = channel();
        h.state_mut().rx = rx;
        h.state_mut().state.chats = build_chat_list(&core, Lang::Ru);
        h.state_mut().state.selected = Some(ChatKey::Group(7));
        h.run();
        (h, tx)
    }

    fn group_preview(h: &Harness<'static, MimirDesktop>, id: i64) -> String {
        h.state().state.chats.iter().find(|r| r.key == ChatKey::Group(id)).unwrap().preview.clone()
    }

    fn open<'a>(h: &'a Harness<'static, MimirDesktop>) -> &'a crate::model::OpenChat {
        h.state().state.open_chat.as_ref().unwrap()
    }

    fn system_row(code: u8, parts: &[&[u8]]) -> Vec<u8> {
        let mut v = vec![code];
        for p in parts {
            v.extend_from_slice(p);
        }
        v.extend([0; 32]);
        v
    }

    #[test]
    fn group_events_update_open_chat() {
        use crate::model::group::{GroupStatus, SYSTEM};
        let (mut h, tx) = open_group_chat();
        let core = h.state().core.clone();
        let id = core.add_group_message(7, 1, 11, vec![1; 32], T, 0, false, "привет".as_bytes().to_vec(), 0, false).unwrap();
        tx.send(Event::GroupMessageReceived { chat_id: 7, message_id: id, msg_type: 0, reply_to: 0 }).unwrap();
        h.run();
        assert_eq!(open(&h).timeline.msgs.last().map(|m| m.id), Some(id), "received row appended");
        assert_eq!(group_preview(&h, 7), "привет");
        assert_eq!(core.get_group_chat(7).unwrap().unread_count, 0, "read while on screen");

        core.delete_group_message_by_guid(7, 11);
        tx.send(Event::GroupMessageDeleted { chat_id: 7, guid: 11 }).unwrap();
        h.run();
        assert!(open(&h).timeline.msgs.is_empty(), "deleted row removed");
        assert_eq!(group_preview(&h, 7), "[ пустой чат ]");

        core.add_group_message(7, 2, 12, vec![1; 32], T + 1, 0, false, "из истории".as_bytes().to_vec(), 0, true).unwrap();
        tx.send(Event::GroupHistorySynced { chat_id: 7 }).unwrap();
        h.run();
        assert_eq!(open(&h).timeline.msgs.len(), 1, "history sync reloads");

        assert_eq!(open(&h).counts, (1, 0));
        core.update_member_info(7, vec![2; 32], Some("Митя".into()), None, None).unwrap();
        core.add_group_message(7, 3, 13, vec![9; 32], T + 2, SYSTEM, true, system_row(0x01, &[&[2; 32], &[1; 32]]), 0, false).unwrap();
        tx.send(Event::GroupMembersChanged { chat_id: 7 }).unwrap();
        h.run();
        assert_eq!(open(&h).counts, (2, 0), "members reloaded");
        assert_eq!(open(&h).timeline.msgs.len(), 2, "system row appended");
        assert!(h.query_by_label("Митя принял(а) приглашение от Аня").is_some());

        core.update_member_online(7, vec![2; 32], true, 0).unwrap();
        tx.send(Event::GroupMemberOnline { chat_id: 7, pubkey: vec![2; 32], online: true }).unwrap();
        h.run();
        assert_eq!(open(&h).counts, (2, 1));
        assert!(h.query_by_label("2 участников, 1 в сети").is_some());

        tx.send(Event::GroupChatStatus { chat_id: 7, status: "SUBSCRIBED".into() }).unwrap();
        h.run();
        assert_eq!(open(&h).status, GroupStatus::Subscribed);
        tx.send(Event::GroupChatStatus { chat_id: 7, status: "DISCONNECTED".into() }).unwrap();
        h.run();
        assert_eq!(open(&h).status, GroupStatus::Disconnected);

        // Another group: only its row changes.
        core.save_group_chat(8, "Дача".into(), String::new(), None, vec![9; 32], vec![], vec![3; 32], 0).unwrap();
        let other = core.add_group_message(8, 1, 21, vec![1; 32], T + 3, 0, false, "там".as_bytes().to_vec(), 0, false).unwrap();
        tx.send(Event::GroupMessageReceived { chat_id: 8, message_id: other, msg_type: 0, reply_to: 0 }).unwrap();
        tx.send(Event::GroupChatStatus { chat_id: 8, status: "SUBSCRIBED".into() }).unwrap();
        tx.send(Event::GroupMembersChanged { chat_id: 8 }).unwrap();
        h.run();
        assert_eq!(open(&h).timeline.msgs.len(), 2);
        assert_eq!((open(&h).status, open(&h).counts), (GroupStatus::Disconnected, (2, 1)));
        assert_eq!(group_preview(&h, 8), "там");
        assert_eq!(core.get_group_chat(8).unwrap().unread_count, 1, "not the open chat: stays unread");
    }

    #[test]
    fn group_delivered_tick() {
        let (mut h, tx) = open_group_chat();
        let core = h.state().core.clone();
        let id = core.add_group_message(7, 0, 11, core.public_key(), T, 0, false, "мой".as_bytes().to_vec(), 0, false).unwrap();
        tx.send(Event::OutgoingMessageStored { is_group: true, target_id: 7, message_id: id }).unwrap();
        h.run();
        assert!(!open(&h).timeline.msgs[0].delivered);
        tx.send(Event::GroupMessageDelivered { chat_id: 8, guid: 11 }).unwrap();
        h.run();
        assert!(!open(&h).timeline.msgs[0].delivered, "another group's event");
        tx.send(Event::GroupMessageDelivered { chat_id: 7, guid: 11 }).unwrap();
        h.run();
        assert!(open(&h).timeline.msgs[0].delivered, "mediator accepted: single tick");
    }

    #[test]
    fn chat_stats_set_channel_counts() {
        let (mut h, tx) = open_group_chat();
        h.state().core.save_group_chat(7, "Канал".into(), String::new(), None, vec![9; 32], vec![], vec![3; 32], 1).unwrap();
        tx.send(Event::ChatStats { chat_id: 7, subscribers: 40, online: 3 }).unwrap();
        h.run();
        assert_eq!(open(&h).counts, (40, 3));
    }

    #[test]
    fn chat_info_changed_renames_locally() {
        let (mut h, tx) = open_group_chat();
        let core = h.state().core.clone();
        let mut info = vec![0x06, 0x01, 32];
        info.extend([1; 32]);
        info.extend([0x20, "Дача".len() as u8]);
        info.extend("Дача".as_bytes());
        core.add_group_message(7, 1, 11, vec![9; 32], T, crate::model::group::SYSTEM, true, info, 0, false).unwrap();
        tx.send(Event::GroupMembersChanged { chat_id: 7 }).unwrap();
        h.run();
        assert_eq!(core.get_group_chat(7).unwrap().name, "Дача");
        assert!(h.state().state.chats.iter().any(|r| r.key == ChatKey::Group(7) && r.name == "Дача"), "row renamed");
        assert!(h.query_by_role_and_label(egui::accesskit::Role::Label, "Дача").is_some(), "header renamed");
    }

    #[test]
    fn group_mark_read_when_focused() {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        let core = h.state().core.clone();
        core.save_group_chat(7, "Семья".into(), String::new(), None, vec![9; 32], vec![], vec![3; 32], 0).unwrap();
        let now = time::OffsetDateTime::now_utc().unix_timestamp() * 1000;
        core.add_group_message(7, 1, 11, core.public_key(), now - 3000, 0, false, b"m0".to_vec(), 0, false).unwrap();
        let first = core.add_group_message(7, 2, 12, vec![1; 32], now - 2000, 0, false, b"m1".to_vec(), 0, false).unwrap();
        core.add_group_message(7, 3, 13, vec![1; 32], now - 1000, 0, false, b"m2".to_vec(), 0, false).unwrap();
        assert_eq!(core.get_group_chat(7).unwrap().unread_count, 2);
        h.state_mut().state.chats = build_chat_list(&core, Lang::Ru);
        h.state_mut().state.selected = Some(ChatKey::Group(7));
        h.run();
        assert_eq!(core.get_group_chat(7).unwrap().unread_count, 0);
        assert_eq!(h.state().state.chats.iter().find(|r| r.key == ChatKey::Group(7)).unwrap().unread, 0);
        assert_eq!(open(&h).unread_from, Some(first));
        let sep = h.get_by_label("Непрочитанные сообщения").rect();
        assert!(sep.top() >= h.get_by_label("m0").rect().bottom() && sep.bottom() <= h.get_by_label("m1").rect().top(), "separator above the first unread");
    }

    #[test]
    fn draft_synced_ignores_unknown_chat_type() {
        let (mut h, tx) = open_group_chat();
        h.ctx.memory_mut(|m| if let Some(id) = m.focused() { m.surrender_focus(id) });
        h.run();
        h.state().core.save_draft(1, 7, Some("с телефона".into()), None, 0).unwrap();
        tx.send(Event::DraftSynced { chat_type: 2, chat_id: 7 }).unwrap();
        h.run();
        assert_eq!(h.state().state.composer.text, "", "chat_type 2 is not a group");
        tx.send(Event::DraftSynced { chat_type: 1, chat_id: 7 }).unwrap();
        h.run();
        assert_eq!(h.state().state.composer.text, "с телефона");
    }

    #[test]
    fn read_only_survives_disconnect() {
        use crate::model::group::GroupStatus;
        use egui::accesskit::Role;
        let (mut h, tx) = open_group_chat();
        let composer = |h: &Harness<'static, MimirDesktop>| h.query_by_role(Role::MultilineTextInput).is_some();
        assert!(composer(&h), "writable at first");
        tx.send(Event::GroupChatStatus { chat_id: 7, status: "READ_ONLY".into() }).unwrap();
        h.run();
        assert!(!composer(&h));
        // The core reports DISCONNECTED for every chat of a dropped mediator, read-only ones too.
        for s in ["DISCONNECTED", "CONNECTING"] {
            tx.send(Event::GroupChatStatus { chat_id: 7, status: s.into() }).unwrap();
            h.run();
            assert_eq!(open(&h).status, GroupStatus::ReadOnly, "{s}");
            assert!(!composer(&h), "{s}: no composer");
            assert!(h.query_by_label("Только чтение").is_some(), "{s}");
        }
        tx.send(Event::GroupChatStatus { chat_id: 7, status: "SUBSCRIBED".into() }).unwrap();
        h.run();
        assert_eq!(open(&h).status, GroupStatus::Subscribed, "only a subscription ends read-only");
        assert!(composer(&h));
        // The stored flag keeps the chat read-only whatever the subscription says.
        h.state().core.set_group_chat_read_only(7, true).unwrap();
        tx.send(Event::GroupMembersChanged { chat_id: 7 }).unwrap();
        h.run();
        assert!(!composer(&h), "stored read-only flag");
    }

    #[test]
    fn group_lane_does_not_hold_up_contact_jobs() {
        let h = test_harness(Settings { intro_done: true, ..Default::default() });
        let app = h.state();
        // A mediator job stuck on a dial or request timeout, then a 1:1 job.
        app.worker_for(ChatKey::Group(7)).submit(Box::new(|_| {
            std::thread::sleep(Duration::from_secs(3));
            WorkResult::Done
        }));
        let t = Instant::now();
        app.worker_for(ChatKey::Contact(1)).submit(Box::new(|_| WorkResult::Started));
        let first = app.rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(first, Event::WorkDone(WorkResult::Started)), "the 1:1 job is not queued behind the mediator job");
        assert!(t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());
        assert!(matches!(app.rx.recv_timeout(Duration::from_secs(5)).unwrap(), Event::WorkDone(WorkResult::Done)), "same result channel");
        app.worker_for(ChatKey::SAVED).submit(Box::new(|_| WorkResult::Started));
        assert!(matches!(app.rx.recv_timeout(Duration::from_secs(1)).unwrap(), Event::WorkDone(WorkResult::Started)));
    }

    #[test]
    fn notices_and_errors_last_8s() {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        let app = h.state_mut();
        for ok in [true, false] {
            let t = Instant::now();
            if ok { app.notice_text("готово".into()) } else { app.error_text("ошибка".into()) }
            let left = app.state.status.as_ref().unwrap().1.unwrap() - t;
            assert!(left >= Duration::from_secs(8) && left < Duration::from_secs(9), "{left:?}");
            assert_eq!(app.state.status_ok, ok);
        }
    }

    #[test]
    fn worker_wakes_harness_context() {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        h.state().worker.submit(Box::new(|_| WorkResult::Started));
        let t = Instant::now();
        while !h.ctx.has_requested_repaint() && t.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(h.ctx.has_requested_repaint(), "the worker must wake the harness's own context");
        h.run();
        assert!(h.state().state.core_started);
    }

    #[test]
    fn file_progress_tracks_and_clears() {
        use crate::model::attachment::Transfer;
        let (mut h, tx) = open_group_chat();
        let core = h.state().core.clone();
        let meta = br#"{"name":"Ab3d.jpg","originalName":"p.jpg","size":10,"mimeType":"image/jpeg","hash":"cd","key":"a2V5","server":"00"}"#;
        core.add_group_message(7, 1, 11, vec![1; 32], T, 1, false, meta.to_vec(), 0, false).unwrap();
        tx.send(Event::GroupHistorySynced { chat_id: 7 }).unwrap();
        h.run();
        tx.send(Event::FileProgress { guid: 11, bytes_done: 4, total_bytes: 26, is_upload: false }).unwrap();
        h.run();
        assert_eq!(h.state().state.transfers.get(&11), Some(&Transfer::Downloading { done: 4, total: 26 }));
        tx.send(Event::FileProgress { guid: 12, bytes_done: 1, total_bytes: 2, is_upload: true }).unwrap();
        h.run();
        assert_eq!(h.state().state.transfers.get(&12), Some(&Transfer::Uploading { done: 1, total: 2 }));
        // The core always reports chat_id 0: the row is found by guid in the open chat and re-read.
        let newer = br#"{"name":"Ab3d.jpg","originalName":"p.jpg","size":10,"mimeType":"image/jpeg","hash":"cd","key":"a2V5","server":"00","text":"x"}"#;
        core.update_group_message_data(7, 11, newer.to_vec()).unwrap();
        tx.send(Event::FileDownloaded { chat_id: 0, guid: 11, file_path: "stale".into() }).unwrap();
        h.run();
        assert!(!h.state().state.transfers.contains_key(&11), "cleared");
        assert_eq!(open(&h).timeline.msgs[0].data.as_deref(), Some(newer.as_slice()), "row re-read by guid");
        assert!(h.state().state.transfers.contains_key(&12), "another transfer stays");
    }

    #[test]
    fn download_failure_is_localized() {
        use crate::model::attachment::Transfer;
        let (mut h, _c, tx) = open_contact_chat();
        h.state_mut().state.transfers.insert(5, Transfer::Downloading { done: 0, total: 1 });
        tx.send(Event::WorkDone(WorkResult::DownloadFailed { guid: 5, err: Some("нет".into()) })).unwrap();
        h.run();
        assert!(h.state().state.transfers.is_empty());
        assert_eq!(h.state().state.status.as_ref().map(|s| s.0.as_str()), Some("Не удалось получить файл: нет"));
        assert!(!h.state().state.status_ok);
    }

    #[test]
    fn received_and_synced_files_clear_transfers() {
        use crate::model::attachment::Transfer;
        let (mut h, c, tx) = open_contact_chat();
        let core = h.state().core.clone();
        for (guid, name) in [(21, "Aa.pdf"), (22, "Bb.pdf")] {
            let meta = format!(r#"{{"name":"{name}","originalName":"a.pdf","size":5,"hash":"ab","mimeType":"application/pdf"}}"#);
            core.add_local_message(c, guid, 0, true, true, T + guid, 0, 3, meta.into_bytes()).unwrap();
        }
        let files = h.state().paths.files_dir.clone();
        std::fs::create_dir_all(&files).unwrap();
        for g in [21, 22] {
            h.state_mut().state.transfers.insert(g, Transfer::Downloading { done: 0, total: 5 });
        }
        // An outgoing file being uploaded: already on disk, but not done.
        core.add_local_message(c, 23, 0, false, false, T + 23, 0, 3, br#"{"name":"Cc.pdf","size":5,"hash":"ab"}"#.to_vec()).unwrap();
        std::fs::write(files.join("Cc.pdf"), b"12345").unwrap();
        h.state_mut().state.transfers.insert(23, Transfer::Uploading { done: 1, total: 5 });
        // P2P: the file_path given is the stale temp path; only the guid counts.
        tx.send(Event::FileReceived { pubkey: vec![7; 32], guid: 21, meta_json: "{}".into(), file_path: "stale".into() }).unwrap();
        h.run();
        assert!(!h.state().state.transfers.contains_key(&21));
        // Sibling blob: no guid in the event; downloads whose file is now on disk end, the others stay.
        tx.send(Event::MessagesSynced { contact_id: c }).unwrap();
        h.run();
        assert!(h.state().state.transfers.contains_key(&22), "not on disk yet");
        std::fs::write(files.join("Bb.pdf"), b"12345").unwrap();
        tx.send(Event::MessagesSynced { contact_id: c }).unwrap();
        h.run();
        assert!(!h.state().state.transfers.contains_key(&22), "landed");
        assert_eq!(h.state().state.transfers.get(&23), Some(&Transfer::Uploading { done: 1, total: 5 }), "uploads survive MessagesSynced");
    }

    #[test]
    fn group_upload_failure_reported() {
        use crate::model::attachment::Transfer;
        let (mut h, tx) = open_group_chat();
        let core = h.state().core.clone();
        // The row as send_group_chat_message stores it before the upload: no key, no server, undelivered.
        let meta = br#"{"name":"Ab.pdf","originalName":"a.pdf","size":5,"mimeType":"application/pdf","hash":"ab"}"#;
        let id = core.add_group_message(7, 0, 11, core.public_key(), T, 3, false, meta.to_vec(), 0, false).unwrap();
        h.state_mut().state.pending_sends.insert(ChatKey::Group(7), 1); // as send_attachment does
        tx.send(Event::OutgoingMessageStored { is_group: true, target_id: 7, message_id: id }).unwrap();
        h.run();
        assert_eq!(h.state().state.transfers.get(&11), Some(&Transfer::Uploading { done: 0, total: 5 }), "uploading from the start");
        tx.send(Event::FileProgress { guid: 11, bytes_done: 2, total_bytes: 26, is_upload: true }).unwrap();
        h.run();
        assert_eq!(h.state().state.transfers.get(&11), Some(&Transfer::Uploading { done: 2, total: 26 }));
        let err = mimir::MimirError::Connection("files node not ready".into());
        tx.send(Event::WorkDone(WorkResult::AttachmentSent { chat: ChatKey::Group(7), err: Some(err) })).unwrap();
        h.run();
        assert!(h.state().state.transfers.is_empty() && h.state().state.sending.is_empty(), "the job ended: no ring left");
        let status = h.state().state.status.as_ref().map(|s| s.0.clone()).unwrap_or_default();
        assert!(status.starts_with("Не удалось отправить файл: ") && status.contains("files node not ready"), "{status}");
        assert!(!h.state().state.status_ok);
        assert!(!core.get_group_message_by_guid(7, 11).unwrap().delivered, "the row stays undelivered");
    }

    #[test]
    fn uploaded_group_file_waiting_for_the_mediator_is_queued() {
        let (mut h, tx) = open_group_chat();
        let core = h.state().core.clone();
        let meta = br#"{"name":"Ab.pdf","originalName":"a.pdf","size":5,"mimeType":"application/pdf","hash":"cd","key":"a2V5","server":"00"}"#;
        let id = core.add_group_message(7, 0, 12, core.public_key(), T, 3, false, meta.to_vec(), 0, false).unwrap();
        h.state_mut().state.pending_sends.insert(ChatKey::Group(7), 2);
        tx.send(Event::OutgoingMessageStored { is_group: true, target_id: 7, message_id: id }).unwrap();
        // Uploaded, then no mediator: resend_undelivered sends it later as it is.
        let err = mimir::MimirError::Connection("no mediator".into());
        tx.send(Event::WorkDone(WorkResult::AttachmentSent { chat: ChatKey::Group(7), err: Some(err) })).unwrap();
        h.run();
        assert!(h.state().state.transfers.is_empty());
        assert_eq!(h.state().state.status, None, "queued, not an error");
        // A finished send: the row is re-read (the upload rewrote its JSON) and its ring goes.
        let id = core.add_group_message(7, 0, 13, core.public_key(), T + 1, 1, false, meta.to_vec(), 0, false).unwrap();
        tx.send(Event::OutgoingMessageStored { is_group: true, target_id: 7, message_id: id }).unwrap();
        h.run();
        assert!(h.state().state.transfers.contains_key(&13));
        tx.send(Event::WorkDone(WorkResult::AttachmentSent { chat: ChatKey::Group(7), err: None })).unwrap();
        h.run();
        assert!(h.state().state.transfers.is_empty());
        assert_eq!(h.state().state.status, None);
        assert_eq!(h.state().state.pending_sends.get(&ChatKey::Group(7)).copied().unwrap_or(0), 0, "both jobs ended");
        h.state_mut().state.pending_sends.insert(ChatKey::Group(7), 1);
        // A text row is not an upload.
        let id = core.add_group_message(7, 0, 14, core.public_key(), T + 2, 0, false, b"hi".to_vec(), 0, false).unwrap();
        tx.send(Event::OutgoingMessageStored { is_group: true, target_id: 7, message_id: id }).unwrap();
        h.run();
        assert!(h.state().state.transfers.is_empty() && h.state().state.sending.is_empty());
    }

    #[test]
    fn sibling_send_to_a_closed_chat_refreshes_the_list() {
        // Our other device's message to a chat that is not open still moves the chat list.
        let (mut h, _, tx) = open_contact_chat();
        let core = h.state().core.clone();
        let b = core.add_contact(vec![8; 32], "Борис".into()).unwrap();
        h.state_mut().state.chats = build_chat_list(&core, Lang::Ru);
        let id = core.add_local_message(b, 41, 0, false, true, T, 0, 0, "с телефона".as_bytes().to_vec()).unwrap();
        tx.send(Event::OutgoingMessageStored { is_group: false, target_id: b, message_id: id }).unwrap();
        h.run();
        assert!(preview(&h, b).contains("с телефона"), "{}", preview(&h, b));
    }

    #[test]
    fn only_our_own_sends_show_uploading() {
        use crate::model::attachment::Transfer;
        let (mut h, c, tx) = open_contact_chat();
        let core = h.state().core.clone();
        let meta = |n: &str| format!(r#"{{"name":"{n}","originalName":"a.pdf","size":5,"hash":"ab","mimeType":"application/pdf"}}"#).into_bytes();
        // A row our other device sent, synced here (sibling_sync fires OutgoingMessageStored; stored delivered).
        let sib = core.add_local_message(c, 31, 0, false, true, T, 0, 3, meta("Sa.pdf")).unwrap();
        tx.send(Event::OutgoingMessageStored { is_group: false, target_id: c, message_id: sib }).unwrap();
        h.run();
        assert!(h.state().state.transfers.is_empty() && h.state().state.sending.is_empty(), "no send in flight: a normal row");
        // Our send in flight: its row is uploading; a sibling row arriving meanwhile still is not.
        h.state_mut().state.pending_sends.insert(ChatKey::Contact(c), 1);
        let sib2 = core.add_local_message(c, 33, 0, false, true, T + 2, 0, 3, meta("Sb.pdf")).unwrap();
        tx.send(Event::OutgoingMessageStored { is_group: false, target_id: c, message_id: sib2 }).unwrap();
        let ours = core.add_local_message(c, 32, 0, false, false, T + 1, 0, 3, meta("Oa.pdf")).unwrap();
        tx.send(Event::OutgoingMessageStored { is_group: false, target_id: c, message_id: ours }).unwrap();
        h.run();
        assert_eq!(h.state().state.transfers.get(&32), Some(&Transfer::Uploading { done: 0, total: 5 }));
        assert!(!h.state().state.transfers.contains_key(&33));
        tx.send(Event::WorkDone(WorkResult::AttachmentSent { chat: ChatKey::Contact(c), err: None })).unwrap();
        h.run();
        assert!(h.state().state.transfers.is_empty() && h.state().state.sending.is_empty(), "the right row cleared");
        assert_eq!(h.state().state.pending_sends.get(&ChatKey::Contact(c)).copied().unwrap_or(0), 0);
    }
}
