//! The attachment part of a message bubble: image thumbnail, file panel, voice player, video tile, download row.
use crate::{
    i18n::{size_units, tr, Key, Lang},
    media::{ffmpeg::{Poster, Posters}, player::Player, thumbs::{Thumb, ThumbCache}},
    model::{
        attachment::{is_present, kind, local_path, AttachmentKind, AttachmentMeta, Transfer},
        message::MsgBody,
    },
    ui::{message_menu::Action, theme::Palette, voice_bubble::{self, play_pause, Voice}},
};
use eframe::egui::{
    pos2, text::LayoutJob, vec2, Color32, CursorIcon, FontFamily, FontId, Galley, Id, Image, Painter, Pos2, Rect, Response,
    Role, Sense, Shape, Stroke, StrokeKind, TextFormat, TextureHandle, Ui, Vec2, WidgetInfo,
};
use std::{f32::consts::{FRAC_PI_2, TAU}, path::{Path, PathBuf}, sync::Arc};

/// Picture panel width and the picture's max height (message_incoming_layout picture_panel / picture).
const IMAGE_W: f32 = 240.;
const IMAGE_MAX_H: f32 = 360.;
/// Margin above and below the picture.
const IMAGE_PAD: f32 = 4.;
const VIDEO: f32 = 220.;
const FILE_MAX_W: f32 = 300.;
const ICON: f32 = 40.;

enum Block {
    /// A picture fitted into 240×360; `None` while it decodes (a neutral placeholder).
    Image { tex: Option<TextureHandle>, img: Vec2 },
    /// The poster frame (none: the neutral tile) and the clip's length in ms.
    Video { poster: Option<TextureHandle>, ms: Option<u64> },
    Voice(Voice),
    /// File panel: icon box, bold name, size line. `open`: the file is on disk (a click opens it), else a click
    /// downloads it; `progress`: a transfer runs (ring instead of the icon); `audio`: an audio file on disk whose icon
    /// box plays it inline (Some(playing now)).
    File { name: Arc<Galley>, size: Arc<Galley>, label: String, progress: Option<f32>, open: bool, audio: Option<bool> },
    /// A call record: the file panel's look with a phone icon; nothing to click.
    Call { name: Arc<Galley>, size: Arc<Galley>, label: String },
}

pub struct Attachment {
    block: Block,
    pub size: Vec2,
    /// The file, when it is complete on disk (not while a download runs into it).
    pub file: Option<PathBuf>,
}

/// The attachment a body carries, if any.
pub fn meta_of(body: &MsgBody) -> Option<&AttachmentMeta> {
    match body {
        MsgBody::Image(a) | MsgBody::File(a) | MsgBody::Voice(a) | MsgBody::VideoMessage(a) => Some(a),
        _ => None,
    }
}

/// «999 Б», «1.5 КБ», «5.0 МБ», «2.0 ГБ».
pub fn format_size(bytes: i64, lang: Lang) -> String {
    let units = size_units(lang);
    if bytes < 1024 {
        return format!("{bytes} {}", units[0]);
    }
    let (mut v, mut i) = (bytes as f64 / 1024., 1);
    while v >= 1024. && i < units.len() - 1 {
        v /= 1024.;
        i += 1;
    }
    format!("{v:.1} {}", units[i])
}

/// `src` scaled to fit `bounds`, aspect kept.
pub fn fit(src: Vec2, bounds: Vec2) -> Vec2 {
    let src = src.max(Vec2::splat(1.));
    src * (bounds.x / src.x).min(bounds.y / src.y)
}

/// Lays out the attachment of row `guid` of `msg_type`.
// ponytail: blocks keep their Android sizes (240 / 220 / ≤ 300 px); a chat narrower than ~300 px lets them overflow.
#[allow(clippy::too_many_arguments)]
pub fn prepare(ui: &Ui, guid: i64, msg_type: i32, meta: &AttachmentMeta, files_dir: &Path, transfer: Transfer, thumbs: &mut ThumbCache, posters: &mut Posters, ffmpeg: Option<&Path>, player: &Player, lang: Lang, p: &Palette, wrap: f32) -> Attachment {
    let k = kind(msg_type, meta);
    let downloading = matches!(transfer, Transfer::Downloading { .. });
    // A download in progress may already be writing the file: it counts as absent until it ends.
    let path = local_path(files_dir, meta).filter(|_| !downloading && is_present(files_dir, meta));
    match (k, &path) {
        (AttachmentKind::Image, Some(file)) => match thumbs.get(ui.ctx(), file) {
            Thumb::Ready(tex, [w, h]) => return picture(Some(tex), fit(vec2(w as f32, h as f32), vec2(IMAGE_W, IMAGE_MAX_H)), path),
            Thumb::Loading => return picture(None, vec2(IMAGE_W, IMAGE_W * 2. / 3.), path),
            Thumb::Failed => {} // not a picture after all: the file bubble
        },
        (AttachmentKind::Video, Some(file)) => {
            let (poster, ms) = match posters.get(ui.ctx(), ffmpeg, file, &files_dir.join(".thumbs")) {
                Poster::Ready(jpg, ms) => (if let Thumb::Ready(tex, _) = thumbs.get(ui.ctx(), &jpg) { Some(tex) } else { None }, ms),
                _ => (None, None),
            };
            return Attachment { block: Block::Video { poster, ms }, size: Vec2::splat(VIDEO), file: path };
        }
        // Our own voice message shows the file panel's upload ring until it is sent.
        (AttachmentKind::Voice, Some(file)) if !matches!(transfer, Transfer::Uploading { .. }) => {
            let voice = voice_bubble::prepare(ui, guid, player.len_ms(guid, file), player.state(), p);
            return Attachment { block: Block::Voice(voice), size: voice_bubble::SIZE, file: path };
        }
        _ => {}
    }
    let progress = match transfer {
        Transfer::Downloading { done, total } | Transfer::Uploading { done, total } => Some((done, if total > 0 { total } else { meta.size })),
        Transfer::Idle => None,
    };
    let label = match (k, path.is_some()) {
        // A voice message has no name to show.
        (AttachmentKind::Image | AttachmentKind::Voice, false) => tr(lang, if downloading { Key::DownloadingFile } else { Key::DownloadFile }).to_string(),
        (AttachmentKind::Voice, true) => tr(lang, Key::VoiceMessage).to_string(),
        (AttachmentKind::Video, false) => tr(lang, Key::Video).to_string(),
        _ => meta.display_name().to_string(),
    };
    let size_text = match progress {
        Some((done, total)) => format!("{} / {}", format_size(done, lang), format_size(total, lang)),
        None if meta.size > 0 => format_size(meta.size, lang),
        None => String::new(),
    };
    let (name, size, panel) = panel_text(ui, &label, size_text, p, wrap);
    let progress = progress.map(|(done, total)| if total > 0 { (done as f32 / total as f32).clamp(0., 1.) } else { 0. });
    let audio = (k == AttachmentKind::Audio && path.is_some() && progress.is_none()).then(|| player.state().is_playing(guid));
    Attachment { block: Block::File { name, size, label, progress, open: path.is_some(), audio }, size: panel, file: path }
}

/// The file panel's bold name (up to two rows) and size line, and the panel's size with its icon box.
fn panel_text(ui: &Ui, label: &str, size_text: String, p: &Palette, wrap: f32) -> (Arc<Galley>, Arc<Galley>, Vec2) {
    let text_w = FILE_MAX_W.min(wrap) - ICON - 8.;
    let mut job = LayoutJob::default();
    job.wrap = eframe::egui::text::TextWrapping { max_width: text_w, max_rows: 2, break_anywhere: false, overflow_character: Some('…') };
    // ponytail: egui ellipsizes at the end, Android in the middle.
    job.append(label, 0., TextFormat { font_id: FontId::new(15., FontFamily::Name("bold".into())), color: p.text, ..Default::default() });
    let name = ui.fonts_mut(|f| f.layout_job(job));
    let size = ui.painter().layout(size_text, FontId::proportional(13.), p.text.gamma_multiply(0.7), text_w);
    let h = ICON.max(name.size().y + 2. + size.size().y) + 8.;
    let w = ICON + 8. + name.size().x.max(size.size().x);
    (name, size, vec2(w, h))
}

/// A call record (Android MessageAdapter type 2): the file panel with a phone icon, the kind of call and its length.
pub fn call_record(ui: &Ui, label: &str, duration: String, p: &Palette, wrap: f32) -> Attachment {
    let spoken = if duration.is_empty() { label.to_string() } else { format!("{label}, {duration}") };
    let (name, size, panel) = panel_text(ui, label, duration, p, wrap);
    Attachment { block: Block::Call { name, size, label: spoken }, size: panel, file: None }
}

fn picture(tex: Option<TextureHandle>, img: Vec2, file: Option<PathBuf>) -> Attachment {
    Attachment { size: vec2(IMAGE_W, img.y + 2. * IMAGE_PAD), block: Block::Image { tex, img }, file }
}

impl Attachment {
    /// Paints the block at `min` and handles its clicks; the response is for the caller's context menu.
    pub fn show(self, ui: &Ui, min: Pos2, id: Id, p: &Palette, lang: Lang) -> (Response, Option<Action>) {
        let rect = Rect::from_min_size(min, self.size);
        let painter = ui.painter();
        let (resp, action) = match self.block {
            Block::Image { tex, img } => {
                let r = Rect::from_min_size(pos2(min.x + (IMAGE_W - img.x) / 2., min.y + IMAGE_PAD), img);
                let resp = ui.interact(r, id, Sense::click());
                resp.widget_info(|| WidgetInfo::labeled(Role::Image, true, tr(lang, Key::Image)));
                let view = match &tex {
                    Some(tex) => {
                        Image::new((tex.id(), img)).corner_radius(2).paint_at(ui, r);
                        resp.clicked().then_some(Action::View)
                    }
                    None => {
                        painter.rect_filled(r, 2, Color32::from_gray(128).gamma_multiply(0.25));
                        None
                    }
                };
                (resp, view)
            }
            Block::Video { poster, ms } => {
                let resp = ui.interact(rect, id, Sense::click());
                resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, tr(lang, Key::Video)));
                match &poster {
                    // The centre square of the frame (Android centerCrop).
                    Some(tex) => {
                        let [w, h] = tex.size().map(|v| v as f32);
                        let e = w.min(h);
                        let uv = Rect::from_min_max(pos2((w - e) / 2. / w, (h - e) / 2. / h), pos2((w + e) / 2. / w, (h + e) / 2. / h));
                        Image::new((tex.id(), rect.size())).uv(uv).corner_radius(10).paint_at(ui, rect);
                    }
                    None => {
                        painter.rect_filled(rect, 10, Color32::from_gray(0x20));
                    }
                }
                play_badge(painter, rect.center());
                if let Some(ms) = ms {
                    let text = painter.layout_no_wrap(voice_bubble::clock(ms), FontId::proportional(12.), Color32::WHITE);
                    let pill = Rect::from_min_size(pos2(rect.left() + 12., rect.bottom() - 12. - text.size().y - 4.), text.size() + vec2(12., 4.));
                    painter.rect_filled(pill, 4, Color32::from_black_alpha(140));
                    painter.galley(pill.min + vec2(6., 2.), text, Color32::WHITE);
                }
                let open = resp.clicked().then_some(Action::Open);
                (resp, open)
            }
            Block::Voice(v) => v.show(ui, min, id, p, lang),
            Block::Call { name, size, label } => {
                let icon = Rect::from_min_size(pos2(min.x, rect.center().y - ICON / 2.), Vec2::splat(ICON));
                let resp = ui.interact(rect, id, Sense::click());
                resp.widget_info(|| WidgetInfo::labeled(Role::Label, true, &label));
                painter.rect_filled(icon, 3, p.file_icon);
                crate::ui::call::phone_icon(ui, icon.center(), 20., p.text, false);
                let text_h = name.size().y + 2. + size.size().y;
                let text_min = pos2(icon.right() + 8., rect.center().y - text_h / 2.);
                painter.galley(text_min, name, p.text);
                painter.galley(text_min + vec2(0., text_h - size.size().y), size, p.text);
                (resp, None)
            }
            Block::File { name, size, label, progress, open, audio } => {
                let icon = Rect::from_min_size(pos2(min.x, rect.center().y - ICON / 2.), Vec2::splat(ICON));
                if progress.is_some() {
                    // Registered first: the panel's click widget below stays on top.
                    ui.interact(icon, id.with("progress"), Sense::hover()).widget_info(|| WidgetInfo::new(Role::ProgressIndicator));
                }
                let resp = ui.interact(rect, id, Sense::click());
                resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, &label));
                painter.rect_filled(icon, 3, p.file_icon);
                match progress {
                    Some(f) => ring(painter, icon.center(), f, p),
                    None if let Some(playing) = audio => play_pause(painter, icon.center(), playing, p.text),
                    None if open => document_icon(painter, icon.center(), p.text),
                    None => download_icon(painter, icon.center(), p.text),
                }
                let text_h = name.size().y + 2. + size.size().y;
                let text_min = pos2(icon.right() + 8., rect.center().y - text_h / 2.);
                let size_min = text_min + vec2(0., name.size().y + 2.);
                painter.galley(text_min, name, p.text);
                painter.galley(size_min, size, p.text);
                // A click on a running download asks again: one that never ends would otherwise leave the row
                // stuck; the core dedupes P2P requests by guid.
                // ponytail: on a group download that is really running, the retry queues a second fetch behind it.
                let mut action = match (resp.clicked(), open) {
                    (true, true) => Some(Action::Open),
                    (true, false) => Some(Action::Download),
                    _ => None,
                };
                let mut resp = resp.on_hover_cursor(CursorIcon::PointingHand);
                // Audio: the icon box plays inline, the rest of the panel opens the file. Registered last: on top.
                if let Some(playing) = audio {
                    let play = ui.interact(icon, id.with("play"), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
                    play.widget_info(|| WidgetInfo::labeled(Role::Button, true, tr(lang, if playing { Key::Pause } else { Key::Play })));
                    if play.clicked() {
                        action = Some(Action::Play);
                    }
                    resp = resp.union(play);
                }
                (resp, action)
            }
        };
        (resp, action)
    }
}

/// Outline document (ic_file_document_outline), 24 px class.
pub(crate) fn document_icon(painter: &Painter, c: Pos2, color: Color32) {
    let s = Stroke::new(1.5, color);
    let r = Rect::from_center_size(c, vec2(14., 18.));
    painter.rect_stroke(r, 2, s, StrokeKind::Middle);
    for dy in [-2., 2.] {
        painter.line_segment([pos2(r.left() + 3., c.y + dy), pos2(r.right() - 3., c.y + dy)], s);
    }
}

/// Arrow into a tray (ic_folder_download_outline).
fn download_icon(painter: &Painter, c: Pos2, color: Color32) {
    let s = Stroke::new(1.8, color);
    painter.line_segment([c + vec2(0., -9.), c + vec2(0., 4.)], s);
    painter.add(Shape::line(vec![c + vec2(-5., -1.), c + vec2(0., 4.), c + vec2(5., -1.)], s));
    painter.line_segment([c + vec2(-8., 9.), c + vec2(8., 9.)], s);
}

/// 36 px progress ring, 3 px track (CircularProgressIndicator).
fn ring(painter: &Painter, c: Pos2, frac: f32, p: &Palette) {
    painter.circle_stroke(c, 16.5, Stroke::new(3., p.text.gamma_multiply(0.2)));
    if frac > 0. {
        let n = (frac * 48.).ceil().max(2.) as usize;
        let pts = (0..=n).map(|i| c + 16.5 * Vec2::angled(-FRAC_PI_2 + frac * TAU * i as f32 / n as f32)).collect();
        painter.add(Shape::line(pts, Stroke::new(3., p.accent)));
    }
}

/// 56 px play badge.
fn play_badge(painter: &Painter, c: Pos2) {
    painter.circle_filled(c, 28., Color32::from_black_alpha(140));
    let tri = vec![c + vec2(-7., -11.), c + vec2(-7., 11.), c + vec2(12., 0.)];
    painter.add(Shape::convex_polygon(tri, Color32::WHITE, Stroke::NONE));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::MimirDesktop,
        model::{attachment::{AttachmentMeta, Transfer}, chat_key::ChatKey, chat_list::build_chat_list},
        settings::Settings,
        test_support::test_harness,
    };
    use eframe::egui::{self, pos2, CornerRadius, FontFamily, OutputCommand, Rect, Role, Shape};
    use egui_kittest::{kittest::Queryable as _, Harness};
    use std::{path::PathBuf, time::Duration};

    const GUID: i64 = 21;
    const MIB: i64 = 1024 * 1024;

    fn encoded(w: u32, h: u32, f: image::ImageFormat) -> Vec<u8> {
        let mut out = std::io::Cursor::new(vec![]);
        image::RgbImage::from_pixel(w, h, image::Rgb([40, 120, 200])).write_to(&mut out, f).unwrap();
        out.into_inner()
    }

    fn image_meta(caption: &str, size: i64) -> String {
        format!(r#"{{"name":"Ab3dEf7hIj9kLm1n.jpg","originalName":"photo.jpg","size":{size},"hash":"9f","mimeType":"image/jpeg","text":"{caption}"}}"#)
    }

    /// Contact «Аня»'s chat with one incoming row `(msg_type, meta)`; `file` is first written to files_dir/<name>.
    fn row(msg_type: i32, meta: &str, file: Option<&[u8]>) -> (Harness<'static, MimirDesktop>, PathBuf) {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        let core = h.state().core.clone();
        let c = core.add_contact(vec![7; 32], "Аня".into()).unwrap();
        let name = AttachmentMeta::parse(meta.as_bytes()).unwrap().name;
        let path = h.state().paths.files_dir.join(name);
        if let Some(bytes) = file {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, bytes).unwrap();
        }
        let now = time::OffsetDateTime::now_utc().unix_timestamp() * 1000;
        core.add_local_message(c, GUID, 0, true, true, now - 1000, 0, msg_type, meta.as_bytes().to_vec()).unwrap();
        core.mark_all_read(c).unwrap();
        h.state_mut().state.chats = build_chat_list(&core, Lang::Ru);
        h.state_mut().state.selected = Some(ChatKey::Contact(c));
        settle(&mut h);
        (h, path)
    }

    /// Runs frames until no image is still decoding.
    fn settle(h: &mut Harness<'static, MimirDesktop>) {
        for _ in 0..500 {
            h.step(); // not run(): the viewer's spinner repaints while it waits
            if !h.state().state.thumbs.loading() {
                h.run();
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("an image never finished decoding");
    }

    /// Rects of the nodes with `role` and `label` in the chat area (the chat list on the left shows previews).
    fn in_chat(h: &Harness<'static, MimirDesktop>, role: Role, label: &str) -> Vec<Rect> {
        h.query_all_by_role_and_label(role, label).map(|n| n.rect()).filter(|r| r.left() > 250.).collect()
    }

    fn painted_text(h: &Harness<'static, MimirDesktop>, pred: impl Fn(&str) -> bool) -> Vec<std::sync::Arc<egui::Galley>> {
        h.output().shapes.iter().filter_map(|cs| match &cs.shape {
            Shape::Text(t) if t.pos.x > 250. && pred(t.galley.text()) => Some(t.galley.clone()),
            _ => None,
        }).collect()
    }

    fn browser_opened(h: &Harness<'static, MimirDesktop>) -> bool {
        h.output().platform_output.commands.iter().any(|c| matches!(c, OutputCommand::OpenUrl(_)))
    }

    #[test]
    fn format_size_localized() {
        assert_eq!(format_size(999, Lang::Ru), "999 Б");
        assert_eq!(format_size(999, Lang::En), "999 B");
        assert_eq!(format_size(1536, Lang::Ru), "1.5 КБ");
        assert_eq!(format_size(5 * MIB, Lang::Ru), "5.0 МБ");
        assert_eq!(format_size(2048 * MIB, Lang::Ru), "2.0 ГБ");
        assert_eq!(format_size(2048 * MIB, Lang::En), "2.0 GB");
    }

    #[test]
    fn image_bubble_shows_thumbnail() {
        let (h, _) = row(1, &image_meta("", 12345), Some(&encoded(1200, 800, image::ImageFormat::Jpeg)));
        let img = in_chat(&h, Role::Image, "Изображение");
        assert_eq!(img.len(), 1, "one picture");
        assert_eq!((img[0].width(), img[0].height()), (240., 160.), "240 px wide, aspect kept");
        assert!(painted_text(&h, |t| t.contains("photo.jpg")).is_empty(), "no file name under the picture");
        // A tall picture is capped at 360 px.
        let (h, _) = row(1, &image_meta("", 1), Some(&encoded(400, 1200, image::ImageFormat::Png)));
        let img = in_chat(&h, Role::Image, "Изображение")[0];
        assert_eq!((img.width(), img.height()), (120., 360.));
        // A caption shows below the picture.
        let (h, _) = row(1, &image_meta("море", 1), Some(&encoded(1200, 800, image::ImageFormat::Png)));
        let img = in_chat(&h, Role::Image, "Изображение")[0];
        let cap = h.output().shapes.iter().find_map(|cs| match &cs.shape {
            Shape::Text(t) if t.pos.x > 250. && t.galley.text() == "море" => Some(t.pos),
            _ => None,
        });
        assert!(cap.expect("caption painted").y >= img.bottom(), "caption {cap:?} below {img:?}");
    }

    #[test]
    fn missing_file_shows_download_row() {
        let (mut h, path) = row(1, &image_meta("", 3 * MIB), None);
        assert!(!path.exists());
        assert!(in_chat(&h, Role::Image, "Изображение").is_empty());
        assert_eq!(in_chat(&h, Role::Button, "Получить").len(), 1);
        assert!(!painted_text(&h, |t| t == "3.0 МБ").is_empty(), "size line");
        h.get_by_role_and_label(Role::Button, "Получить").click();
        h.run();
        assert_eq!(h.state().state.transfers.get(&GUID), Some(&Transfer::Downloading { done: 0, total: 3 * MIB }), "marked at once");
        assert_eq!(in_chat(&h, Role::Button, "Получение...").len(), 1);
        assert!(h.query_by_role(Role::ProgressIndicator).is_some(), "progress ring instead of the icon");
        assert!(!painted_text(&h, |t| t == "0 Б / 3.0 МБ").is_empty());
        h.state_mut().state.transfers.insert(GUID, Transfer::Downloading { done: 1536, total: 3 * MIB });
        h.run();
        assert!(!painted_text(&h, |t| t == "1.5 КБ / 3.0 МБ").is_empty(), "X / Y");
    }

    #[test]
    fn corrupt_image_falls_back_to_file_bubble() {
        let (h, _) = row(1, &image_meta("", 12345), Some(b"definitely not a jpeg"));
        assert!(in_chat(&h, Role::Image, "Изображение").is_empty());
        assert_eq!(in_chat(&h, Role::Button, "photo.jpg").len(), 1, "file bubble with the name");
        assert!(!painted_text(&h, |t| t == "12.1 КБ").is_empty(), "and the size");
    }

    #[test]
    fn file_bubble_layout() {
        let name = "Годовой отчёт отдела продаж за две тысячи двадцать шестой год, окончательная версия.pdf";
        let meta = format!(r#"{{"name":"Ab3dEf7hIj9kLm1n.pdf","originalName":"{name}","size":5,"hash":"ab","mimeType":"application/pdf"}}"#);
        let (mut h, path) = row(3, &meta, Some(b"%PDF-"));
        for dark in [true, false] {
            h.ctx.set_theme(if dark { egui::Theme::Dark } else { egui::Theme::Light });
            h.run();
            let icon = h.output().shapes.iter().find_map(|cs| match &cs.shape {
                Shape::Rect(r) if r.rect.left() > 250. && r.rect.size() == egui::vec2(40., 40.) => Some((r.fill, r.corner_radius)),
                _ => None,
            });
            assert_eq!(icon, Some((crate::ui::theme::palette(dark).file_icon, CornerRadius::same(3))), "40x40 icon box, dark={dark}");
        }
        let names = painted_text(&h, |t| t.starts_with("Годовой"));
        assert_eq!(names.len(), 1);
        assert!(names[0].rows.len() <= 2 && names[0].rows.len() > 1, "long name wraps to at most 2 lines");
        assert_eq!(names[0].job.sections[0].format.font_id.family, FontFamily::Name("bold".into()));
        assert!(!painted_text(&h, |t| t == "5 Б").is_empty(), "size line");
        // Click opens the file with the OS default app, not the browser.
        h.get_by_role_and_label(Role::Button, name).click();
        h.step();
        assert!(!browser_opened(&h));
        assert_eq!(crate::media::take_opened(), vec![crate::media::open_command(&path)]);
        h.run();
        // The menu has Save and Delete file; Delete file removes only the bytes.
        h.get_by_role_and_label(Role::Button, name).click_secondary();
        h.run();
        assert!(h.query_by_label_contains("Сохранить").is_some());
        h.get_by_label_contains("Удалить файл").click();
        let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while path.exists() && std::time::Instant::now() < end {
            std::thread::sleep(std::time::Duration::from_millis(20));
            h.run();
        }
        h.run();
        assert!(!path.exists(), "file deleted (by the worker)");
        assert_eq!(h.state().state.open_chat.as_ref().unwrap().timeline.msgs.len(), 1, "message kept");
        h.get_by_role_and_label(Role::Button, name).click();
        h.run();
        assert!(matches!(h.state().state.transfers.get(&GUID), Some(Transfer::Downloading { .. })), "now a download row");
    }

    #[test]
    fn failed_file_delete_is_reported() {
        let meta = r#"{"name":"Ab3dEf7hIj9kLm1n.pdf","originalName":"a.pdf","size":5,"hash":"ab","mimeType":"application/pdf"}"#;
        let (mut h, path) = row(3, meta, Some(b"%PDF-"));
        h.get_by_role_and_label(Role::Button, "a.pdf").click_secondary();
        h.run();
        // Something replaced the file so that it cannot be removed (on Windows: open in another app).
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        h.get_by_label_contains("Удалить файл").click();
        let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while h.state().state.status.is_none() && std::time::Instant::now() < end {
            std::thread::sleep(std::time::Duration::from_millis(20));
            h.run();
        }
        assert!(path.exists());
        let (text, _) = h.state().state.status.clone().expect("error in the status line");
        assert!(text.starts_with("Удалить файл: ") && !h.state().state.status_ok, "{text}");
    }

    #[test]
    fn download_in_progress_wins_over_a_partial_file() {
        // A group download decrypts straight into the destination: the file exists before it is complete.
        let (mut h, _) = row(1, &image_meta("", 3 * MIB), Some(&encoded(1200, 800, image::ImageFormat::Png)));
        h.state_mut().state.transfers.insert(GUID, Transfer::Downloading { done: 1536, total: 3 * MIB });
        h.run();
        assert!(in_chat(&h, Role::Image, "Изображение").is_empty(), "no picture yet");
        assert!(h.query_by_role(Role::ProgressIndicator).is_some());
        assert!(!painted_text(&h, |t| t == "1.5 КБ / 3.0 МБ").is_empty());
        h.get_by_role_and_label(Role::Button, "Получение...").click_secondary();
        h.run();
        assert!(h.query_by_label_contains("Удалить файл").is_none(), "no file actions while it downloads");
    }

    #[test]
    fn stuck_download_can_be_retried() {
        // A transfer that never ends (peer gone mid-way, request ignored) must not leave the row dead.
        let (mut h, _) = row(1, &image_meta("", 3 * MIB), None);
        h.state_mut().state.transfers.insert(GUID, Transfer::Downloading { done: 1536, total: 3 * MIB });
        h.run();
        h.get_by_role_and_label(Role::Button, "Получение...").click();
        h.run();
        assert_eq!(h.state().state.transfers.get(&GUID), Some(&Transfer::Downloading { done: 0, total: 3 * MIB }), "requested again");
    }

    #[test]
    fn video_tile() {
        let meta = r#"{"name":"Vv1.mp4","originalName":"video_message.mp4","size":200000,"mimeType":"video/mp4"}"#;
        let (mut h, _) = row(4, meta, Some(b"mp4"));
        let tile = in_chat(&h, Role::Button, "Видеосообщение");
        assert_eq!(tile.len(), 1);
        assert_eq!(tile[0].size(), egui::vec2(220., 220.), "220 px square tile");
        h.get_by_role_and_label(Role::Button, "Видеосообщение").click();
        h.run();
        let opened = crate::media::take_opened();
        assert!(opened.len() == 1 && opened[0].1.last().is_some_and(|a| a.to_string_lossy().ends_with("Vv1.mp4")), "system player: {opened:?}");
        let (h, _) = row(4, meta, None);
        let rowr = in_chat(&h, Role::Button, "Видеосообщение");
        assert!(rowr.len() == 1 && rowr[0].height() < 100., "download row, not the tile: {rowr:?}");
        assert!(!painted_text(&h, |t| t == "195.3 КБ").is_empty());
    }

    #[test]
    fn video_tile_shows_poster_and_duration() {
        let meta = r#"{"name":"Vv1.mp4","originalName":"video_message.mp4","size":200000,"mimeType":"video/mp4"}"#;
        let (mut h, _) = row(4, meta, Some(b"mp4"));
        assert!(painted_text(&h, |t| t == "0:07").is_empty(), "no ffmpeg, no cache: neutral tile, no duration");
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("frame.jpg");
        std::fs::write(&src, encoded(64, 48, image::ImageFormat::Jpeg)).unwrap();
        h.state_mut().state.ffmpeg = Some(crate::media::ffmpeg::tests::fake_ffmpeg(tmp.path(), 0, "  Duration: 00:00:07.40, start: 0", Some(&src)));
        h.state_mut().state.posters = Default::default();
        let poster = h.state().paths.files_dir.join(".thumbs").join("Vv1.mp4.jpg");
        for _ in 0..300 {
            h.step();
            if h.state().state.thumbs.cached(&poster, crate::media::thumbs::THUMB_PX) && !h.state().state.thumbs.loading() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        h.run();
        assert!(poster.is_file(), "cached under files_dir/.thumbs");
        assert!(!painted_text(&h, |t| t == "0:07").is_empty(), "the duration on the tile");
        let tile = in_chat(&h, Role::Button, "Видеосообщение")[0];
        let textured = h.output().shapes.iter().any(|cs| matches!(&cs.shape, Shape::Rect(r) if r.fill_texture_id() != egui::TextureId::default() && r.rect == tile));
        assert!(textured, "the poster frame fills the tile");
    }

    const VOICE: &str = r#"{"name":"Vc1.m4a","originalName":"voice_message.m4a","size":9264,"mimeType":"audio/mp4"}"#;

    /// Runs frames until the player thread reports a state that passes `want`.
    fn wait_player(h: &mut Harness<'static, MimirDesktop>, want: impl Fn(crate::media::player::PlayState) -> bool) {
        for _ in 0..300 {
            h.step();
            if want(h.state().player.state()) {
                h.run();
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("player never reached the state: {:?}", h.state().player.state());
    }

    #[test]
    fn opened_chat_shows_the_last_row_whole() {
        // F7: a chat once opened away from the bottom (at its first unread) reopened at that old offset: egui keeps
        // each scroll area's state, and its stuck-to-bottom flag stayed off, so the last bubble was cut or not shown.
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        h.set_size(egui::vec2(1362., 833.));
        let core = h.state().core.clone();
        let c = core.add_contact(vec![7; 32], "Аня".into()).unwrap();
        let files = h.state().paths.files_dir.clone();
        std::fs::create_dir_all(&files).unwrap();
        std::fs::write(files.join("Ab3dEf7hIj9kLm1n.jpg"), encoded(1200, 800, image::ImageFormat::Jpeg)).unwrap();
        std::fs::write(files.join("Vc1.m4a"), std::fs::read(crate::media::player::tests::FIXTURE).unwrap()).unwrap();
        let now = time::OffsetDateTime::now_utc().unix_timestamp() * 1000;
        let add = |guid: i64, msg_type: i32, data: &str| {
            core.add_local_message(c, guid, 0, guid % 2 == 0, true, now - 100_000 + guid, 0, msg_type, data.as_bytes().to_vec()).unwrap()
        };
        let mut ids = vec![];
        for g in 1..=8 {
            ids.push(add(g, 0, &format!("строка {g}")));
        }
        add(9, 1, &image_meta("This is the view from the top", 12345));
        add(10, 3, VOICE);
        add(11, 0, "Deal. I'll bring coffee");
        core.mark_all_read(c).unwrap();
        core.set_message_read(c, ids[1], false).unwrap(); // the first open lands on it, near the top
        h.state_mut().state.chats = build_chat_list(&core, Lang::Ru);
        h.state_mut().state.selected = Some(ChatKey::Contact(c));
        settle(&mut h);
        assert!(!h.state().state.open_chat.as_ref().unwrap().timeline.at_bottom, "opened at the unread row");
        // Read now: every later open starts at the latest message.
        for _ in 0..2 {
            h.state_mut().state.selected = Some(ChatKey::SAVED);
            h.run();
            h.state_mut().state.selected = Some(ChatKey::Contact(c));
            settle(&mut h);
            let last = h.query_all_by_label("Deal. I'll bring coffee").map(|n| n.rect()).find(|r| r.left() > 250.);
            let last = last.expect("the last row is on screen");
            // The bubble as painted, and the clip of the chat's scroll area it was painted in.
            let clip = h.output().shapes.iter().find_map(|cs| match &cs.shape {
                Shape::Rect(r) if r.rect == last => Some(cs.clip_rect),
                _ => None,
            }).expect("the last bubble painted");
            assert!(last.bottom() <= clip.bottom() + 0.5 && last.top() >= clip.top(), "last bubble {last:?} cut by the chat area {clip:?}");
        }
    }

    #[test]
    fn voice_bubble_has_no_file_name() {
        let fixture = std::fs::read(crate::media::player::tests::FIXTURE).unwrap();
        let (mut h, _) = row(3, VOICE, Some(&fixture));
        // The length is probed off the UI thread: «--:--» until it is known.
        for _ in 0..300 {
            if !painted_text(&h, |t| t == "0:02").is_empty() {
                break;
            }
            assert!(!painted_text(&h, |t| t == "--:--").is_empty(), "placeholder while probing");
            std::thread::sleep(Duration::from_millis(10));
            h.step();
        }
        h.run();
        assert_eq!(in_chat(&h, Role::Button, "Играть").len(), 1, "▶");
        assert_eq!(h.query_all_by_role(Role::ProgressIndicator).filter(|n| n.rect().left() > 250.).count(), 1, "progress bar");
        assert!(!painted_text(&h, |t| t == "0:02").is_empty(), "duration");
        assert!(painted_text(&h, |t| t.contains("voice_message")).is_empty(), "no file name");
        assert!(h.query_by_label_contains("voice_message").is_none(), "not even in accessibility labels");
        // The bar of a message that is not loaded does not seek: the click plays from the start.
        h.get_by_role(Role::ProgressIndicator).click();
        wait_player(&mut h, |s| s.is_playing(GUID));
        assert_eq!(h.state().player.state().pos_ms, 0, "no seek");
        h.get_by_role_and_label(Role::Button, "Пауза").click();
        wait_player(&mut h, |s| s.guid == Some(GUID) && !s.playing);
        h.get_by_role_and_label(Role::Button, "Играть").click();
        wait_player(&mut h, |s| s.is_playing(GUID));
        assert_eq!(in_chat(&h, Role::Button, "Пауза").len(), 1, "⏸ while it plays");
        assert!(!painted_text(&h, |t| t == "0:00 / 0:02").is_empty(), "position / length");
        // A click in the middle of the bar seeks there.
        h.get_by_role(Role::ProgressIndicator).click();
        wait_player(&mut h, |s| s.pos_ms > 0);
        assert!(h.state().player.state().pos_ms.abs_diff(1000) < 100, "{:?}", h.state().player.state());
        h.get_by_role_and_label(Role::Button, "Пауза").click();
        wait_player(&mut h, |s| s.guid == Some(GUID) && !s.playing);
        assert_eq!(in_chat(&h, Role::Button, "Играть").len(), 1);
        assert!(crate::media::take_opened().is_empty(), "plays inline, no external player");
    }

    #[test]
    fn uploading_voice_shows_ring() {
        let fixture = std::fs::read(crate::media::player::tests::FIXTURE).unwrap();
        let (mut h, _) = row(3, VOICE, Some(&fixture));
        h.state_mut().state.transfers.insert(GUID, Transfer::Uploading { done: 4096, total: 9264 });
        h.run();
        assert!(h.query_by_role(Role::ProgressIndicator).is_some(), "upload ring");
        assert!(in_chat(&h, Role::Button, "Играть").is_empty(), "no player while it uploads");
        assert!(!painted_text(&h, |t| t == "4.0 КБ / 9.0 КБ").is_empty());
        assert!(painted_text(&h, |t| t.contains("voice_message")).is_empty(), "no file name");
        h.state_mut().state.transfers.remove(&GUID);
        h.run();
        assert_eq!(in_chat(&h, Role::Button, "Играть").len(), 1, "sent: the voice bubble");
    }

    #[test]
    fn audio_file_bubble_plays_inline() {
        let meta = r#"{"name":"Sg1.mp3","originalName":"song.mp3","size":5,"mimeType":"audio/mpeg"}"#;
        let (mut h, path) = row(3, meta, Some(b"ID3.."));
        assert_eq!(in_chat(&h, Role::Button, "song.mp3").len(), 1, "file bubble with the name");
        assert!(!painted_text(&h, |t| t == "5 Б").is_empty(), "and the size");
        h.get_by_role_and_label(Role::Button, "Играть").click();
        wait_player(&mut h, |s| s.is_playing(GUID));
        assert!(crate::media::take_opened().is_empty(), "the icon box plays");
        assert_eq!(in_chat(&h, Role::Button, "Пауза").len(), 1);
        h.get_by_role_and_label(Role::Button, "song.mp3").click();
        h.step();
        assert_eq!(crate::media::take_opened(), vec![crate::media::open_command(&path)], "the rest of the panel opens it");
    }

    #[test]
    fn disguised_program_is_revealed_not_run() {
        // The stored name is the sender's; «invoice.pdf» over Q.bat must not shell-execute.
        let meta = r#"{"name":"Q.bat","originalName":"invoice.pdf","size":5,"mimeType":"application/pdf"}"#;
        let (mut h, path) = row(3, meta, Some(b"@echo"));
        h.get_by_role_and_label(Role::Button, "invoice.pdf").click();
        h.run();
        let opened = crate::media::take_opened();
        assert_eq!(opened, vec![crate::media::open_command(&path)]);
        let (prog, args) = &opened[0];
        let shown = args.iter().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>().join(" ");
        if cfg!(windows) {
            assert_eq!((*prog, shown), ("explorer.exe", format!("/select,{}", path.display())), "shown in its folder");
        } else {
            assert_ne!(args, &vec![path.as_os_str().to_owned()], "never opened: {shown}");
        }
    }

    #[test]
    fn forward_resends_the_file() {
        // Forwarding a picture re-sends the file as an attachment, not its preview text.
        let (mut h, path) = row(1, &image_meta("подпись", 1), Some(&encoded(60, 40, image::ImageFormat::Png)));
        h.state_mut().state.core_started = true; // sends wait for the core
        h.get_by_role_and_label(Role::Image, "Изображение").click_secondary();
        h.run();
        h.get_by_label_contains("Переслать").click();
        h.run();
        h.query_all_by_role_and_label(Role::Button, "Избранное").last().unwrap().click();
        // The send job fails at once on the never-started core and its result may land mid-run (more repaints).
        h.run_ok();
        assert_eq!(h.state().state.pending_sends.get(&ChatKey::SAVED).copied(), Some(1), "an attachment send, not send_text");
        assert!(path.is_file(), "the original stays");
        // Without the file there is nothing to forward.
        let (mut h, _) = row(1, &image_meta("", 1), None);
        h.get_by_role_and_label(Role::Button, "Получить").click_secondary();
        h.run();
        assert!(h.query_by_label_contains("Переслать").is_none(), "no Forward for an absent file");
    }

    #[test]
    fn missing_voice_shows_download() {
        let (mut h, _) = row(3, VOICE, None);
        assert!(in_chat(&h, Role::Button, "Играть").is_empty());
        assert_eq!(in_chat(&h, Role::Button, "Получить").len(), 1, "download row");
        assert!(!painted_text(&h, |t| t == "9.0 КБ").is_empty(), "size line");
        assert!(painted_text(&h, |t| t.contains("voice_message")).is_empty(), "no file name");
        h.get_by_role_and_label(Role::Button, "Получить").click();
        h.run();
        assert!(matches!(h.state().state.transfers.get(&GUID), Some(Transfer::Downloading { .. })));
        assert_eq!(in_chat(&h, Role::Button, "Получение...").len(), 1);
    }

    #[test]
    fn image_viewer_opens_and_closes() {
        let (mut h, _) = row(1, &image_meta("", 1), Some(&encoded(1200, 800, image::ImageFormat::Png)));
        h.get_by_role_and_label(Role::Image, "Изображение").click();
        settle(&mut h);
        assert!(h.state().state.viewer.is_some());
        let screen = h.ctx.content_rect();
        let big = h.query_all_by_role_and_label(Role::Image, "Изображение").map(|n| n.rect()).max_by(|a, b| a.area().total_cmp(&b.area())).unwrap();
        assert!(big.width() > 240. && screen.contains_rect(big), "fits the window: {big:?} in {screen:?}");
        assert!((big.aspect_ratio() - 1.5).abs() < 0.01, "aspect kept");
        assert!(h.query_by_role_and_label(Role::Button, "Сохранить").is_some());
        let path = h.state().state.viewer.clone().unwrap().0;
        assert!(h.state().state.thumbs.cached(&path, crate::ui::image_viewer::VIEW_PX));
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(h.state().state.viewer.is_none(), "Esc closes");
        assert!(!h.state().state.thumbs.cached(&path, crate::ui::image_viewer::VIEW_PX), "the big texture is released");
        assert!(h.state().state.thumbs.cached(&path, crate::media::thumbs::THUMB_PX), "the bubble's thumbnail stays");
        h.get_by_role_and_label(Role::Image, "Изображение").click();
        settle(&mut h);
        assert!(h.state().state.viewer.is_some());
        let corner = pos2(screen.left() + 4., screen.bottom() - 4.);
        h.hover_at(corner);
        for pressed in [true, false] {
            h.event(egui::Event::PointerButton { pos: corner, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() });
        }
        h.run();
        assert!(h.state().state.viewer.is_none(), "a click outside the picture closes");
    }
}
