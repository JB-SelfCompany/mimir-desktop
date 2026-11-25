//! Attachment metadata (the JSON body of types 1, 3, 4; CA §3), where its file lives, its transfer state and the
//! manual download (P3-R9).
use crate::{app::MimirDesktop, core::worker::WorkResult, model::chat_key::ChatKey};
use mimir::MimirError;
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// CA §3.1 fields. `key`/`server` are present only on a group row whose upload finished.
#[derive(Deserialize, Default, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct AttachmentMeta {
    /// Storage name in the attachments dir (random, with an extension).
    pub name: String,
    pub original_name: String,
    /// Plaintext bytes; 0 = unknown.
    pub size: i64,
    #[serde(rename = "mimeType")]
    pub mime: String,
    /// 1:1: SHA-256 hex of the plaintext; group: of the ciphertext.
    pub hash: String,
    /// Group file key, base64 (or hex); its presence means "fetch from the file server".
    pub key: Option<String>,
    /// File server public key, hex.
    pub server: Option<String>,
    #[serde(rename = "text")]
    pub caption: String,
}

impl AttachmentMeta {
    pub fn parse(data: &[u8]) -> Option<AttachmentMeta> {
        serde_json::from_slice(data).ok()
    }

    /// The name the sender picked, else the storage name.
    pub fn display_name(&self) -> &str {
        if self.original_name.is_empty() { &self.name } else { &self.original_name }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentKind {
    Image,
    File,
    Voice,
    /// Any other `audio/*` file: a file bubble that plays inline (P3-R4).
    Audio,
    Video,
}

/// P3-R4: voice is type 3 named "voice_message.m4a", or audio/mp4 with a .m4a name.
pub fn kind(msg_type: i32, m: &AttachmentMeta) -> AttachmentKind {
    let m4a = m.display_name().to_ascii_lowercase().ends_with(".m4a");
    match msg_type {
        1 => AttachmentKind::Image,
        4 => AttachmentKind::Video,
        _ if m.original_name == "voice_message.m4a" || (m.mime == "audio/mp4" && m4a) => AttachmentKind::Voice,
        _ if m.mime.starts_with("audio/") => AttachmentKind::Audio,
        _ => AttachmentKind::File,
    }
}

/// `<files_dir>/<name>`; `None` for a name the core itself refuses (empty, `/`, `\`, `..`; CA §3.2).
pub fn local_path(files_dir: &Path, m: &AttachmentMeta) -> Option<PathBuf> {
    let bad = m.name.is_empty() || m.name.contains(['/', '\\']) || m.name.contains("..");
    (!bad).then(|| files_dir.join(&m.name))
}

/// The attachment's bytes are on disk (the core's own test, CA §0.3).
pub fn is_present(files_dir: &Path, m: &AttachmentMeta) -> bool {
    local_path(files_dir, m).is_some_and(|p| p.is_file())
}

/// A row's file transfer, kept in `AppState::transfers` by guid; no entry means `Idle`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Transfer {
    #[default]
    Idle,
    Downloading { done: i64, total: i64 },
    Uploading { done: i64, total: i64 },
}

/// Starts fetching the file of row `guid` in chat `key` (P3-R9, CA §8). The caller marks the row
/// `Transfer::Downloading`; progress and completion arrive as file events, a failed start as
/// `WorkResult::DownloadFailed` (which resets it to Idle).
pub fn request_download(app: &MimirDesktop, key: ChatKey, guid: i64, m: &AttachmentMeta) {
    let Some(dest) = local_path(&app.paths.files_dir, m) else { return };
    match (key, m.key.is_some()) {
        // Blocks for the whole transfer (CA §7): the group lane.
        (ChatKey::Group(chat_id), true) => app.group_worker.submit(Box::new(move |core| {
            match core.download_group_file(chat_id, guid, dest.to_string_lossy().into_owned()) {
                Ok(()) => WorkResult::Done,
                Err(e) => WorkResult::DownloadFailed { guid, err: Some(e.to_string()) },
            }
        })),
        // Android parity (AM §10): ask our other devices and the sender at once.
        (ChatKey::Contact(id), false) => {
            let (name, hash, size) = (m.name.clone(), m.hash.clone(), m.size);
            app.worker.submit(Box::new(move |core| {
                let sibling = core.download_sibling_file(id, guid);
                let Some(pubkey) = core.get_contact(id).map(|c| c.pubkey) else {
                    // ponytail: Saved Messages has no contact row, so core 0.3.5 offers no manual download for it.
                    return sibling.map_or_else(|e| WorkResult::DownloadFailed { guid, err: Some(e.to_string()) }, |()| WorkResult::Done);
                };
                // ponytail: a peer that silently drops the request (file gone, hash mismatch) leaves the row Downloading
                // until the user clicks it again (I6).
                match core.request_file(pubkey.clone(), guid, name, hash, size) {
                    Ok(()) => WorkResult::Done,
                    Err(MimirError::Connection(_)) => {
                        let _ = core.connect_to_contact(pubkey);
                        WorkResult::DownloadFailed { guid, err: None }
                    }
                    Err(e) => WorkResult::DownloadFailed { guid, err: Some(e.to_string()) },
                }
            }));
        }
        // A group row without a key (its upload failed, CA §9.1) cannot be fetched by anyone.
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{core::{events::Event, worker::WorkResult}, settings::Settings, test_support::{start_app, test_harness}};
    use std::time::{Duration, Instant};

    const KEY_B64: &str = "q83vEjRWeJCrze8SNFZ4kKvN7xI0VniQq83vEjRWeJA=";
    const SERVER: &str = "0000128c1179f819faaa07fcada9f9803cb288bc68d278da82ca23d8b4b0ea9e";
    const PDF: &[u8] = br#"{"name":"Ab3dEf7hIj9kLm1n.pdf","originalName":"a.pdf","size":5,"hash":"ab","mimeType":"application/pdf"}"#;

    fn meta(original_name: &str, mime: &str) -> AttachmentMeta {
        AttachmentMeta { name: "Ab3dEf7hIj9kLm1n.bin".into(), original_name: original_name.into(), mime: mime.into(), ..Default::default() }
    }

    #[test]
    fn parse_1to1_and_group_meta() {
        // CA §3.1, 1:1 form.
        let one = r#"{"name":"Ab3dEf7hIj9kLm1n.jpg","originalName":"vacation.jpg","size":12345,"hash":"9f86d0","mimeType":"image/jpeg","text":"море"}"#.as_bytes();
        let want = AttachmentMeta {
            name: "Ab3dEf7hIj9kLm1n.jpg".into(), original_name: "vacation.jpg".into(), size: 12345, mime: "image/jpeg".into(),
            hash: "9f86d0".into(), key: None, server: None, caption: "море".into(),
        };
        assert_eq!(AttachmentMeta::parse(one), Some(want));
        // Group form after upload: ciphertext hash, base64 key, hex server; no caption, no originalName.
        let group = format!(r#"{{"name":"Zx9.pdf","size":5,"mimeType":"application/pdf","hash":"cd","key":"{KEY_B64}","server":"{SERVER}"}}"#);
        let g = AttachmentMeta::parse(group.as_bytes()).unwrap();
        assert_eq!((g.key.as_deref(), g.server.as_deref(), g.caption.as_str(), g.size), (Some(KEY_B64), Some(SERVER), "", 5));
        assert_eq!(g.display_name(), "Zx9.pdf", "no originalName: the storage name");
        assert_eq!(AttachmentMeta::parse(one).unwrap().display_name(), "vacation.jpg");
        assert_eq!(AttachmentMeta::parse(b"{oops"), None);
        assert_eq!(AttachmentMeta::parse(br#""text""#), None);
    }

    #[test]
    fn kind_rules() {
        use AttachmentKind::*;
        assert_eq!(kind(3, &meta("voice_message.m4a", "")), Voice, "Android's voice name");
        assert_eq!(kind(3, &meta("memo.M4A", "audio/mp4")), Voice, "audio/mp4 with a .m4a name");
        assert_eq!(kind(3, &meta("song.mp3", "audio/mpeg")), Audio);
        assert_eq!(kind(3, &meta("talk.mp4", "audio/mp4")), Audio, "audio/mp4 but not .m4a");
        assert_eq!(kind(1, &meta("p.png", "image/png")), Image);
        assert_eq!(kind(4, &meta("video_message.mp4", "video/mp4")), Video);
        assert_eq!(kind(3, &meta("a.pdf", "application/pdf")), File);
    }

    #[test]
    fn local_path_rejects_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let named = |n: &str| AttachmentMeta { name: n.into(), ..Default::default() };
        for bad in ["../x", "a/b", "a\\b", "", "..", "x..y"] {
            assert_eq!(local_path(dir, &named(bad)), None, "{bad:?}");
            assert!(!is_present(dir, &named(bad)), "{bad:?}");
        }
        assert_eq!(local_path(dir, &named("Ab3d.jpg")), Some(dir.join("Ab3d.jpg")));
        assert!(!is_present(dir, &named("Ab3d.jpg")), "not downloaded yet");
        std::fs::write(dir.join("Ab3d.jpg"), b"x").unwrap();
        assert!(is_present(dir, &named("Ab3d.jpg")));
    }

    fn next_work(rx: &std::sync::mpsc::Receiver<Event>, within: Duration) -> WorkResult {
        let end = Instant::now() + within;
        loop {
            match rx.recv_timeout(end.saturating_duration_since(Instant::now())) {
                Ok(Event::WorkDone(r)) => return r,
                Ok(_) => continue,
                Err(e) => panic!("no job result within {within:?}: {e}"),
            }
        }
    }

    #[test]
    fn download_routes_by_key() {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        let core = h.state().core.clone();
        let c = core.add_contact(vec![7; 32], "Аня".into()).unwrap();
        core.add_local_message(c, 21, 0, true, true, 1_700_000_000_000, 0, 3, PDF.to_vec()).unwrap();
        let one = AttachmentMeta::parse(PDF).unwrap();
        let group = AttachmentMeta { key: Some(KEY_B64.into()), server: Some(SERVER.into()), ..one.clone() };
        let busy = |w: &crate::core::worker::Worker| w.submit(Box::new(|_| {
            std::thread::sleep(Duration::from_secs(2));
            WorkResult::Done
        }));
        {
            let app = h.state();
            // No key: the 1:1 lane, not queued behind a slow mediator job (core not started: request_file is a no-op).
            busy(&app.group_worker);
            request_download(app, ChatKey::Contact(c), 21, &one);
            assert!(matches!(next_work(&app.rx, Duration::from_secs(1)), WorkResult::Done));
            assert!(matches!(next_work(&app.rx, Duration::from_secs(3)), WorkResult::Done), "the busy job");
            // A key: download_group_file on the group lane (fails at once: the files node is not up).
            busy(&app.worker);
            request_download(app, ChatKey::Group(7), 31, &group);
            assert!(matches!(next_work(&app.rx, Duration::from_secs(1)), WorkResult::DownloadFailed { guid: 31, err: Some(_) }));
            assert!(matches!(next_work(&app.rx, Duration::from_secs(3)), WorkResult::Done), "the busy job");
            // A group row without a key (its upload failed, CA §9.1) has nothing to fetch.
            request_download(app, ChatKey::Group(7), 32, &one);
            assert!(app.rx.recv_timeout(Duration::from_millis(300)).is_err(), "no job");
        }
        // Started but the contact is offline: request_file throws Connection -> back to Idle, redial, no error line.
        let ctx = h.ctx.clone();
        start_app(h.state_mut(), &ctx);
        h.state_mut().state.transfers.insert(21, Transfer::Downloading { done: 0, total: 5 });
        request_download(h.state(), ChatKey::Contact(c), 21, &one);
        let end = Instant::now() + Duration::from_secs(5);
        while h.state().state.transfers.contains_key(&21) && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
            h.run();
        }
        assert_eq!(h.state().state.transfers.get(&21).copied().unwrap_or_default(), Transfer::Idle);
        assert!(h.state().state.status.is_none(), "a Connection error is not reported: {:?}", h.state().state.status);
    }
}
