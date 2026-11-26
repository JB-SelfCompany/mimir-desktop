//! Voice messages from the composer: the mic button, the recording bar (timer, ✕, ✓) and the encode job.
//! ✓ sends at once; the 60 s cap stops the take and stages it in the attachment panel; ✕ throws it away.
use crate::{
    app::MimirDesktop,
    i18n::{tr, Key as K, Lang},
    media::{ffmpeg, recorder::{self, Recorder}, stage::{self, Staged}},
    model::send::{send_attachment, Target},
    ui::{theme::Palette, voice_bubble::clock},
};
use eframe::egui::{self, vec2, Align, Color32, Layout, Painter, Pos2, Rect, Role, Sense, Shape, Stroke, Ui, Vec2, WidgetInfo};
use std::{f32::consts::PI, path::Path, sync::mpsc::{self, Receiver, TryRecvError}, time::Duration};

/// The voice wire format.
const VOICE_NAME: &str = "voice_message.m4a";
const VOICE_MIME: &str = "audio/mp4";

/// What becomes of a take once encoded.
enum Then {
    /// ✓: sent to this chat, as a reply to this guid (0 = none).
    Send(Target, i64),
    /// The 60 s cap: staged in the attachment panel, sent with the next Send.
    Stage,
}

#[derive(Default)]
pub struct Voice {
    rec: Option<Recorder>,
    /// The encode job of a stopped take; its result is localized text on failure.
    encoding: Option<(Receiver<Result<Staged, String>>, Then)>,
    /// The file the last take was encoded into (tests check it is gone after a failure).
    #[cfg(test)]
    last_out: Option<std::path::PathBuf>,
}

impl Voice {
    pub fn recording(&self) -> bool {
        self.rec.is_some()
    }

    /// Chat switch: a take in progress is thrown away, and so is a capped one that would land in the new chat's
    /// panel. A take already sent with ✓ still goes to its own chat.
    pub fn reset(&mut self) {
        if let Some(rec) = self.rec.take() {
            rec.cancel();
        }
        if matches!(self.encoding, Some((_, Then::Stage))) {
            self.encoding = None; // the job finds nobody waiting and deletes its file
        }
    }
}

#[cfg(test)]
thread_local! {
    #[allow(clippy::missing_const_for_thread_local, reason = "already const; clippy misreads the cfg(test) seam")]
    static NEXT: std::cell::RefCell<Option<Recorder>> = const { std::cell::RefCell::new(None) };
}

/// Test seam: the recorder the next mic click "opens" on this thread (tests never touch a device).
#[cfg(test)]
pub fn set_recorder(rec: Recorder) {
    NEXT.with(|n| *n.borrow_mut() = Some(rec));
}

fn open() -> Result<Recorder, String> {
    #[cfg(test)]
    return NEXT.with(|n| n.borrow_mut().take()).ok_or_else(|| "No microphone found".into());
    #[cfg(not(test))]
    Recorder::start()
}

/// The round mic button in the send button's place (empty composer, no attachment). Without ffmpeg it is
/// disabled and its tooltip says why.
pub fn mic(app: &mut MimirDesktop, ui: &mut Ui, size: f32, p: &Palette) {
    let lang = app.lang;
    let have_ffmpeg = app.state.ffmpeg.is_some();
    let enabled = have_ffmpeg && app.state.core_started && app.state.open_chat.is_some() && app.state.composer.voice.encoding.is_none();
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, tr(lang, K::RecordVoice)));
    let c = rect.center();
    let hover = if enabled && resp.hovered() { 1.1 } else { 1. };
    ui.painter().circle_filled(c, size / 2. * hover, if enabled { p.accent } else { p.accent.gamma_multiply(0.35) });
    mic_glyph(ui.painter(), c, Color32::WHITE);
    let resp = resp.on_hover_text(tr(lang, if have_ffmpeg { K::RecordVoice } else { K::InstallFfmpeg }));
    if resp.clicked() && enabled {
        match open() {
            Ok(rec) => app.state.composer.voice.rec = Some(rec),
            Err(e) => app.error_text(device_error(lang, &e)),
        }
    }
}

/// The input row while recording: ✕, a blinking red dot and the timer, ✓ on the right.
pub fn bar(app: &mut MimirDesktop, ui: &mut Ui, size: f32, p: &Palette) {
    let lang = app.lang;
    let Some(rec) = &app.state.composer.voice.rec else { return };
    if let Some(e) = rec.failed() {
        app.state.composer.voice.rec = None;
        app.error_text(device_error(lang, &e));
        return;
    }
    let ms = rec.elapsed_ms();
    let capped = ms >= recorder::MAX_MS;
    ui.ctx().request_repaint_after(Duration::from_millis(250)); // the timer and the dot
    let (mut cancel, mut send) = (false, false);
    ui.horizontal(|ui| {
        ui.set_height(size);
        cancel = round_button(ui, size, tr(lang, K::Cancel), p.field, |pt, c| cross(pt, c, p.text));
        ui.add_space(6.);
        let (dot, _) = ui.allocate_exact_size(vec2(12., size), Sense::hover());
        let lit = (ms / 500) % 2 == 0;
        ui.painter().circle_filled(dot.center(), 5., if lit { p.danger } else { p.danger.gamma_multiply(0.3) });
        ui.label(egui::RichText::new(clock(ms.min(recorder::MAX_MS) / 1000 * 1000)).size(15.).color(p.text));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            send = round_button(ui, size, tr(lang, K::Send), p.accent, |pt, c| check(pt, c, Color32::WHITE));
        });
    });
    if cancel {
        if let Some(rec) = app.state.composer.voice.rec.take() {
            rec.cancel();
        }
    } else if send || capped {
        let then = match (&app.state.open_chat, capped) {
            // The reply target stays in the composer until the take is really sent (a too-short take keeps it).
            (Some(chat), false) => Then::Send(Target::for_key(chat.key, chat.pubkey.clone()), app.state.reply_to.as_ref().map_or(0, |(g, _)| *g)),
            _ => Then::Stage,
        };
        finish(app, ui.ctx(), then);
    }
}

/// Stops the take and encodes it on a thread of its own (finishing waits for the capture thread; ffmpeg runs
/// for a while): never on the UI thread, nor on a worker lane a big upload could hold up.
fn finish(app: &mut MimirDesktop, ctx: &egui::Context, then: Then) {
    let Some(rec) = app.state.composer.voice.rec.take() else { return };
    let Some(bin) = app.state.ffmpeg.clone() else { return rec.cancel() };
    let (bitrate, lang, ctx) = (voice_bitrate(app.settings.media.voice_bitrate), app.lang, ctx.clone());
    let out = stage::temp_path("m4a"); // a new file per take
    #[cfg(test)]
    {
        app.state.composer.voice.last_out = Some(out.clone());
    }
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let r = encode(rec, &bin, bitrate, &out, lang);
        if let Err(back) = tx.send(r) {
            back.0.into_iter().for_each(stage::discard); // nobody waits any more: our file goes
        }
        ctx.request_repaint();
    });
    app.state.composer.voice.encoding = Some((rx, then));
}

/// The setting if it is one of Android's three bitrates, else the default 32000.
fn voice_bitrate(b: u32) -> u32 {
    if matches!(b, 32000 | 64000 | 128000) { b } else { 32000 }
}

/// A capture failure in the user's language: no device has its own text, anything else keeps the detail.
fn device_error(lang: Lang, e: &str) -> String {
    match e {
        recorder::NO_MIC => tr(lang, K::NoMicrophone).to_string(),
        _ => format!("{}: {e}", tr(lang, K::FailedToStartRecording)),
    }
}

fn encode(rec: Recorder, bin: &Path, bitrate: u32, out: &Path, lang: Lang) -> Result<Staged, String> {
    let failed = |e: String| format!("{}: {e}", tr(lang, K::RecordingFailed));
    let r = rec.finish().map_err(|e| match e.as_str() {
        recorder::TOO_SHORT => tr(lang, K::RecordingTooShort).to_string(),
        recorder::NO_MIC => device_error(lang, &e),
        _ => failed(e),
    })?;
    ffmpeg::encode_voice(bin, &r.pcm, r.rate, r.channels, bitrate, out).map_err(failed)?;
    let size = std::fs::metadata(out).map_or(0, |m| m.len());
    Ok(Staged { path: out.to_path_buf(), original_name: VOICE_NAME.into(), mime: VOICE_MIME.into(), msg_type: 3, size, temp: true, resized: false })
}

/// Picks up a finished encode: sends it, stages it, or reports why not.
pub fn poll(app: &mut MimirDesktop) {
    let Some((rx, _)) = &app.state.composer.voice.encoding else { return };
    let r = match rx.try_recv() {
        Ok(r) => r,
        Err(TryRecvError::Empty) => return,
        // The encode thread died (a panic) without a word.
        Err(TryRecvError::Disconnected) => Err(tr(app.lang, K::RecordingFailed).to_string()),
    };
    let Some((_, then)) = app.state.composer.voice.encoding.take() else { return };
    match (r, then) {
        (Ok(s), Then::Send(to, reply)) => {
            if reply != 0 && app.state.reply_to.as_ref().is_some_and(|(g, _)| *g == reply) {
                app.state.reply_to = None;
            }
            send_attachment(app, to, reply, String::new(), s);
        }
        (Ok(s), Then::Stage) => {
            let c = &mut app.state.composer;
            match c.attachment {
                None => c.attachment = Some(s),
                Some(_) => c.more.push(s),
            }
        }
        (Err(e), _) => app.error_text(e),
    }
}

pub(crate) fn round_button(ui: &mut Ui, size: f32, label: &str, fill: Color32, glyph: impl Fn(&Painter, Pos2)) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, label));
    let hover = if resp.hovered() { 1.1 } else { 1. };
    ui.painter().circle_filled(rect.center(), size / 2. * hover, fill);
    glyph(ui.painter(), rect.center());
    resp.on_hover_text(label).clicked()
}

/// A microphone (ic_microphone_outline): capsule, holder, stem and base.
fn mic_glyph(painter: &Painter, c: Pos2, colour: Color32) {
    let s = Stroke::new(1.6, colour);
    painter.rect_filled(Rect::from_center_size(c + vec2(0., -3.), vec2(6., 10.)), 3, colour);
    let holder = (0..=10).map(|i| c + vec2(0., -1.) + 6. * Vec2::angled(PI * i as f32 / 10.)).collect();
    painter.add(Shape::line(holder, s));
    painter.line_segment([c + vec2(0., 5.), c + vec2(0., 8.)], s);
    painter.line_segment([c + vec2(-3.5, 8.), c + vec2(3.5, 8.)], s);
}

pub(crate) fn cross(painter: &Painter, c: Pos2, colour: Color32) {
    let (s, d) = (Stroke::new(1.8, colour), 5.);
    painter.line_segment([c + vec2(-d, -d), c + vec2(d, d)], s);
    painter.line_segment([c + vec2(-d, d), c + vec2(d, -d)], s);
}

pub(crate) fn check(painter: &Painter, c: Pos2, colour: Color32) {
    painter.add(Shape::line(vec![c + vec2(-6., 0.), c + vec2(-2., 4.5), c + vec2(6., -4.5)], Stroke::new(2., colour)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        media::{ffmpeg::tests::fake_ffmpeg, recorder::Recording},
        model::{attachment::AttachmentMeta, chat_key::ChatKey},
        settings::Settings,
        test_support::{start_app, test_harness},
    };
    use egui_kittest::{kittest::{NodeT as _, Queryable as _}, Harness};
    use std::time::Instant;

    const MIC: &str = "Записать голосовое сообщение";

    /// Saved Messages open with the core started; \`ffmpeg\` is what the app "found".
    fn saved(ffmpeg: Option<std::path::PathBuf>) -> Harness<'static, MimirDesktop> {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        let ctx = h.ctx.clone();
        start_app(h.state_mut(), &ctx);
        h.state_mut().state.core_started = true;
        h.state_mut().state.ffmpeg = ffmpeg;
        h.state_mut().state.selected = Some(ChatKey::SAVED);
        h.run_steps(4);
        h
    }

    /// A take of \`secs\` seconds (8 kHz mono) that started \`secs\` ago.
    fn take(secs: f32) -> Recorder {
        let rec = Recording { pcm: vec![0.1; (secs * 8000.) as usize], rate: 8000, channels: 1 };
        Recorder::fake(rec, Instant::now() - Duration::from_millis((secs * 1000.) as u64))
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

    #[test]
    fn mic_disabled_without_ffmpeg() {
        let mut h = saved(None);
        let mic = h.get_by_label(MIC);
        assert!(mic.accesskit_node().is_disabled(), "no ffmpeg: no recording");
        assert!(h.query_by_label("Отправить").is_none(), "an empty composer shows the mic, not send");
        set_recorder(take(3.));
        mic.click();
        h.run_steps(3);
        assert!(!h.state().state.composer.voice.recording(), "a click does nothing");
        h.get_by_label(MIC).hover();
        for _ in 0..40 {
            h.step();
            if h.query_by_label("Установите ffmpeg, чтобы записывать голосовые").is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(h.query_by_label("Установите ffmpeg, чтобы записывать голосовые").is_some(), "the hint as its tooltip");
        // With text the button is send again.
        h.state_mut().state.composer.text = "привет".into();
        h.run_steps(2);
        assert!(h.query_by_label(MIC).is_none() && h.query_by_label("Отправить").is_some());
    }

    #[test]
    fn record_bar_flow() {
        let tmp = tempfile::tempdir().unwrap();
        let mut h = saved(Some(fake_ffmpeg(tmp.path(), 0, "", None)));
        set_recorder(take(3.2));
        h.get_by_label(MIC).click();
        h.run_steps(3);
        assert!(h.state().state.composer.voice.recording());
        assert!(h.query_by_label("0:03").is_some(), "the timer");
        assert!(h.query_by_role(Role::MultilineTextInput).is_none(), "the bar replaces the input row");
        h.get_by_label("Отмена").click();
        h.run_steps(3);
        assert!(!h.state().state.composer.voice.recording(), "✕ closes the bar");
        assert!(!h.get_by_label(MIC).accesskit_node().is_disabled());
        std::thread::sleep(Duration::from_millis(200));
        h.run_steps(3);
        assert_eq!(msgs(&h), 0, "✕ sends nothing");
        // ✓ encodes and sends right away, as a reply when one is set.
        h.state_mut().state.reply_to = Some((5, "привет".into()));
        h.run_steps(2); // the reply row moves the buttons
        set_recorder(take(3.2));
        h.get_by_label(MIC).click();
        h.run_steps(3);
        h.get_by_label("Отправить").click();
        h.step();
        assert!(!h.state().state.composer.voice.recording());
        wait_until(&mut h, "voice message stored", |h| msgs(h) == 1);
        let m = &h.state().state.open_chat.as_ref().unwrap().timeline.msgs[0];
        let meta = AttachmentMeta::parse(m.data.as_deref().unwrap()).unwrap();
        assert_eq!((m.msg_type, meta.original_name.as_str(), meta.mime.as_str(), meta.caption.as_str()), (3, "voice_message.m4a", "audio/mp4", ""));
        assert!(h.state().paths.files_dir.join(&meta.name).is_file(), "the core copied it into files_dir");
        assert!(h.state().state.composer.attachment.is_none(), "sent, not staged");
        assert_eq!(h.state().state.reply_to, None, "the reply went with the take");
        assert_eq!(h.state().state.status, None);
    }

    #[test]
    fn encode_failure_reports_and_resets() {
        let tmp = tempfile::tempdir().unwrap();
        let mut h = saved(Some(fake_ffmpeg(tmp.path(), 1, "Encoder aac not found", None)));
        set_recorder(take(2.));
        h.get_by_label(MIC).click();
        h.run_steps(3);
        h.get_by_label("Отправить").click();
        h.step();
        wait_until(&mut h, "error reported", |h| h.state().state.status.is_some());
        let (text, _) = h.state().state.status.clone().unwrap();
        assert!(text.contains("Encoder aac not found") && !h.state().state.status_ok, "{text}");
        assert!(!h.state().state.composer.voice.recording(), "record bar closed");
        assert!(h.state().state.composer.voice.encoding.is_none());
        assert!(!h.get_by_label(MIC).accesskit_node().is_disabled(), "ready for another take");
        let out = h.state().state.composer.voice.last_out.clone().expect("the take's output path");
        assert!(!out.exists(), "no output file left: {out:?}");
        assert_eq!(msgs(&h), 0);
    }

    #[test]
    fn too_short_recording() {
        let tmp = tempfile::tempdir().unwrap();
        let mut h = saved(Some(fake_ffmpeg(tmp.path(), 0, "", None)));
        h.state_mut().state.reply_to = Some((5, "привет".into()));
        h.run_steps(2); // the reply row moves the buttons
        set_recorder(take(0.6));
        h.get_by_label(MIC).click();
        h.run_steps(3);
        h.get_by_label("Отправить").click();
        h.step();
        wait_until(&mut h, "too short reported", |h| h.state().state.status.is_some());
        assert_eq!(h.state().state.status.as_ref().unwrap().0, "Запись слишком короткая");
        assert_eq!(h.state().state.reply_to, Some((5, "привет".into())), "a take not sent keeps the reply target");
        std::thread::sleep(Duration::from_millis(200));
        h.run_steps(3);
        assert_eq!(msgs(&h), 0);
    }

    #[test]
    fn sixty_second_cap_stops() {
        let tmp = tempfile::tempdir().unwrap();
        let mut h = saved(Some(fake_ffmpeg(tmp.path(), 0, "", None)));
        set_recorder(take(61.));
        h.get_by_label(MIC).click();
        h.run_steps(3);
        assert!(!h.state().state.composer.voice.recording(), "the cap stops the take by itself");
        wait_until(&mut h, "staged", |h| h.state().state.composer.attachment.is_some());
        h.run_steps(2); // the panel's first frame moves the send button
        let s = h.state().state.composer.attachment.clone().unwrap();
        assert_eq!((s.original_name.as_str(), s.mime.as_str(), s.msg_type, s.temp), ("voice_message.m4a", "audio/mp4", 3, true));
        assert_eq!(msgs(&h), 0, "staged, not sent");
        h.get_by_label("Отправить").click();
        h.step();
        wait_until(&mut h, "sent with Send", |h| msgs(h) == 1);
        wait_until(&mut h, "temp file deleted once sent", |_| !s.path.exists());
    }

    #[test]
    fn device_failure_closes_the_bar() {
        let tmp = tempfile::tempdir().unwrap();
        let mut h = saved(Some(fake_ffmpeg(tmp.path(), 0, "", None)));
        set_recorder(Recorder::failing(recorder::NO_MIC));
        h.get_by_label(MIC).click();
        wait_until(&mut h, "error reported", |h| h.state().state.status.is_some());
        assert_eq!(h.state().state.status.as_ref().unwrap().0, "Микрофон не найден", "localized, no raw English");
        assert!(!h.state().state.composer.voice.recording());
        h.state_mut().state.status = None;
        h.run_steps(2);
        set_recorder(Recorder::failing("device busy"));
        h.get_by_label(MIC).click();
        wait_until(&mut h, "error reported", |h| h.state().state.status.is_some());
        assert_eq!(h.state().state.status.as_ref().unwrap().0, "Не удалось начать запись: device busy");
    }

    #[test]
    fn dead_encode_job_is_reported() {
        let mut h = saved(None);
        let (tx, rx) = mpsc::channel();
        drop(tx); // the encode thread panicked before sending
        h.state_mut().state.composer.voice.encoding = Some((rx, Then::Stage));
        h.run_steps(2);
        assert_eq!(h.state().state.status.as_ref().map(|s| s.0.as_str()), Some("Не удалось сохранить запись"));
        assert!(h.state().state.composer.voice.encoding.is_none());
    }

    #[test]
    fn bitrate_is_one_of_androids() {
        assert_eq!([32000, 64000, 128000, 0, 48000, u32::MAX].map(voice_bitrate), [32000, 64000, 128000, 32000, 32000, 32000]);
    }
}
