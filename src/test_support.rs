//! Shared test fixtures.
use crate::{app::MimirDesktop, core::{self, events::{Bridge, Event}, worker::Worker}, i18n::Lang, paths::DataPaths, settings::Settings, ui::theme};
use egui_kittest::Harness;
use mimir::MimirApp;
use std::sync::{mpsc::Sender, Arc};

/// Real core on a temp dir; keep the TempDir alive for the test's duration.
pub fn test_app() -> (tempfile::TempDir, Arc<MimirApp>) {
    let tmp = tempfile::tempdir().unwrap();
    let app = core::open(&DataPaths::under(tmp.path())).unwrap();
    (tmp, app)
}

/// Starts the core with no peers, trackers or multicast, so sends are stored and then find nobody to dial.
pub fn start_offline(core: &MimirApp, tx: Sender<Event>, ctx: &eframe::egui::Context) {
    start_offline_into(core, tx, ctx, String::new());
}

/// `start_offline` with attachments in `attachments_dir` ("" = the core's own `<data_dir>/files`).
fn start_offline_into(core: &MimirApp, tx: Sender<Event>, ctx: &eframe::egui::Context, attachments_dir: String) {
    let cfg = mimir::AppConfig {
        ygg_peers: vec![], trackers: vec![], default_mediators: vec![], peer_port: 0, mediator_port: 0, files_port: 0,
        auto_download_contacts: 0, auto_download_others: 0, auto_download_groups: 0, multicast_enabled: false,
        priority: 0, attachments_dir,
    };
    core.start(cfg, Box::new(Bridge::new(tx, ctx.clone()))).unwrap();
}

/// Rewires an app so its core is started and its workers and bridge feed the app's own event channel; attachments
/// go to the app's files_dir, as with `net::config_for`.
pub fn start_app(app: &mut MimirDesktop, ctx: &eframe::egui::Context) {
    let (tx, rx) = std::sync::mpsc::channel();
    app.worker = Worker::spawn(app.core.clone(), tx.clone(), ctx.clone());
    app.group_worker = Worker::spawn(app.core.clone(), tx.clone(), ctx.clone());
    app.rx = rx;
    start_offline_into(&app.core, tx, ctx, app.paths.files_dir.to_string_lossy().into_owned());
}

/// Whole app (Russian UI) in a headless harness on a temp data dir.
pub fn test_harness(settings: Settings) -> Harness<'static, MimirDesktop> {
    harness(settings, false)
}

/// Like `test_harness`, but missing glyphs render as tofu (production eframe behaviour) instead of panicking.
pub fn test_harness_tofu(settings: Settings) -> Harness<'static, MimirDesktop> {
    harness(settings, true)
}

fn harness(mut settings: Settings, tofu: bool) -> Harness<'static, MimirDesktop> {
    let tmp = tempfile::tempdir().unwrap();
    let paths = DataPaths::under(tmp.path());
    let core = core::open(&paths).unwrap();
    settings.lang = Some(Lang::Ru);
    let b = Harness::builder().with_context_setup(theme::install);
    let b = if tofu { b.allow_missing_glyphs() } else { b };
    // Built on the harness's own context, and run through eframe::App (logic + ui) like the real window.
    b.build_eframe(move |cc| {
        let mut app = MimirDesktop::new_with(&cc.egui_ctx, core, paths, settings, None);
        app.test_tmp = Some(tmp);
        app
    })
}

/// A `w`×`h` PNG of one colour.
pub fn png_bytes(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
    let mut out = std::io::Cursor::new(vec![]);
    image::RgbImage::from_pixel(w, h, image::Rgb(rgb)).write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

/// Sizes of the textured rects (pictures, not text) painted in the last frame.
pub fn textured(h: &Harness<'static, MimirDesktop>) -> Vec<eframe::egui::Vec2> {
    h.output()
        .shapes
        .iter()
        .filter_map(|cs| match &cs.shape {
            eframe::egui::epaint::Shape::Rect(r) if r.fill_texture_id() != eframe::egui::TextureId::default() => Some(r.rect.size()),
            _ => None,
        })
        .collect()
}

/// Runs frames until `done` holds (off-thread decodes and worker jobs land meanwhile); panics after 10 s.
pub fn run_until(h: &mut Harness<'static, MimirDesktop>, what: &str, mut done: impl FnMut(&Harness<'static, MimirDesktop>) -> bool) {
    let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !done(h) {
        assert!(std::time::Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(std::time::Duration::from_millis(20));
        h.run_steps(2);
    }
}

/// Fill of the button labelled `label` in the last frame (the filled rect painted under its centre, smallest first).
pub fn button_fill(h: &Harness<'static, MimirDesktop>, label: &str) -> Option<eframe::egui::Color32> {
    use egui_kittest::kittest::Queryable as _;
    let r = h.get_by_label(label).rect();
    h.output()
        .shapes
        .iter()
        .filter_map(|cs| match &cs.shape {
            eframe::egui::epaint::Shape::Rect(s) if s.rect.contains(r.center()) && s.rect.width() <= r.width() + 8. && s.fill.a() > 0 => Some(s.fill),
            _ => None,
        })
        .last()
}
