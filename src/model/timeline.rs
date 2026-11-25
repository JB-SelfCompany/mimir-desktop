//! Chat timeline: one page of 50 messages, older pages on demand, row heights for virtualization.
use crate::model::{chat_key::ChatKey, group::{group_to_stored, hidden}};
use eframe::egui;
use mimir::{MimirApp, StoredMessage};
use std::ops::Range;

const PAGE: i32 = 50;
const EST_HEIGHT: f32 = 48.0;
pub const REACTION: i32 = 10;

pub struct Timeline {
    pub key: ChatKey,
    pub msgs: Vec<StoredMessage>,
    pub heights: Vec<f32>,
    pub at_bottom: bool,
    pub has_more: bool,
    oldest_id: i64,
    armed: bool,
    pending_offset: Option<f32>,
    /// Height just prepended: egui clamps a new offset by last frame's content size, so first grow the content
    /// (old rows shown at the top, padding at the bottom), and only then move the offset.
    settle: Option<f32>,
    /// Just opened: the first frame drops the scroll state egui kept from this chat's last visit.
    fresh: bool,
}

impl Timeline {
    pub fn empty() -> Timeline {
        Timeline { key: ChatKey::Contact(0), msgs: vec![], heights: vec![], at_bottom: true, has_more: false, oldest_id: i64::MAX, armed: false, pending_offset: None, settle: None, fresh: false }
    }

    /// Forgets the measured row heights (the message font changed): rows on screen are re-measured next frame,
    /// the others count as estimates until scrolled to.
    pub fn invalidate_heights(&mut self) {
        self.heights.fill(EST_HEIGHT);
    }

    pub fn open(app: &MimirApp, key: ChatKey) -> Timeline {
        let mut t = Timeline { key, fresh: true, ..Timeline::empty() };
        t.load_before(app, i64::MAX);
        // A page of only reactions shows nothing (and cannot be scrolled to load more): keep going.
        while t.msgs.is_empty() && t.has_more {
            t.load_older(app);
        }
        t
    }

    pub fn load_older(&mut self, app: &MimirApp) {
        self.load_before(app, self.oldest_id);
    }

    /// Up to a page of rows older than `before_id`, newest first, reactions included.
    fn page(&self, app: &MimirApp, before_id: i64) -> Vec<StoredMessage> {
        match self.key {
            ChatKey::Contact(id) => app.get_messages(id, before_id, PAGE),
            ChatKey::Group(id) => app.get_group_messages(id, before_id, PAGE).iter().map(group_to_stored).collect(),
        }
    }

    fn load_before(&mut self, app: &MimirApp, before_id: i64) {
        let mut page = self.page(app, before_id);
        self.has_more = page.len() == PAGE as usize;
        if let Some(last) = page.last() { self.oldest_id = last.id; }
        page.reverse();
        page.retain(|m| !hidden(m.msg_type, m.data.as_deref()));
        self.heights.splice(0..0, std::iter::repeat(EST_HEIGHT).take(page.len()));
        self.msgs.splice(0..0, page);
    }

    /// Rows intersecting [scroll_top, scroll_top + view_h).
    pub fn visible_range(&self, scroll_top: f32, view_h: f32) -> Range<usize> {
        let bottom = scroll_top + view_h;
        let (mut y, mut start, mut end) = (0.0, None, 0);
        for (i, h) in self.heights.iter().enumerate() {
            if start.is_none() && y + h > scroll_top { start = Some(i); }
            if y >= bottom { break; }
            y += h;
            end = i + 1;
        }
        start.unwrap_or(self.heights.len())..end.max(start.unwrap_or(0))
    }

    /// Loads older pages until message `id` is in, then opens the view at it instead of at the bottom.
    // ponytail: offset from estimated row heights, so the row lands near (not exactly at) the top;
    // a first unread thousands of messages back loads all of them.
    pub fn open_at(&mut self, app: &MimirApp, id: i64) {
        while self.has_more && self.msgs.first().is_some_and(|m| m.id > id) {
            self.load_older(app);
        }
        if let Some(i) = self.msgs.iter().position(|m| m.id == id) {
            self.at_bottom = false;
            self.pending_offset = Some(self.heights[..i].iter().sum());
        }
    }

    /// Appends rows stored after the newest loaded one that arrived without their own event (group system rows).
    // ponytail: reads one page; more than a page of new rows leaves a gap until the chat is reopened.
    pub fn load_newer(&mut self, app: &MimirApp) {
        let last = self.msgs.last().map_or(0, |m| m.id);
        let mut page = self.page(app, i64::MAX);
        page.retain(|m| m.id > last && !hidden(m.msg_type, m.data.as_deref()));
        for m in page.into_iter().rev() {
            self.on_new(m);
        }
    }

    /// Appends a live message only if it is newer than the last loaded row (the chat may have opened, and
    /// loaded the row, between its store and its event); true when it was added.
    pub fn push_if_newer(&mut self, m: Option<StoredMessage>) -> bool {
        match m.filter(|m| self.msgs.last().is_none_or(|l| l.id < m.id)) {
            Some(m) => { self.on_new(m); true }
            None => false,
        }
    }

    /// Appends a live message; true when the view should scroll to the bottom.
    pub fn on_new(&mut self, m: StoredMessage) -> bool {
        if m.msg_type == REACTION { return false; }
        self.msgs.push(m);
        self.heights.push(EST_HEIGHT);
        self.at_bottom
    }
}

/// Renders only the visible rows; the caller supplies the bubble row.
/// load_older fires once per arrival at the top (re-armed after scrolling away); the scroll offset is
/// then shifted by the new rows' heights so the reader keeps their place. Rows that fit the viewport (a very tall
/// window) cannot be scrolled up to arm it, so older pages load until they fill it.
pub fn show_timeline(
    ui: &mut egui::Ui,
    t: &mut Timeline,
    mut load_older: impl FnMut(&mut Timeline),
    mut row: impl FnMut(&mut egui::Ui, &StoredMessage) -> egui::Response,
) {
    t.heights.resize(t.msgs.len(), EST_HEIGHT);
    // egui keeps each scroll area's state for as long as the app runs. A chat last left away from the bottom (opened at
    // its first unread, scrolled up) would reopen at that offset with sticking off, the latest row cut or off screen:
    // a fresh open starts from the default state (top, stuck to the end) like a first visit.
    if std::mem::take(&mut t.fresh) {
        egui::scroll_area::State::default().store(ui.ctx(), ui.make_persistent_id(egui::IdSalt::new(t.key)));
    }
    // Per-chat id: another chat must not inherit this one's offset or stuck-to-bottom state.
    let mut area = egui::ScrollArea::vertical().id_salt(t.key).auto_shrink(false).stick_to_bottom(t.at_bottom);
    if let Some(y) = t.pending_offset.take() { area = area.vertical_scroll_offset(y); }
    let mut shift = 0.0; // height change of a row straddling the viewport top
    let out = area.show_viewport(ui, |ui, viewport| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let pad = t.settle.unwrap_or(0.0);
        let range = t.visible_range(viewport.min.y + pad, viewport.height());
        let mut top: f32 = t.heights[..range.start].iter().sum();
        ui.add_space((top - pad).max(0.0));
        for i in range.clone() {
            let h = row(ui, &t.msgs[i]).rect.height();
            if top < viewport.min.y + pad { shift += h - t.heights[i]; }
            t.heights[i] = h;
            top += h;
        }
        ui.add_space(t.heights[range.end..].iter().sum::<f32>() + pad);
    });
    let offset = out.state.clamped_offset().y;
    t.at_bottom = offset + out.inner_rect.height() >= out.content_size.y - 2.0;
    if let Some(pad) = t.settle.take() {
        t.pending_offset = Some(pad);
        ui.ctx().request_repaint();
    } else if offset <= 0.0 && (t.armed || out.content_size.y <= out.inner_rect.height()) && t.has_more {
        let before = t.msgs.len();
        load_older(t);
        let added = t.msgs.len().saturating_sub(before);
        if added > 0 {
            t.settle = Some(t.heights[..added].iter().sum());
            t.armed = false;
            ui.ctx().request_repaint();
        }
    } else {
        if offset > 1.0 { t.armed = true; }
        if shift != 0.0 && !t.at_bottom {
            t.pending_offset = Some(offset + shift);
            ui.ctx().request_repaint();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_app;
    use eframe::egui;

    fn msg(id: i64) -> StoredMessage {
        StoredMessage { id, contact_id: 1, guid: id, reply_to: 0, incoming: true, delivered: true, read: true, send_time: id, edit_time: 0, msg_type: 0, data: Some(b"x".to_vec()) }
    }

    #[test] fn push_if_newer_skips_loaded_rows() {
        let mut t = Timeline::empty();
        assert!(t.push_if_newer(Some(msg(5))));
        assert!(!t.push_if_newer(Some(msg(5))), "already loaded");
        assert!(!t.push_if_newer(Some(msg(3))), "older");
        assert!(!t.push_if_newer(None));
        assert!(t.push_if_newer(Some(msg(6))));
        assert_eq!((t.msgs.len(), t.heights.len()), (2, 2));
    }
    #[test] fn visible_range_by_heights() {
        let t = Timeline { heights: vec![10.; 100], ..Timeline::empty() };
        assert_eq!(t.visible_range(95., 30.), 9..13);
    }
    #[test] fn opening_chat_loads_one_page() {
        let (_t, app) = test_app();
        let c = app.add_contact(vec![1; 32], "A".into()).unwrap();
        for i in 0..10_000 {
            app.add_local_message(c, i + 1, 0, true, true, i, 0, 0, b"m".to_vec()).unwrap();
        }
        let mut t = Timeline::open(&app, ChatKey::Contact(c));
        assert_eq!(t.msgs.len(), 50);
        assert!(t.has_more);
        assert!(t.msgs.windows(2).all(|w| w[0].id < w[1].id));
        let newest = t.msgs[49].id;
        t.load_older(&app);
        assert_eq!(t.msgs.len(), 100);
        assert_eq!(t.heights.len(), 100);
        assert_eq!(t.msgs[99].id, newest);
        assert!(t.msgs.windows(2).all(|w| w[0].id < w[1].id));
    }
    #[test] fn page_of_only_reactions_still_loads_older() {
        let (_t, app) = test_app();
        let c = app.add_contact(vec![1; 32], "A".into()).unwrap();
        for i in 0..10 {
            app.add_local_message(c, i + 1, 0, true, true, i, 0, 0, b"m".to_vec()).unwrap();
        }
        for i in 0..60 {
            app.add_local_message(c, 100 + i, 1, true, true, 100 + i, 0, REACTION, crate::model::message::reaction_payload("👍", 1)).unwrap();
        }
        let t = Timeline::open(&app, ChatKey::Contact(c));
        assert_eq!(t.msgs.len(), 10);
    }
    #[test] fn group_timeline_pages_and_skips_hidden_rows() {
        let (_t, app) = test_app();
        app.save_group_chat(7, "G".into(), String::new(), None, vec![9; 32], vec![], vec![3; 32], 0).unwrap();
        for i in 0..60 {
            app.add_group_message(7, i + 1, i + 1, vec![1; 32], i, 0, false, b"m".to_vec(), 0, false).unwrap();
        }
        app.add_group_message(7, 100, 100, vec![1; 32], 100, REACTION, false, crate::model::message::reaction_payload("👍", 1), 1, false).unwrap();
        let mut deleted = vec![0x08];
        deleted.extend(1i64.to_be_bytes());
        deleted.extend([1; 32]);
        app.add_group_message(7, 101, 101, vec![9; 32], 101, crate::model::group::SYSTEM, true, deleted, 0, false).unwrap();
        let mut t = Timeline::open(&app, ChatKey::Group(7));
        assert_eq!(t.msgs.len(), 48, "a page of 50 minus the reaction and the deletion notice");
        assert!(t.has_more);
        t.load_older(&app);
        assert_eq!(t.msgs.len(), 60);
        assert!(t.msgs.windows(2).all(|w| w[0].id < w[1].id));
        let id = app.add_group_message(7, 102, 102, vec![1; 32], 102, 0, false, b"new".to_vec(), 0, false).unwrap();
        t.load_newer(&app);
        t.load_newer(&app);
        assert_eq!(t.msgs.last().map(|m| m.id), Some(id));
        assert_eq!(t.msgs.len(), 61, "appended once");
    }
    #[test] fn timeline_sticks_only_when_at_bottom() {
        let mut t = Timeline::empty();
        t.at_bottom = false;
        assert!(!t.on_new(msg(1)));
        t.at_bottom = true;
        assert!(t.on_new(msg(2)));
    }
    #[test] fn show_timeline_renders_only_visible_rows() {
        let mut t = Timeline::empty();
        for i in 1..=1000 { t.on_new(msg(i)); }
        let mut drawn = 0;
        let mut h = egui_kittest::Harness::new_ui_state(|ui, t: &mut Timeline| {
            drawn = 0;
            show_timeline(ui, t, |_| {}, |ui, _| { drawn += 1; ui.label("row") });
        }, t);
        h.run();
        drop(h);
        assert!(drawn > 0 && drawn < 100, "drawn {drawn}");
    }
    struct View {
        app: std::sync::Arc<MimirApp>,
        t: Timeline,
        loads: usize,
        ys: std::cell::RefCell<std::collections::HashMap<i64, f32>>,
        anchor: i64,
    }

    fn view(n: i64) -> (tempfile::TempDir, egui_kittest::Harness<'static, View>) {
        let (tmp, app) = test_app();
        let c = app.add_contact(vec![1; 32], "A".into()).unwrap();
        for i in 0..n { app.add_local_message(c, i + 1, 0, true, true, i, 0, 0, b"m".to_vec()).unwrap(); }
        let t = Timeline::open(&app, ChatKey::Contact(c));
        let h = egui_kittest::Harness::new_ui_state(|ui, v: &mut View| {
            let View { app, t, loads, ys, .. } = v;
            ys.borrow_mut().clear();
            show_timeline(ui, t, |t| {
                *loads += 1;
                t.load_older(app)
            }, |ui, m| {
                let r = ui.label(format!("message {}", m.id));
                ys.borrow_mut().insert(m.id, r.rect.min.y);
                r
            });
        }, View { anchor: t.msgs[0].id, app, t, loads: 0, ys: Default::default() });
        (tmp, h)
    }

    fn wheel(h: &mut egui_kittest::Harness<'static, View>, dy: f32) {
        h.hover_at(egui::pos2(100., 100.));
        h.event(egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0., dy), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown, modifiers: Default::default() });
        h.run_steps(40);
    }

    #[test] fn opening_chat_loads_one_page_across_frames() {
        let (_tmp, mut h) = view(200);
        for _ in 0..10 { h.step(); }
        assert_eq!(h.state().loads, 0);
        assert_eq!(h.state().t.msgs.len(), 50);
        assert!(h.state().t.at_bottom);
    }

    #[test] fn scrolling_to_top_loads_one_page_and_keeps_anchor() {
        let (_tmp, mut h) = view(200);
        for _ in 0..30 { h.step(); }
        let anchor = h.state().anchor;
        let t = &mut h.state_mut().t;
        (t.pending_offset, t.at_bottom) = (Some(0.), false); // user scrolls to the top
        for _ in 0..3 { h.step(); }
        for _ in 0..30 { h.step(); }
        assert_eq!(h.state().loads, 1, "must not keep loading");
        assert_eq!(h.state().t.msgs.len(), 100);
        // The old top row must still sit at the very top of the viewport: new rows are above it, off screen.
        let ys = h.state().ys.borrow();
        let top = ys.values().copied().fold(f32::MAX, f32::min);
        assert!((ys[&anchor] - top).abs() <= 1.0, "anchor y {} but topmost row y {top}", ys[&anchor]);
    }

    #[test] fn tall_window_loads_older_until_filled() {
        let (_tmp, mut h) = view(200);
        h.set_size(egui::vec2(400., 3000.)); // more than a page of rows fits
        for _ in 0..40 { h.step(); }
        let t = &h.state().t;
        let rows: f32 = t.heights.iter().sum();
        assert!(t.msgs.len() > 50, "older pages loaded: {}", t.msgs.len());
        assert!(rows > 3000. || !t.has_more, "loaded until the rows fill the view ({rows} px) or history ends");
    }

    #[test] fn at_bottom_tracks_scroll_position() {
        let (_tmp, mut h) = view(200);
        for _ in 0..5 { h.step(); }
        assert!(h.state().t.at_bottom);
        wheel(&mut h, 300.);
        assert!(!h.state().t.at_bottom);
        wheel(&mut h, -100_000.);
        assert!(h.state().t.at_bottom);
    }

    #[test] fn reactions_excluded() {
        let mut t = Timeline::empty();
        assert!(!t.on_new(StoredMessage { msg_type: 10, ..msg(1) }));
        assert!(t.msgs.is_empty());
    }
}
