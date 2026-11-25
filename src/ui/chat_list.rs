//! Left column rows (mockup option B): avatar, name, preview, time, unread badge.
use crate::{
    media::{avatar::resolve, thumbs::ThumbCache},
    model::{chat_key::ChatKey, chat_list::{ChatRow, Request}},
    ui::{avatar::{self, avatar_color, avatar_seed_color, paint_saved}, theme::Palette},
};
use eframe::egui::{
    pos2, vec2, Align2, Color32, CornerRadius, FontFamily, FontId, Rect, Response, ScrollArea, Sense, Stroke,
    Painter, Pos2, Shape, Ui, Role, WidgetInfo,
};
use std::path::Path;
use time::{OffsetDateTime, UtcOffset};

/// A 48 px avatar with 8 px above and below.
const ROW_H: f32 = 64.;
const RIGHT_W: f32 = 46.;

/// Today (the day of `now_ms`) as HH:MM, otherwise DD.MM; empty when there is no message.
fn fmt_time(ms: i64, now_ms: i64) -> String {
    let Ok(t) = OffsetDateTime::from_unix_timestamp(ms / 1000) else { return String::new() };
    if ms <= 0 {
        return String::new();
    }
    // ponytail: falls back to UTC when the OS refuses the local offset.
    let off = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
    let now = OffsetDateTime::from_unix_timestamp(now_ms / 1000).unwrap_or(OffsetDateTime::UNIX_EPOCH);
    let (t, now) = (t.to_offset(off), now.to_offset(off));
    if t.date() == now.date() {
        format!("{:02}:{:02}", t.hour(), t.minute())
    } else {
        format!("{:02}.{:02}", t.day(), t.month() as u8)
    }
}

/// Right-hand badge: «?» / «!» for our pending / declined request (it takes the unread slot, like Android), else the unread count.
fn badge(r: &ChatRow) -> Option<String> {
    match r.request {
        Request::Pending => Some("?".into()),
        Request::Declined => Some("!".into()),
        Request::None => (r.unread > 0).then(|| r.unread.to_string()),
    }
}

fn row(ui: &mut Ui, r: &ChatRow, selected: bool, p: &Palette, thumbs: &mut ThumbCache, data_dir: &Path) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, &r.name));
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let dark = ui.visuals().dark_mode;
    let g = ui.painter();
    if selected {
        g.rect_filled(rect, 0., p.selected);
        g.rect_filled(Rect::from_min_size(rect.min, vec2(3., ROW_H)), 0., p.accent);
    } else if resp.hovered() {
        g.rect_filled(rect, 0., p.divider);
    }
    g.hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1., p.divider));

    let av = Rect::from_min_size(rect.min + vec2(9., 8.), vec2(48., 48.));
    let path = resolve(data_dir, &r.avatar);
    match r.key {
        ChatKey::SAVED => paint_saved(g, av, p.accent),
        // Android getAvatarColor(chatId.toString().toByteArray()).
        ChatKey::Group(id) => avatar::paint(ui, thumbs, av, path.as_deref(), avatar_seed_color(id.to_string().as_bytes(), dark), 3.),
        ChatKey::Contact(_) => avatar::paint(ui, thumbs, av, path.as_deref(), avatar_color(&r.pubkey), 3.),
    }

    let text_x = av.right() + 9.;
    let clip = Rect::from_min_max(rect.min, pos2(rect.right() - RIGHT_W, rect.bottom())).intersect(ui.clip_rect());
    let tg = g.with_clip_rect(clip);
    let mut name_x = text_x;
    if r.group.is_some() {
        // A 16 px people icon at alpha .7 before the name, for groups and channels alike.
        paint_people(&tg, pos2(text_x, rect.top() + 12.), 16., p.text.gamma_multiply(0.7));
        name_x += 20.;
    }
    let name = tg.text(pos2(name_x, rect.top() + 12.), Align2::LEFT_TOP, &r.name, FontId::new(13.5, FontFamily::Name("bold".into())), p.text);
    if r.group.is_some_and(|g| g.muted) {
        paint_muted_bell(&tg, pos2(name.right() + 5., name.center().y - 6.), 12., p.text_dim);
    }
    let preview = r.preview.lines().next().unwrap_or_default();
    tg.text(pos2(text_x, rect.top() + 34.), Align2::LEFT_TOP, preview, FontId::proportional(12.), p.text_dim);

    g.text(pos2(rect.right() - 9., rect.top() + 13.), Align2::RIGHT_TOP, fmt_time(r.time, OffsetDateTime::now_utc().unix_timestamp() * 1000), FontId::proportional(11.), p.text_dim);
    if let Some(text) = badge(r) {
        let galley = g.layout_no_wrap(text, FontId::proportional(10.5), Color32::WHITE);
        let w = (galley.size().x + 10.).max(16.);
        let pill = Rect::from_min_size(pos2(rect.right() - 9. - w, rect.top() + 34.), vec2(w, 16.));
        g.rect_filled(pill, CornerRadius::same(2), Color32::from_rgb(0x80, 0x80, 0x80));
        g.galley(pill.center() - galley.size() / 2., galley, Color32::WHITE);
    }
    resp
}

/// MDI account-multiple-outline (assets/icons) as strokes, in a `size` square at `at`.
fn paint_people(g: &Painter, at: Pos2, size: f32, c: Color32) {
    let k = size / 24.;
    let pt = |x: f32, y: f32| at + vec2(x, y) * k;
    let arc = |cx: f32, cy: f32, r: f32, from: f32, to: f32| -> Vec<Pos2> {
        (0..=12).map(|i| (from + (to - from) * i as f32 / 12.).to_radians()).map(|a| pt(cx + r * a.cos(), cy + r * a.sin())).collect()
    };
    let s = Stroke::new(2. * k, c);
    g.circle_stroke(pt(9., 7.5), 2.5 * k, s);
    let mut front = arc(9., 18., 6., 180., 360.);
    front.push(pt(15., 18.));
    g.add(Shape::closed_line(front, s));
    g.add(Shape::line(arc(15., 7.5, 2.8, -100., 100.), s));
    g.add(Shape::line(arc(17., 18., 4., 250., 360.), s));
}

/// Bell with a slash: the chat is muted.
fn paint_muted_bell(g: &Painter, at: Pos2, size: f32, c: Color32) {
    let k = size / 24.;
    let pt = |x: f32, y: f32| at + vec2(x, y) * k;
    let s = Stroke::new(2. * k, c);
    let mut bell: Vec<Pos2> = (0..=12).map(|i| (180. + 180. * i as f32 / 12.).to_radians()).map(|a: f32| pt(12. + 6. * a.cos(), 10. + 6. * a.sin())).collect();
    bell.extend([pt(18., 16.), pt(20., 18.), pt(4., 18.), pt(6., 16.)]);
    g.add(Shape::closed_line(bell, s));
    g.line_segment([pt(10., 21.), pt(14., 21.)], s);
    g.line_segment([pt(3., 3.), pt(21., 21.)], s);
}

pub fn show(ui: &mut Ui, chats: &[ChatRow], selected: &mut Option<ChatKey>, p: &Palette, thumbs: &mut ThumbCache, data_dir: &Path) {
    ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.;
        for r in chats {
            if row(ui, r, *selected == Some(r.key), p, thumbs, data_dir).clicked() {
                *selected = Some(r.key);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn badge_text_request_takes_unread_slot() {
        use crate::model::chat_list::Request;
        let r = |unread, request| ChatRow { key: ChatKey::Contact(1), pubkey: vec![], name: String::new(), avatar: String::new(), preview: String::new(), time: 0, unread, has_draft: false, request, group: None };
        assert_eq!(badge(&r(0, Request::None)), None);
        assert_eq!(badge(&r(3, Request::None)).as_deref(), Some("3"));
        assert_eq!(badge(&r(0, Request::Pending)).as_deref(), Some("?"));
        assert_eq!(badge(&r(3, Request::Pending)).as_deref(), Some("?"));
        assert_eq!(badge(&r(0, Request::Declined)).as_deref(), Some("!"));
        assert_eq!(badge(&r(3, Request::Declined)).as_deref(), Some("!"));
    }

    #[test]
    fn time_formatting() {
        // Local 00:30 on a fixed day: a wall-clock "now" near midnight made "earlier today" yesterday.
        let off = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
        let at = |d: u8, h: u8, m: u8| {
            time::Date::from_calendar_date(2026, time::Month::November, d).unwrap().with_hms(h, m, 0).unwrap().assume_offset(off).unix_timestamp() * 1000
        };
        let now = at(20, 0, 30);
        assert_eq!(fmt_time(0, now), "");
        assert_eq!(fmt_time(at(20, 0, 5), now), "00:05");
        assert_eq!(fmt_time(at(19, 23, 50), now), "19.11");
        assert_eq!(fmt_time(at(17, 12, 0), now), "17.11");
    }
}
