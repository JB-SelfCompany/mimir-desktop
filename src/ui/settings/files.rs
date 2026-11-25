//! The Files section: what lies in `files/`, newest first, each row joined with its message's names and chat.
//! Click opens the file (only safe file types open), the ✕ deletes it after asking. The message stays: its
//! bubble just shows the attachment as not present. The listing runs on the worker when the section opens.
use crate::{
    app::MimirDesktop,
    core::worker::WorkResult,
    i18n::{tr, trf, Key, Lang},
    media,
    model::chat_list::display_name,
    ui::{
        attachment_bubble::format_size,
        theme::{close_button, danger_button, dialog, dialog_title, palette, Palette},
    },
};
use eframe::egui::{self, text::LayoutJob, vec2, Align, CursorIcon, ScrollArea, FontId, Id, Layout, Rect, RichText, Role, Sense, UiBuilder, WidgetInfo};
use mimir::MimirApp;
use crate::model::{attachment::AttachmentMeta, chat_list::SAVED_ID};
use std::{collections::HashMap, fs, path::Path, time::SystemTime};

/// One stored file: size and date come from the disk, names and chat from the message that holds it.
#[derive(Clone, Debug, PartialEq)]
pub struct FileRow {
    pub storage_name: String,
    pub original_name: String,
    pub size: u64,
    pub modified: SystemTime,
    pub chat: String,
}

/// Every attachment a message names: storage name -> (original name, chat). `get_all_file_items` lacks video
/// messages (type 4), so those are read from the chats themselves.
pub fn referenced(core: &MimirApp) -> HashMap<String, (String, String)> {
    let contact = |id| core.get_contact(id).map(|c| display_name(&c.name, &c.pubkey)).unwrap_or_default();
    let mut names: HashMap<String, (String, String)> = core
        .get_all_file_items()
        .into_iter()
        .map(|i| {
            let chat = if i.chat_id > 0 { core.get_group_chat(i.chat_id).map(|g| g.name).unwrap_or_default() } else { contact(i.contact_id) };
            (i.storage_name, (i.original_name, chat))
        })
        .collect();
    let mut add = |data: Option<Vec<u8>>, chat: &dyn Fn() -> String| {
        if let Some(m) = data.and_then(|d| AttachmentMeta::parse(&d)).filter(|m| !m.name.is_empty()) {
            names.insert(m.name.clone(), (m.original_name.clone(), chat()));
        }
    };
    let contacts = core.get_contacts().into_iter().map(|c| c.id).chain([SAVED_ID]);
    for id in contacts {
        for m in core.get_messages(id, 0, i32::MAX).into_iter().filter(|m| m.msg_type == 4) {
            add(m.data, &|| contact(id));
        }
    }
    for g in core.get_group_chats() {
        for m in core.get_group_messages(g.chat_id, 0, i32::MAX).into_iter().filter(|m| m.msg_type == 4) {
            add(m.data, &|| g.name.clone());
        }
    }
    names
}

/// The regular files directly in `files_dir`, newest first. A file no message names (an orphan) keeps its disk name
/// and has no chat. `.thumbs/` (a directory) and staged sends (`mimir_staged_*`) are not listed.
pub fn list_files(core: &MimirApp, files_dir: &Path) -> Vec<FileRow> {
    let items = referenced(core);
    let Ok(dir) = fs::read_dir(files_dir) else { return vec![] };
    let mut rows: Vec<FileRow> = dir
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| {
            let storage_name = e.file_name().to_string_lossy().into_owned();
            if storage_name.starts_with("mimir_staged_") {
                return None;
            }
            let meta = e.metadata().ok()?;
            let item = items.get(&storage_name);
            let chat = item.map_or_else(String::new, |i| i.1.clone());
            let original_name = item.map(|i| i.0.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| storage_name.clone());
            Some(FileRow { storage_name, original_name, size: meta.len(), modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), chat })
        })
        .collect();
    rows.sort_by(|a, b| b.modified.cmp(&a.modified));
    rows
}

/// The listing arrived (a section opened again before it did just gets the newer one).
pub fn listed(app: &mut MimirDesktop, rows: Vec<FileRow>) {
    app.state.settings_ui.files = Some(rows);
    app.state.settings_ui.files_loading = false;
}

/// «dd.mm.yyyy hh:mm» in local time (UTC when the offset is unknown).
fn date(t: SystemTime) -> String {
    let utc = time::OffsetDateTime::from(t);
    let d = utc.to_offset(time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC));
    format!("{:02}.{:02}.{} {:02}:{:02}", d.day(), d.month() as u8, d.year(), d.hour(), d.minute())
}

/// Fixed, so the list can be virtualised (a files dir may hold thousands).
const ROW_H: f32 = 44.;

/// A file row: name (14) over «size · date · chat» (12, dim), the ✕ at the right. Returns (open, delete).
fn row(ui: &mut egui::Ui, p: &Palette, lang: Lang, r: &FileRow) -> (bool, bool) {
    let width = ui.available_width();
    let mut info = format!("{} · {}", format_size(r.size as i64, lang), date(r.modified));
    if !r.chat.is_empty() {
        info = format!("{info} · {}", r.chat);
    }
    let one_line = |text: String, size: f32, color| {
        let mut job = LayoutJob::simple(text, FontId::proportional(size), color, width - 36.);
        job.wrap.max_rows = 1;
        job.wrap.break_anywhere = true;
        ui.painter().layout_job(job)
    };
    let (t, d) = (one_line(r.original_name.clone(), 14., p.text), one_line(info, 12., p.text_dim));
    let (rect, resp) = ui.allocate_exact_size(vec2(width, ROW_H), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, &r.original_name));
    if resp.hovered() {
        ui.painter().rect_filled(rect.expand2(vec2(4., 0.)), 3., p.field);
    }
    let top = rect.min + vec2(0., 5.);
    ui.painter().galley(top + vec2(0., t.size().y + 2.), d, p.text_dim);
    ui.painter().galley(top, t, p.text);
    let x = Rect::from_center_size(rect.right_center() - vec2(12., 0.), vec2(22., 22.));
    let label = format!("{} {}", tr(lang, Key::Delete), r.original_name);
    let delete = ui.scope_builder(UiBuilder::new().max_rect(x), |ui| close_button(ui, p, &label).clicked()).inner;
    (resp.on_hover_cursor(CursorIcon::PointingHand).clicked() && !delete, delete)
}

pub fn files(app: &mut MimirDesktop, ui: &mut egui::Ui) {
    let (lang, p) = (app.lang, palette(ui.visuals().dark_mode));
    let s = &mut app.state.settings_ui;
    if s.files.is_none() {
        if !s.files_loading {
            s.files_loading = true;
            let dir = app.paths.files_dir.clone();
            app.worker.submit(Box::new(move |core| WorkResult::Files(list_files(core, &dir))));
        }
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(RichText::new(tr(lang, Key::ScanningMedia)).size(12.).color(p.text_dim));
        });
        return;
    }
    let rows = s.files.as_deref().unwrap_or_default();
    if rows.is_empty() {
        ui.label(RichText::new(tr(lang, Key::NoFiles)).color(p.text_dim));
        return;
    }
    let total: u64 = rows.iter().map(|r| r.size).sum();
    let summary = trf(lang, Key::FilesSummary, &[&rows.len().to_string(), &format_size(total as i64, lang)]);
    ui.label(RichText::new(summary).size(12.).color(p.text_dim));
    if let Some(e) = &s.file_error {
        ui.label(RichText::new(e).size(12.).color(p.danger));
    }
    ui.add_space(4.);
    // The modal's own pane scrolls too, so the list gets a height of its own (what is left of the 560 px modal).
    let h = (ui.ctx().content_rect().height() - 80.).min(560.) - 48. - 90.;
    let (mut open, mut delete) = (None, None);
    ScrollArea::vertical().max_height(h.max(120.)).auto_shrink([false, true]).show_rows(ui, ROW_H, rows.len(), |ui, range| {
        for r in &rows[range] {
            let (o, d) = row(ui, &p, lang, r);
            if o {
                open = Some(r.storage_name.clone());
            }
            if d {
                delete = Some(r.clone());
            }
        }
    });
    if let Some(name) = open {
        media::open_file(&app.worker, lang, &app.paths.files_dir.join(name));
    }
    if delete.is_some() {
        app.state.settings_ui.file_delete = delete;
    }
}

/// «Удалить файл»: asks, then the worker removes the file and its cached poster; `deleted` drops its row.
pub fn confirm(app: &mut MimirDesktop, ctx: &egui::Context) {
    let Some(r) = app.state.settings_ui.file_delete.clone() else { return };
    let lang = app.lang;
    let (mut go, mut cancel) = (false, false);
    let res = dialog(ctx, Id::unique("settings_file_delete")).show(ctx, |ui| {
        ui.set_width(380.);
        let p = palette(ui.visuals().dark_mode);
        dialog_title(ui, tr(lang, Key::DeleteFileTitle));
        ui.label(trf(lang, Key::DeleteFileConfirm, &[&r.original_name]));
        ui.add_space(8.);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            go = ui.add(danger_button(&p, tr(lang, Key::Delete))).clicked();
            cancel = ui.button(tr(lang, Key::Cancel)).clicked();
        });
    });
    if go {
        let path = app.paths.files_dir.join(&r.storage_name);
        let poster = app.paths.files_dir.join(".thumbs").join(format!("{}.jpg", r.storage_name));
        let storage_name = r.storage_name.clone();
        app.worker.submit(Box::new(move |_| {
            // Already gone is fine; anything else (in use, no permission) keeps the row and says why.
            let err = [&path, &poster].into_iter().find_map(|p| fs::remove_file(p).err().filter(|e| e.kind() != std::io::ErrorKind::NotFound));
            WorkResult::FileDeleted { storage_name, err: err.map(|e| e.to_string()) }
        }));
    }
    if go || cancel || res.should_close() {
        app.state.settings_ui.file_delete = None;
    }
}

/// The worker deleted `storage_name` (its row goes), or says why it could not (the row stays).
pub fn deleted(app: &mut MimirDesktop, storage_name: &str, err: Option<&String>) {
    app.state.settings_ui.file_error = err.map(|e| trf(app.lang, Key::DeleteFileFailed, &[e]));
    if err.is_none() {
        app.state.thumbs.invalidate(&app.paths.files_dir.join(storage_name));
        if let Some(rows) = &mut app.state.settings_ui.files {
            rows.retain(|x| x.storage_name != storage_name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        settings::Settings,
        test_support::{run_until, test_app, test_harness},
        ui::settings::{open, Section},
    };
    use egui_kittest::kittest::Queryable as _;
    use std::time::Duration;

    const META: &str = r#"{"name":"h.jpg","originalName":"cat.jpg","size":3,"hash":"9f","mimeType":"image/jpeg"}"#;

    fn old(path: &Path, secs: u64) {
        let f = fs::File::options().write(true).open(path).unwrap();
        f.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs)).unwrap();
    }

    #[test]
    fn list_joins_db_names() {
        let (tmp, core) = test_app();
        let files = tmp.path().join("files");
        let c = core.add_contact(vec![7; 32], "Аня".into()).unwrap();
        core.add_local_message(c, 1, 0, true, true, 1000, 0, 1, META.as_bytes().to_vec()).unwrap();
        fs::create_dir_all(files.join(".thumbs")).unwrap();
        for (name, body) in [("h.jpg", "abc"), ("stray", "12"), ("mimir_staged_x", "3"), (".thumbs/h.jpg.jpg", "4")] {
            fs::write(files.join(name), body).unwrap();
        }
        old(&files.join("stray"), 100);
        old(&files.join("h.jpg"), 200);
        let rows = list_files(&core, &files);
        let got: Vec<_> = rows.iter().map(|r| (r.storage_name.as_str(), r.original_name.as_str(), r.size, r.chat.as_str())).collect();
        assert_eq!(got, [("h.jpg", "cat.jpg", 3, "Аня"), ("stray", "stray", 2, "")], "newest first; no .thumbs, no staged");
    }

    #[test]
    fn listing_runs_on_worker() {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        let files = h.state().paths.files_dir.clone();
        fs::create_dir_all(&files).unwrap();
        fs::write(files.join("stray"), "x").unwrap();
        // Hold the worker, so the listing is queued behind this job and the spinner stays up.
        let (release, gate) = std::sync::mpsc::channel::<()>();
        h.state().worker.submit(Box::new(move |_| {
            let _ = gate.recv();
            WorkResult::Done
        }));
        open(h.state_mut());
        h.state_mut().state.settings_ui.section = Section::Files;
        h.run_steps(3);
        assert!(h.state().state.settings_ui.files.is_none());
        assert!(h.query_by_label("Сканирование медиафайлов…").is_some(), "spinner while listing");
        release.send(()).unwrap();
        run_until(&mut h, "the listing", |h| h.state().state.settings_ui.files.is_some());
        h.run();
        assert!(h.query_by_label("Сканирование медиафайлов…").is_none());
        assert!(h.query_by_label("stray").is_some());
        assert!(h.query_by_label_contains("Файлов: 1 — 1 Б").is_some(), "the subtitle");
    }

    #[test]
    fn list_names_video_messages() {
        let (tmp, core) = test_app();
        let files = tmp.path().join("files");
        let c = core.add_contact(vec![7; 32], "Аня".into()).unwrap();
        let v = r#"{"name":"v.mp4","originalName":"clip.mp4","size":2,"hash":"9f","mimeType":"video/mp4"}"#;
        core.add_local_message(c, 1, 0, true, true, 1000, 0, 4, v.as_bytes().to_vec()).unwrap();
        let chat = 7;
        core.save_group_chat(chat, "Клуб".into(), String::new(), None, vec![9; 32], vec![], vec![3; 32], 0).unwrap();
        let w = v.replace("v.mp4", "w.mp4").replace("clip", "reel");
        core.add_group_message(chat, 1, 2, vec![8; 32], 1000, 4, false, w.into_bytes(), 0, false).unwrap();
        fs::create_dir_all(&files).unwrap();
        fs::write(files.join("v.mp4"), "ab").unwrap();
        fs::write(files.join("w.mp4"), "cd").unwrap();
        let mut got: Vec<_> = list_files(&core, &files).into_iter().map(|r| (r.original_name, r.chat)).collect();
        got.sort();
        assert_eq!(got, [("clip.mp4".to_string(), "Аня".to_string()), ("reel.mp4".to_string(), "Клуб".to_string())]);
    }

    #[test]
    fn delete_removes_file_keeps_message() {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        let (core, files) = (h.state().core.clone(), h.state().paths.files_dir.clone());
        let c = core.add_contact(vec![7; 32], "Аня".into()).unwrap();
        core.add_local_message(c, 1, 0, true, true, 1000, 0, 1, META.as_bytes().to_vec()).unwrap();
        fs::create_dir_all(files.join(".thumbs")).unwrap();
        fs::write(files.join("h.jpg"), "abc").unwrap();
        fs::write(files.join(".thumbs/h.jpg.jpg"), "p").unwrap();
        open(h.state_mut());
        h.state_mut().state.settings_ui.section = Section::Files;
        run_until(&mut h, "the listing", |h| h.state().state.settings_ui.files.is_some());
        h.run();
        h.get_by_label("cat.jpg").click();
        h.run();
        let opened = crate::media::take_opened();
        assert_eq!(opened.len(), 1, "a click opens the file");
        assert!(opened[0].1.iter().any(|a| a.to_string_lossy().ends_with("h.jpg")), "{opened:?}");
        h.get_by_label("Удалить cat.jpg").click();
        h.run();
        assert!(files.join("h.jpg").exists(), "nothing deleted before the answer");
        assert!(h.query_by_label("Удалить \"cat.jpg\"? Это нельзя отменить.").is_some());
        h.get_by_label("Удалить").click();
        run_until(&mut h, "the delete", |h| h.state().state.settings_ui.files.as_ref().is_some_and(|f| f.is_empty()));
        h.run();
        assert!(!files.join("h.jpg").exists() && !files.join(".thumbs/h.jpg.jpg").exists(), "the file and its poster");
        assert_eq!(core.get_all_file_items().len(), 1, "the message stays");
        assert!(h.query_by_label("Нет сохранённых файлов").is_some(), "the row is gone");
    }

    #[test]
    fn delete_failure_keeps_row_and_says_why() {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        let files = h.state().paths.files_dir.clone();
        fs::create_dir_all(&files).unwrap();
        fs::write(files.join("stray"), "x").unwrap();
        open(h.state_mut());
        h.state_mut().state.settings_ui.section = Section::Files;
        run_until(&mut h, "the listing", |h| h.state().state.settings_ui.files.is_some());
        h.run();
        // A directory in its place cannot be removed as a file.
        fs::remove_file(files.join("stray")).unwrap();
        fs::create_dir(files.join("stray")).unwrap();
        h.get_by_label("Удалить stray").click();
        h.run();
        h.get_by_label("Удалить").click();
        run_until(&mut h, "the delete", |h| h.state().state.settings_ui.file_error.is_some());
        h.run();
        assert_eq!(h.state().state.settings_ui.files.as_ref().unwrap().len(), 1, "the row stays");
        assert!(h.query_by_label_contains("Не удалось удалить файл:").is_some(), "the error shows");
        assert_eq!(h.state().state.settings_ui.file_delete, None);
    }
}
