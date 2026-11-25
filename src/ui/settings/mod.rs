//! Settings modal (Android SettingsActivity and its screens, AS §1): categories on the left, the selected one on
//! the right (P4-R5: sub-dialogs are inline fields). Every change is saved to settings.json at once, and the values
//! the core takes live are pushed to it (`apply_live`); Esc closes.
mod advanced;
mod files;
mod general;

pub use files::{listed as files_listed, FileRow};

use crate::{
    app::MimirDesktop,
    backup,
    core::worker::WorkResult,
    i18n::{tr, trf, Key, Lang},
    model::{chat_key::ChatKey, chat_list::SAVED_ID},
    paths::DataPaths,
    ui::theme::{close_button, dialog, palette, Palette},
};
use std::path::PathBuf;
use eframe::egui::{self, vec2, Align, Align2, Color32, FontId, Id, Layout, RichText, ScrollArea, Sense, Role, WidgetInfo};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Section {
    #[default]
    Receive,
    Images,
    Priority,
    Voice,
    Font,
    Appearance,
    Backup,
    Files,
    Advanced,
    About,
}

impl Section {
    pub const ALL: [Section; 10] = [
        Section::Receive, Section::Images, Section::Priority, Section::Voice, Section::Font,
        Section::Appearance, Section::Backup, Section::Files, Section::Advanced, Section::About,
    ];

    /// The Android settings row title (AS §1.1); «Внешний вид» is ours.
    pub fn title(self) -> Key {
        match self {
            Section::Receive => Key::ReceiveSettings,
            Section::Images => Key::ResizeBigPics,
            Section::Priority => Key::ClientPriority,
            Section::Voice => Key::VoiceQuality,
            Section::Font => Key::MessageFontSize,
            Section::Appearance => Key::Appearance,
            Section::Backup => Key::BackupAndRestore,
            Section::Files => Key::ManageFiles,
            Section::Advanced => Key::Advanced,
            Section::About => Key::About,
        }
    }

    /// The row description under the section title; Receive and Appearance have their own per-row texts.
    fn desc(self) -> Option<Key> {
        match self {
            Section::Receive | Section::Appearance => None,
            Section::Images => Some(Key::ResizeBigPicsDesc),
            Section::Priority => Some(Key::ClientPriorityDesc),
            Section::Voice => Some(Key::VoiceQualityDesc),
            Section::Font => Some(Key::MessageFontSizeDesc),
            Section::Backup => Some(Key::BackupAndRestoreDesc),
            Section::Files => Some(Key::ManageFilesDesc),
            Section::Advanced => Some(Key::AdvancedDesc),
            Section::About => None,
        }
    }
}

/// A confirmation shown over the modal.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Confirm {
    Orphans,
    Exit,
    /// Android's import warning for `SettingsUi::import_path`.
    Import,
}

/// A maintenance job of the Advanced section, or a backup export/import.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Job {
    Fix,
    Orphans,
    Export,
    Import,
}

#[derive(Default)]
pub struct SettingsUi {
    pub open: bool,
    pub section: Section,
    /// The client priority field (Android's number dialog), applied on «Сохранить».
    pub priority: String,
    /// The «Адрес узла:» field; `peer_invalid` after «Добавить» refused it.
    pub peer_input: String,
    pub peer_invalid: bool,
    /// The file server and call mediator key fields, applied on «Сохранить».
    pub file_server: String,
    pub call_mediator: String,
    pub confirm: Option<Confirm>,
    /// The last maintenance job: running (`None`) or its result text.
    pub job: Option<(Job, Option<String>)>,
    /// The result text of `job` is a failure (red).
    pub job_failed: bool,
    /// The backup picked for import, waiting for the warning's «Импорт».
    pub import_path: Option<PathBuf>,
    /// An import is staged and the app relaunches (P4-R1).
    pub restarting: bool,
    /// The Files section: the listing (`None` until it arrives; each opening of the section lists again), whether
    /// the worker is on it, and the file asked about deleting.
    pub files: Option<Vec<FileRow>>,
    pub files_loading: bool,
    pub file_delete: Option<FileRow>,
    /// Why the last delete failed.
    pub file_error: Option<String>,
}

/// A maintenance job ended: its result shows under its row (also in the status line, for a closed modal).
pub fn job_done(app: &mut MimirDesktop, job: Job, text: String) {
    app.state.settings_ui.job = Some((job, Some(text.clone())));
    app.state.settings_ui.job_failed = false;
    app.notice_text(text);
}

/// A job failed: the text shows in red under its row (and on the welcome screen for an import), and in the status line.
pub fn job_failed(app: &mut MimirDesktop, job: Job, text: String) {
    app.state.settings_ui.job = Some((job, Some(text.clone())));
    app.state.settings_ui.job_failed = true;
    app.error_text(text);
}

#[cfg(test)]
thread_local! {
    static PICKED: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Test seam: what the next backup file dialog "returns" on this thread (tests never open one).
#[cfg(test)]
pub fn set_backup_picked(path: PathBuf) {
    PICKED.with(|p| *p.borrow_mut() = Some(path));
}

/// The file dialog `real`, or in tests the path set by `set_backup_picked` (read here, on the UI thread).
fn file_dialog(real: impl FnOnce() -> Option<PathBuf> + Send + 'static) -> Box<dyn FnOnce() -> Option<PathBuf> + Send> {
    #[cfg(test)]
    {
        drop(real);
        let picked = PICKED.with(|p| p.borrow_mut().take());
        Box::new(move || picked)
    }
    #[cfg(not(test))]
    Box::new(real)
}

/// «Бэкап данных»: save dialog, then the export, both on a thread of their own (the dialog blocks, the zip is large).
fn export(app: &mut MimirDesktop) {
    app.state.settings_ui.job = Some((Job::Export, None));
    let name = backup::default_name();
    let pick = file_dialog(move || rfd::FileDialog::new().set_file_name(name).add_filter("ZIP", &["zip"]).save_file());
    let (core, root, settings, worker) = (app.core.clone(), app.paths.root.clone(), app.settings.clone(), app.worker.clone());
    std::thread::spawn(move || {
        let result = match pick() {
            None => WorkResult::BackupSaved { path: None, err: None },
            Some(dest) => {
                let err = backup::export(&core, &DataPaths::under(&root), &settings, &dest).err();
                WorkResult::BackupSaved { path: Some(dest), err }
            }
        };
        worker.submit(Box::new(move |_| result));
    });
}

pub fn backup_saved(app: &mut MimirDesktop, path: Option<&PathBuf>, err: Option<&String>) {
    match (path, err) {
        (None, _) => app.state.settings_ui.job = None,
        (Some(_), None) => job_done(app, Job::Export, tr(app.lang, Key::BackupExportSuccess).into()),
        (Some(_), Some(e)) => job_failed(app, Job::Export, trf(app.lang, Key::BackupExportFailed, &[e])),
    }
}

/// «Восстановление данных» (and the welcome screen's «Импортировать с телефона»): open dialog and validation off the UI
/// thread; `backup_picked` takes it from there.
pub fn pick_import(app: &mut MimirDesktop) {
    // Busy while the dialog is open: one backup dialog at a time (M3).
    app.state.settings_ui.job = Some((Job::Import, None));
    let pick = file_dialog(|| rfd::FileDialog::new().add_filter("ZIP", &["zip"]).pick_file());
    let worker = app.worker.clone();
    std::thread::spawn(move || {
        let path = pick();
        let valid = path.as_deref().is_some_and(|p| backup::validate(p).is_ok());
        worker.submit(Box::new(move |_| WorkResult::BackupPicked { path, valid }));
    });
}

/// A foreign or broken file is refused with Android's text. Android's warning asks first, unless the welcome screen
/// shows over a database with nothing in it but Saved Messages (M2: a lost settings.json also brings the welcome back).
pub fn backup_picked(app: &mut MimirDesktop, path: Option<PathBuf>, valid: bool) {
    let has_data = || app.state.chats.iter().any(|r| r.key != ChatKey::SAVED) || app.core.get_contacts().iter().any(|c| c.id != SAVED_ID);
    let Some(path) = path else {
        app.state.settings_ui.job = None; // dialog cancelled
        return;
    };
    if !valid {
        job_failed(app, Job::Import, tr(app.lang, Key::BackupInvalidFile).into());
    } else if app.settings.intro_done || has_data() {
        app.state.settings_ui.job = None;
        app.state.settings_ui.import_path = Some(path);
        app.state.settings_ui.confirm = Some(Confirm::Import);
    } else {
        stage_import(app, path);
    }
}

/// Copies the backup next to the data (it may be large: a thread of its own); `import_staged` relaunches.
pub(crate) fn stage_import(app: &mut MimirDesktop, path: PathBuf) {
    app.state.settings_ui.job = Some((Job::Import, None));
    let (root, worker) = (app.paths.root.clone(), app.worker.clone());
    std::thread::spawn(move || {
        let err = backup::stage(&DataPaths::under(&root), &path).err();
        worker.submit(Box::new(move |_| WorkResult::BackupStaged { err }));
    });
}

pub fn import_staged(app: &mut MimirDesktop, ctx: &egui::Context, err: Option<&String>) {
    match err {
        Some(e) => job_failed(app, Job::Import, trf(app.lang, Key::BackupImportFailed, &[e])),
        None => {
            app.state.settings_ui.restarting = true;
            relaunch(app, ctx);
        }
    }
}

/// P4-R1: starts a new instance (same exe, args and environment) and quits this one for real (not to the tray); the new
/// one waits for our instance lock (`platform::RELAUNCHED`) and our files (`backup::finish_pending` retries its renames).
/// ponytail: if the spawn fails the app still quits, and the import is applied at the next manual start.
fn relaunch(app: &mut MimirDesktop, ctx: &egui::Context) {
    #[cfg(not(test))]
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).args(std::env::args_os().skip(1)).env(crate::platform::RELAUNCHED, "1").spawn();
    }
    app.quit(ctx);
}

/// Android's BackupActivity rows; progress and result under each (the status line is hidden under the modal).
fn backup_section(app: &mut MimirDesktop, ui: &mut egui::Ui) {
    let (lang, p) = (app.lang, palette(ui.visuals().dark_mode));
    let busy = matches!(app.state.settings_ui.job, Some((Job::Export | Job::Import, None)));
    if advanced::action_row(ui, &p, lang, Key::BackupExport, Key::BackupExportDesc) && !busy {
        export(app);
    }
    advanced::job_line(app, ui, &p, Job::Export);
    ui.separator();
    if advanced::action_row(ui, &p, lang, Key::BackupImport, Key::BackupImportDesc) && !busy {
        pick_import(app);
    }
    advanced::job_line(app, ui, &p, Job::Import);
}

pub fn open(app: &mut MimirDesktop) {
    let s = &app.settings;
    let (priority, file_server, call_mediator) = (s.priority.to_string(), s.media.file_server.clone(), s.call_mediator.clone());
    app.state.settings_ui = SettingsUi { open: true, priority, file_server, call_mediator, ..Default::default() };
}

/// Saves settings.json (settings the core does not hold: images, voice, font, appearance).
pub(crate) fn save(app: &mut MimirDesktop) {
    // ponytail: a failed settings write is ignored (as elsewhere); the change still holds until exit.
    let _ = app.settings.save(&app.paths.settings_file);
}

/// `save`, then pushes the live values to the core (non-blocking setters, see `net::apply_live`).
pub(crate) fn commit(app: &mut MimirDesktop) {
    save(app);
    app.apply_live();
}

/// A setting row heading: title (14) and the Android description (12, dim) under it.
pub(crate) fn row_title(ui: &mut egui::Ui, p: &Palette, lang: Lang, title: Key, desc: Option<Key>) {
    ui.label(RichText::new(tr(lang, title)).size(14.).color(p.text));
    if let Some(d) = desc {
        ui.label(RichText::new(tr(lang, d)).size(12.).color(p.text_dim));
    }
    ui.add_space(4.);
}

/// The selected section's content.
fn body(app: &mut MimirDesktop, ui: &mut egui::Ui, section: Section) {
    match section {
        Section::Receive => general::receive(app, ui),
        Section::Images => general::images(app, ui),
        Section::Priority => general::priority(app, ui),
        Section::Voice => general::voice(app, ui),
        Section::Font => general::font(app, ui),
        Section::Appearance => general::appearance(app, ui),
        Section::About => general::about(app, ui),
        Section::Advanced => advanced::advanced(app, ui),
        Section::Files => files::files(app, ui),
        Section::Backup => backup_section(app, ui),
    }
}

/// A left-aligned category row (Android settings rows are left-aligned titles); accent fill when selected.
fn nav_row(ui: &mut egui::Ui, p: &Palette, text: &str, selected: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(NAV_W, 32.), Sense::click());
    resp.widget_info(|| WidgetInfo::selected(Role::Button, true, selected, text));
    let fill = if selected { p.accent } else if resp.hovered() { p.field } else { Color32::TRANSPARENT };
    ui.painter().rect_filled(rect, 3., fill);
    let color = if selected { Color32::BLACK } else { p.text };
    ui.painter().text(rect.left_center() + vec2(12., 0.), Align2::LEFT_CENTER, text, FontId::proportional(14.), color);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}

/// Wide enough for the longest title on one line («Качество голосовых сообщений», 14 px).
const NAV_W: f32 = 260.;

/// The welcome screen's import warning (no settings modal under it).
pub fn welcome_confirm(app: &mut MimirDesktop, ctx: &egui::Context) {
    if let Some(which) = app.state.settings_ui.confirm {
        advanced::confirm(app, ctx, which);
    }
}

pub fn show(app: &mut MimirDesktop, ctx: &egui::Context) {
    if !app.state.settings_ui.open {
        return;
    }
    let lang = app.lang;
    let mut close = false;
    let r = dialog(ctx, Id::unique("settings")).show(ctx, |ui| {
        let p = palette(ui.visuals().dark_mode);
        // The modal is at most 560 px tall (less on short windows, but never shorter than the category list); the
        // panes take what is left under the header and the content pane scrolls.
        let h = ((ctx.content_rect().height() - 80.).min(560.) - 48.).max(Section::ALL.len() as f32 * 34.);
        ui.set_width(800.);
        ui.horizontal(|ui| {
            ui.label(RichText::new(tr(lang, Key::Settings)).size(16.).family(egui::FontFamily::Name("bold".into())));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| close = close_button(ui, &p, tr(lang, Key::Close)).clicked());
        });
        ui.add_space(8.);
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(vec2(NAV_W, h), Layout::top_down(Align::Min), |ui| {
                ui.spacing_mut().item_spacing.y = 2.;
                for s in Section::ALL {
                    if nav_row(ui, &p, tr(lang, s.title()), app.state.settings_ui.section == s) {
                        app.state.settings_ui.section = s;
                        if s == Section::Files && !app.state.settings_ui.files_loading {
                            app.state.settings_ui.files = None;
                            app.state.settings_ui.file_error = None;
                        }
                    }
                }
            });
            // Not ui.separator(): in a row it spans the whole available height, which kept the modal at its old
            // size after the window shrank.
            let (line, _) = ui.allocate_exact_size(vec2(1., h), Sense::hover());
            ui.painter().vline(line.center().x, line.y_range(), ui.visuals().widgets.noninteractive.bg_stroke);
            ui.allocate_ui_with_layout(vec2(ui.available_width(), h), Layout::top_down(Align::Min), |ui| {
                ui.set_min_height(h);
                ScrollArea::vertical().min_scrolled_height(h).max_height(h).auto_shrink([false, false]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing = vec2(8., 6.);
                    let s = app.state.settings_ui.section;
                    ui.label(RichText::new(tr(lang, s.title())).size(16.).family(egui::FontFamily::Name("bold".into())).color(p.text));
                    if let Some(d) = s.desc() {
                        ui.label(RichText::new(tr(lang, d)).size(12.).color(p.text_dim));
                    }
                    ui.add_space(8.);
                    body(app, ui, s);
                });
            });
        });
    });
    if close || r.should_close() {
        app.state.settings_ui.open = false;
    }
    if let Some(which) = app.state.settings_ui.confirm.filter(|_| app.state.settings_ui.open) {
        advanced::confirm(app, ctx, which);
    }
    if app.state.settings_ui.open {
        files::confirm(app, ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::{open, Section};
    use crate::{app::MimirDesktop, settings::Settings, test_support::test_harness};
    use eframe::egui::{self, Role};
    use egui_kittest::{kittest::Queryable as _, Harness};

    type H = Harness<'static, MimirDesktop>;

    /// Main window with the settings modal opened from the ⋮ menu.
    fn opened() -> H {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        h.run();
        h.get_by_label("Главное меню").click();
        h.run();
        h.get_by_label("Настройки").click();
        h.run();
        h
    }

    fn go(h: &mut H, section: &str) {
        h.get_by_label(section).click();
        h.run();
    }

    fn saved(h: &H) -> Settings {
        Settings::load(&h.state().paths.settings_file)
    }

    #[test]
    fn menu_opens_settings() {
        let mut h = opened();
        assert!(h.state().state.settings_ui.open);
        assert_eq!(h.state().state.settings_ui.section, Section::Receive);
        assert!(h.query_by_label("Кто может отправлять вам запросы контакта").is_some());
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(!h.state().state.settings_ui.open, "Esc closes");
        assert!(h.query_by_label("Кто может отправлять вам запросы контакта").is_none());
        // The ✕ in the header closes it too.
        open(h.state_mut());
        h.run();
        h.get_by_label("Закрыть").click();
        h.run();
        assert!(!h.state().state.settings_ui.open, "✕ closes");
    }

    #[test]
    fn receive_rows_fit_and_scroll() {
        // F2: on a 720 px window the last row (groups) was cut off below the modal.
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        h.set_size(egui::vec2(1100., 720.));
        open(h.state_mut());
        h.run();
        h.run();
        let always = h.get_all_by_label("Всегда").nth(2).expect("groups «Всегда»").rect();
        let modal_bottom = h.get_by_label("Настройки").rect().top() + 560.;
        assert!(always.bottom() <= modal_bottom.min(720.), "{always:?} below the modal");
        // On a short window the modal stays inside it: the content pane scrolls, the category list still fits.
        h.set_size(egui::vec2(1100., 460.));
        h.run();
        h.run();
        let last_nav = h.get_by_label("О программе").rect();
        assert!(h.get_by_label("Настройки").rect().top() >= 0. && last_nav.bottom() <= 460., "{last_nav:?}");
        h.get_all_by_label("Всегда").nth(2).unwrap().scroll_to_me();
        h.run();
        let always = h.get_all_by_label("Всегда").nth(2).unwrap().rect();
        assert!(always.bottom() <= 460., "scrolled into view: {always:?}");
        h.get_all_by_label("Всегда").nth(2).unwrap().click();
        h.run();
        assert_eq!(h.state().settings.media.auto_groups, -1);
    }

    #[test]
    fn category_rows_are_single_left_aligned_lines() {
        let h = opened();
        let rows: Vec<_> = Section::ALL.iter().map(|s| h.get_all_by_label(crate::i18n::tr(crate::i18n::Lang::Ru, s.title())).next().unwrap().rect()).collect();
        let left = rows[0].left();
        assert!(rows.iter().all(|r| r.left() == left && r.height() == 32.), "{rows:?}");
    }

    #[test]
    fn receive_nobody_persists_and_applies() {
        let mut h = opened();
        let before = h.state().state.live_applied;
        h.get_by_label("Никто").click();
        h.run();
        assert!(h.state().settings.block_strangers);
        assert!(saved(&h).block_strangers, "written to settings.json");
        assert_eq!(h.state().state.live_applied, before + 1, "pushed to the core");
    }

    #[test]
    fn auto_cap_choice_updates_settings() {
        let mut h = opened();
        // Three rows of the same five options: contacts, others, groups.
        h.get_all_by_label("До 5 мегабайт").next().unwrap().click();
        h.run();
        assert_eq!(h.state().settings.media.auto_contacts, 5242880);
        h.get_all_by_label("Всегда").nth(1).unwrap().click();
        h.run();
        let m = &h.state().settings.media;
        assert_eq!((m.auto_others, m.auto_groups), (-1, 10 << 20));
        assert_eq!(saved(&h).media, h.state().settings.media);
    }

    #[test]
    fn priority_clamped() {
        let mut h = opened();
        go(&mut h, "Приоритет клиента");
        h.state_mut().state.settings_ui.priority.clear();
        h.run();
        h.get_by_role(Role::TextInput).focus();
        h.run();
        h.get_by_role(Role::TextInput).type_text("500");
        h.run();
        let before = h.state().state.live_applied;
        h.get_by_label("Сохранить").click();
        h.run();
        assert_eq!(h.state().settings.priority, 100);
        assert_eq!(h.state().state.settings_ui.priority, "100", "the field shows what was stored");
        assert_eq!(h.state().state.live_applied, before + 1);
        h.state_mut().state.settings_ui.priority = "abc".into();
        h.run();
        h.get_by_label("Сохранить").click();
        h.run();
        assert_eq!((h.state().settings.priority, saved(&h).priority), (1, 1), "invalid gives 1");
    }

    #[test]
    fn font_step_changes_message_font() {
        let mut h = opened();
        go(&mut h, "Размер шрифта сообщений");
        assert!(h.query_by_label_contains("СРЕДНИЙ").is_some());
        h.get_by_role(Role::Slider).set_value_accesskit(4.);
        h.run();
        assert_eq!(h.state().settings.message_font, 21.0);
        assert_eq!(saved(&h).message_font, 21.0);
        assert!(h.query_by_label_contains("САМЫЙ КРУПНЫЙ").is_some());
        let height = |h: &H| h.get_by_label("Это предпросмотр того, как будут выглядеть ваши сообщения с выбранным размером шрифта.").rect().height();
        let big = height(&h);
        h.get_by_role(Role::Slider).set_value_accesskit(0.);
        h.run();
        assert_eq!(h.state().settings.message_font, 11.0);
        assert!(height(&h) < big, "the sample follows the size: {} vs {big}", height(&h));
    }

    #[test]
    fn images_and_voice_saved() {
        let mut h = opened();
        go(&mut h, "Сжимать большие картинки");
        h.get_by_role(Role::Slider).set_value_accesskit(70.);
        h.run();
        assert_eq!(h.state().settings.media.image_quality, 70);
        go(&mut h, "Качество голосовых сообщений");
        h.get_by_label("Высокий (128 kbps)").click();
        h.run();
        assert_eq!(saved(&h).media.voice_bitrate, 128000);
    }

    #[test]
    fn theme_switch_applies() {
        let mut h = opened();
        go(&mut h, "Внешний вид");
        h.get_by_label("Светлая").click();
        h.run();
        h.run();
        assert_eq!(h.state().settings.dark, Some(false));
        assert_eq!(saved(&h).dark, Some(false));
        assert!(!h.ctx.global_style().visuals.dark_mode);
        h.get_by_label("Тёмная").click();
        h.run();
        h.run();
        assert!(h.ctx.global_style().visuals.dark_mode);
    }

    #[test]
    fn backup_section_exports_then_stages_import() {
        use crate::test_support::run_until;
        let mut h = opened();
        go(&mut h, "Бэкап и восстановление");
        let out = tempfile::tempdir().unwrap();
        let dest = out.path().join("b.zip");
        super::set_backup_picked(dest.clone());
        h.get_by_label("Бэкап данных").click();
        h.run_steps(2);
        run_until(&mut h, "the export", |h| h.query_all_by_label("Бэкап сохранён успешно!").next().is_some());
        crate::backup::validate(&dest).unwrap();
        // A file that is no backup: Android's refusal under the row, nothing asked.
        let bad = out.path().join("bad.zip");
        std::fs::write(&bad, "nope").unwrap();
        super::set_backup_picked(bad);
        h.get_by_label("Восстановление данных").click();
        h.run_steps(2);
        run_until(&mut h, "the refusal", |h| h.query_all_by_label("Это не файл бэкапа Mimir!").next().is_some());
        assert!(h.state().state.settings_ui.job_failed && h.state().state.settings_ui.confirm.is_none());
        // Our backup: Android's warning first, then it is staged and the app relaunches (P4-R1).
        super::set_backup_picked(dest);
        h.get_by_label("Восстановление данных").click();
        h.run_steps(2);
        run_until(&mut h, "the warning", |h| h.query_by_label("Импорт бэкапа?").is_some());
        let pending = h.state().paths.root.join("restore_pending.zip");
        assert!(!pending.exists(), "nothing staged before «Импорт»");
        h.get_by_label("Импорт").click();
        h.run_steps(2);
        run_until(&mut h, "the staging", |h| h.state().state.settings_ui.restarting);
        assert!(pending.is_file());
        assert!(h.state().state.quitting, "the relaunch quits for real: the tray does not keep this instance");
    }

    #[test]
    fn language_switch_applies() {
        let mut h = opened();
        go(&mut h, "Внешний вид");
        h.get_by_label("English").click();
        h.run();
        assert_eq!(h.state().settings.lang, Some(crate::i18n::Lang::En));
        assert_eq!(h.state().lang, crate::i18n::Lang::En);
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(h.query_by_label("Saved Messages").is_some(), "the chat list follows");
        h.get_by_label("Main menu").click();
        h.run();
        assert!(h.query_by_label("Settings").is_some());
    }
}
