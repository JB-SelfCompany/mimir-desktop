//! Video messages from the composer: the camera button beside the mic, the recording dialog (round live preview,
//! timer, camera picker, ✕, ✓) and the job that stops ffmpeg and sends the take as Android does (msg_type 4).
//! ✓ and the 60 s cap send at once (Android sends on release and at the cap); ✕, Esc or a click outside throw it away.
use crate::{
    app::MimirDesktop,
    i18n::{tr, Key as K, Lang},
    media::{camera::{self, Camera}, stage::{self, Staged}},
    model::send::{send_attachment, Target},
    ui::{record_bar::{check, cross, round_button}, theme::{self, Palette}, voice_bubble::clock},
};
use eframe::egui::{self, vec2, Align, Color32, ComboBox, Id, Image, Layout, Rect, Role, Sense, TextureHandle, TextureOptions, Ui, Vec2, WidgetInfo};
use std::{sync::mpsc::{self, Receiver, TryRecvError}, time::Duration};

/// The video-message wire format (Android stageVideoForSending).
const VIDEO_NAME: &str = "video_message.mp4";
const VIDEO_MIME: &str = "video/mp4";
const MSG_TYPE: i32 = 4;
/// Diameter of the live preview.
const CIRCLE: f32 = 240.;

/// The stop-and-send job of a finished take, with its chat and reply guid (0 = none).
type Finishing = (Receiver<Result<Staged, String>>, Target, i64);

#[derive(Default)]
pub struct Video {
    cam: Option<Camera>,
    preview: Option<TextureHandle>,
    /// The camera picked in the dialog; the next takes use it too.
    // ponytail: for this session only, not kept in the settings.
    camera: Option<String>,
    finishing: Option<Finishing>,
}

impl Video {
    pub fn recording(&self) -> bool {
        self.cam.is_some()
    }

    /// Chat switch: a take in progress is thrown away; one already sent with ✓ still goes to its own chat.
    pub fn reset(&mut self) {
        if let Some(cam) = self.cam.take() {
            cam.cancel();
        }
        self.preview = None;
    }
}

/// What the next camera click "opens" in tests, given the take's file.
#[cfg(test)]
type MakeCamera = Box<dyn FnOnce(std::path::PathBuf) -> Camera>;

#[cfg(test)]
thread_local! {
    #[allow(clippy::missing_const_for_thread_local, reason = "already const; clippy misreads the cfg(test) seam")]
    static NEXT: std::cell::RefCell<Option<MakeCamera>> = const { std::cell::RefCell::new(None) };
}

/// Test seam: what the next camera click "opens" on this thread, given the take's file (tests never touch a device).
#[cfg(test)]
pub fn set_camera(make: impl FnOnce(std::path::PathBuf) -> Camera + 'static) {
    NEXT.with(|n| *n.borrow_mut() = Some(Box::new(make)));
}

fn open(app: &MimirDesktop, ctx: &egui::Context) -> Option<Camera> {
    let bin = app.state.ffmpeg.clone()?;
    let out = stage::temp_path("mp4"); // a new file per take
    #[cfg(test)]
    {
        let _ = (bin, ctx);
        Some(NEXT.with(|n| n.borrow_mut().take()).map_or_else(|| Camera::failing(camera::NO_CAMERA), |make| make(out)))
    }
    #[cfg(not(test))]
    Some(Camera::start(bin, app.state.composer.video.camera.clone(), out, ctx.clone()))
}

/// The camera button left of the mic/send button. Without ffmpeg it is disabled and its tooltip says why.
pub fn button(app: &mut MimirDesktop, ui: &mut Ui, size: f32, p: &Palette) {
    let lang = app.lang;
    let have_ffmpeg = app.state.ffmpeg.is_some();
    let v = &app.state.composer.video;
    let enabled = have_ffmpeg && app.state.core_started && app.state.open_chat.is_some() && v.cam.is_none() && v.finishing.is_none();
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), if enabled { Sense::click() } else { Sense::hover() });
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, tr(lang, K::RecordVideo)));
    let colour = match () {
        _ if !enabled => p.text_dim.gamma_multiply(0.5),
        _ if resp.hovered() => p.accent,
        _ => p.text_dim,
    };
    // MDI video-outline, as the attach paperclip: a dim 22 px glyph, accent on hover.
    Image::new(egui::include_image!("../../assets/icons/video-outline.svg")).tint(colour).paint_at(ui, Rect::from_center_size(rect.center(), Vec2::splat(22.)));
    let resp = resp.on_hover_text(tr(lang, if have_ffmpeg { K::RecordVideo } else { K::InstallFfmpegVideo }));
    if resp.clicked() && enabled {
        app.state.composer.video.cam = open(app, ui.ctx());
    }
}

/// A capture failure in the user's language: no camera has its own text, anything else keeps the detail. A take
/// that had started failed to record, not to start.
fn device_error(lang: Lang, e: &str, started: bool) -> String {
    match e {
        camera::NO_CAMERA => tr(lang, K::NoCamera).to_string(),
        _ => format!("{}: {e}", tr(lang, if started { K::RecordingFailed } else { K::FailedToStartCamera })),
    }
}

/// The recording dialog, while a take is open: the camera picker (several cameras), the round preview, ✕, the
/// blinking dot and timer, ✓.
pub fn show(app: &mut MimirDesktop, ctx: &egui::Context, p: &Palette) {
    let lang = app.lang;
    let v = &mut app.state.composer.video;
    let Some(cam) = &v.cam else { return };
    if let Some(e) = cam.failed() {
        let started = cam.elapsed_ms() > 0;
        v.reset();
        app.error_text(device_error(lang, &e, started));
        return;
    }
    if let Some(img) = cam.take_frame() {
        match &mut v.preview {
            Some(t) => t.set(img, TextureOptions::LINEAR),
            None => v.preview = Some(ctx.load_texture("video_preview", img, TextureOptions::LINEAR)),
        }
    }
    let ms = cam.elapsed_ms();
    let capped = ms >= camera::MAX_MS || cam.ended();
    ctx.request_repaint_after(Duration::from_millis(250)); // the timer and the dot
    let (cams, current) = cam.cameras();
    let preview = v.preview.as_ref().map(|t| t.id());
    let (mut cancel, mut send, mut switch) = (false, false, None);
    let r = theme::dialog(ctx, Id::unique("video_message")).show(ctx, |ui| {
        ui.set_width(CIRCLE);
        theme::dialog_title(ui, tr(lang, K::Video));
        if cams.len() > 1 {
            let mut pick = current.clone().unwrap_or_default();
            let l = theme::field_label(ui, p, tr(lang, K::Camera));
            ComboBox::from_id_salt("video_camera")
                .selected_text(pick.clone())
                .width(CIRCLE)
                .show_ui(ui, |ui| {
                    for c in &cams {
                        ui.selectable_value(&mut pick, c.clone(), c);
                    }
                })
                .response
                .labelled_by(l.id);
            if current.as_ref() != Some(&pick) {
                switch = Some(pick);
            }
            ui.add_space(8.);
        }
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(CIRCLE), Sense::hover());
        match preview {
            Some(tex) => Image::new((tex, rect.size())).corner_radius(CIRCLE / 2.).paint_at(ui, rect),
            None => {
                // The camera is still starting.
                ui.painter().circle_filled(rect.center(), CIRCLE / 2., p.field);
                ui.put(Rect::from_center_size(rect.center(), Vec2::splat(24.)), egui::Spinner::new());
            }
        }
        ui.add_space(12.);
        ui.horizontal(|ui| {
            const SIZE: f32 = 30.;
            ui.set_height(SIZE);
            cancel = round_button(ui, SIZE, tr(lang, K::Cancel), p.field, |pt, c| cross(pt, c, p.text));
            ui.add_space(6.);
            let (dot, _) = ui.allocate_exact_size(vec2(12., SIZE), Sense::hover());
            let lit = (ms / 500) % 2 == 0;
            ui.painter().circle_filled(dot.center(), 5., if lit { p.danger } else { p.danger.gamma_multiply(0.3) });
            ui.label(egui::RichText::new(clock(ms.min(camera::MAX_MS) / 1000 * 1000)).size(15.).color(p.text));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                send = round_button(ui, SIZE, tr(lang, K::Send), p.accent, |pt, c| check(pt, c, Color32::WHITE));
            });
        });
    });
    if cancel || r.should_close() {
        app.state.composer.video.reset();
    } else if let Some(pick) = switch {
        app.state.composer.video.reset();
        app.state.composer.video.camera = Some(pick);
        app.state.composer.video.cam = open(app, ctx);
    } else if send || capped {
        finish(app, ctx);
    }
}

/// Stops the take (ffmpeg finishes the mp4) on a thread of its own and queues it for sending to the open chat.
fn finish(app: &mut MimirDesktop, ctx: &egui::Context) {
    let v = &mut app.state.composer.video;
    let Some(cam) = v.cam.take() else { return };
    v.preview = None;
    let Some(chat) = &app.state.open_chat else { return cam.cancel() };
    // The reply target stays in the composer until the take is really sent (a too-short take keeps it).
    let (to, reply) = (Target::for_key(chat.key, chat.pubkey.clone()), app.state.reply_to.as_ref().map_or(0, |(g, _)| *g));
    let (lang, ctx) = (app.lang, ctx.clone());
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let r = cam.finish().map_err(|e| match e.as_str() {
            camera::TOO_SHORT => tr(lang, K::RecordingTooShort).to_string(),
            _ => format!("{}: {e}", tr(lang, K::RecordingFailed)),
        });
        let r = r.map(|(path, _)| {
            let size = std::fs::metadata(&path).map_or(0, |m| m.len());
            Staged { path, original_name: VIDEO_NAME.into(), mime: VIDEO_MIME.into(), msg_type: MSG_TYPE, size, temp: true, resized: false }
        });
        if let Err(back) = tx.send(r) {
            back.0.into_iter().for_each(stage::discard); // nobody waits any more: our file goes
        }
        ctx.request_repaint();
    });
    app.state.composer.video.finishing = Some((rx, to, reply));
}

/// Picks up a finished take: sends it, or reports why not.
pub fn poll(app: &mut MimirDesktop) {
    let Some((rx, _, _)) = &app.state.composer.video.finishing else { return };
    let r = match rx.try_recv() {
        Ok(r) => r,
        Err(TryRecvError::Empty) => return,
        // The job died (a panic) without a word.
        Err(TryRecvError::Disconnected) => Err(tr(app.lang, K::RecordingFailed).to_string()),
    };
    let Some((_, to, reply)) = app.state.composer.video.finishing.take() else { return };
    match r {
        Ok(s) => {
            if reply != 0 && app.state.reply_to.as_ref().is_some_and(|(g, _)| *g == reply) {
                app.state.reply_to = None;
            }
            send_attachment(app, to, reply, String::new(), s);
        }
        Err(e) => app.error_text(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        media::ffmpeg::tests::fake_ffmpeg,
        model::{attachment::AttachmentMeta, chat_key::ChatKey},
        settings::Settings,
        test_support::{start_app, test_harness},
    };
    use egui_kittest::{kittest::{NodeT as _, Queryable as _}, Harness};
    use std::{cell::RefCell, path::PathBuf, rc::Rc, time::Instant};

    const CAM: &str = "Записать видеосообщение";

    /// Saved Messages open with the core started; `ffmpeg` is what the app "found".
    fn saved(ffmpeg: Option<PathBuf>) -> Harness<'static, MimirDesktop> {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        let ctx = h.ctx.clone();
        start_app(h.state_mut(), &ctx);
        h.state_mut().state.core_started = true;
        h.state_mut().state.ffmpeg = ffmpeg;
        h.state_mut().state.selected = Some(ChatKey::SAVED);
        h.run_steps(4);
        h
    }

    fn wait_until(h: &mut Harness<'static, MimirDesktop>, what: &str, f: impl Fn(&Harness<'static, MimirDesktop>) -> bool) {
        let end = Instant::now() + Duration::from_secs(10);
        while !f(h) && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
            h.step();
        }
        assert!(f(h), "{what}");
    }

    fn msgs(h: &Harness<'static, MimirDesktop>) -> usize {
        h.state().state.open_chat.as_ref().unwrap().timeline.msgs.len()
    }

    /// The next click opens a fake take of `ms` on `cams`; the cell gets the take's file.
    fn fake(ms: u64, cams: &'static [&'static str]) -> Rc<RefCell<Option<PathBuf>>> {
        let file = Rc::new(RefCell::new(None));
        let seen = file.clone();
        set_camera(move |out| {
            *seen.borrow_mut() = Some(out.clone());
            Camera::fake(ms, cams, out)
        });
        file
    }

    fn open_dialog(h: &mut Harness<'static, MimirDesktop>) {
        h.get_by_label(CAM).click();
        wait_until(h, "the dialog with the preview", |h| h.state().state.composer.video.preview.is_some());
        h.run_steps(2);
    }

    #[test]
    fn camera_button_disabled_without_ffmpeg() {
        let mut h = saved(None);
        let cam = h.get_by_label(CAM);
        assert!(cam.accesskit_node().is_disabled(), "no ffmpeg: no recording");
        fake(5_000, &["cam"]);
        cam.click();
        h.run_steps(3);
        assert!(!h.state().state.composer.video.recording(), "a click does nothing");
        h.get_by_label(CAM).hover();
        wait_until(&mut h, "the hint as its tooltip", |h| h.query_by_label("Установите ffmpeg, чтобы записывать видеосообщения").is_some());
        // With text the button stays, beside send.
        h.state_mut().state.composer.text = "привет".into();
        h.run_steps(2);
        assert!(h.query_by_label(CAM).is_some() && h.query_by_label("Отправить").is_some());
    }

    #[test]
    fn record_dialog_flow() {
        let tmp = tempfile::tempdir().unwrap();
        let mut h = saved(Some(fake_ffmpeg(tmp.path(), 0, "", None)));
        let file = fake(5_200, &["cam"]);
        open_dialog(&mut h);
        assert!(h.state().state.composer.video.recording());
        assert!(h.query_by_label("0:05").is_some(), "the timer");
        assert!(h.query_by_label("Видеосообщение").is_some(), "the dialog title");
        assert!(h.query_by_label("Камера").is_none(), "one camera: no picker");
        h.get_by_label("Отмена").click();
        h.run_steps(3);
        assert!(!h.state().state.composer.video.recording(), "✕ closes the dialog");
        let f = file.borrow().clone().unwrap();
        wait_until(&mut h, "the cancelled take is deleted", |_| !f.exists());
        assert_eq!(msgs(&h), 0, "✕ sends nothing");
        // ✓ stops and sends right away, as a reply when one is set.
        h.state_mut().state.reply_to = Some((5, "привет".into()));
        h.run_steps(2);
        fake(5_200, &["cam"]);
        open_dialog(&mut h);
        h.get_by_label("Отправить").click();
        h.step();
        assert!(!h.state().state.composer.video.recording());
        wait_until(&mut h, "video message stored", |h| msgs(h) == 1);
        let m = &h.state().state.open_chat.as_ref().unwrap().timeline.msgs[0];
        let meta = AttachmentMeta::parse(m.data.as_deref().unwrap()).unwrap();
        assert_eq!((m.msg_type, meta.original_name.as_str(), meta.mime.as_str(), meta.caption.as_str()), (4, "video_message.mp4", "video/mp4", ""));
        assert!(h.state().paths.files_dir.join(&meta.name).is_file(), "the core copied it into files_dir");
        assert_eq!(h.state().state.reply_to, None, "the reply went with the take");
        assert_eq!(h.state().state.status, None);
        assert!(!h.get_by_label(CAM).accesskit_node().is_disabled(), "ready for another take");
    }

    #[test]
    fn too_short_take() {
        let tmp = tempfile::tempdir().unwrap();
        let mut h = saved(Some(fake_ffmpeg(tmp.path(), 0, "", None)));
        h.state_mut().state.reply_to = Some((5, "привет".into()));
        h.run_steps(2);
        let file = fake(2_500, &["cam"]);
        open_dialog(&mut h);
        h.get_by_label("Отправить").click();
        h.step();
        wait_until(&mut h, "too short reported", |h| h.state().state.status.is_some());
        assert_eq!(h.state().state.status.as_ref().unwrap().0, "Запись слишком короткая");
        assert_eq!(h.state().state.reply_to, Some((5, "привет".into())), "a take not sent keeps the reply target");
        assert!(!file.borrow().clone().unwrap().exists(), "and leaves no file");
        assert_eq!(msgs(&h), 0);
    }

    #[test]
    fn sixty_second_cap_sends() {
        let tmp = tempfile::tempdir().unwrap();
        let mut h = saved(Some(fake_ffmpeg(tmp.path(), 0, "", None)));
        fake(61_000, &["cam"]);
        h.get_by_label(CAM).click();
        wait_until(&mut h, "sent by the cap alone", |h| msgs(h) == 1);
        assert!(!h.state().state.composer.video.recording());
        assert_eq!(h.state().state.open_chat.as_ref().unwrap().timeline.msgs[0].msg_type, 4);
    }

    #[test]
    fn camera_failures_are_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let mut h = saved(Some(fake_ffmpeg(tmp.path(), 0, "", None)));
        set_camera(|_| Camera::failing(camera::NO_CAMERA));
        h.get_by_label(CAM).click();
        wait_until(&mut h, "error reported", |h| h.state().state.status.is_some());
        assert_eq!(h.state().state.status.as_ref().unwrap().0, "Камера не найдена", "localized, no raw English");
        assert!(!h.state().state.composer.video.recording());
        h.state_mut().state.status = None;
        h.run_steps(2);
        set_camera(|_| Camera::failing("Could not run graph"));
        h.get_by_label(CAM).click();
        wait_until(&mut h, "error reported", |h| h.state().state.status.is_some());
        assert_eq!(h.state().state.status.as_ref().unwrap().0, "Не удалось запустить камеру: Could not run graph");
        h.state_mut().state.status = None;
        h.run_steps(2);
        set_camera(|_| Camera::failing_after(2_000, "Device lost"));
        h.get_by_label(CAM).click();
        wait_until(&mut h, "error reported", |h| h.state().state.status.is_some());
        assert_eq!(h.state().state.status.as_ref().unwrap().0, "Не удалось сохранить запись: Device lost", "mid-take: it had started");
    }

    #[test]
    fn camera_picker_switches_and_is_remembered() {
        let tmp = tempfile::tempdir().unwrap();
        let mut h = saved(Some(fake_ffmpeg(tmp.path(), 0, "", None)));
        let first = fake(4_000, &["Front", "Back"]);
        open_dialog(&mut h);
        h.get_by_label("Камера").click();
        h.run_steps(2);
        let second = fake(4_000, &["Back", "Front"]);
        h.get_by_label("Back").click();
        h.run_steps(2);
        assert_eq!(h.state().state.composer.video.camera.as_deref(), Some("Back"));
        let f = first.borrow().clone().unwrap();
        wait_until(&mut h, "the first camera's take is thrown away", |_| !f.exists());
        assert!(second.borrow().is_some(), "the picked camera opened");
        assert!(h.state().state.composer.video.recording());
    }
}
