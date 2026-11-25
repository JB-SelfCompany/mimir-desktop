use crate::{
    i18n::{tr, Key, Lang},
    media::{avatar::{self, AvatarError, Picking}, thumbs::{Thumb, ThumbCache}},
};
use eframe::egui::epaint::{Mesh, Vertex, WHITE_UV};
use eframe::egui::{
    vec2, Color32, Context, CursorIcon, Image, Painter, Pos2, Rect, Response, Role, Sense, Shape, Stroke, TextureHandle, TextureOptions, Ui,
    Vec2, WidgetInfo,
};
use std::{path::Path, sync::mpsc::TryRecvError};

// lightColors from Utils.kt (MimirForAndroid)
const COLORS: [u32; 30] = [
    0x6F8F8F, 0x8FB2D4, 0x95AB5F, 0xFDF7BB, 0xCFFCCF, 0xA6EDE6, 0x5151FF, 0xD3A0FF, 0x7CD7A1, 0xAB98FF, 0x52FFBA, 0x8AF1EC, 0xABCE63, 0xD8FFD8, 0xCFFFFF, 0xF8C64B, 0x4E9E4E, 0xE9E9E9, 0xFFBD4D, 0xBD6CFF, 0xFFD6BB, 0x52EFF1, 0xD44CFF, 0x52DFFF, 0xA9A9A9, 0x62BB62, 0xFFE755, 0xCDFF6F, 0xCDE8FF, 0xB0FFB0,
];

/// Java `Arrays.hashCode(byte[])` over signed bytes, abs(h) % 30.
pub fn avatar_color(pubkey: &[u8]) -> Color32 {
    let h = pubkey.iter().fold(1i32, |h, &b| h.wrapping_mul(31).wrapping_add(b as i8 as i32));
    let c = COLORS[(h.unsigned_abs() % 30) as usize];
    Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

/// Android `getAvatarColor(seed)`: the same palette and hash as `avatar_color`. Android uses the light palette in
/// both themes, so `dark` does not change the colour. Groups seed with `chat_id.to_string()`, members with their pubkey.
pub fn avatar_seed_color(seed: &[u8], _dark: bool) -> Color32 {
    avatar_color(seed)
}

/// Rounded square (radius 3), lighter at the top, `base` at the bottom.
pub fn paint_placeholder(painter: &Painter, rect: Rect, base: Color32) {
    paint_rounded(painter, rect, base, 3.);
}

/// The placeholder with corner radius `r`.
fn paint_rounded(painter: &Painter, rect: Rect, base: Color32, r: f32) {
    let top = base.lerp_to_gamma(Color32::WHITE, 0.3);
    let mut pts = Vec::new();
    for (corner, start) in [
        (rect.right_top() + vec2(-r, r), -90.0_f32),
        (rect.right_bottom() + vec2(-r, -r), 0.0),
        (rect.left_bottom() + vec2(r, -r), 90.0),
        (rect.left_top() + vec2(r, r), 180.0),
    ] {
        for i in 0..=4 {
            let a = (start + 22.5 * i as f32).to_radians();
            pts.push(corner + vec2(a.cos(), a.sin()) * r);
        }
    }
    let colour = |p: Pos2| top.lerp_to_gamma(base, ((p.y - rect.top()) / rect.height()).clamp(0.0, 1.0));
    let mut mesh = Mesh::default();
    for p in &pts {
        mesh.vertices.push(Vertex { pos: *p, uv: WHITE_UV, color: colour(*p) });
    }
    for i in 1..pts.len() as u32 - 1 {
        mesh.indices.extend([0, i, i + 1]);
    }
    painter.add(Shape::mesh(mesh));
}

/// Saved Messages avatar: accent placeholder with a white bookmark, scaled to `rect` (drawn for 38 px).
pub fn paint_saved(painter: &Painter, rect: Rect, accent: Color32) {
    paint_placeholder(painter, rect, accent);
    let (c, k) = (rect.center(), rect.width() / 38.);
    let pts = [(-6., -9.), (6., -9.), (6., 9.), (0., 4.), (-6., 9.)].map(|(x, y)| c + eframe::egui::vec2(x, y) * k);
    painter.add(Shape::closed_line(pts.to_vec(), eframe::egui::Stroke::new(2. * k.max(0.75), Color32::WHITE)));
}

/// An avatar in `rect` (every place shows it square, corner `radius`): the picture at `path`, decoded off the
/// UI thread at the size drawn; the coloured placeholder while it loads, and for none or a missing or broken file.
// ponytail: every visible avatar stats its file each frame (the cache's mtime check); fine for a screen of rows.
pub fn paint(ui: &Ui, thumbs: &mut ThumbCache, rect: Rect, path: Option<&Path>, base: Color32, radius: f32) {
    let px = (rect.width() * ui.ctx().pixels_per_point()).ceil() as u32;
    match path.map(|p| thumbs.get_square(ui.ctx(), p, px)) {
        Some(Thumb::Ready(tex, _)) => Image::new(&tex).corner_radius(radius).paint_at(ui, rect),
        _ => paint_rounded(ui.painter(), rect, base, radius),
    }
}

/// `paint` in a `size` square of its own.
pub fn show(ui: &mut Ui, thumbs: &mut ThumbCache, path: Option<&Path>, base: Color32, size: f32, radius: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    paint(ui, thumbs, rect, path, base, radius);
    resp
}

/// A group avatar that could not be used.
pub fn error_text(e: AvatarError, lang: Lang) -> &'static str {
    tr(lang, match e {
        AvatarError::TooLarge => Key::AvatarTooLarge,
        AvatarError::Unreadable => Key::ErrorLoadingImage,
    })
}

/// A new avatar being picked in a dialog: the picker's answer, then the bytes to save and their preview.
#[derive(Default)]
pub struct AvatarPick {
    pub picking: Option<Picking>,
    pub bytes: Option<Vec<u8>>,
    pub preview: Option<TextureHandle>,
}

impl AvatarPick {
    /// Opens the picker (`filter` names the file type); the picture is prepared for `max_bytes` off the UI thread.
    pub fn start(&mut self, ctx: &Context, filter: &str, max_bytes: usize) {
        self.picking = Some(avatar::pick(ctx, filter, max_bytes));
    }

    /// Takes the picker's answer once it has come: a good picture is kept (bytes and preview), a bad one returned.
    pub fn poll(&mut self, ctx: &Context) -> Option<AvatarError> {
        let answer = match self.picking.as_ref()?.try_recv() {
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => None, // cancelled
            Ok(a) => Some(a),
        };
        self.picking = None;
        match answer? {
            Ok(p) => {
                self.preview = Some(ctx.load_texture("avatar_pick", p.image, TextureOptions::LINEAR));
                self.bytes = Some(p.bytes);
                None
            }
            Err(e) => Some(e),
        }
    }
}

/// The avatar as the button that picks a new one (Android: a tap on the avatar), with a camera badge bottom-left
/// like Android's overlay: `preview` once picked, else the stored picture or the placeholder. Inert while a picker
/// is open: a second one would drop the first one's answer.
pub fn pick_button(ui: &mut Ui, thumbs: &mut ThumbCache, pick: &AvatarPick, path: Option<&Path>, base: Color32, size: f32, label: &str) -> bool {
    let busy = pick.picking.is_some();
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), if busy { Sense::hover() } else { Sense::click() });
    let enabled = ui.is_enabled() && !busy;
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, label));
    match &pick.preview {
        Some(tex) => Image::new(tex).corner_radius(4).paint_at(ui, rect),
        None => paint(ui, thumbs, rect, path, base, 4.),
    }
    let c = rect.left_bottom() + vec2(18., -18.);
    ui.painter().circle_filled(c, 14., Color32::from_black_alpha(if resp.hovered() { 170 } else { 110 }));
    paint_camera(ui.painter(), c, Color32::WHITE);
    let cursor = if busy { CursorIcon::Wait } else { CursorIcon::PointingHand };
    resp.on_hover_cursor(cursor).on_hover_text(label).clicked() && !busy
}

/// A 16 px camera outline (ic_camera_plus_outline class) centred on `c`.
fn paint_camera(painter: &Painter, c: Pos2, colour: Color32) {
    let s = Stroke::new(1.5, colour);
    painter.rect_stroke(Rect::from_center_size(c + vec2(0., 1.), vec2(16., 11.)), 2, s, eframe::egui::StrokeKind::Middle);
    painter.line_segment([c + vec2(-3., -5.), c + vec2(3., -5.)], s);
    painter.circle_stroke(c + vec2(0., 1.), 3., s);
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Color32;
    use crate::{
        core::events::Event,
        media::avatar::resolve,
        model::{chat_key::ChatKey, chat_list::build_chat_list},
        settings::Settings,
        test_support::{png_bytes, run_until, test_harness, textured},
    };
    use eframe::egui::vec2;
    use mimir::MemberUpdate;

    fn has(h: &egui_kittest::Harness<'static, crate::app::MimirDesktop>, side: f32) -> bool {
        textured(h).contains(&vec2(side, side))
    }

    #[test]
    fn avatars_shown_in_list_header_info_members() {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        h.set_size(vec2(1200., 900.));
        let core = h.state().core.clone();
        let anya = core.add_contact(vec![7; 32], "Аня".into()).unwrap();
        core.update_contact_avatar(anya, png_bytes(256, 256, [200, 30, 30])).unwrap();
        let my = core.public_key();
        core.save_group_chat(9, "Дача".into(), String::new(), Some(png_bytes(256, 256, [30, 200, 30])), vec![0xA1; 32], my.clone(), vec![5; 32], 0).unwrap();
        core.update_members_bulk(9, vec![
            MemberUpdate { pubkey: my, permissions: 0x80, online: false, last_seen: 0 },
            MemberUpdate { pubkey: vec![8; 32], permissions: 0x10, online: false, last_seen: 0 },
        ])
        .unwrap();
        core.update_member_info(9, vec![8; 32], Some("Боб".into()), None, Some(png_bytes(64, 64, [30, 30, 200]))).unwrap();
        h.state_mut().state.chats = build_chat_list(&core, crate::i18n::Lang::Ru);
        h.state_mut().state.selected = Some(ChatKey::Contact(anya));
        h.state_mut().state.info_open = true;
        // List 48, chat toolbar 32, contact info 128.
        run_until(&mut h, "the contact's avatars", |h| has(h, 48.) && has(h, 32.) && has(h, 128.));
        h.state_mut().state.selected = Some(ChatKey::Group(9));
        // Group info 128 and the members list 48 (Боб's, besides the two list rows).
        run_until(&mut h, "the group's avatars", |h| has(h, 128.) && textured(h).iter().filter(|s| **s == vec2(48., 48.)).count() >= 3);
    }

    #[test]
    fn group_sender_avatar_beside_incoming_bubbles() {
        // A 48 px sender avatar left of each incoming group bubble; none for our own.
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        let core = h.state().core.clone();
        let my = core.public_key();
        core.save_group_chat(9, "Дача".into(), String::new(), None, vec![0xA1; 32], my.clone(), vec![5; 32], 0).unwrap();
        core.update_members_bulk(9, vec![
            MemberUpdate { pubkey: my.clone(), permissions: 0x80, online: false, last_seen: 0 },
            MemberUpdate { pubkey: vec![8; 32], permissions: 0x10, online: false, last_seen: 0 },
        ])
        .unwrap();
        core.update_member_info(9, vec![8; 32], Some("Боб".into()), None, Some(png_bytes(64, 64, [30, 30, 200]))).unwrap();
        let t = time::OffsetDateTime::now_utc().unix_timestamp() * 1000 - 5000;
        core.add_group_message(9, 1, 11, vec![8; 32], t, 0, false, "привет".as_bytes().to_vec(), 0, false).unwrap();
        core.add_group_message(9, 2, 12, my, t + 1, 0, false, "мой".as_bytes().to_vec(), 0, false).unwrap();
        h.state_mut().state.chats = build_chat_list(&core, crate::i18n::Lang::Ru);
        h.state_mut().state.selected = Some(ChatKey::Group(9));
        run_until(&mut h, "the sender's avatar", |h| has(h, 48.));
        assert_eq!(textured(&h).iter().filter(|s| **s == vec2(48., 48.)).count(), 1, "one incoming row");
    }

    #[test]
    fn own_avatar_in_main_toolbar() {
        // The main toolbar shows our account avatar, 32/6.
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        h.state().core.update_account_avatar(1, png_bytes(64, 64, [200, 30, 30])).unwrap();
        h.state_mut().state.own_avatar = None; // read again, as at start-up (the harness drew a frame already)
        run_until(&mut h, "the account avatar", |h| has(h, 32.));
        egui_kittest::kittest::Queryable::get_by_role_and_label(&h, eframe::egui::Role::Button, "Профиль").click();
        h.run();
        assert!(h.state().state.show_profile, "it is the profile button");
    }

    #[test]
    fn contact_avatar_refreshes_on_connect() {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        let core = h.state().core.clone();
        let id = core.add_contact(vec![7; 32], "Аня".into()).unwrap();
        let stored = core.update_contact_avatar(id, png_bytes(64, 64, [200, 30, 30])).unwrap().unwrap();
        let path = resolve(&h.state().paths.core_dir, &stored).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        h.state_mut().rx = rx;
        h.state_mut().state.chats = build_chat_list(&core, crate::i18n::Lang::Ru);
        run_until(&mut h, "the avatar", |h| has(h, 48.));
        let before = h.state().state.thumbs.square_texture(&path, 48).unwrap();
        // The peer's new profile lands in the same file; its mtime may not change (no event says so).
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::fs::write(&path, png_bytes(64, 64, [30, 30, 200])).unwrap();
        std::fs::File::options().write(true).open(&path).unwrap().set_modified(mtime).unwrap();
        tx.send(Event::ContactConnected { pubkey: vec![7; 32] }).unwrap();
        run_until(&mut h, "a fresh texture", |h| has(h, 48.) && h.state().state.thumbs.square_texture(&path, 48).is_some_and(|t| t != before));
    }

    // Java Arrays.hashCode(byte[]) over signed bytes, abs(h) % 30, lightColors palette from Utils.kt
    #[test]
    fn avatar_color_matches_android() {
        assert_eq!(avatar_color(&[1, 2, 3]), Color32::from_rgb(0xD3, 0xA0, 0xFF)); // h=30817 -> idx 7
        assert_eq!(avatar_color(&[0xFF]), Color32::from_rgb(0x6F, 0x8F, 0x8F)); // h=30 -> idx 0
    }
}
