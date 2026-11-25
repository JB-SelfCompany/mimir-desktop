//! The Advanced section: peers, mediators, the two maintenance jobs, file server and call mediator keys,
//! «Собрать логи» and «Закрыть Мимир». The jobs run on the worker; progress and result show under their row.
use super::{commit, row_title, save, Confirm, Job};
use crate::{
    app::MimirDesktop,
    core::worker::WorkResult,
    i18n::{tr, Key, Lang},
    logs,
    settings::Peer,
    ui::{
        mediators::{self, middle_ellipsis},
        theme::{close_button, danger_button, dialog, dialog_title, field_label, palette, primary_button, Palette},
    },
};
use eframe::egui::{self, vec2, Align, CursorIcon, FontId, Id, Layout, RichText, Role, Sense, TextEdit, WidgetInfo};
use mimir::MimirApp;
use std::{fs, path::{Path, PathBuf}};

/// Yggdrasil peer URL schemes.
const SCHEMES: [&str; 5] = ["tcp", "tls", "quic", "ws", "wss"];

/// `scheme://host:port` with a Yggdrasil scheme; a `?key=…` or path may follow the port (the default peers have one).
/// Android only refuses empty or multi-line text; this is ours.
pub fn valid_peer(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else { return false };
    let addr = rest.split(['?', '/']).next().unwrap_or_default();
    let Some((host, port)) = addr.rsplit_once(':') else { return false };
    SCHEMES.contains(&scheme) && !host.is_empty() && port.parse::<u16>().is_ok()
}

/// A 32-byte public key as 64 hex digits.
fn is_hex_key(key: &str) -> bool {
    key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Regular files directly in `files_dir` that no message names as its attachment (`files::referenced`: images, files, voice and video), as Android's
/// cleaner finds them; `.thumbs/` (a directory) and staged sends (`mimir_staged_*`) are skipped.
pub fn orphans(core: &MimirApp, files_dir: &Path) -> Vec<PathBuf> {
    let known = super::files::referenced(core);
    let Ok(dir) = fs::read_dir(files_dir) else { return vec![] };
    dir.flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            !name.starts_with("mimir_staged_") && !known.contains_key(&name)
        })
        .map(|e| e.path())
        .collect()
}

/// Deletes the orphans (and a video poster cached for one under `.thumbs/`); counts what was removed.
fn delete_orphans(core: &MimirApp, files_dir: &Path) -> WorkResult {
    let (mut count, mut bytes) = (0, 0);
    for path in orphans(core, files_dir) {
        let len = fs::metadata(&path).map_or(0, |m| m.len());
        if fs::remove_file(&path).is_ok() {
            (count, bytes) = (count + 1, bytes + len);
            if let Some(name) = path.file_name() {
                let _ = fs::remove_file(files_dir.join(".thumbs").join(format!("{}.jpg", name.to_string_lossy())));
            }
        }
    }
    WorkResult::OrphansDeleted { count, bytes }
}

/// An Android settings row that does something when clicked: title (14) and description (12, dim), hover fill.
pub(super) fn action_row(ui: &mut egui::Ui, p: &Palette, lang: Lang, title: Key, desc: Key) -> bool {
    let (title, desc) = (tr(lang, title), tr(lang, desc));
    let width = ui.available_width();
    let t = ui.painter().layout(title.into(), FontId::proportional(14.), p.text, width);
    let d = ui.painter().layout(desc.into(), FontId::proportional(12.), p.text_dim, width);
    let (rect, resp) = ui.allocate_exact_size(vec2(width, t.size().y + d.size().y + 10.), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, title));
    if resp.hovered() {
        ui.painter().rect_filled(rect.expand2(vec2(4., 0.)), 3., p.field);
    }
    let top = rect.min + vec2(0., 4.);
    ui.painter().galley(top + vec2(0., t.size().y + 2.), d, p.text_dim);
    ui.painter().galley(top, t, p.text);
    resp.on_hover_cursor(CursorIcon::PointingHand).clicked()
}

/// A key row: title, description, then the field (labelled with Android's dialog title) and «Сохранить».
fn key_row(ui: &mut egui::Ui, p: &Palette, lang: Lang, [title, desc, label, hint]: [Key; 4], text: &mut String) -> bool {
    row_title(ui, p, lang, title, Some(desc));
    let label = field_label(ui, p, tr(lang, label));
    let mut save = false;
    ui.horizontal(|ui| {
        let field = TextEdit::singleline(text).hint_text(tr(lang, hint)).desired_width(ui.available_width() - 110.);
        let field = ui.add(field).labelled_by(label.id);
        save = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        save |= ui.add(primary_button(p, tr(lang, Key::Save))).clicked();
    });
    save
}

fn running(app: &MimirDesktop, job: Job) -> bool {
    app.state.settings_ui.job == Some((job, None))
}

/// Under a maintenance or backup row: Android's progress text with a spinner while its job runs, then its result
/// (in place of Android's progress and result dialogs; the status line is hidden under the modal).
pub(super) fn job_line(app: &MimirDesktop, ui: &mut egui::Ui, p: &Palette, job: Job) {
    let Some((j, result)) = &app.state.settings_ui.job else { return };
    if *j != job {
        return;
    }
    match result {
        Some(text) => {
            let color = if app.state.settings_ui.job_failed { p.danger } else { p.accent };
            ui.label(RichText::new(text).size(12.).color(color));
        }
        None => {
            ui.horizontal(|ui| {
                ui.spinner();
                let key = match job {
                    Job::Fix => Key::FixingMessages,
                    Job::Orphans => Key::ScanningMedia,
                    Job::Export => Key::BackupExporting,
                    Job::Import => Key::BackupImporting,
                };
                ui.label(RichText::new(tr(app.lang, key)).size(12.).color(p.text_dim));
            });
        }
    }
}

fn peers(app: &mut MimirDesktop, ui: &mut egui::Ui, p: &Palette) {
    let lang = app.lang;
    row_title(ui, p, lang, Key::ConfigurePeers, Some(Key::ConfigurePeersDesc));
    let (mut changed, mut remove) = (false, None);
    for (i, peer) in app.settings.peers.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            changed |= ui.checkbox(&mut peer.enabled, middle_ellipsis(&peer.url, 28, 12)).on_hover_text(&peer.url).changed();
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if close_button(ui, p, &format!("{} {}", tr(lang, Key::Delete), peer.url)).clicked() {
                    remove = Some(i);
                }
            });
        });
    }
    ui.add_space(4.);
    let s = &mut app.state.settings_ui;
    let label = field_label(ui, p, tr(lang, Key::PeerUrl));
    let mut add = false;
    ui.horizontal(|ui| {
        let field = TextEdit::singleline(&mut s.peer_input).hint_text(tr(lang, Key::PeerUrlHint)).desired_width(ui.available_width() - 110.);
        let field = ui.add(field).labelled_by(label.id);
        add = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        add |= ui.add(primary_button(p, tr(lang, Key::AddButton))).clicked();
    });
    if let Some(key) = s.peer_error {
        // A bad URL gets the format hint, in red: it says what is expected.
        ui.label(RichText::new(tr(lang, key)).size(12.).color(p.danger));
    }
    if add {
        let url = s.peer_input.trim().to_string();
        s.peer_error = if !valid_peer(&url) {
            Some(Key::PeerUrlHint)
        } else if app.settings.peers.iter().any(|p| p.url == url) {
            Some(Key::PeerExists)
        } else {
            None
        };
        if s.peer_error.is_none() {
            s.peer_input.clear();
            app.settings.peers.push(Peer { url, enabled: true });
            changed = true;
        }
    }
    if let Some(i) = remove {
        app.settings.peers.remove(i);
        changed = true;
    }
    if changed {
        commit(app);
    }
}

pub fn advanced(app: &mut MimirDesktop, ui: &mut egui::Ui) {
    let (lang, p) = (app.lang, palette(ui.visuals().dark_mode));
    peers(app, ui, &p);
    ui.separator();
    if action_row(ui, &p, lang, Key::Mediators, Key::MediatorsDesc) {
        mediators::open(app);
    }
    ui.separator();
    if action_row(ui, &p, lang, Key::FixCorrupted, Key::FixCorruptedDesc) && !running(app, Job::Fix) {
        app.state.settings_ui.job = Some((Job::Fix, None));
        app.worker.submit(Box::new(|core| {
            let s = core.fix_corrupted_group_messages();
            WorkResult::Fixed { fixed: s.fixed, scanned: s.scanned }
        }));
    }
    job_line(app, ui, &p, Job::Fix);
    ui.separator();
    if action_row(ui, &p, lang, Key::DeleteOrphanedMedia, Key::DeleteOrphanedMediaDesc) && !running(app, Job::Orphans) {
        app.state.settings_ui.confirm = Some(Confirm::Orphans);
    }
    job_line(app, ui, &p, Job::Orphans);
    ui.separator();
    let keys = [Key::FileServer, Key::FileServerDesc, Key::FileServerDialogTitle, Key::FileServerHint];
    if key_row(ui, &p, lang, keys, &mut app.state.settings_ui.file_server) {
        // Kept only when it is 64 hex digits (Android checks the length only); anything else is silently ignored.
        let key = app.state.settings_ui.file_server.trim().to_string();
        if is_hex_key(&key) {
            app.settings.media.file_server = key;
            save(app);
        }
        app.state.settings_ui.file_server = app.settings.media.file_server.clone();
    }
    ui.separator();
    let keys = [Key::CallMediator, Key::CallMediatorDesc, Key::CallMediatorDialogTitle, Key::CallMediatorHint];
    if key_row(ui, &p, lang, keys, &mut app.state.settings_ui.call_mediator) {
        // Android: empty removes it; else lowercased and kept only when it is 64 hex digits.
        let key = app.state.settings_ui.call_mediator.trim().to_lowercase();
        if key.is_empty() || is_hex_key(&key) {
            app.settings.call_mediator = key;
            save(app);
        }
        app.state.settings_ui.call_mediator = app.settings.call_mediator.clone();
    }
    ui.separator();
    if action_row(ui, &p, lang, Key::CollectLogs, Key::CollectLogsDesc) {
        crate::media::save_as(&app.worker, lang, logs::path(&app.paths.root), "mimir.log".into());
    }
    ui.separator();
    if action_row(ui, &p, lang, Key::ExitMimir, Key::ExitMimirDesc) {
        app.state.settings_ui.confirm = Some(Confirm::Exit);
    }
}

/// The confirmation over the settings modal: deleting orphans (ours: title and text are the row's), exiting or
/// importing a backup (Android's). Exit quits for real, past the tray.
pub fn confirm(app: &mut MimirDesktop, ctx: &egui::Context, which: Confirm) {
    let lang = app.lang;
    let (title, text, ok) = match which {
        Confirm::Orphans => (Key::DeleteOrphanedMedia, Key::DeleteOrphanedMediaDesc, Key::Delete),
        Confirm::Exit => (Key::ExitConfirmTitle, Key::ExitConfirmMessage, Key::Ok),
        Confirm::Import => (Key::BackupImportWarningTitle, Key::BackupImportWarningMessage, Key::BackupImportContinue),
    };
    let (mut go, mut cancel) = (false, false);
    let r = dialog(ctx, Id::unique("settings_confirm")).show(ctx, |ui| {
        ui.set_width(380.);
        let p = palette(ui.visuals().dark_mode);
        dialog_title(ui, tr(lang, title));
        ui.label(tr(lang, text));
        ui.add_space(8.);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let button = if which == Confirm::Orphans { danger_button(&p, tr(lang, ok)) } else { primary_button(&p, tr(lang, ok)) };
            go = ui.add(button).clicked();
            cancel = ui.button(tr(lang, Key::Cancel)).clicked();
        });
    });
    if go {
        match which {
            Confirm::Orphans => {
                let dir = app.paths.files_dir.clone();
                app.state.settings_ui.job = Some((Job::Orphans, None));
                app.worker.submit(Box::new(move |core| delete_orphans(core, &dir)));
            }
            Confirm::Exit => app.quit(ctx),
            Confirm::Import => {
                if let Some(path) = app.state.settings_ui.import_path.take() {
                    super::stage_import(app, path);
                }
            }
        }
    }
    if go || cancel || r.should_close() {
        app.state.settings_ui.confirm = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{core::{events::Event, net::config_for}, settings::Settings, test_support::{test_app, test_harness}, ui::settings::{open, Section}};
    use eframe::egui::ViewportCommand;
    use egui_kittest::{kittest::Queryable as _, Harness};

    type H = Harness<'static, MimirDesktop>;

    fn peer(url: &str) -> Peer {
        Peer { url: url.into(), enabled: true }
    }

    /// The settings modal on Advanced, with two short peers.
    fn opened() -> H {
        let mut h = test_harness(Settings { intro_done: true, peers: vec![peer("tcp://a:1"), peer("tcp://b:2")], ..Default::default() });
        h.set_size(vec2(1100., 720.));
        open(h.state_mut());
        h.state_mut().state.settings_ui.section = Section::Advanced;
        h.run();
        h
    }

    fn click(h: &mut H, label: &str) {
        h.get_by_label(label).scroll_to_me();
        h.run();
        h.get_by_label(label).click();
        h.run();
    }

    fn saved(h: &H) -> Settings {
        Settings::load(&h.state().paths.settings_file)
    }

    fn status(h: &H) -> Option<String> {
        h.state().state.status.as_ref().map(|s| s.0.clone())
    }

    #[test]
    fn peer_url_validation() {
        for ok in ["tcp://1.2.3.4:1234", "tls://example.com:443", "quic://[::1]:9", "wss://h:1/path", "tcp://de1.mimir.im:7743?key=1bb8"] {
            assert!(valid_peer(ok), "{ok}");
        }
        for bad in ["example.com", "http://h:1", "tcp://h", "tcp://:1", "tcp://h:port", "tcp://h:70000", ""] {
            assert!(!valid_peer(bad), "{bad}");
        }
        assert!(Settings::default().peers.iter().all(|p| valid_peer(&p.url)), "the defaults pass");
    }

    #[test]
    fn peer_toggle_reaches_config() {
        let mut h = opened();
        let before = h.state().state.live_applied;
        click(&mut h, "tcp://a:1");
        assert!(!h.state().settings.peers[0].enabled);
        let cfg = config_for(&h.state().paths, &h.state().settings);
        assert_eq!(cfg.ygg_peers, vec!["tcp://b:2".to_string()]);
        assert_eq!(saved(&h).peers, h.state().settings.peers);
        assert_eq!(h.state().state.live_applied, before + 1, "pushed to the core");
        click(&mut h, "Удалить tcp://b:2");
        assert_eq!(h.state().settings.peers, vec![Peer { url: "tcp://a:1".into(), enabled: false }]);
        assert_eq!(saved(&h).peers.len(), 1);
    }

    #[test]
    fn bad_peer_rejected() {
        let mut h = opened();
        h.state_mut().state.settings_ui.peer_input = "example.com".into();
        click(&mut h, "Добавить");
        assert_eq!(h.state().settings.peers.len(), 2, "not added");
        assert_eq!(h.state().state.settings_ui.peer_input, "example.com", "kept for fixing");
        assert!(h.query_by_label("tcp://1.2.3.4:1234 или tls://example.com:443").is_some(), "the format hint shows");
        h.state_mut().state.settings_ui.peer_input = " tls://example.com:443 ".into();
        click(&mut h, "Добавить");
        assert_eq!(h.state().settings.peers[2], peer("tls://example.com:443"));
        assert_eq!(saved(&h).peers.len(), 3);
        assert!(h.state().state.settings_ui.peer_input.is_empty());
        assert!(h.query_by_label("tcp://1.2.3.4:1234 или tls://example.com:443").is_none());
    }

    #[test]
    fn duplicate_peer_says_so() {
        let mut h = opened();
        h.state_mut().state.settings_ui.peer_input = "tcp://a:1".into();
        click(&mut h, "Добавить");
        assert_eq!(h.state().settings.peers.len(), 2, "not added twice");
        assert_eq!(h.state().state.settings_ui.peer_input, "tcp://a:1", "kept, not silently cleared");
        assert!(h.query_by_label("Этот узел уже есть в списке").is_some());
        h.state_mut().state.settings_ui.peer_input = "tcp://c:3".into();
        click(&mut h, "Добавить");
        assert_eq!(h.state().settings.peers.len(), 3);
        assert!(h.query_by_label("Этот узел уже есть в списке").is_none(), "cleared by the next good add");
    }

    fn save_nth(h: &mut H, n: usize) {
        h.get_all_by_label("Сохранить").nth(n).unwrap().scroll_to_me();
        h.run();
        h.get_all_by_label("Сохранить").nth(n).unwrap().click();
        h.run();
    }

    #[test]
    fn file_server_requires_64_chars() {
        let mut h = opened();
        let default = Settings::default().media.file_server;
        assert_eq!(h.state().state.settings_ui.file_server, default, "the field shows the current key");
        h.state_mut().state.settings_ui.file_server = "a".repeat(63);
        save_nth(&mut h, 0);
        assert_eq!(h.state().settings.media.file_server, default, "63 chars ignored");
        assert_eq!(h.state().state.settings_ui.file_server, default);
        h.state_mut().state.settings_ui.file_server = "z".repeat(64);
        save_nth(&mut h, 0);
        assert_eq!(h.state().settings.media.file_server, default, "64 chars but not hex: ignored");
        assert_eq!(h.state().state.settings_ui.file_server, default);
        h.state_mut().state.settings_ui.file_server = format!(" {} ", "b".repeat(64));
        save_nth(&mut h, 0);
        assert_eq!(h.state().settings.media.file_server, "b".repeat(64));
        assert_eq!(saved(&h).media.file_server, "b".repeat(64));
    }

    #[test]
    fn call_mediator_lowercased_and_validated() {
        let mut h = opened();
        h.state_mut().state.settings_ui.call_mediator = "AB".repeat(32);
        save_nth(&mut h, 1);
        assert_eq!(h.state().settings.call_mediator, "ab".repeat(32));
        assert_eq!(saved(&h).call_mediator, "ab".repeat(32));
        h.state_mut().state.settings_ui.call_mediator = "zz".repeat(32);
        save_nth(&mut h, 1);
        assert_eq!(h.state().settings.call_mediator, "ab".repeat(32), "not hex: ignored");
        h.state_mut().state.settings_ui.call_mediator = "ab".into();
        save_nth(&mut h, 1);
        assert_eq!(h.state().settings.call_mediator, "ab".repeat(32), "short: ignored");
        h.state_mut().state.settings_ui.call_mediator = "  ".into();
        save_nth(&mut h, 1);
        assert_eq!((h.state().settings.call_mediator.as_str(), saved(&h).call_mediator.as_str()), ("", ""), "empty clears");
    }

    #[test]
    fn orphans_found_and_staged_skipped() {
        let (tmp, core) = test_app();
        let files = tmp.path().join("files");
        let c = core.add_contact(vec![7; 32], "Аня".into()).unwrap();
        let meta = r#"{"name":"a","originalName":"a.jpg","size":1,"hash":"9f","mimeType":"image/jpeg"}"#;
        core.add_local_message(c, 1, 0, true, true, 1000, 0, 1, meta.as_bytes().to_vec()).unwrap();
        fs::create_dir_all(files.join(".thumbs")).unwrap();
        for (name, body) in [("a", "1"), ("b", "22"), ("mimir_staged_x", "3"), (".thumbs/t", "4"), (".thumbs/b.jpg", "5")] {
            fs::write(files.join(name), body).unwrap();
        }
        assert_eq!(orphans(&core, &files), vec![files.join("b")]);
        assert!(matches!(delete_orphans(&core, &files), WorkResult::OrphansDeleted { count: 1, bytes: 2 }));
        assert!(!files.join("b").exists() && !files.join(".thumbs/b.jpg").exists(), "the orphan and its poster");
        assert!(files.join("a").exists() && files.join("mimir_staged_x").exists() && files.join(".thumbs/t").exists());
        assert!(matches!(delete_orphans(&core, &files), WorkResult::OrphansDeleted { count: 0, bytes: 0 }));
    }

    #[test]
    fn video_messages_are_not_orphans() {
        let (tmp, core) = test_app();
        let files = tmp.path().join("files");
        let c = core.add_contact(vec![7; 32], "Аня".into()).unwrap();
        let v = r#"{"name":"v.mp4","originalName":"clip.mp4","size":2,"hash":"9f","mimeType":"video/mp4"}"#;
        core.add_local_message(c, 1, 0, true, true, 1000, 0, 4, v.as_bytes().to_vec()).unwrap();
        let chat = 7;
        core.save_group_chat(chat, "Клуб".into(), String::new(), None, vec![9; 32], vec![], vec![3; 32], 0).unwrap();
        let w = v.replace("v.mp4", "w.mp4");
        core.add_group_message(chat, 1, 2, vec![8; 32], 1000, 4, false, w.into_bytes(), 0, false).unwrap();
        fs::create_dir_all(&files).unwrap();
        for n in ["v.mp4", "w.mp4", "stray"] {
            fs::write(files.join(n), "x").unwrap();
        }
        assert_eq!(orphans(&core, &files), vec![files.join("stray")]);
    }

    #[test]
    fn orphans_delete_asks_first() {
        let mut h = opened();
        let files = h.state().paths.files_dir.clone();
        fs::create_dir_all(&files).unwrap();
        fs::write(files.join("stray"), "x").unwrap();
        click(&mut h, "Удалить потерянные медиафайлы");
        assert_eq!(h.state().state.settings_ui.confirm, Some(Confirm::Orphans));
        assert!(files.join("stray").exists(), "nothing deleted before the answer");
        // Not click(): the job's spinner keeps repainting, so run() would never settle.
        h.get_by_label("Удалить").click();
        crate::test_support::run_until(&mut h, "the delete job", |h| status(h).is_some());
        assert!(!files.join("stray").exists());
        assert_eq!(status(&h).as_deref(), Some("Удалено 1 потерянных файлов (освобождено 1 Б)"));
        assert_eq!(h.state().state.settings_ui.confirm, None);
    }

    /// `text` is shown in the modal, under the row titled `row` (the status line, under the backdrop, may show it too).
    fn shown_under(h: &H, row: &str, text: &str) -> bool {
        let r = h.get_by_label(row).rect();
        h.get_all_by_label(text).any(|n| n.rect().top() >= r.bottom() && n.rect().top() < r.bottom() + 40. && n.rect().left() >= r.left() - 1.)
    }

    #[test]
    fn fix_reports_counts() {
        let mut h = opened();
        let (tx, rx) = std::sync::mpsc::channel();
        h.state_mut().rx = rx;
        tx.send(Event::WorkDone(WorkResult::Fixed { fixed: 2, scanned: 10 })).unwrap();
        h.run();
        let fixed = "Исправлено 2 сообщений из 10 просканированных";
        assert!(shown_under(&h, "Исправить поломанные сообщения", fixed), "inside the modal, under its row");
        assert_eq!(status(&h).as_deref(), Some(fixed));
        tx.send(Event::WorkDone(WorkResult::Fixed { fixed: 0, scanned: 10 })).unwrap();
        h.run();
        assert!(shown_under(&h, "Исправить поломанные сообщения", "Не найдено поломанных сообщений"));
        tx.send(Event::WorkDone(WorkResult::OrphansDeleted { count: 3, bytes: 2048 })).unwrap();
        h.run();
        let deleted = "Удалено 3 потерянных файлов (освобождено 2.0 КБ)";
        assert!(shown_under(&h, "Удалить потерянные медиафайлы", deleted));
        assert!(h.query_by_label("Не найдено поломанных сообщений").is_none(), "one result at a time");
        tx.send(Event::WorkDone(WorkResult::OrphansDeleted { count: 0, bytes: 0 })).unwrap();
        h.run();
        assert!(shown_under(&h, "Удалить потерянные медиафайлы", "Потерянные медиафайлы не найдены"));
    }

    #[test]
    fn job_progress_shown() {
        let mut h = opened();
        // step(), not run(): the spinner repaints for as long as the job runs.
        h.state_mut().state.settings_ui.job = Some((Job::Fix, None));
        h.step();
        h.step();
        assert!(shown_under(&h, "Исправить поломанные сообщения", "Исправление сообщений…"));
        h.state_mut().state.settings_ui.job = Some((Job::Orphans, None));
        h.step();
        h.step();
        assert!(shown_under(&h, "Удалить потерянные медиафайлы", "Сканирование медиафайлов…"));
        assert!(h.query_by_label("Исправление сообщений…").is_none());
    }

    #[test]
    fn fix_runs_on_worker() {
        let mut h = opened();
        h.get_by_label("Исправить поломанные сообщения").click();
        crate::test_support::run_until(&mut h, "the fix job", |h| status(h).is_some());
        assert!(shown_under(&h, "Исправить поломанные сообщения", "Не найдено поломанных сообщений"));
        assert_eq!(status(&h).as_deref(), Some("Не найдено поломанных сообщений"), "nothing to fix in an empty database");
    }

    #[test]
    fn mediators_row_opens_window() {
        let mut h = opened();
        click(&mut h, "Медиаторы");
        assert!(h.state().state.show_mediators);
        assert!(h.state().state.settings_ui.open, "settings stay under it");
    }

    fn closed(h: &H) -> bool {
        h.output().viewport_output.values().any(|v| v.commands.contains(&ViewportCommand::Close))
    }

    #[test]
    fn exit_asks_then_closes() {
        let mut h = opened();
        click(&mut h, "Закрыть Мимир");
        assert!(h.query_by_label("Закрыть Мимир?").is_some());
        assert!(!closed(&h));
        h.get_by_label("Отмена").click();
        h.run();
        assert!(h.query_by_label("Закрыть Мимир?").is_none() && !closed(&h), "cancel keeps the app");
        click(&mut h, "Закрыть Мимир");
        h.get_by_label("OK").click();
        h.step();
        assert!(closed(&h), "OK closes the window");
    }
}
