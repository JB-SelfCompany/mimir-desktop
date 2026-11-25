//! The composer's attachment panel (AM §1) and where attachments come from: the 📎 picker, dropped files and a
//! pasted picture (P3-R2). Staging runs on a thread of its own; the composer polls its result every frame.
use crate::{
    app::MimirDesktop,
    i18n::{tr, Key as K},
    media::{stage::{self, StageError, Staged}, thumbs::Thumb},
    settings::MediaSettings,
    ui::{attachment_bubble::{document_icon, format_size}, theme::Palette},
};
use eframe::egui::{
    self, emath::Rot2, pos2, text::LayoutJob, vec2, Align, Color32, Context, FontFamily, FontId, Image, Label, Layout, Painter, Pos2,
    Rect, Role, Sense, Shape, Stroke, Ui, Vec2, WidgetInfo,
};
use std::{f32::consts::{FRAC_PI_4, PI}, path::PathBuf, sync::mpsc::{self, TryRecvError}};

/// Preview square (Android 80dp) and the size its thumbnail is decoded at.
const PREVIEW: f32 = 80.;
const PREVIEW_PX: u32 = 160;

/// What one staging run produced, one entry per file.
pub type Batch = Vec<Result<Staged, StageError>>;

#[cfg(test)]
thread_local! {
    static PICKED: std::cell::RefCell<Option<Vec<PathBuf>>> = const { std::cell::RefCell::new(None) };
}

/// Test seam: what the next 📎 picker "returns" on this thread (tests never open a dialog).
#[cfg(test)]
pub fn set_picked(paths: Vec<PathBuf>) {
    PICKED.with(|p| *p.borrow_mut() = Some(paths));
}

#[cfg(not(test))]
fn picker() -> impl FnOnce() -> Option<Vec<PathBuf>> + Send {
    || rfd::FileDialog::new().pick_files()
}

#[cfg(test)]
fn picker() -> impl FnOnce() -> Option<Vec<PathBuf>> + Send {
    let picked = PICKED.with(|p| p.borrow_mut().take());
    move || picked
}

/// Runs `job` (dialog, decode, re-encode: all blocking) on a thread of its own with the open chat's limit.
/// Not on a worker lane: a 500 MiB upload in front of it would hold the panel back.
fn start(app: &mut MimirDesktop, ctx: &Context, job: impl FnOnce(&MediaSettings, u64) -> Batch + Send + 'static) {
    let Some(chat) = &app.state.open_chat else { return };
    let limit = if chat.key.is_group() { stage::MAX_GROUP_FILE } else { stage::MAX_FILE };
    let media = app.settings.media.clone();
    let (tx, rx) = mpsc::channel();
    app.state.composer.staging = Some(rx); // a newer run replaces an older one
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let batch = job(&media, limit);
        if batch.is_empty() {
            return; // the picker was cancelled
        }
        // Nobody waits any more (chat switched, panel cleared, newer run): our temp files go.
        if let Err(back) = tx.send(batch) {
            back.0.into_iter().flatten().for_each(stage::discard);
        }
        ctx.request_repaint();
    });
}

/// 📎: the system file picker, several files allowed.
pub fn pick(app: &mut MimirDesktop, ctx: &Context) {
    let pick = picker();
    start(app, ctx, move |media, limit| pick().unwrap_or_default().iter().map(|p| stage::stage(p, media, limit)).collect());
}

/// Picks up this frame's dropped files and pasted picture, and a finished staging run.
pub fn take_input(app: &mut MimirDesktop, ctx: &Context) {
    match app.state.composer.staging.as_ref().map(|rx| rx.try_recv()) {
        Some(Ok(batch)) => {
            app.state.composer.staging = None;
            apply(app, batch);
        }
        Some(Err(TryRecvError::Disconnected)) => app.state.composer.staging = None,
        _ => {}
    }
    let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
    if !dropped.is_empty() {
        start(app, ctx, move |media, limit| dropped.iter().map(|p| stage::stage(p, media, limit)).collect());
    }
    // egui sends PasteImage only when the clipboard holds no text, so a text paste still goes to the field.
    // ponytail: egui-winit's own arboard reads the clipboard (no arboard of ours, no clipboard trait); files copied
    // in Explorer are a file list, not a picture, and are not pasted.
    let pasted = ctx.input(|i| i.events.iter().find_map(|e| match e {
        egui::Event::PasteImage(img) => Some(img.clone()),
        _ => None,
    }));
    if let Some(img) = pasted {
        start(app, ctx, move |media, limit| vec![stage::stage_pasted(&img, media, limit)]);
    }
}

/// The first good file goes in the panel, the rest wait behind it (sent after it, P3-R2); the first failure is shown.
fn apply(app: &mut MimirDesktop, batch: Batch) {
    let lang = app.lang;
    let (mut ok, mut error) = (vec![], None);
    for r in batch {
        match r {
            Ok(s) => ok.push(s),
            Err(e) => {
                error.get_or_insert_with(|| stage::error_text(&e, lang));
            }
        }
    }
    if ok.iter().any(|s| s.resized) {
        app.notice_text(tr(lang, K::TooBigPictureResizing).into());
    }
    let c = &mut app.state.composer;
    if !ok.is_empty() {
        c.clear_attachments();
        let mut ok = ok.into_iter();
        c.attachment = ok.next();
        c.more = ok.collect();
    }
    c.attach_error = error;
}

/// The panel above the input: preview, name, size and ✕; or the staging error with ✕.
pub fn show(app: &mut MimirDesktop, ui: &mut Ui, p: &Palette) {
    let lang = app.lang;
    let (c, thumbs) = (&app.state.composer, &mut app.state.thumbs);
    if c.attachment.is_none() && c.attach_error.is_none() {
        return;
    }
    let mut cancel = false;
    ui.horizontal(|ui| {
        let text_w = (ui.available_width() - PREVIEW - 24. - 2. * ui.spacing().item_spacing.x).max(0.);
        if let Some(s) = &c.attachment {
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(PREVIEW), Sense::hover());
            ui.painter().rect_filled(rect, 4, p.file_icon);
            match (s.msg_type == 1).then(|| thumbs.get_sized(ui.ctx(), &s.path, PREVIEW_PX)) {
                // Centre square of the picture (Android centerCrop).
                Some(Thumb::Ready(tex, _)) => {
                    let [w, h] = tex.size().map(|v| v as f32);
                    let e = w.min(h);
                    let uv = Rect::from_min_max(pos2((w - e) / 2. / w, (h - e) / 2. / h), pos2((w + e) / 2. / w, (h + e) / 2. / h));
                    Image::new(&tex).uv(uv).corner_radius(4).paint_at(ui, rect);
                }
                _ => document_icon(ui.painter(), rect.center(), p.text),
            }
            ui.vertical(|ui| {
                ui.set_width(text_w);
                ui.add_space(4.);
                // ponytail: egui ellipsizes at the end, Android in the middle.
                let mut job = LayoutJob::simple(s.original_name.clone(), FontId::new(14., FontFamily::Name("bold".into())), p.text, text_w);
                job.wrap.max_rows = 2;
                job.wrap.break_anywhere = true;
                ui.add(Label::new(job));
                let mut size = format_size(s.size as i64, lang);
                if !c.more.is_empty() {
                    size = format!("{size}   +{}", c.more.len());
                }
                ui.label(egui::RichText::new(size).size(12.).color(p.text.gamma_multiply(0.7)));
                if let Some(err) = &c.attach_error {
                    ui.label(egui::RichText::new(err).size(12.).color(p.danger));
                }
            });
        } else if let Some(err) = &c.attach_error {
            ui.allocate_ui(vec2(text_w + PREVIEW, 24.), |ui| ui.add(Label::new(egui::RichText::new(err).size(13.).color(p.danger)).wrap()));
        }
        ui.with_layout(Layout::right_to_left(Align::Min), |ui| cancel = close_button(ui, tr(lang, K::Cancel), p));
    });
    ui.add_space(6.);
    if cancel {
        app.state.composer.clear_attachments();
    }
}

/// 24 px ✕ (Android attachment_cancel).
fn close_button(ui: &mut Ui, label: &str, p: &Palette) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(24.), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, label));
    let colour = if resp.hovered() { p.text } else { p.text_dim };
    let (c, d) = (rect.center(), 5.);
    for (a, b) in [(vec2(-d, -d), vec2(d, d)), (vec2(-d, d), vec2(d, -d))] {
        ui.painter().line_segment([c + a, c + b], Stroke::new(1.6, colour));
    }
    resp.clicked()
}

/// The 📎 button left of the input: a tinted paperclip like Android's attach_button.
pub fn clip_button(ui: &mut Ui, size: f32, label: &str, enabled: bool, p: &Palette) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), if enabled { Sense::click() } else { Sense::hover() });
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, label));
    let colour = match () {
        _ if !enabled => p.text_dim.gamma_multiply(0.5),
        _ if resp.hovered() => p.accent,
        _ => p.text_dim,
    };
    paperclip(ui.painter(), rect.center(), colour);
    resp.on_hover_text(label).clicked()
}

/// Two nested U loops, tilted 45° (ic_attach_file), 24 px class.
fn paperclip(painter: &Painter, c: Pos2, colour: Color32) {
    let mut pts = vec![vec2(1.5, -4.)];
    let mut arc = |cx: f32, cy: f32, r: f32, from: f32| pts.extend((0..=8).map(|i| vec2(cx, cy) + r * Vec2::angled(from + PI * i as f32 / 8.)));
    arc(0., 4., 1.5, 0.); // inner bottom
    arc(1.5, -6., 3., PI); // over the top, to the outer side
    arc(0., 5., 4.5, 0.); // outer bottom
    pts.push(vec2(-4.5, -3.));
    let rot = Rot2::from_angle(FRAC_PI_4);
    painter.add(Shape::line(pts.into_iter().map(|v| c + rot * v).collect(), Stroke::new(1.6, colour)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{settings::Settings, test_support::test_harness};

    #[test]
    fn shrunk_picture_says_so() {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        let s = Staged { path: "x.jpg".into(), original_name: "x.jpg".into(), mime: "image/jpeg".into(), msg_type: 1, size: 1, temp: false, resized: false };
        apply(h.state_mut(), vec![Ok(s.clone())]);
        assert_eq!(h.state().state.status, None, "an untouched file: no notice");
        apply(h.state_mut(), vec![Ok(Staged { resized: true, ..s })]);
        let app = h.state();
        assert_eq!(app.state.status.as_ref().map(|s| s.0.as_str()), Some("Картинка больше 5 мегабайт, ужимаем"));
        assert!(app.state.status_ok);
        assert!(app.state.composer.attachment.as_ref().is_some_and(|a| a.resized));
    }
}
