//! Chat area: header, day separators, message bubbles with reaction chips, hover toolbar, menu, forward and delete dialogs.
use crate::{
    app::MimirDesktop,
    core::{net, worker::WorkResult},
    i18n::{month, tr, trf, Key, Lang},
    model::{
        attachment::{local_path, request_download, AttachmentMeta, Transfer},
        chat_key::ChatKey,
        chat_list::SAVED_ID,
        group::{self, system_text, GroupStatus, Members, SYSTEM},
        message::{decode, preview, MsgBody},
        reactions::chips,
        timeline::{show_timeline, Timeline},
        AppState, Forward, ForwardDialog, Menu, MsgRef, OpenChat,
    },
    media::{self, stage::Staged, thumbs::ThumbCache},
    ui::{
        attachment_bubble, image_viewer,
        avatar::{self, avatar_color, avatar_seed_color, paint_saved},
        bubble::{geom, max_width},
        message_menu::{self, Action},
        call, composer, info_panel,
        theme::{danger_button, palette, Palette},
    },
};
use eframe::egui::{
    self, pos2, vec2, Align, Button, Color32, CornerRadius, FontFamily, FontId, Frame, Id, Layout,
    OpenUrl, Panel, Rect, RichText, Role, ScrollArea, Sense, Shape, Stroke, TextEdit, TextFormat, Ui, UiBuilder, Vec2, WidgetInfo,
    text::LayoutJob,
};
use mimir::{MimirApp, StoredMessage};
use std::{collections::HashSet, path::Path};
use time::{Date, OffsetDateTime, UtcOffset};

const SEP_H: f32 = 26.;
const CHIP_H: f32 = 20.;
/// Group sender avatar beside an incoming bubble (message_incoming_layout, 48/6).
const SENDER_AV: f32 = 48.;

fn local() -> UtcOffset {
    // ponytail: falls back to UTC when the OS refuses the local offset; read once, so a DST change needs a restart.
    static OFFSET: std::sync::OnceLock<UtcOffset> = std::sync::OnceLock::new();
    *OFFSET.get_or_init(|| UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC))
}

fn at(ms: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(ms / 1000).unwrap_or(OffsetDateTime::UNIX_EPOCH).to_offset(local())
}

fn hhmm(ms: i64) -> String {
    let t = at(ms);
    format!("{:02}:{:02}", t.hour(), t.minute())
}

/// «Сегодня» / «Вчера» / «12 ноября» (with the year when it differs from now).
pub fn day_label(ts_ms: i64, now_ms: i64, lang: Lang) -> String {
    let (day, today): (Date, Date) = (at(ts_ms).date(), at(now_ms).date());
    if day == today {
        tr(lang, Key::Today).into()
    } else if today.previous_day() == Some(day) {
        tr(lang, Key::Yesterday).into()
    } else if day.year() == today.year() {
        format!("{} {}", day.day(), month(lang, day.month() as u8))
    } else {
        format!("{} {} {}", day.day(), month(lang, day.month() as u8), day.year())
    }
}

/// «был(а) сегодня, 14:05» for a `last_seen` stamp; `None` when it was never set (0).
pub fn last_seen_text(ls: i64, lang: Lang) -> Option<String> {
    // ponytail: last_seen unit is not documented; values below 1e11 are taken as seconds.
    let ms = if ls < 100_000_000_000 { ls * 1000 } else { ls };
    (ls > 0).then(|| format!("{}{}, {}", tr(lang, Key::LastSeen), day_label(ms, now_ms(), lang).to_lowercase(), hhmm(ms)))
}

fn now_ms() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp() * 1000
}

/// Text of a message for display, copy and forward; non-text bodies show their chat-list preview.
fn text_of(m: &StoredMessage, lang: Lang) -> (String, bool) {
    match decode(m.msg_type, m.data.as_deref()) {
        MsgBody::Text(t) => (t, false),
        other => (preview(&other, lang), true),
    }
}

/// Text split into plain and link runs: (char range, url).
fn links(text: &str) -> Vec<(std::ops::Range<usize>, String)> {
    let (mut out, mut chars) = (vec![], 0);
    for word in text.split_inclusive(char::is_whitespace) {
        let w = word.trim_end();
        let w = w.trim_end_matches(['.', ',', ')', '!', '?', ';', ':']);
        if w.starts_with("http://") || w.starts_with("https://") || w.starts_with("mimir://") {
            out.push((chars..chars + w.chars().count(), w.to_string()));
        }
        chars += word.chars().count();
    }
    out
}

struct RowCx<'a> {
    core: &'a MimirApp,
    lang: Lang,
    p: &'a Palette,
    key: ChatKey,
    day_starts: &'a HashSet<i64>,
    quotes: &'a std::collections::HashMap<i64, Option<(bool, i64, String)>>,
    /// Contact name for quotes of incoming messages.
    name: &'a str,
    /// Sender names and colours in a group.
    members: Option<&'a Members>,
    unread_from: Option<i64>,
    reactions: &'a mut std::collections::HashMap<i64, Vec<(String, u32, bool)>>,
    menu: &'a mut Option<Menu>,
    actions: &'a mut Vec<(Action, MsgRef)>,
    files: &'a Path,
    /// The core's data dir, which stored avatar paths are relative to.
    core_dir: &'a Path,
    transfers: &'a std::collections::HashMap<i64, Transfer>,
    thumbs: &'a mut ThumbCache,
    posters: &'a mut media::ffmpeg::Posters,
    ffmpeg: Option<&'a Path>,
    player: &'a media::player::Player,
    /// Message text size (settings «Размер шрифта сообщений»).
    font: f32,
}

fn day_separator(painter: &egui::Painter, row_rect: Rect, m: &StoredMessage, lang: Lang, p: &Palette) {
    let label = painter.layout_no_wrap(day_label(m.send_time, now_ms(), lang), FontId::proportional(11.), p.text_dim);
    let r = Rect::from_center_size(pos2(row_rect.center().x, row_rect.top() + SEP_H / 2.), label.size() + vec2(16., 2.));
    painter.rect_filled(r, 2., p.list_bg);
    painter.galley(r.center() - label.size() / 2., label, p.text_dim);
}

/// A group system row: a centred muted line like the day separator; no bubble, menu or toolbar.
fn system_row(ui: &mut Ui, m: &StoredMessage, cx: &mut RowCx) -> egui::Response {
    let (p, lang) = (cx.p, cx.lang);
    let none = Members::default();
    let text = system_text(m.data.as_deref().unwrap_or_default(), cx.members.unwrap_or(&none), lang).unwrap_or_default();
    let width = ui.available_width();
    let galley = ui.painter().layout(text, FontId::proportional(11.), p.text_dim, max_width(width));
    let day_sep = if cx.day_starts.contains(&m.id) { SEP_H } else { 0. };
    let (row_rect, row_resp) = ui.allocate_exact_size(vec2(width, day_sep + galley.size().y + 14.), Sense::hover());
    let painter = ui.painter();
    if day_sep > 0. {
        day_separator(painter, row_rect, m, lang, p);
    }
    let r = Rect::from_center_size(pos2(row_rect.center().x, row_rect.top() + day_sep + 7. + galley.size().y / 2.), galley.size() + vec2(16., 4.));
    let label = galley.text().to_string();
    painter.rect_filled(r, 2., p.list_bg);
    painter.galley(r.center() - galley.size() / 2., galley, p.text_dim);
    ui.interact(r, ui.scope_id().with(("system", m.id)), Sense::hover()).widget_info(|| WidgetInfo::labeled(Role::Label, true, &label));
    row_resp
}

/// Header badge on a group avatar (Android GroupChatActivity): red disconnected, yellow connecting, green subscribed,
/// grey read-only.
pub fn status_color(s: GroupStatus) -> Color32 {
    match s {
        GroupStatus::Disconnected => Color32::from_rgb(0xE5, 0x39, 0x35),
        GroupStatus::Connecting => Color32::from_rgb(0xF9, 0xA8, 0x25),
        GroupStatus::Subscribed => Color32::from_rgb(0x00, 0xAA, 0x00),
        GroupStatus::ReadOnly => Color32::from_gray(0x9E),
    }
}

fn message_row(ui: &mut Ui, m: &StoredMessage, cx: &mut RowCx) -> egui::Response {
    if m.msg_type == SYSTEM {
        return system_row(ui, m, cx);
    }
    let (p, lang, out) = (cx.p, cx.lang, !m.incoming);
    // Group: the sender's avatar left of an incoming bubble, SENDER_AV + a 4 px gap.
    let av_w = if cx.members.is_some() && m.incoming { SENDER_AV + 4. } else { 0. };
    let width = ui.available_width();
    let wrap = max_width(width) - 24. - av_w;
    let body_msg = decode(m.msg_type, m.data.as_deref());
    let meta = attachment_bubble::meta_of(&body_msg);
    let transfer = cx.transfers.get(&m.guid).copied().unwrap_or_default();
    let att = meta.map(|a| attachment_bubble::prepare(ui, m.guid, m.msg_type, a, cx.files, transfer, cx.thumbs, cx.posters, cx.ffmpeg, cx.player, lang, p, wrap));
    let is_call = body_msg == MsgBody::Call;
    let att = att.or_else(|| {
        is_call.then(|| {
            let (label, duration) = call::record_text(m.incoming, m.send_time, m.edit_time, lang);
            attachment_bubble::call_record(ui, label, duration, p, wrap)
        })
    });
    // Save / Delete file: types 1, 3 and 4 only (BaseChatActivity), and only for a complete file.
    let file = att.as_ref().and_then(|a| a.file.clone()).filter(|_| matches!(m.msg_type, 1 | 3 | 4)).zip(meta.map(|a| a.display_name().to_string()));
    let forward = match (meta, &file) {
        // A call record is not forwarded (Android: no menu entry for it).
        (None, _) if is_call => None,
        (None, _) => Some(Forward::Text(text_of(m, lang).0)),
        // ponytail: forwarded as is, without stage_path's checks; a group's 500 MB cap is left to the core to refuse.
        (Some(a), Some((path, name))) => {
            let staged = Staged { path: path.clone(), original_name: name.clone(), mime: a.mime.clone(), msg_type: m.msg_type, size: a.size.max(0) as u64, temp: false, resized: false };
            Some(Forward::File(staged, a.caption.clone()))
        }
        (Some(_), None) => None,
    };
    let mref = MsgRef { id: m.id, guid: m.guid, text: text_of(m, lang).0, file, forward };
    // An attachment's text is its caption (none: no text); other bodies without text show their preview.
    let (text, italic) = match (&body_msg, meta) {
        (_, Some(a)) => (a.caption.clone(), false),
        (MsgBody::Text(t), None) => (t.clone(), false),
        (MsgBody::Call, None) => (String::new(), false),
        (b, None) => (preview(b, lang), true),
    };
    let link_runs = links(&text);

    // Layout.
    let mut job = LayoutJob::default();
    job.wrap.max_width = wrap;
    let font = FontId::proportional(cx.font);
    let mut pos = 0;
    let push = |job: &mut LayoutJob, s: &str, link: bool| {
        let fmt = TextFormat {
            font_id: font.clone(),
            color: if link { p.accent } else { p.text },
            italics: italic,
            underline: if link { Stroke::new(1., p.accent) } else { Stroke::NONE },
            ..Default::default()
        };
        job.append(s, 0., fmt);
    };
    let chars: Vec<char> = text.chars().collect();
    for (r, _) in &link_runs {
        push(&mut job, &chars[pos..r.start].iter().collect::<String>(), false);
        push(&mut job, &chars[r.clone()].iter().collect::<String>(), true);
        pos = r.end;
    }
    push(&mut job, &chars[pos..].iter().collect::<String>(), false);
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    // Group: the sender's name above an incoming message, in their avatar colour (Android MessageAdapter);
    // neutral when the sender has no member row.
    let sender = cx.members.filter(|_| m.incoming).map(|ms| {
        let color = ms.pubkey(m.contact_id).map_or(p.text_dim, |k| avatar_seed_color(k, ui.visuals().dark_mode));
        let mut job = LayoutJob::default();
        job.wrap = egui::text::TextWrapping { max_width: wrap, max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
        let font_id = FontId::new(12., FontFamily::Name("bold".into()));
        job.append(&ms.name(m.contact_id, lang), 0., TextFormat { font_id, color, ..Default::default() });
        ui.fonts_mut(|f| f.layout_job(job))
    });
    let name_h = sender.as_ref().map_or(0., |g| g.size().y + 2.);
    // Reply quote: one line «sender preview» above the text, behind an accent bar (like the composer's preview).
    let quote = (m.reply_to != 0).then(|| {
        let (who, said) = match cx.quotes.get(&m.reply_to).cloned().flatten() {
            Some((incoming, sender, t)) => (
                match (incoming, cx.members) {
                    (false, _) => tr(lang, Key::You).to_string(),
                    (true, Some(ms)) => ms.name(sender, lang),
                    (true, None) => cx.name.to_string(),
                },
                t,
            ),
            None => (String::new(), "…".to_string()),
        };
        let said = said.lines().next().unwrap_or("").to_string();
        let mut job = LayoutJob::default();
        job.wrap = egui::text::TextWrapping { max_width: wrap - 6., max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
        let fmt = |color| TextFormat { font_id: FontId::proportional(12.), color, ..Default::default() };
        if !who.is_empty() {
            job.append(&format!("{who} "), 0., fmt(p.accent));
        }
        job.append(&said, 0., fmt(p.text_dim));
        let label = if who.is_empty() { said } else { format!("{who}: {said}") };
        (label, ui.fonts_mut(|f| f.layout_job(job)))
    });
    let quote_h = quote.as_ref().map_or(0., |(_, g)| g.size().y + 4.);
    let quote_w = quote.as_ref().map_or(0., |(_, g)| g.size().x + 6.);
    // A call record has no delivery tick (Android hides it).
    let footer = format!("{}{}", hhmm(m.send_time), if out && m.delivered && !is_call { " ✔" } else { "" });
    let footer_g = ui.painter().layout_no_wrap(footer, FontId::proportional(11.), p.text_dim);

    let chip_list = cx.reactions.entry(m.guid).or_insert_with(|| chips(cx.core, cx.key, m.guid)).clone();
    let chip_gs: Vec<_> = chip_list
        .iter()
        .map(|(e, n, _)| {
            let t = if *n > 1 { format!("{e} {n}") } else { e.clone() };
            (t.clone(), ui.painter().layout_no_wrap(t, FontId::proportional(12.), p.text))
        })
        .collect();
    let chips_w: f32 = chip_gs.iter().map(|(_, g)| g.size().x + 14.).sum::<f32>();
    let name_w = sender.as_ref().map_or(0., |g| g.size().x);
    let (att_w, att_h) = att.as_ref().map_or((0., 0.), |a| (a.size.x, a.size.y + 4.));
    let text_h = if text.is_empty() { 0. } else { galley.size().y };
    let content_w = galley.size().x.max(footer_g.size().x).max(chips_w.min(wrap)).max(quote_w).max(name_w).max(att_w);
    // ponytail: chips share one line; a message with very many reactions overflows the bubble.
    let chips_h = if chip_gs.is_empty() { 0. } else { CHIP_H + 4. };
    let content_size = vec2(content_w, name_h + quote_h + att_h + text_h + 2. + footer_g.size().y + chips_h);

    let day_sep = if cx.day_starts.contains(&m.id) { SEP_H } else { 0. };
    let unread_sep = cx.unread_from == Some(m.id);
    let sep = day_sep + if unread_sep { SEP_H } else { 0. };
    let g0 = geom(Rect::from_min_size(egui::Pos2::ZERO, content_size), out);
    let body_h = if av_w > 0. { g0.body.height().max(SENDER_AV) } else { g0.body.height() };
    let (row_rect, row_resp) = ui.allocate_exact_size(vec2(width, sep + 8. + body_h), Sense::hover());
    let body_min = pos2(
        if out { row_rect.right() - 16. - g0.body.width() } else { row_rect.left() + 16. + av_w },
        row_rect.top() + sep + 4.,
    );
    let g = geom(Rect::from_min_size(body_min - g0.body.min.to_vec2(), content_size), out);
    let content = Rect::from_min_size(body_min - g0.body.min.to_vec2(), content_size);
    let quote_min = content.min + vec2(0., name_h);
    let att_min = quote_min + vec2(0., quote_h);
    let text_min = att_min + vec2(0., att_h);

    // Paint.
    let painter = ui.painter();
    if unread_sep {
        let band = Rect::from_min_size(pos2(row_rect.left(), row_rect.top() + day_sep + 2.), vec2(width, SEP_H - 4.));
        painter.rect_filled(band, 0., p.list_bg);
        let text = tr(lang, Key::UnreadMessages);
        painter.text(band.center(), egui::Align2::CENTER_CENTER, text, FontId::proportional(11.), p.text_dim);
        ui.interact(band, ui.scope_id().with(("unread", m.id)), Sense::hover()).widget_info(|| WidgetInfo::labeled(Role::Label, true, text));
    }
    if day_sep > 0. {
        day_separator(painter, row_rect, m, lang, p);
    }
    let colour = if out { p.bubble_out } else { p.bubble_in };
    let r = crate::ui::bubble::RADIUS as u8;
    let corners = if out { CornerRadius { nw: r, ne: r, sw: r, se: 0 } } else { CornerRadius { nw: r, ne: r, sw: 0, se: r } };
    painter.rect_filled(g.body, corners, colour);
    painter.add(Shape::convex_polygon(g.tail.to_vec(), colour, Stroke::NONE));
    if let Some(ms) = cx.members.filter(|_| av_w > 0.) {
        let r = Rect::from_min_size(pos2(row_rect.left() + 8., body_min.y), Vec2::splat(SENDER_AV));
        let base = ms.pubkey(m.contact_id).map_or(p.text_dim, |k| avatar_seed_color(k, ui.visuals().dark_mode));
        let path = ms.avatar(m.contact_id).and_then(|a| media::avatar::resolve(cx.core_dir, a));
        avatar::paint(ui, cx.thumbs, r, path.as_deref(), base, 3.);
    }
    let painter = ui.painter();
    if let Some(ng) = sender {
        let r = Rect::from_min_size(content.min, ng.size());
        let name = ng.text().to_string();
        painter.galley(content.min, ng, p.text);
        ui.interact(r, ui.scope_id().with(("sender", m.id)), Sense::hover()).widget_info(|| WidgetInfo::labeled(Role::Label, true, &name));
    }
    let painter = ui.painter();
    if let Some((label, qg)) = &quote {
        let bar = Rect::from_min_size(quote_min, vec2(2., qg.size().y));
        painter.rect_filled(bar, 0., p.accent);
        painter.galley(quote_min + vec2(6., 0.), qg.clone(), p.text);
        let r = Rect::from_min_size(quote_min, vec2(quote_w, qg.size().y));
        ui.interact(r, ui.scope_id().with(("quote", m.id)), Sense::hover()).widget_info(|| WidgetInfo::labeled(Role::Label, true, label));
    }
    let painter = ui.painter();
    if !text.is_empty() {
        painter.galley(text_min, galley.clone(), p.text);
    }
    painter.galley(pos2(content.right() - footer_g.size().x, text_min.y + text_h + 2.), footer_g, p.text_dim);

    // Interaction: the body is one widget labelled with the text; chips come after so they sit on top.
    let body = ui.interact(g.body, ui.scope_id().with(("msg", m.id)), Sense::click());
    // An attachment without a caption is labelled with its preview.
    body.widget_info(|| WidgetInfo::labeled(Role::Label, true, if text.is_empty() { &mref.text } else { &text }));
    if let Some(pt) = body.hover_pos() {
        let idx = galley.cursor_from_pos(pt - text_min).index.0;
        if link_runs.iter().any(|(r, _)| r.contains(&idx)) && body.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }
    if body.clicked() {
        if let Some(pt) = body.interact_pointer_pos() {
            let idx = galley.cursor_from_pos(pt - text_min).index.0;
            match link_runs.iter().find(|(r, _)| r.contains(&idx)) {
                Some((_, url)) if url.starts_with("mimir://") => cx.actions.push((Action::Link(url.clone()), mref.clone())),
                Some((_, url)) => ui.ctx().open_url(OpenUrl::new_tab(url)),
                None => {}
            }
        }
    }
    // The attachment is registered after the body, so its clicks win.
    let att_resp = att.map(|a| {
        let (resp, action) = a.show(ui, att_min, ui.scope_id().with(("att", m.id)), p, lang);
        if let Some(action) = action {
            cx.actions.push((action, mref.clone()));
        }
        resp
    });
    if let Some(menu_at) = [Some(&body), att_resp.as_ref()].into_iter().flatten().find(|r| r.secondary_clicked()) {
        let pos = menu_at.interact_pointer_pos().unwrap_or(g.body.center());
        *cx.menu = Some(Menu { msg: mref.clone(), pos, expanded: false });
    }
    let mut x = content.min.x;
    let y = content.bottom() - chips_h + 2.;
    for ((label, gal), (emoji, _, mine)) in chip_gs.into_iter().zip(&chip_list) {
        let rect = Rect::from_min_size(pos2(x, y), vec2(gal.size().x + 10., CHIP_H));
        x += rect.width() + 4.;
        let chip = ui.interact(rect, ui.scope_id().with(("chip", m.id, emoji.as_str())), Sense::click());
        chip.widget_info(|| WidgetInfo::labeled(Role::Button, true, &label));
        let fill = if *mine { p.accent.gamma_multiply(0.45) } else { Color32::from_gray(128).gamma_multiply(0.25) };
        ui.painter().rect_filled(rect, 2., fill);
        ui.painter().galley(rect.center() - gal.size() / 2., gal, p.text);
        if chip.clicked() {
            cx.actions.push((Action::React(emoji.clone()), mref.clone()));
        }
    }

    // Hover mini-toolbar at the top corner of the bubble.
    let tb = Rect::from_min_size(pos2(if out { g.body.left() - 84. } else { g.body.right() + 4. }, g.body.top() - 2.), vec2(80., 24.));
    if ui.rect_contains_pointer(g.body.union(tb)) {
        ui.scope_builder(UiBuilder::new().max_rect(tb).layout(Layout::left_to_right(Align::Center)), |ui| {
            Frame::popup(ui.style()).inner_margin(2).show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 0.;
                let btn = |ui: &mut Ui, s: &str| ui.add(Button::new(s).frame(false).min_size(vec2(24., 18.)));
                let smile = btn(ui, "😊");
                if smile.clicked() {
                    *cx.menu = Some(Menu { msg: mref.clone(), pos: smile.rect.left_bottom(), expanded: false });
                }
                if btn(ui, "↩").clicked() {
                    cx.actions.push((Action::Reply(m.guid), mref.clone()));
                }
                let more = btn(ui, "…");
                if more.clicked() {
                    *cx.menu = Some(Menu { msg: mref.clone(), pos: more.rect.left_bottom(), expanded: false });
                }
            });
        });
    }
    row_resp
}

/// Height of the chat header (the narrow info overlay starts below it, so ⋮ stays reachable).
pub const HEADER_H: f32 = 44.;

/// Draws the header; returns whether the avatar/name was clicked (toggles the info panel), the ⋮ button, and whether
/// the call button (`call_btn` = enabled, name, hint) was clicked.
fn header(ui: &mut Ui, state: &mut AppState, data_dir: &Path, lang: Lang, p: &Palette, call_btn: Option<(bool, &str, &str)>) -> (bool, Option<egui::Response>, bool) {
    let AppState { chats, online, open_chat, thumbs, .. } = state;
    let Some(chat) = open_chat.as_ref() else { return (false, None, false) };
    let saved = chat.key == ChatKey::SAVED;
    let group = chat.key.is_group();
    let row = chats.iter().find(|r| r.key == chat.key);
    let path = row.and_then(|r| crate::media::avatar::resolve(data_dir, &r.avatar));
    let name = row.map_or("", |r| r.name.as_str());
    let is_channel = row.and_then(|r| r.group).is_some_and(|g| g.is_channel);
    let online = !group && online.contains(&chat.pubkey);
    let (mut toggle, mut more, mut calling) = (false, None, false);
    Frame::NONE.fill(p.toolbar).inner_margin(egui::Margin::symmetric(12, 0)).show(ui, |ui| {
        ui.set_height(HEADER_H);
        ui.set_width(ui.available_width());
        ui.horizontal_centered(|ui| {
            // 32 px in the chat toolbar.
            let (av, _) = ui.allocate_exact_size(vec2(32., 32.), Sense::hover());
            let path = path.as_deref();
            match chat.key {
                ChatKey::SAVED => paint_saved(ui.painter(), av, p.accent),
                ChatKey::Group(id) => avatar::paint(ui, thumbs, av, path, avatar_seed_color(id.to_string().as_bytes(), ui.visuals().dark_mode), 3.),
                ChatKey::Contact(_) => avatar::paint(ui, thumbs, av, path, avatar_color(&chat.pubkey), 3.),
            }
            let badge = if group { Some(status_color(chat.status)) } else { (online && !saved).then_some(Color32::from_rgb(0x00, 0xAA, 0x00)) };
            if let Some(colour) = badge {
                let c = av.right_bottom() - vec2(1., 1.);
                ui.painter().circle_filled(c, 5.5, p.canvas);
                ui.painter().circle_filled(c, 3.5, colour);
            }
            ui.add_space(4.);
            let (members, on) = (chat.counts.0.to_string(), chat.counts.1.to_string());
            let status = if group {
                Some(match (chat.status, is_channel) {
                    // A subscriber is read-only by design: a channel always shows its counts (Android applyReadOnlyMode).
                    (_, true) => trf(lang, Key::ChannelSubscribers, &[&members, &on]),
                    (GroupStatus::ReadOnly, false) => tr(lang, Key::ReadOnly).to_string(),
                    (_, false) => trf(lang, Key::MemberCount, &[&members, &on]),
                })
            } else if saved {
                None
            } else if online {
                Some(tr(lang, Key::Online).to_string())
            } else {
                last_seen_text(chat.last_seen, lang)
            };
            let title_font = FontId::new(14.5, FontFamily::Name("bold".into()));
            let sub_font = FontId::proportional(11.);
            // The one or two text lines are centred in the header height.
            let (title_h, sub_h) = ui.fonts_mut(|f| (f.row_height(&title_font), f.row_height(&sub_font)));
            let lines_h = title_h + status.as_ref().map_or(0., |_| sub_h);
            let text = ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.;
                ui.add_space(((44. - lines_h) / 2.).max(0.));
                ui.add(egui::Label::new(RichText::new(name).font(title_font).color(Color32::WHITE)).selectable(false));
                if let Some(s) = status {
                    let c = if online { Color32::from_rgb(0x9f, 0xe5, 0xc9) } else { Color32::from_white_alpha(160) };
                    ui.add(egui::Label::new(RichText::new(s).font(sub_font).color(c)).selectable(false));
                }
            });
            // Avatar, name and subtitle are one click target (Saved Messages has no info panel).
            if !saved {
                let area = ui.interact(av.union(text.response.rect), ui.scope_id().with("chat_title"), Sense::click());
                area.widget_info(|| WidgetInfo::labeled(Role::Button, true, name));
                toggle |= area.on_hover_cursor(egui::CursorIcon::PointingHand).clicked();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let (r, resp) = ui.allocate_exact_size(vec2(24., 28.), Sense::click());
                resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "⋮"));
                for dy in [-5., 0., 5.] {
                    ui.painter().circle_filled(r.center() + vec2(0., dy), 1.8, Color32::WHITE);
                }
                more = Some(resp);
                if let Some((enabled, label, hint)) = call_btn {
                    ui.add_space(8.);
                    let (r, resp) = ui.allocate_exact_size(vec2(28., 28.), if enabled { Sense::click() } else { Sense::hover() });
                    resp.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, label));
                    let color = if enabled { Color32::WHITE } else { Color32::from_white_alpha(90) };
                    call::phone_icon(ui, r.center(), 20., color, false);
                    let resp = resp.on_hover_text(hint);
                    calling = enabled && resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked();
                }
            });
        });
    });
    (toggle, more, calling)
}

fn handle(app: &mut MimirDesktop, ctx: &egui::Context, action: Action, m: &MsgRef) {
    let text = m.text.clone();
    match action {
        Action::Copy => ctx.copy_text(text),
        Action::Link(url) => crate::platform::handle(app, ctx, &crate::platform::Ext::Link(url)),
        Action::Reply(guid) => {
            app.state.reply_to = Some((guid, text));
            app.state.composer.focus();
        }
        Action::React(emoji) => app.state.pending_reaction = Some((m.guid, emoji)),
        Action::Forward(_) => app.state.forward = m.forward.clone().map(|forward| ForwardDialog { forward, query: String::new() }),
        Action::Delete(id) => app.state.confirm_delete = Some(id),
        Action::Download => download(app, m.guid),
        Action::Open => match &m.file {
            Some((path, _)) if path.is_file() => media::open_file(&app.worker, app.lang, path),
            _ => app.error_text(tr(app.lang, Key::MediaFileNotFound).into()),
        },
        Action::View => app.state.viewer = m.file.clone(),
        Action::Play => {
            if let Some((path, _)) = &m.file {
                app.player.toggle(m.guid, path.clone());
            }
        }
        Action::Seek(frac) => app.player.seek(m.guid, frac),
        Action::SaveFile => {
            if let Some((path, name)) = m.file.clone() {
                media::save_as(&app.worker, app.lang, path, name);
            }
        }
        Action::DeleteFile => {
            if let Some((path, _)) = m.file.clone() {
                // On the worker; the bubble re-checks the file every frame, and the thumbnail cache follows its mtime.
                let what = tr(app.lang, Key::DeleteFile);
                app.worker.submit(Box::new(move |_| match std::fs::remove_file(&path) {
                    Ok(()) => WorkResult::Done,
                    Err(e) => WorkResult::Error(format!("{what}: {e}")),
                }));
            }
        }
    }
}

/// Download click: marks the row at once (sibling fetches send no progress), then starts the fetch.
fn download(app: &mut MimirDesktop, guid: i64) {
    let Some(chat) = &app.state.open_chat else { return };
    let key = chat.key;
    let Some(meta) = chat.timeline.msgs.iter().find(|m| m.guid == guid).and_then(|m| AttachmentMeta::parse(m.data.as_deref()?)) else { return };
    // request_download fetches a group row only with its key, a 1:1 row only without one.
    if key.is_group() == meta.key.is_some() {
        app.state.transfers.insert(guid, Transfer::Downloading { done: 0, total: meta.size });
    }
    request_download(app, key, guid, &meta);
}

fn confirm_delete(app: &mut MimirDesktop, ctx: &egui::Context) {
    let Some(id) = app.state.confirm_delete else { return };
    let lang = app.lang;
    let (mut yes, mut no) = (false, false);
    let r = crate::ui::theme::dialog(ctx, Id::unique("confirm_delete")).show(ctx, |ui| {
        ui.set_width(260.);
        let p = palette(ui.visuals().dark_mode);
        crate::ui::theme::dialog_title(ui, tr(lang, Key::DeleteQuestion));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            yes = ui.add(danger_button(&p, tr(lang, Key::Delete))).clicked();
            no = ui.button(tr(lang, Key::Cancel)).clicked();
        });
    });
    if yes {
        if let Some(chat) = &mut app.state.open_chat {
            let t = &mut chat.timeline;
            if let Some(i) = t.msgs.iter().position(|m| m.id == id) {
                let m = t.msgs.remove(i);
                // ponytail: the local delete runs on the UI thread; it is a single DB row. A group row id is not a
                // 1:1 row id, so the group path goes by guid, and the server call goes through the worker.
                let file = match chat.key {
                    ChatKey::Group(gid) => crate::model::send::delete_group_message(&app.core, &app.group_worker, gid, m.guid),
                    ChatKey::Contact(_) => app.core.delete_message(id),
                };
                // The core hands back the attachment's storage name; the file goes with its row, on the
                // worker. ponytail: a failure leaves an orphan file (the orphan cleaner finds it).
                let meta = AttachmentMeta { name: file.unwrap_or_default(), ..Default::default() };
                if let Some(path) = local_path(&app.paths.files_dir, &meta) {
                    app.state.thumbs.invalidate(&path);
                    app.worker.submit(Box::new(move |_| {
                        let _ = std::fs::remove_file(&path);
                        WorkResult::Done
                    }));
                }
                if i < t.heights.len() { t.heights.remove(i); }
                chat.reactions.remove(&m.guid);
            }
        }
        app.state.chats = crate::model::chat_list::build_chat_list(&app.core, lang);
    }
    if yes || no || r.should_close() {
        app.state.confirm_delete = None;
    }
}

fn forward_dialog(app: &mut MimirDesktop, ctx: &egui::Context) {
    let Some(dlg) = &mut app.state.forward else { return };
    let lang = app.lang;
    let (mut picked, mut cancel) = (None, false);
    let p = palette(ctx.global_style().visuals.dark_mode);
    let r = crate::ui::theme::dialog(ctx, Id::unique("forward")).show(ctx, |ui| {
        ui.set_width(300.);
        crate::ui::theme::dialog_title(ui, tr(lang, Key::ForwardTo));
        ui.add(TextEdit::singleline(&mut dlg.query).hint_text(tr(lang, Key::Search)).desired_width(f32::INFINITY));
        let q = dlg.query.to_lowercase();
        ScrollArea::vertical().max_height(300.).show(ui, |ui| {
            // Only chats with a composer: no read-only group, no channel of somebody else's.
            let writable = |r: &&crate::model::chat_list::ChatRow| r.group.is_none_or(|g| g.can_post());
            for row in app.state.chats.iter().filter(writable).filter(|r| r.name.to_lowercase().contains(&q)) {
                let w = ui.available_width();
                let r = crate::ui::theme::click_row(ui, |ui| {
                    ui.set_min_width(w);
                    ui.horizontal(|ui| {
                        let path = media::avatar::resolve(&app.paths.core_dir, &row.avatar);
                        let thumbs = &mut app.state.thumbs;
                        match row.key {
                            ChatKey::SAVED => {
                                let (rect, _) = ui.allocate_exact_size(vec2(32., 32.), Sense::hover());
                                paint_saved(ui.painter(), rect, p.accent);
                            }
                            ChatKey::Group(id) => {
                                avatar::show(ui, thumbs, path.as_deref(), avatar_seed_color(id.to_string().as_bytes(), ui.visuals().dark_mode), 32., 3.);
                            }
                            ChatKey::Contact(_) => {
                                avatar::show(ui, thumbs, path.as_deref(), avatar_color(&row.pubkey), 32., 3.);
                            }
                        }
                        ui.add(egui::Label::new(&row.name).selectable(false));
                    });
                });
                r.response.widget_info(|| WidgetInfo::labeled(Role::Button, true, &row.name));
                if r.response.clicked() {
                    picked = Some(row.key);
                }
            }
        });
        ui.add_space(8.);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| cancel = ui.button(tr(lang, Key::Cancel)).clicked());
    });
    if let Some(id) = picked {
        app.state.pending_forward = Some((id, dlg.forward.clone()));
        app.state.forward = None;
    } else if cancel || r.should_close() {
        app.state.forward = None;
    }
}

/// No composer drawn, so no recording dialog either: a take in progress stops and is thrown away.
fn stop_takes(app: &mut MimirDesktop) {
    app.state.composer.video.reset();
    app.state.composer.voice.reset();
}

pub fn show(app: &mut MimirDesktop, ui: &mut Ui, p: &Palette) {
    let Some(key) = app.state.selected else {
        stop_takes(app);
        ui.centered_and_justified(|ui| ui.label(RichText::new(tr(app.lang, Key::SelectChat)).size(14.).color(p.text_dim)));
        return;
    };
    if app.state.open_chat.as_ref().map(|c| c.key) != Some(key) {
        let row = app.state.chats.iter().find(|r| r.key == key);
        let pubkey = match key {
            ChatKey::Contact(id) => row.map(|r| r.pubkey.clone()).or_else(|| app.core.get_contact(id).map(|c| c.pubkey)).filter(|k| crate::core::valid_key(k, "open a chat")),
            ChatKey::Group(_) => row.map(|_| vec![]), // a group has no peer key
        };
        let Some(pubkey) = pubkey else {
            app.state.selected = None;
            stop_takes(app);
            return;
        };
        composer::switch_chat(app, app.state.open_chat.as_ref().map(|c| c.key), key);
        let mut timeline = Timeline::open(&app.core, key);
        let (mut last_seen, mut members, mut status, mut counts) = (0, None, GroupStatus::default(), (0, 0));
        let unread_from = match key {
            ChatKey::Contact(id) => app.core.get_first_unread_id(id),
            ChatKey::Group(id) => app.core.get_first_unread_group_message_id(id),
        };
        if let Some(first) = unread_from {
            timeline.open_at(&app.core, first);
            // ponytail: a failed mark-read only leaves the unread badge until the next open.
            let _ = match key {
                ChatKey::Contact(id) => app.core.mark_all_read(id),
                ChatKey::Group(id) => app.core.mark_all_group_read(id),
            };
            app.state.chats = crate::model::chat_list::build_chat_list(&app.core, app.lang);
        }
        match key {
            ChatKey::Contact(id) => {
                if id != SAVED_ID {
                    net::connect(&app.worker, pubkey.clone());
                }
                last_seen = app.core.get_contact(id).map_or(0, |c| c.last_seen);
            }
            ChatKey::Group(id) => {
                members = Some(Members::load(&app.core, id));
                status = GroupStatus::parse(&app.core.get_group_chat_status(id));
                counts = group::counts(&app.core, id);
            }
        }
        app.state.open_chat = Some(OpenChat {
            key, pubkey, timeline, reactions: Default::default(), menu: None, last_seen, day_starts: Default::default(),
            day_key: (usize::MAX, 0, 0), unread_from, quotes: Default::default(), unread_while_away: false, members, status, counts,
        });
        app.state.reply_to = None;
    }
    let lang = app.lang;
    let ctx = ui.ctx().clone();
    let mut actions = vec![];
    // A read-only group (removed, banned, deleted) has no composer, nor has a channel for anyone but its owner (Android
    // hides the tools; the header still shows the subscriber count). Read-only by the stored flag or the subscription state.
    let group = app.state.chats.iter().find(|r| r.key == key).and_then(|r| r.group);
    let is_channel = group.is_some_and(|g| g.is_channel);
    let read_only = group.is_some_and(|g| g.read_only) || app.state.open_chat.as_ref().is_some_and(|c| c.key.is_group() && c.status == GroupStatus::ReadOnly);
    {
        // The call button: a contact's call (Saved Messages shows it disabled), or a group call in a group we can
        // write to (its invite is posted there).
        let call_btn = match key {
            ChatKey::SAVED => Some((false, tr(lang, Key::MakeCall), tr(lang, Key::MakeCall))),
            ChatKey::Contact(_) => {
                let (can, hint) = call::can_call(app);
                Some((can, tr(lang, Key::MakeCall), hint))
            }
            ChatKey::Group(_) if is_channel || read_only => None,
            ChatKey::Group(_) => {
                let (can, hint) = call::can_group_call(app);
                Some((can, tr(lang, Key::StartGroupCall), hint))
            }
        };
        let (toggle, more, calling) = header(ui, &mut app.state, &app.paths.core_dir, lang, p, call_btn);
        match key {
            ChatKey::Group(id) if calling => call::start_group(app, id),
            _ if calling => {
                let pubkey = app.state.open_chat.as_ref().map(|c| c.pubkey.clone()).unwrap_or_default();
                call::start(app, pubkey);
            }
            _ => {}
        }
        app.state.info_open ^= toggle;
        if let Some(more) = more {
            info_panel::menu(app, &more);
        }
    }
    if !read_only && group.is_none_or(|g| g.can_post()) {
        Panel::bottom("composer").resizable(false).show_separator_line(false).frame(Frame::NONE).show(ui, |ui| composer::show(app, ui, p));
    } else {
        stop_takes(app);
    }
    let core = app.core.clone();
    let name = app.state.chats.iter().find(|r| Some(r.key) == app.state.selected).map_or(String::new(), |r| r.name.clone());
    let chat = app.state.open_chat.as_mut().unwrap();
    let OpenChat { key: chat_key, timeline, reactions, menu, day_starts, day_key, unread_from, quotes, members, .. } = chat;
    // Recomputed only when the loaded messages change (open, load_older, new message, delete).
    let key = (timeline.msgs.len(), timeline.msgs.first().map_or(0, |m| m.id), timeline.msgs.last().map_or(0, |m| m.id));
    if *day_key != key {
        *day_key = key;
        day_starts.clear();
        let mut prev = None;
        for m in &timeline.msgs {
            let d = at(m.send_time).date();
            if prev != Some(d) { day_starts.insert(m.id); }
            prev = Some(d);
        }
        quotes.clear();
        for m in timeline.msgs.iter().filter(|m| m.reply_to != 0) {
            let quote = |t: &StoredMessage| (t.incoming, t.contact_id, text_of(t, lang).0);
            let found = timeline.msgs.iter().find(|t| t.guid == m.reply_to).map(quote);
            let stored = || match *chat_key {
                ChatKey::Contact(_) => core.get_message_by_guid(m.reply_to),
                ChatKey::Group(id) => core.get_group_message_by_guid(id, m.reply_to),
            };
            quotes.insert(m.reply_to, found.or_else(|| stored().as_ref().map(quote)));
        }
        if let Some(ms) = members.as_mut() {
            ms.learn(&core, &timeline.msgs);
        }
    }
    let mut cx = RowCx {
        core: &core, lang, p, key: *chat_key, day_starts, quotes, name: &name, members: members.as_ref(), unread_from: *unread_from, reactions, menu,
        actions: &mut actions, files: &app.paths.files_dir, core_dir: &app.paths.core_dir, transfers: &app.state.transfers, thumbs: &mut app.state.thumbs,
        posters: &mut app.state.posters, ffmpeg: app.state.ffmpeg.as_deref(), player: &app.player,
        font: app.settings.message_font,
    };
    show_timeline(ui, timeline, |t| t.load_older(&core), |ui, m| message_row(ui, m, &mut cx));
    if let Some(a) = message_menu::show(&ctx, menu, lang) {
        actions.push(a);
    }
    for (a, m) in actions {
        handle(app, &ctx, a, &m);
    }
    composer::send_pending(app);
    confirm_delete(app, &ctx);
    forward_dialog(app, &ctx);
    image_viewer::show(app, &ctx);
    info_panel::dialogs(app, &ctx);
}

#[cfg(test)]
mod tests {
    use crate::model::chat_key::ChatKey;
    use super::*;
    use crate::{model::{chat_list::SAVED_ID, message::reaction_payload}, settings::Settings, test_support::{test_harness, test_harness_tofu}};
    use egui_kittest::{kittest::Queryable as _, Harness};
    use crate::app::MimirDesktop;
    use time::{Date, Month, UtcOffset};

    fn chat_with(texts: &[&str]) -> Harness<'static, MimirDesktop> {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        for (i, t) in texts.iter().enumerate() {
            h.state().core.add_local_message(SAVED_ID, i as i64 + 1, 0, false, true, now_ms() - 1000, 0, 0, t.as_bytes().to_vec()).unwrap();
        }
        h.state_mut().state.selected = Some(ChatKey::SAVED);
        h.run();
        h
    }

    #[test]
    fn message_font_setting_resizes_bubbles() {
        // F1: the settings font reaches the chat; the row is laid out again at the new size.
        let mut h = chat_with(&["привет, это сообщение"]);
        h.run();
        let height = |h: &Harness<'static, MimirDesktop>| h.state().state.open_chat.as_ref().unwrap().timeline.heights[0];
        let medium = height(&h);
        crate::ui::settings::open(h.state_mut());
        h.state_mut().state.settings_ui.section = crate::ui::settings::Section::Font;
        h.run();
        h.get_by_role(egui::Role::Slider).set_value_accesskit(4.);
        h.run();
        h.run();
        assert_eq!(h.state().settings.message_font, 21.0);
        let large = height(&h);
        assert!(large > medium + 4., "21 px row {large} vs 15 px row {medium}");
    }

    fn ts(y: i32, m: u8, d: u8, h: u8) -> i64 {
        let off = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
        Date::from_calendar_date(y, Month::try_from(m).unwrap(), d).unwrap().with_hms(h, 0, 0).unwrap().assume_offset(off).unix_timestamp() * 1000
    }
/// Group 7 «Семья» with members Аня (pk 1, online), Митя (pk 2) and us («Я»); `setup` adds rows before it opens.
    fn group_chat(flags: i32, setup: impl FnOnce(&MimirApp)) -> Harness<'static, MimirDesktop> {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        let core = h.state().core.clone();
        core.save_group_chat(7, "Семья".into(), String::new(), None, vec![9; 32], vec![], vec![3; 32], flags).unwrap();
        for (k, n) in [(vec![1; 32], "Аня"), (vec![2; 32], "Митя"), (core.public_key(), "Я")] {
            core.update_member_info(7, k, Some(n.into()), None, None).unwrap();
        }
        core.update_member_online(7, vec![1; 32], true, 0).unwrap();
        setup(&core);
        h.state_mut().state.chats = crate::model::chat_list::build_chat_list(&core, Lang::Ru);
        h.state_mut().state.selected = Some(ChatKey::Group(7));
        h.run();
        h
    }

    /// Colour of the first text shape painted with exactly `text`.
    fn text_color(h: &Harness<'static, MimirDesktop>, text: &str) -> Option<Color32> {
        h.output().shapes.iter().find_map(|cs| match &cs.shape {
            Shape::Text(t) if t.galley.text() == text => t.galley.job.sections.first().map(|s| s.format.color),
            _ => None,
        })
    }

    fn chat_label(h: &Harness<'static, MimirDesktop>, text: &str) -> Option<Rect> {
        // The chat list (left) may show the same text as a preview.
        h.query_all_by_label(text).map(|n| n.rect()).find(|r| r.left() > 250.)
    }

    fn system(core: &MimirApp, guid: i64, t: i64, data: Vec<u8>) {
        core.add_group_message(7, guid, guid, vec![9; 32], t, crate::model::group::SYSTEM, true, data, 0, false).unwrap();
    }

    #[test] fn group_bubbles_show_sender_and_system_rows() {
        let t = now_ms() - 10_000;
        let mut h = group_chat(0, |core| {
            core.add_group_message(7, 1, 11, vec![1; 32], t, 0, false, "привет".as_bytes().to_vec(), 0, false).unwrap();
            core.add_group_message(7, 2, 12, core.public_key(), t + 1, 0, false, "мой".as_bytes().to_vec(), 0, false).unwrap();
            let mut left = vec![0x03];
            left.extend([2; 32]);
            left.extend([0; 32]);
            system(core, 3, t + 2, left);
            let mut deleted = vec![0x08];
            deleted.extend(11i64.to_be_bytes());
            deleted.extend([1; 32]);
            system(core, 4, t + 3, deleted);
            core.add_group_message(7, 5, 15, vec![5; 32], t + 4, 0, false, "кто я".as_bytes().to_vec(), 0, false).unwrap();
            core.mark_all_group_read(7).unwrap();
        });
        h.run();
        let dark = h.ctx.global_style().visuals.dark_mode;
        let name = chat_label(&h, "Аня").expect("sender name");
        let text = chat_label(&h, "привет").unwrap();
        let glyphs = h.output().shapes.iter().find_map(|cs| match &cs.shape {
            Shape::Text(t) if t.galley.text() == "привет" => Some(t.pos),
            _ => None,
        }).unwrap();
        assert!(text.contains_rect(name) && name.bottom() <= glyphs.y + 1., "name {name:?} in the bubble {text:?}, above the text at {glyphs:?}");
        assert_eq!(text_color(&h, "Аня"), Some(crate::ui::avatar::avatar_seed_color(&[1; 32], dark)));
        assert!(chat_label(&h, "Я").is_none(), "our own bubble has no name");
        assert!(chat_label(&h, "Неизвестный").is_some(), "sender without a member row");
        assert_eq!(text_color(&h, "Неизвестный"), Some(palette(dark).text_dim), "neutral colour");
        let sys = chat_label(&h, "Митя покинул(а) чат").expect("system row");
        let mine = chat_label(&h, "мой").unwrap();
        assert!((sys.center().x - (text.left() + mine.right()) / 2.).abs() < 30., "centred: {sys:?} between {text:?} and {mine:?}");
        assert_eq!(h.state().state.open_chat.as_ref().unwrap().timeline.msgs.len(), 4, "MessageDeleted row not shown");
    }

    #[test] fn group_header_title_subtitle_and_status_badge() {
        let mut h = group_chat(0, |_| {});
        assert!(h.query_by_role_and_label(Role::Label, "Семья").is_some_and(|n| n.rect().left() > 250.), "title");
        assert!(h.query_by_label("3 участников, 1 в сети").is_some(), "member_count");
        for s in [GroupStatus::Connecting, GroupStatus::Subscribed, GroupStatus::Disconnected, GroupStatus::ReadOnly] {
            h.state_mut().state.open_chat.as_mut().unwrap().status = s;
            h.run();
            let fills: Vec<Color32> = h.output().shapes.iter().filter_map(|cs| match &cs.shape {
                Shape::Circle(c) => Some(c.fill),
                _ => None,
            }).collect();
            assert!(fills.contains(&status_color(s)), "{s:?} badge");
        }
        let [red, yellow, green, grey] = [GroupStatus::Disconnected, GroupStatus::Connecting, GroupStatus::Subscribed, GroupStatus::ReadOnly].map(status_color);
        assert!(red.r() > red.g() && yellow.r() > yellow.b() && yellow.g() > yellow.b() && green.g() > green.r());
        assert!(grey.r() == grey.g() && grey.g() == grey.b());
    }

    #[test] fn read_only_hides_composer() {
        let h = group_chat(0, |core| core.set_group_chat_read_only(7, true).unwrap());
        assert!(h.query_by_role(Role::MultilineTextInput).is_none(), "no composer");
        assert!(h.query_by_label("Только чтение").is_some());
        assert_eq!(h.state().state.open_chat.as_ref().unwrap().status, GroupStatus::ReadOnly);
    }

    #[test] fn take_stops_when_the_composer_goes() {
        // The group turns read-only mid-take (deleted elsewhere): the dialog went with the composer, so must the take.
        let mut h = group_chat(0, |_| {});
        h.state_mut().state.core_started = true;
        h.state_mut().state.ffmpeg = Some("ffmpeg".into());
        let file = std::rc::Rc::new(std::cell::RefCell::new(None));
        let seen = file.clone();
        crate::ui::video_rec::set_camera(move |out| {
            *seen.borrow_mut() = Some(out.clone());
            crate::media::camera::Camera::fake(5_000, &["cam"], out)
        });
        h.run_steps(2);
        h.get_by_label("Записать видеосообщение").click();
        h.run_steps(2);
        assert!(h.state().state.composer.video.recording());
        h.state_mut().state.open_chat.as_mut().unwrap().status = GroupStatus::ReadOnly;
        h.run_steps(2);
        assert!(!h.state().state.composer.video.recording(), "no composer: the take is cancelled");
        let f = file.borrow().clone().unwrap();
        crate::test_support::run_until(&mut h, "the take's file is deleted", |_| !f.exists());
    }

    #[test] fn group_reply_quotes_name_the_member() {
        let t = now_ms() - 10_000;
        let h = group_chat(0, |core| {
            core.add_group_message(7, 1, 11, vec![1; 32], t, 0, false, "привет".as_bytes().to_vec(), 0, false).unwrap();
            core.add_group_message(7, 2, 12, core.public_key(), t + 1, 0, false, "мой".as_bytes().to_vec(), 11, false).unwrap();
            core.add_group_message(7, 3, 13, vec![2; 32], t + 2, 0, false, "ага".as_bytes().to_vec(), 12, false).unwrap();
            core.mark_all_group_read(7).unwrap();
        });
        assert!(h.query_by_label("Аня: привет").is_some(), "my reply quotes Аня");
        assert!(h.query_by_label("Вы: мой").is_some(), "Митя's reply quotes me");
    }

    #[test] fn channel_composer_is_the_owners_only() {
        // A subscriber (somebody else's channel, read-only by design): no composer, the counts stay in the header.
        let mut h = group_chat(1, |core| core.set_group_chat_read_only(7, true).unwrap());
        h.state_mut().state.open_chat.as_mut().unwrap().counts = (40, 3); // as Event::ChatStats sets them
        h.run();
        assert!(h.query_by_role(Role::MultilineTextInput).is_none(), "no composer for a subscriber");
        assert!(h.query_by_label("40 подписчиков, 3 в сети").is_some(), "subscribers, not «Только чтение»");
        assert!(h.query_by_label("Только чтение").is_none());
        assert!(h.query_by_label_contains("в следующей версии").is_none(), "the v1 placeholder is gone");
        // Our own channel: the owner posts.
        let h = group_chat(1, |core| {
            core.save_group_chat(7, "Семья".into(), String::new(), None, vec![9; 32], core.public_key(), vec![3; 32], 1).unwrap();
        });
        assert!(h.query_by_role(Role::MultilineTextInput).is_some(), "the owner has a composer");
        assert!(h.query_by_label("0 подписчиков, 0 в сети").is_some());
    }

    #[test] fn forward_dialog_lists_writable_groups_only() {
        let mut h = group_chat(0, |core| {
            core.save_group_chat(8, "Канал".into(), String::new(), None, vec![9; 32], vec![], vec![3; 32], 1).unwrap();
            core.save_group_chat(9, "Старая".into(), String::new(), None, vec![9; 32], vec![], vec![3; 32], 0).unwrap();
            core.save_group_chat(10, "Мой канал".into(), String::new(), None, vec![9; 32], core.public_key(), vec![3; 32], 1).unwrap();
            core.set_group_chat_read_only(9, true).unwrap();
            core.add_group_message(7, 1, 11, vec![1; 32], now_ms() - 10_000, 0, false, "привет".as_bytes().to_vec(), 0, false).unwrap();
        });
        h.get_by_label("привет").click_secondary();
        h.run();
        h.get_by_label_contains("Переслать").click();
        h.run();
        assert!(h.state().state.forward.is_some());
        assert!(h.query_all_by_label("Семья").count() >= 2, "listed in the dialog as well as the chat list");
        // Each listed once (the chat list) unless the dialog lists it too; a dialog row starts at x = 250.
        let count = |t: &str| h.query_all_by_label(t).count();
        assert!(count("Канал") == 1 && count("Старая") == 1, "no channel of somebody else's, no read-only group");
        assert!(h.query_all_by_label("Мой канал").count() >= 2, "our own channel takes posts: listed in the dialog too");
    }

    #[test] fn forward_rows_show_avatars() {
        let mut h = group_chat(0, |core| {
            let id = core.add_contact(vec![5; 32], "Аня".into()).unwrap();
            core.update_contact_avatar(id, crate::test_support::png_bytes(64, 64, [200, 30, 30])).unwrap();
            core.add_group_message(7, 1, 11, vec![1; 32], now_ms() - 10_000, 0, false, "привет".as_bytes().to_vec(), 0, false).unwrap();
        });
        h.state_mut().state.chats = crate::model::chat_list::build_chat_list(&h.state().core.clone(), Lang::Ru);
        h.get_by_label("привет").click_secondary();
        h.run();
        h.get_by_label_contains("Переслать").click();
        h.run();
        crate::test_support::run_until(&mut h, "the forward list's picture", |h| crate::test_support::textured(h).contains(&vec2(32., 32.)));
    }

    #[test] fn forward_rows_highlight_on_hover() {
        let mut h = group_chat(0, |core| {
            core.add_group_message(7, 1, 11, vec![1; 32], now_ms() - 10_000, 0, false, "привет".as_bytes().to_vec(), 0, false).unwrap();
        });
        h.get_by_label("привет").click_secondary();
        h.run();
        h.get_by_label_contains("Переслать").click();
        h.run();
        // The dialog's row: right of the chat list, below the chat header.
        let in_dialog = |r: egui::Rect| r.left() > 100. && r.top() > 60.;
        let row = |h: &Harness<'static, MimirDesktop>| h.query_all_by_label("Семья").map(|n| n.rect()).find(|r| in_dialog(*r)).expect("the dialog's row");
        let divider = crate::ui::theme::palette(h.ctx.global_style().visuals.dark_mode).divider;
        let lit = |h: &Harness<'static, MimirDesktop>| {
            let r = row(h);
            h.output().shapes.iter().any(|cs| matches!(&cs.shape, egui::epaint::Shape::Rect(s) if s.fill == divider && s.rect.contains(r.center())))
        };
        assert!(!lit(&h), "not before the pointer is on it");
        h.query_all_by_label("Семья").find(|n| in_dialog(n.rect())).unwrap().hover();
        h.run();
        assert!(lit(&h), "the chat list's hover fill");
    }

    #[test] fn delete_in_group_never_touches_a_contact_message() {
        // Group row ids and 1:1 row ids are separate spaces: delete goes by guid.
        let t = now_ms() - 10_000;
        let mut h = group_chat(0, |core| {
            core.add_group_message(7, 1, 11, core.public_key(), t, 0, false, "мой".as_bytes().to_vec(), 0, false).unwrap();
            core.add_local_message(SAVED_ID, 1, 0, false, true, t, 0, 0, b"saved".to_vec()).unwrap();
        });
        h.get_by_label("мой").click_secondary();
        h.run();
        h.get_by_label_contains("Удалить Del").click();
        h.run();
        assert_eq!(h.state().state.confirm_delete, Some(1), "asks first");
        h.get_by_label("Удалить").click();
        h.run();
        assert!(h.state().core.get_group_message_by_guid(7, 11).is_none(), "group row deleted");
        assert!(h.state().state.open_chat.as_ref().unwrap().timeline.msgs.is_empty());
        assert_eq!(h.state().core.get_messages(SAVED_ID, i64::MAX, 10).len(), 1, "Saved row with the same id kept");
    }

    #[test] fn saved_header_title_is_vertically_centred() {
        let h = chat_with(&["hello"]);
        let title = h.get_by_role_and_label(Role::Label, "Избранное").rect();
        assert!((title.center().y - 22.).abs() < 2., "title centre {}", title.center().y);
    }
    #[test] fn day_labels() {
        let now = ts(2026, 11, 20, 12);
        assert_eq!(day_label(now, now, Lang::Ru), "Сегодня");
        assert_eq!(day_label(ts(2026, 11, 19, 23), now, Lang::Ru), "Вчера");
        assert_eq!(day_label(ts(2026, 11, 12, 9), now, Lang::Ru), "12 ноября");
    }

    #[test] fn right_click_opens_menu() {
        let mut h = chat_with(&["hello"]);
        h.get_by_label("hello").click_secondary();
        h.run();
        assert!(h.query_by_label_contains("Ответить").is_some());
        assert!(h.query_by_label("👍").is_some());
    }
    #[test] fn hover_shows_mini_toolbar() {
        let mut h = chat_with(&["hello"]);
        assert!(h.query_by_label("↩").is_none());
        h.get_by_label("hello").hover();
        h.run();
        assert!(h.query_by_label("↩").is_some());
    }
    #[test] fn reaction_chip_with_zwj_emoji_renders() {
        let mut h = chat_with(&["hello"]);
        h.state().core.add_local_message(SAVED_ID, 100, 1, false, true, now_ms(), 0, 10, reaction_payload("❤️‍🔥", 1)).unwrap();
        h.state_mut().state.open_chat = None;
        h.run();
        assert!(h.query_by_label("❤️‍🔥").is_some());
    }
    #[test] fn long_unbroken_text_stays_within_max_width() {
        let long = "a".repeat(2000);
        let h = chat_with(&[&long]);
        let w = h.get_by_label(&long).rect().width();
        assert!(w <= max_width(800.), "bubble width {w}");
        assert!(w > 100.);
    }
    #[test] fn reply_action_stores_reply_target() {
        let mut h = chat_with(&["hello"]);
        h.get_by_label("hello").click_secondary();
        h.run();
        h.get_by_label_contains("Ответить").click();
        h.run();
        assert_eq!(h.state().state.reply_to, Some((1, "hello".to_string())));
        assert!(h.query_by_label_contains("Ответить").is_none());
    }
    #[test] fn reply_moves_focus_to_composer() {
        // Menu item, R hotkey and the hover toolbar's ↩ all put the cursor in the composer.
        for how in ["menu", "hotkey", "toolbar"] {
            let mut h = chat_with(&["hello"]);
            match how {
                "toolbar" => {
                    h.get_by_label("hello").hover();
                    h.run();
                    h.get_by_label("↩").click();
                }
                _ => {
                    h.get_by_label("hello").click_secondary();
                    h.run();
                    if how == "menu" { h.get_by_label_contains("Ответить").click() } else { h.key_press(egui::Key::R) }
                }
            }
            h.run();
            assert!(h.state().state.reply_to.is_some(), "{how}");
            assert!(h.get_by_role(Role::MultilineTextInput).is_focused(), "{how}: composer focused");
            h.event(egui::Event::Text("ok".into()));
            h.run();
            assert_eq!(h.state().state.composer.text, "ok", "{how}: typing goes to the composer");
        }
    }
    #[test] fn clicking_header_title_or_subtitle_opens_info() {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        let core = h.state().core.clone();
        let c = core.add_contact(vec![7; 32], "Аня".into()).unwrap();
        h.state_mut().state.chats = crate::model::chat_list::build_chat_list(&core, Lang::Ru);
        h.state_mut().state.online.insert(vec![7; 32]);
        h.state_mut().state.selected = Some(ChatKey::Contact(c));
        h.run();
        h.get_by_label("в сети").click();
        h.run();
        assert!(h.state().state.info_open, "subtitle click opens the info panel");
        h.state_mut().state.info_open = false;
        h.run();
        h.get_by_role_and_label(Role::Label, "Аня").click();
        h.run();
        assert!(h.state().state.info_open, "title click opens the info panel");
        h.state_mut().state.info_open = false;
        h.run();
        // The click target is an accessible button named after the contact; its left end is the avatar.
        let header = h.query_all_by_role_and_label(Role::Button, "Аня").map(|n| n.rect()).find(|r| r.left() > 200.).expect("header button");
        let avatar = pos2(header.left() + 14., header.center().y);
        h.hover_at(avatar);
        for pressed in [true, false] {
            h.event(egui::Event::PointerButton { pos: avatar, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() });
        }
        h.run();
        assert!(h.state().state.info_open, "avatar click opens the info panel");
    }
    #[test] fn nameless_contact_shows_short_key_in_header_and_quotes() {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        let core = h.state().core.clone();
        let c = core.add_contact(vec![0xab; 32], String::new()).unwrap();
        core.add_local_message(c, 1, 0, false, true, now_ms() - 2000, 0, 0, b"mine".to_vec()).unwrap();
        core.add_local_message(c, 2, 0, true, true, now_ms() - 1500, 0, 0, b"theirs".to_vec()).unwrap();
        core.add_local_message(c, 3, 2, false, true, now_ms() - 1000, 0, 0, b"re".to_vec()).unwrap();
        h.state_mut().state.chats = crate::model::chat_list::build_chat_list(&core, Lang::Ru);
        h.state_mut().state.selected = Some(ChatKey::Contact(c));
        h.run();
        assert!(h.query_by_role_and_label(Role::Label, "abababab").is_some(), "header title");
        assert!(h.query_by_label("abababab: theirs").is_some(), "reply quote names the contact");
    }
    #[test] fn saved_header_click_does_nothing() {
        let mut h = chat_with(&[]);
        h.get_by_role_and_label(Role::Label, "Избранное").click();
        h.run();
        assert!(!h.state().state.info_open);
    }
    #[test] fn react_action_sends_reaction() {
        let mut h = chat_with(&["hello"]);
        h.state_mut().state.core_started = true; // Saved reactions are local, but sends wait for the core
        h.get_by_label("hello").click_secondary();
        h.run();
        h.get_by_label("🔥").click();
        h.run();
        assert_eq!(h.state().state.pending_reaction, None, "consumed by the composer");
        assert_eq!(h.state().core.get_user_reaction(SAVED_ID, 1).as_deref(), Some("🔥"));
    }
    #[test] fn forward_dialog_has_cancel() {
        let mut h = chat_with(&["hello"]);
        h.get_by_label("hello").click_secondary();
        h.run();
        h.get_by_label_contains("Переслать").click();
        h.run();
        assert!(h.state().state.forward.is_some());
        h.get_by_label("Отмена").click();
        h.run();
        assert!(h.state().state.forward.is_none());
        assert!(h.state().state.pending_forward.is_none(), "nothing sent");
    }
    #[test] fn delete_asks_then_removes() {
        let mut h = chat_with(&["hello", "world"]);
        h.get_by_label("hello").click_secondary();
        h.run();
        h.get_by_label_contains("Удалить Del").click();
        h.run();
        assert_eq!(crate::test_support::button_fill(&h, "Удалить"), Some(crate::ui::theme::palette(true).danger), "red, not the accent");
        assert_eq!(h.state().state.open_chat.as_ref().unwrap().timeline.msgs.len(), 2, "not before confirm");
        h.get_by_label("Удалить").click();
        h.run();
        let chat = h.state().state.open_chat.as_ref().unwrap();
        assert_eq!(chat.timeline.msgs.len(), 1);
        assert!(h.query_by_label("hello").is_none());
    }
    #[test] fn opens_at_first_unread() {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        let core = h.state().core.clone();
        let c = core.add_contact(vec![7; 32], "Аня".into()).unwrap();
        let ids: Vec<i64> = (0..5).map(|i| core.add_local_message(c, i + 1, 0, true, true, now_ms() - 1000 + i, 0, 0, format!("m{i}").into_bytes()).unwrap()).collect();
        for id in &ids[..3] {
            core.set_message_read(c, *id, true).unwrap();
        }
        assert_eq!(core.get_unread_count(c), 2);
        h.state_mut().state.chats = crate::model::chat_list::build_chat_list(&core, Lang::Ru);
        h.state_mut().state.selected = Some(ChatKey::Contact(c));
        h.run();
        assert_eq!(h.state().state.open_chat.as_ref().unwrap().unread_from, Some(ids[3]));
        let sep = h.get_by_label("Непрочитанные сообщения").rect();
        assert!(sep.top() >= h.get_by_label("m2").rect().bottom() && sep.bottom() <= h.get_by_label("m3").rect().top(), "separator between 3rd and 4th");
        assert_eq!(core.get_unread_count(c), 0);
        assert_eq!(h.state().state.chats.iter().find(|r| r.key == ChatKey::Contact(c)).unwrap().unread, 0);
    }
    #[test] fn unread_separator_far_down_is_scrolled_into_view() {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        let core = h.state().core.clone();
        let c = core.add_contact(vec![7; 32], "Аня".into()).unwrap();
        for i in 0..60 {
            let id = core.add_local_message(c, i + 1, 0, true, true, now_ms() - 1000 + i, 0, 0, format!("m{i}").into_bytes()).unwrap();
            if i < 54 {
                core.set_message_read(c, id, true).unwrap();
            }
        }
        h.state_mut().state.chats = crate::model::chat_list::build_chat_list(&core, Lang::Ru);
        h.state_mut().state.selected = Some(ChatKey::Contact(c));
        h.run();
        let sep = h.query_by_label("Непрочитанные сообщения").expect("separator drawn").rect();
        let screen = h.ctx.content_rect();
        assert!(screen.contains_rect(sep), "separator {sep:?} outside {screen:?}");
    }
    #[test] fn reply_shows_quote_of_its_target() {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        let core = h.state().core.clone();
        let c = core.add_contact(vec![7; 32], "Аня".into()).unwrap();
        let t = now_ms() - 100_000;
        core.add_local_message(c, 1, 0, false, true, t, 0, 0, b"first".to_vec()).unwrap(); // outside the loaded page
        for i in 0..55 {
            core.add_local_message(c, 10 + i, 0, true, true, t + 1 + i, 0, 0, format!("f{i}").into_bytes()).unwrap();
        }
        core.add_local_message(c, 100, 0, false, true, t + 100, 0, 0, b"hello".to_vec()).unwrap();
        core.add_local_message(c, 101, 100, true, true, t + 101, 0, 0, "ответ".as_bytes().to_vec()).unwrap();
        core.add_local_message(c, 102, 101, false, true, t + 102, 0, 0, b"ok".to_vec()).unwrap();
        core.add_local_message(c, 103, 1, true, true, t + 103, 0, 0, b"old".to_vec()).unwrap();
        core.mark_all_read(c).unwrap(); // open at the bottom, not at the first unread
        h.state_mut().state.chats = crate::model::chat_list::build_chat_list(&core, Lang::Ru);
        h.state_mut().state.selected = Some(ChatKey::Contact(c));
        h.run();
        assert!(h.query_by_label("Вы: hello").is_some(), "incoming reply quotes my loaded message");
        assert!(h.query_by_label("Аня: ответ").is_some(), "my reply quotes the contact");
        assert!(h.query_by_label("Вы: first").is_some(), "target outside the page comes from the core");
        let quote = h.get_by_label("Вы: hello").rect();
        assert!(quote.bottom() <= h.get_by_label("ответ").rect().center().y, "quote sits above the text");
    }
    #[test] fn each_chat_keeps_its_own_scroll() {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        let core = h.state().core.clone();
        let c = core.add_contact(vec![7; 32], "Аня".into()).unwrap();
        for i in 0..100 {
            core.add_local_message(c, i + 1, 0, false, true, now_ms() - 1000 + i, 0, 0, format!("a{i}").into_bytes()).unwrap();
            core.add_local_message(SAVED_ID, 1000 + i, 0, false, true, now_ms() - 1000 + i, 0, 0, format!("s{i}").into_bytes()).unwrap();
        }
        h.state_mut().state.chats = crate::model::chat_list::build_chat_list(&core, Lang::Ru);
        h.state_mut().state.selected = Some(ChatKey::Contact(c));
        h.run();
        h.hover_at(pos2(500., 300.));
        h.event(egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(0., 600.), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown, modifiers: Default::default() });
        h.run_steps(40);
        assert!(!h.state().state.open_chat.as_ref().unwrap().timeline.at_bottom, "chat A scrolled up");
        h.state_mut().state.selected = Some(ChatKey::SAVED);
        h.run_steps(10);
        assert!(h.state().state.open_chat.as_ref().unwrap().timeline.at_bottom, "chat B opens at the bottom");
        assert!(h.query_by_label("s99").is_some());
    }
    #[test] fn chat_without_a_valid_key_is_not_opened() {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        h.state_mut().state.selected = Some(ChatKey::Contact(12345)); // no chat row, no contact
        h.run();
        assert!(h.state().state.open_chat.is_none());
        assert_eq!(h.state().state.selected, None);
    }
    #[test] fn exotic_text_does_not_panic_with_tofu() {
        let mut h = test_harness_tofu(Settings { intro_done: true, ..Default::default() });
        let core = h.state().core.clone();
        let c = core.add_contact(vec![7; 32], "Аня".into()).unwrap();
        core.add_local_message(c, 1, 0, true, true, now_ms(), 0, 0, "漢字 ⋯ ＋ ✓ \u{10FFFF}".as_bytes().to_vec()).unwrap();
        core.add_local_message(c, 2, 0, true, true, now_ms(), 0, 1, vec![0xff, 0xfe, b'{']).unwrap(); // invalid body
        h.state_mut().state.selected = Some(ChatKey::Contact(c));
        h.run();
        assert_eq!(h.state().state.open_chat.as_ref().unwrap().timeline.msgs.len(), 2);
    }

    #[test] fn delete_message_removes_file() {
        let pdf = |name: &str| format!(r#"{{"name":"{name}","originalName":"a.pdf","size":5,"hash":"ab","mimeType":"application/pdf"}}"#).into_bytes();
        // 1:1 (Saved Messages): delete_message hands back the storage name.
        let mut h = chat_with(&[]);
        let core = h.state().core.clone();
        let id = core.add_local_message(SAVED_ID, 5, 0, false, true, now_ms() - 1000, 0, 3, pdf("Aa.pdf")).unwrap();
        let files = h.state().paths.files_dir.clone();
        std::fs::create_dir_all(&files).unwrap();
        std::fs::write(files.join("Aa.pdf"), b"12345").unwrap();
        std::fs::write(files.join("Keep.pdf"), b"12345").unwrap();
        h.state_mut().state.open_chat = None; // reopen with the row loaded
        h.run();
        h.state_mut().state.confirm_delete = Some(id);
        h.run();
        h.get_by_label("Удалить").click();
        h.run();
        assert!(!files.join("Aa.pdf").exists(), "the file goes with its row");
        assert!(files.join("Keep.pdf").exists(), "other files stay");
        // Group: delete_group_message_by_guid does the same.
        let mut h = group_chat(0, |core| {
            core.add_group_message(7, 1, 11, core.public_key(), now_ms() - 1000, 3, false, pdf("Bb.pdf"), 0, false).unwrap();
        });
        let files = h.state().paths.files_dir.clone();
        std::fs::create_dir_all(&files).unwrap();
        std::fs::write(files.join("Bb.pdf"), b"12345").unwrap();
        let id = h.state().state.open_chat.as_ref().unwrap().timeline.msgs[0].id;
        h.state_mut().state.confirm_delete = Some(id);
        h.run();
        h.get_by_label("Удалить").click();
        h.run();
        assert!(h.state().core.get_group_message_by_guid(7, 11).is_none());
        assert!(!files.join("Bb.pdf").exists());
    }
}
