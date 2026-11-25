use super::events::Event;
use crate::model::chat_key::ChatKey;
use eframe::egui;
use mimir::MimirApp;
use std::sync::{Arc, mpsc::{self, Sender}};

#[derive(Debug)]
pub enum WorkResult {
    Sent { chat: ChatKey, message_id: i64 },
    /// `opened_from`: the chat selected when the add was submitted; the new chat opens only if it still is.
    ContactAdded { contact_id: i64, opened_from: Option<ChatKey> },
    /// A group made by `create_group` (network + proof-of-work).
    GroupCreated { chat_id: i64 },
    /// The public group joined by link (`join_public_group`).
    GroupJoined { chat_id: i64 },
    /// `accept_group_invite` succeeded; the chat is stored locally.
    InviteAccepted { chat_id: i64 },
    /// Result of looking a public group up by link; `None` once the retries ran out.
    /// `token`: the join dialog's lookup this answers.
    Preview { token: u64, info: Option<mimir::ChannelInfo> },
    RequestAccepted { contact_id: i64 },
    RequestDeclined,
    Started,
    /// A job with nothing to report (dials, reaction sends).
    Done,
    /// A job that succeeded with something to tell the user (status line, accent colour).
    Notice(crate::i18n::Key),
    /// `leave_group` succeeded (the core has already deleted the chat locally).
    Left { chat_id: i64 },
    /// The owner's `delete_group` succeeded (on the mediator and locally).
    GroupDeleted { chat_id: i64 },
    /// `clear_group_chat_history` finished (rows and attachment files).
    HistoryCleared { chat_id: i64 },
    /// `invite_to_group` succeeded; `name` is the invitee's.
    Invited { name: String },
    /// `update_account_avatar` stored our new avatar at `path` (relative to the core's data dir).
    AccountAvatar { path: String },
    /// `update_group_info` succeeded.
    GroupInfoUpdated { chat_id: i64 },
    /// `change_member_role` to banned succeeded for member `pubkey` of `chat_id`.
    Banned { chat_id: i64, pubkey: Vec<u8>, name: String },
    /// `change_member_role` succeeded; `perms` is the member's new permission byte.
    RoleChanged { chat_id: i64, pubkey: Vec<u8>, perms: u8, name: String },
    /// A manual download could not start; the row goes back to Idle. `None`: the contact is offline and
    /// is being re-dialled (not reported, as on Android).
    DownloadFailed { guid: i64, err: Option<String> },
    /// An attachment send job of chat `chat` ended; `err` is the core's error, if any (a 1:1
    /// `Connection` error means queued).
    AttachmentSent { chat: ChatKey, err: Option<mimir::MimirError> },
    /// `fix_corrupted_group_messages` ended (Advanced).
    Fixed { fixed: u32, scanned: u32 },
    /// «Удалить потерянные медиафайлы» removed `count` files of `bytes` in total.
    OrphansDeleted { count: usize, bytes: u64 },
    /// A backup export ended: `path` `None` = the save dialog was cancelled; `err` the failure.
    BackupSaved { path: Option<std::path::PathBuf>, err: Option<String> },
    /// A backup file was picked for import (`path` `None`: the open dialog was cancelled); `valid`: `backup::validate` accepted it.
    BackupPicked { path: Option<std::path::PathBuf>, valid: bool },
    /// The picked backup was staged for the next start (`err` `None`), which the app now relaunches into.
    BackupStaged { err: Option<String> },
    /// The Files section's listing.
    Files(Vec<crate::ui::settings::FileRow>),
    /// The Files section deleted `storage_name` (`err`: why it could not).
    FileDeleted { storage_name: String, err: Option<String> },
    Failed { what: &'static str, err: String },
    /// A failure already worded for the status line.
    Error(String),
}

pub type Job = Box<dyn FnOnce(&MimirApp) -> WorkResult + Send>;

/// Runs blocking core calls off the UI thread; results come back as `Event::WorkDone`.
#[derive(Clone)]
pub struct Worker {
    jobs: Sender<Job>,
}

impl Worker {
    pub fn spawn(app: Arc<MimirApp>, tx: Sender<Event>, ctx: egui::Context) -> Worker {
        let (jobs, rx) = mpsc::channel::<Job>();
        std::thread::spawn(move || {
            // Ends when the Worker (job sender) is dropped.
            for job in rx {
                // A panicking job (e.g. a send before the core started) must not end the worker.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(&app))).unwrap_or_else(|p| {
                    let err = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned());
                    WorkResult::Failed { what: "panic", err: err.unwrap_or_default() }
                });
                let _ = tx.send(Event::WorkDone(result));
                ctx.request_repaint();
            }
        });
        Worker { jobs }
    }

    pub fn submit(&self, job: Job) {
        let _ = self.jobs.send(job);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{core, paths::DataPaths};
    use std::time::Duration;

    #[test]
    fn worker_runs_off_thread() {
        let tmp = tempfile::tempdir().unwrap();
        let app = core::open(&DataPaths::under(tmp.path())).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let w = Worker::spawn(app, tx, egui::Context::default());
        let ui_thread = std::thread::current().id();
        w.submit(Box::new(move |_| {
            assert_ne!(std::thread::current().id(), ui_thread);
            WorkResult::Started
        }));
        assert!(matches!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), Event::WorkDone(WorkResult::Started)));
    }

    #[test]
    fn panicking_job_does_not_kill_worker() {
        let tmp = tempfile::tempdir().unwrap();
        let app = core::open(&DataPaths::under(tmp.path())).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let w = Worker::spawn(app, tx, egui::Context::default());
        w.submit(Box::new(|_| panic!("boom")));
        w.submit(Box::new(|_| WorkResult::Started));
        let Event::WorkDone(WorkResult::Failed { what, err }) = rx.recv_timeout(Duration::from_secs(5)).unwrap() else { panic!() };
        assert_eq!((what, err.as_str()), ("panic", "boom"));
        assert!(matches!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), Event::WorkDone(WorkResult::Started)));
    }
}
