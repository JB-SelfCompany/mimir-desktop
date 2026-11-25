//! Message composer: text field, reply preview, send button, drafts, and the UI side of reactions/forwarding.
use crate::{
    app::MimirDesktop,
    i18n::{tr, Key as K},
    media::stage::{self, Staged},
    model::{chat_key::ChatKey, send::{send_attachment, send_reaction, send_text, Target}, Forward},
    ui::{attach_panel, record_bar, theme::Palette},
};
use eframe::egui::{
    self, pos2, vec2, Align, Button, Frame, Id, KeyboardShortcut, Label, Layout, Modifiers, Role, ScrollArea, Sense, Shape,
    Stroke, TextEdit, Ui, WidgetInfo,
};
use mimir::MimirApp;

/// Same limit as Android.
const MAX_CHARS: usize = 5000;
/// Height of the one-line input and diameter of the send button.
const BUTTON: f32 = 30.;

#[derive(Default)]
pub struct Composer {
    pub text: String,
    /// Whether the text field had focus last frame; a synced draft never overwrites a field being typed in.
    pub focused: bool,
    want_focus: bool,
    /// Height of the input field last frame (it grows with the text).
    input_h: f32,
    /// The file in the attachment panel; sent with the text as its caption (P3-R2).
    pub attachment: Option<Staged>,
    /// Further picked or dropped files, sent right after `attachment` without a caption.
    pub more: Vec<Staged>,
    /// Why the last pick, drop or paste staged nothing (shown in the panel).
    pub attach_error: Option<String>,
    /// A staging run in progress (attach_panel), polled every frame.
    pub staging: Option<std::sync::mpsc::Receiver<attach_panel::Batch>>,
    /// Voice recording and its encode job (record_bar).
    pub voice: record_bar::Voice,
}

impl Composer {
    /// Replaces the text with the stored draft of `chat`.
    pub fn load_draft(&mut self, core: &MimirApp, chat: ChatKey) {
        self.text = core.get_draft(chat.draft_type(), chat.id()).and_then(|d| d.text).unwrap_or_default();
    }

    /// Empties the attachment panel (our temp files deleted) and forgets a staging run still going.
    pub fn clear_attachments(&mut self) {
        self.attachment.take().into_iter().chain(self.more.drain(..)).for_each(stage::discard);
        self.attach_error = None;
        self.staging = None;
    }

    /// Puts the keyboard focus in the text field next frame.
    pub fn focus(&mut self) {
        self.want_focus = true;
    }
}

fn field_id() -> Id {
    Id::unique("composer_text")
}

/// Chat switch: keeps the old chat's text as its draft and loads the new chat's draft.
pub fn switch_chat(app: &mut MimirDesktop, old: Option<ChatKey>, new: ChatKey) {
    if let Some(old) = old {
        save_draft(app, old);
        app.state.composer.text.clear();
    }
    // ponytail: a staged attachment is not part of the draft; switching chats drops it.
    app.state.composer.clear_attachments();
    app.state.composer.voice.reset();
    app.state.reply_to = None;
    app.state.composer.load_draft(&app.core, new);
    app.state.composer.focus();
    app.state.chats = crate::model::chat_list::build_chat_list(&app.core, app.lang);
}

/// Stores the composer text as the draft of `chat`; an emptied composer deletes the stored draft
/// (delete_draft replicates the deletion to siblings, Android BaseChatActivity.saveDraft).
// ponytail: the draft write runs on the UI thread; it is one local DB row.
pub fn save_draft(app: &MimirDesktop, chat: ChatKey) {
    let text = &app.state.composer.text;
    if !text.trim().is_empty() {
        let _ = app.core.save_draft(chat.draft_type(), chat.id(), Some(text.clone()), None, 0);
    } else {
        delete_draft(&app.core, chat);
    }
}

fn delete_draft(core: &MimirApp, chat: ChatKey) {
    if core.get_draft(chat.draft_type(), chat.id()).is_some() {
        let _ = core.delete_draft(chat.draft_type(), chat.id());
    }
}

/// App exit: the open chat's text becomes its draft.
pub fn save_open_draft(app: &MimirDesktop) {
    if let Some(chat) = &app.state.open_chat {
        save_draft(app, chat.key);
    }
}

fn send_button(ui: &mut Ui, label: &str, enabled: bool, p: &Palette) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(BUTTON, BUTTON), if enabled { Sense::click() } else { Sense::hover() });
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, label));
    let c = rect.center();
    let colour = if enabled { p.accent } else { p.accent.gamma_multiply(0.35) };
    let hover = if enabled && resp.hovered() { 1.1 } else { 1. };
    ui.painter().circle_filled(c, BUTTON / 2. * hover, colour);
    let tri = [pos2(c.x - 5., c.y - 7.), pos2(c.x + 8., c.y), pos2(c.x - 5., c.y + 7.)];
    ui.painter().add(Shape::convex_polygon(tri.to_vec(), egui::Color32::WHITE, Stroke::NONE));
    resp.clicked()
}

pub fn show(app: &mut MimirDesktop, ui: &mut Ui, p: &Palette) {
    let lang = app.lang;
    let ctx = ui.ctx().clone();
    let id = field_id();
    let started = app.state.core_started;
    let mut submit = false;
    let focused = ctx.memory(|m| m.has_focus(id));
    // Bare Enter sends; Shift+Enter is the TextEdit's own newline key.
    if focused {
        submit = ctx.input_mut(|i| !i.modifiers.shift && i.consume_key(Modifiers::NONE, egui::Key::Enter)) && started;
    }
    if app.state.reply_to.is_some() && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.state.reply_to = None;
    }
    attach_panel::take_input(app, &ctx);
    record_bar::poll(app);
    if std::mem::take(&mut app.state.composer.want_focus) {
        ctx.memory_mut(|m| m.request_focus(id));
    }
    let mut pick = false;
    let bar = Frame::NONE.fill(p.list_bg).inner_margin(egui::Margin::symmetric(12, 6)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        if let Some((_, preview)) = &app.state.reply_to {
            let short = preview.lines().next().unwrap_or("");
            let mut close = false;
            ui.horizontal(|ui| {
                ui.colored_label(p.accent, "↩");
                // × keeps its place on the right; the quote takes what is left, one line ending in «…».
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    close = ui.add(Button::new("×").frame(false)).clicked();
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        ui.add(Label::new(egui::RichText::new(short).color(p.text_dim)).truncate());
                    });
                });
            });
            if close {
                app.state.reply_to = None;
            }
        }
        attach_panel::show(app, ui, p);
        if app.state.composer.voice.recording() {
            record_bar::bar(app, ui, BUTTON, p);
            return;
        }
        ui.horizontal(|ui| {
            pick = attach_panel::clip_button(ui, BUTTON, tr(lang, K::SendFile), started, p);
            let w = ui.available_width() - BUTTON - ui.spacing().item_spacing.x;
            let c = &mut app.state.composer;
            // The row is only as tall as the previous frame's field, so the field is sized from that (up to 120 px, then it scrolls).
            let out = ScrollArea::vertical().max_height(120.).min_scrolled_height(c.input_h.max(BUTTON)).max_width(w).show(ui, |ui| {
                ui.add(
                    TextEdit::multiline(&mut c.text)
                        .id(id)
                        .hint_text(tr(lang, K::MessageHint))
                        .desired_rows(1)
                        .min_size(vec2(0., BUTTON))
                        .align(egui::Align2::LEFT_CENTER)
                        .desired_width(w - 8.)
                        .char_limit(MAX_CHARS)
                        .background_color(p.field)
                        .return_key(KeyboardShortcut::new(Modifiers::SHIFT, egui::Key::Enter)),
                )
                .rect
                .height()
            });
            // The field's own height: the scroll content is at least as tall as last frame's area, so it never shrinks.
            let input_h = out.inner.clamp(BUTTON, 120.);
            if input_h != c.input_h {
                c.input_h = input_h;
                ui.ctx().request_repaint(); // the bar is sized from this height; apply it now, not on the next input
            }
            // AM §7: an empty composer with no attachment records voice; anything to send makes it the send button.
            if c.text.trim().is_empty() && c.attachment.is_none() {
                record_bar::mic(app, ui, BUTTON, p);
            } else {
                submit |= send_button(ui, tr(lang, K::Send), started, p);
            }
        });
    });
    // Files dragged over the window: the bar that takes them is outlined.
    if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
        ui.painter().rect_stroke(bar.response.rect, 0, Stroke::new(2., p.accent), egui::StrokeKind::Inside);
    }
    if pick {
        attach_panel::pick(app, &ctx);
    }
    app.state.composer.focused = ctx.memory(|m| m.has_focus(id));
    if submit {
        send_composed(app);
        ctx.memory_mut(|m| m.request_focus(id));
    }
}

fn send_composed(app: &mut MimirDesktop) {
    let text = app.state.composer.text.trim().to_string();
    let Some(chat) = &app.state.open_chat else { return };
    let c = &mut app.state.composer;
    if text.is_empty() && c.attachment.is_none() {
        return;
    }
    let (key, to) = (chat.key, Target::for_key(chat.key, chat.pubkey.clone()));
    let reply = app.state.reply_to.take().map_or(0, |(guid, _)| guid);
    c.text.clear();
    c.attach_error = None;
    match c.attachment.take() {
        // P3-R2: the text is the first file's caption; the other files follow without one.
        Some(first) => {
            let more = std::mem::take(&mut c.more);
            send_attachment(app, to.clone(), reply, text, first);
            for s in more {
                send_attachment(app, to.clone(), 0, String::new(), s);
            }
        }
        None => send_text(app.worker_for(key), to, reply, text),
    }
    delete_draft(&app.core, key); // Android: the draft goes once its text is sent
}

/// Sends what the message menu and forward dialog queued (reaction on the open chat, forwarded text to its chat).
pub fn send_pending(app: &mut MimirDesktop) {
    if !app.state.core_started {
        return; // stays queued until the core is up
    }
    if let (Some((guid, mut emoji)), Some(chat)) = (app.state.pending_reaction.take(), &mut app.state.open_chat) {
        // Android ChatActivity.handleReaction: picking the emoji already chosen sends "" (removes it).
        let mine = match chat.key {
            ChatKey::Contact(id) => app.core.get_user_reaction(id, guid),
            ChatKey::Group(id) => app.core.get_user_group_reaction(id, guid),
        };
        if mine.as_deref() == Some(emoji.as_str()) {
            emoji.clear();
        }
        let w = if chat.key.is_group() { &app.group_worker } else { &app.worker };
        send_reaction(&app.core, w, Target::for_key(chat.key, chat.pubkey.clone()), guid, &emoji);
        chat.reactions.remove(&guid);
    }
    if let Some((key, forward)) = app.state.pending_forward.take() {
        if let Some(to) = app.state.chats.iter().find(|r| r.key == key).map(|row| Target::for_key(key, row.pubkey.clone())) {
            match forward {
                Forward::Text(text) => send_text(app.worker_for(key), to, 0, text),
                Forward::File(staged, caption) => send_attachment(app, to, 0, caption, staged),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::model::chat_key::ChatKey;
    use crate::{
        model::chat_list::{build_chat_list, SAVED_ID},
        settings::Settings,
        test_support::{start_app, test_harness},
    };
    use eframe::egui::{Key, Modifiers, Role};
    use egui_kittest::{kittest::Queryable as _, Harness};
    use crate::app::MimirDesktop;
    use std::time::{Duration, Instant};

    fn open_saved() -> Harness<'static, MimirDesktop> {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        let ctx = h.ctx.clone();
        start_app(h.state_mut(), &ctx);
        h.state_mut().state.core_started = true;
        h.state_mut().state.selected = Some(ChatKey::SAVED);
        h.run();
        h
    }

    fn type_in(h: &mut Harness<'static, MimirDesktop>, s: &str) {
        let f = h.get_by_role(Role::MultilineTextInput);
        f.focus();
        f.type_text(s);
        h.run();
    }

    fn wait_for_messages(h: &mut Harness<'static, MimirDesktop>, n: usize) {
        let end = Instant::now() + Duration::from_secs(5);
        while h.state().state.open_chat.as_ref().unwrap().timeline.msgs.len() < n && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
            h.run();
        }
        assert_eq!(h.state().state.open_chat.as_ref().unwrap().timeline.msgs.len(), n);
    }

    #[test] fn bar_is_input_height_plus_12() {
        let mut h = open_saved();
        h.set_size(eframe::egui::vec2(900., 600.));
        h.run();
        let r = h.get_by_role(Role::MultilineTextInput).rect();
        assert!((600. - r.bottom() - 6.).abs() < 1., "gap below {}", 600. - r.bottom());
        let b = h.get_by_label("Записать голосовое сообщение").rect(); // the empty composer's round button
        assert!((b.center().y - r.center().y).abs() < 1., "button {} input {}", b.center().y, r.center().y);
        assert!(b.height() <= r.height() + 0.5, "button {} input {}", b.height(), r.height());
    }

    #[test] fn input_height_change_asks_for_the_next_frame() {
        // The bar is sized from the previous frame's field height, so a changed height must trigger one more frame
        // (egui's scroll area only repaints when its scroll bar appears, i.e. not when the text shrinks).
        let mut h = open_saved();
        h.state_mut().state.composer.text = "a\nb\nc".into();
        h.run();
        let tall = h.get_by_role(Role::MultilineTextInput).rect().height();
        h.state_mut().state.composer.text.clear(); // a sent message or a synced empty draft, no input event
        h.step();
        assert!(h.ctx.has_requested_repaint(), "the shrunk field must be laid out on the next frame, not on the next input");
        h.run();
        assert!(h.get_by_role(Role::MultilineTextInput).rect().height() < tall - 20.);
    }

    /// The painted input background and the clip rect it is drawn under (what is really visible).
    fn field_shape(h: &Harness<'static, MimirDesktop>) -> (eframe::egui::Rect, eframe::egui::Rect) {
        let field = crate::ui::theme::palette(true).field;
        h.output()
            .shapes
            .iter()
            .filter_map(|cs| match &cs.shape {
                eframe::egui::epaint::Shape::Rect(r) if r.fill == field && r.rect.top() > 300. => Some((r.rect, cs.clip_rect)),
                _ => None,
            })
            .last()
            .expect("composer field painted")
    }

    #[test] fn input_is_not_clipped_in_a_bar_with_6px_padding() {
        for (ppp, text) in [(1., ""), (1.25, ""), (1., "a
b
c
d"), (1.25, "a
b
c
d
e
f
g
h
i
j
k
l
m")] {
            let mut h = open_saved();
            h.set_size(eframe::egui::vec2(1082., 713.));
            h.ctx.set_pixels_per_point(ppp);
            h.state_mut().state.composer.text = text.into();
            h.run_steps(8);
            let (rect, clip) = field_shape(&h);
            // A long text scrolls inside a 120 px field: then the clip is the visible part and the rest is scrolled away.
            let scrolls = rect.height() > 120.;
            assert!(clip.top() <= rect.top() + 0.5 && (scrolls || clip.bottom() >= rect.bottom() - 0.5), "ppp {ppp} {text:?}: input {rect:?} clipped to {clip:?}");
            assert!(scrolls == (clip.height() >= 119.), "ppp {ppp} {text:?}: clip {clip:?}");
            let screen = h.ctx.content_rect();
            assert!(rect.bottom().min(clip.bottom()) <= screen.bottom() - 5.5, "bottom {} of {}", rect.bottom(), screen.bottom());
        }
    }

    #[test] fn enter_sends_shift_enter_newlines() {
        let mut h = open_saved();
        type_in(&mut h, "a");
        h.key_press_modifiers(Modifiers::SHIFT, Key::Enter);
        h.run();
        type_in(&mut h, "b");
        assert_eq!(h.state().state.composer.text, "a\nb");
        h.key_press(Key::Enter);
        h.run();
        assert_eq!(h.state().state.composer.text, "");
        wait_for_messages(&mut h, 1);
        assert_eq!(h.state().state.open_chat.as_ref().unwrap().timeline.msgs[0].data.as_deref(), Some("a\nb".as_bytes()));
    }

    #[test] fn empty_composer_shows_mic_and_text_sends() {
        let mut h = open_saved();
        assert!(h.query_by_label("Отправить").is_none(), "empty: the mic instead of send (AM §7)");
        assert!(h.query_by_label("Записать голосовое сообщение").is_some());
        type_in(&mut h, "привет");
        h.get_by_label("Отправить").click();
        h.run();
        wait_for_messages(&mut h, 1);
    }

    #[test] fn not_started_keeps_text_and_sends_nothing() {
        let mut h = open_saved();
        h.state_mut().state.core_started = false;
        type_in(&mut h, "привет");
        h.key_press(Key::Enter);
        h.run();
        assert_eq!(h.state().state.composer.text, "привет");
        h.state_mut().state.pending_reaction = Some((1, "🔥".into()));
        h.run();
        assert!(h.state().state.pending_reaction.is_some(), "stays queued until the core is up");
        std::thread::sleep(Duration::from_millis(200));
        h.run();
        assert!(h.state().state.open_chat.as_ref().unwrap().timeline.msgs.is_empty());
    }

    #[test] fn switching_chat_drops_reply_target() {
        let mut h = open_saved();
        let ann = h.state().core.add_contact(vec![7; 32], "Аня".into()).unwrap();
        h.state_mut().state.reply_to = Some((7, "hello".into()));
        h.state_mut().state.selected = Some(ChatKey::Contact(ann));
        h.run();
        assert_eq!(h.state().state.reply_to, None);
    }

    #[test] fn exit_saves_open_chat_draft() {
        let mut h = open_saved();
        type_in(&mut h, "недописано");
        eframe::App::on_exit(h.state_mut());
        assert_eq!(h.state().core.get_draft(0, SAVED_ID).unwrap().text.as_deref(), Some("недописано"));
    }

    #[test] fn escape_clears_reply_and_send_consumes_it() {
        let mut h = open_saved();
        h.state_mut().state.reply_to = Some((7, "hello".into()));
        h.run();
        assert!(h.query_by_label_contains("hello").is_some());
        h.key_press(Key::Escape);
        h.run();
        assert_eq!(h.state().state.reply_to, None);
        h.state_mut().state.reply_to = Some((7, "hello".into()));
        type_in(&mut h, "ответ");
        h.key_press(Key::Enter);
        h.run();
        assert_eq!(h.state().state.reply_to, None);
        wait_for_messages(&mut h, 1);
        assert_eq!(h.state().state.open_chat.as_ref().unwrap().timeline.msgs[0].reply_to, 7);
    }

    #[test] fn long_reply_quote_is_one_truncated_line_left_of_close() {
        for w in [520., 800., 1400.] {
            let mut h = open_saved();
            h.set_size(eframe::egui::vec2(w, 600.));
            let long = "очень длинная цитата ".repeat(40);
            h.state_mut().state.reply_to = Some((7, long));
            h.run();
            let quote = h.get_by_label_contains("очень длинная").rect();
            let close = h.get_by_label("×").rect();
            assert!(close.right() <= w, "width {w}: × {close:?} pushed out of the window");
            assert!(quote.right() <= close.left(), "width {w}: quote {quote:?} overlaps × {close:?}");
            assert!(quote.height() < 24., "width {w}: one line, got {quote:?}");
            h.state_mut().state.reply_to = Some((7, "коротко".into()));
            h.run();
            let short = h.get_by_label("коротко").rect();
            assert!((short.left() - quote.left()).abs() < 1., "width {w}: a short quote stays left, next to ↩");
        }
    }

    #[test] fn draft_saved_on_switch() {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        let core = h.state().core.clone();
        let ann = core.add_contact(vec![7; 32], "Аня".into()).unwrap();
        h.state_mut().state.chats = build_chat_list(&core, h.state().lang);
        h.state_mut().state.selected = Some(ChatKey::Contact(ann));
        h.run();
        type_in(&mut h, "пишу");
        h.state_mut().state.selected = Some(ChatKey::SAVED);
        h.run();
        assert_eq!(core.get_draft(0, ann).unwrap().text.as_deref(), Some("пишу"));
        assert_eq!(h.state().state.composer.text, "");
        h.state_mut().state.selected = Some(ChatKey::Contact(ann));
        h.run();
        assert_eq!(h.state().state.composer.text, "пишу");
    }

    #[test] fn same_reaction_again_removes_it() {
        let mut h = open_saved();
        h.state().core.add_local_message(SAVED_ID, 1, 0, false, true, 1000, 0, 0, b"hi".to_vec()).unwrap();
        h.state_mut().state.open_chat = None; // reopen with the row loaded
        h.run();
        h.state_mut().state.pending_reaction = Some((1, "👍".into()));
        h.run();
        assert_eq!(h.state().core.get_user_reaction(SAVED_ID, 1).as_deref(), Some("👍"));
        h.state_mut().state.pending_reaction = Some((1, "👍".into()));
        h.run();
        h.run();
        assert_eq!(h.state().core.get_user_reaction(SAVED_ID, 1), None);
        assert!(h.query_by_label("👍").is_none(), "no chip left");
        // Clicking my own chip toggles it off too.
        h.state_mut().state.pending_reaction = Some((1, "🔥".into()));
        h.run();
        h.run();
        h.get_by_label("🔥").click();
        h.run();
        h.run();
        assert_eq!(h.state().core.get_user_reaction(SAVED_ID, 1), None);
        assert!(h.query_by_label("🔥").is_none());
    }

    #[test] fn emptied_draft_is_deleted_on_switch() {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        let core = h.state().core.clone();
        let ann = core.add_contact(vec![7; 32], "Аня".into()).unwrap();
        core.save_draft(0, ann, Some("старое".into()), None, 0).unwrap();
        h.state_mut().state.chats = build_chat_list(&core, h.state().lang);
        h.state_mut().state.selected = Some(ChatKey::Contact(ann));
        h.run();
        assert_eq!(h.state().state.composer.text, "старое");
        h.state_mut().state.composer.text.clear();
        h.state_mut().state.selected = Some(ChatKey::SAVED);
        h.run();
        assert!(core.get_draft(0, ann).is_none());
        h.state_mut().state.selected = Some(ChatKey::Contact(ann)); // leaves Saved with an empty composer and no draft
        h.run();
        // Only Ann's deletion tombstone (delete_draft); nothing is written for a chat that had no draft.
        assert_eq!(core.cleanup_old_drafts(-1), 1);
    }

    #[test] fn send_deletes_draft() {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        let ctx = h.ctx.clone();
        start_app(h.state_mut(), &ctx);
        h.state_mut().state.core_started = true;
        h.state().core.save_draft(0, SAVED_ID, Some("черновик".into()), None, 0).unwrap();
        h.state_mut().state.selected = Some(ChatKey::SAVED);
        h.run();
        assert_eq!(h.state().state.composer.text, "черновик");
        type_in(&mut h, "!");
        h.key_press(Key::Enter);
        h.run();
        wait_for_messages(&mut h, 1);
        assert!(h.state().core.get_draft(0, SAVED_ID).is_none());
    }

    fn open_group() -> Harness<'static, MimirDesktop> {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        let ctx = h.ctx.clone();
        start_app(h.state_mut(), &ctx);
        let core = h.state().core.clone();
        core.save_group_chat(7, "Семья".into(), String::new(), None, vec![9; 32], vec![], vec![3; 32], 0).unwrap();
        h.state_mut().state.core_started = true;
        h.state_mut().state.chats = build_chat_list(&core, crate::i18n::Lang::Ru);
        h.state_mut().state.selected = Some(ChatKey::Group(7));
        h.run();
        h
    }

    #[test] fn group_draft_uses_chat_type_1() {
        let mut h = open_group();
        type_in(&mut h, "пишу");
        h.state_mut().state.selected = Some(ChatKey::SAVED);
        h.run();
        let core = h.state().core.clone();
        assert_eq!(core.get_draft(1, 7).and_then(|d| d.text).as_deref(), Some("пишу"));
        assert!(core.get_draft(0, 7).is_none());
        // A draft written on another device reloads when the field is not being typed in.
        let (tx, rx) = std::sync::mpsc::channel();
        h.state_mut().rx = rx;
        h.state_mut().state.selected = Some(ChatKey::Group(7));
        h.run();
        h.ctx.memory_mut(|m| if let Some(id) = m.focused() { m.surrender_focus(id) });
        h.run();
        assert!(!h.state().state.composer.focused);
        core.save_draft(1, 7, Some("с телефона".into()), None, 0).unwrap();
        tx.send(crate::core::events::Event::DraftSynced { chat_type: 0, chat_id: 7 }).unwrap();
        h.run();
        assert_ne!(h.state().state.composer.text, "с телефона", "chat_type 0 is contact 7, not this group");
        tx.send(crate::core::events::Event::DraftSynced { chat_type: 1, chat_id: 7 }).unwrap();
        h.run();
        assert_eq!(h.state().state.composer.text, "с телефона");
    }

    #[test] fn group_send_stores_a_row_and_clears_the_composer() {
        let mut h = open_group();
        h.state().core.save_draft(1, 7, Some("привет".into()), None, 0).unwrap();
        type_in(&mut h, "привет");
        h.get_by_label("Отправить").click();
        h.run();
        assert_eq!(h.state().state.composer.text, "");
        assert!(h.state().core.get_draft(1, 7).is_none(), "the draft goes once sent");
        let end = Instant::now() + Duration::from_secs(5);
        while h.state().state.open_chat.as_ref().unwrap().timeline.msgs.is_empty() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
            h.run();
        }
        let chat = h.state().state.open_chat.as_ref().unwrap();
        assert_eq!(chat.timeline.msgs.len(), 1);
        assert_eq!(chat.timeline.msgs[0].data.as_deref(), Some("привет".as_bytes()));
        assert_eq!(h.state().state.status, None, "offline: queued, not an error");
    }

    #[test] fn forward_into_group_sends_to_the_group() {
        let mut h = open_saved();
        let core = h.state().core.clone();
        core.save_group_chat(7, "Семья".into(), String::new(), None, vec![9; 32], vec![], vec![3; 32], 0).unwrap();
        h.state_mut().state.chats = build_chat_list(&core, crate::i18n::Lang::Ru);
        h.state_mut().state.pending_forward = Some((ChatKey::Group(7), crate::model::Forward::Text("переслано".into())));
        h.run();
        assert_eq!(h.state().state.pending_forward, None);
        let end = Instant::now() + Duration::from_secs(5);
        while core.get_group_messages(7, 0, 10).is_empty() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
        }
        let rows = core.get_group_messages(7, 0, 10);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].data.as_deref(), Some("переслано".as_bytes()));
    }

    #[test] fn pending_reaction_and_forward_are_consumed() {
        let mut h = open_saved();
        h.state().core.add_local_message(SAVED_ID, 1, 0, false, true, 1000, 0, 0, b"hi".to_vec()).unwrap();
        h.state_mut().state.pending_reaction = Some((1, "🔥".into()));
        h.state_mut().state.pending_forward = Some((ChatKey::SAVED, crate::model::Forward::Text("переслано".into())));
        h.run();
        assert_eq!(h.state().state.pending_reaction, None);
        assert_eq!(h.state().state.pending_forward, None);
        assert_eq!(h.state().core.get_user_reaction(SAVED_ID, 1).as_deref(), Some("🔥"));
        wait_for_messages(&mut h, 1);
    }

    // --- attachments (P3 Task 3) ---

    #[derive(Debug)]
    struct Dropped(std::path::PathBuf);
    impl eframe::egui::DroppedFile for Dropped {
        fn path(&self) -> &std::path::Path {
            &self.0
        }
        fn bytes(&self) -> Result<Vec<u8>, String> {
            std::fs::read(&self.0).map_err(|e| e.to_string())
        }
    }

    fn drop_files(h: &mut Harness<'static, MimirDesktop>, paths: &[&std::path::Path]) {
        h.input_mut().dropped_files = paths.iter().map(|p| std::sync::Arc::new(Dropped(p.to_path_buf())) as eframe::egui::DroppedFileHandle).collect();
        h.run_steps(3);
    }

    // These tests step frames (`run_steps`, `step`) instead of `Harness::run`: staging threads, thumbnail decodes
    // and send jobs keep waking the UI, which `run` reports as a UI that never settles.

    /// Runs frames until the staging thread has filled the panel (a file or an error).
    fn wait_staged(h: &mut Harness<'static, MimirDesktop>) {
        let end = Instant::now() + Duration::from_secs(5);
        let done = |h: &Harness<'static, MimirDesktop>| h.state().state.composer.attachment.is_some() || h.state().state.composer.attach_error.is_some();
        while !done(h) && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
            h.step(); // events may keep arriving: step, not run
        }
        assert!(done(h), "nothing staged");
        h.run_steps(2); // the panel's first frame may still move it
    }

    /// `wait_for_messages` by single frames: send jobs keep sending events, which `Harness::run` refuses.
    fn wait_msgs(h: &mut Harness<'static, MimirDesktop>, n: usize) {
        wait_until(h, "messages stored", |h| h.state().state.open_chat.as_ref().unwrap().timeline.msgs.len() >= n);
        assert_eq!(h.state().state.open_chat.as_ref().unwrap().timeline.msgs.len(), n);
    }

    fn meta(h: &Harness<'static, MimirDesktop>, i: usize) -> crate::model::attachment::AttachmentMeta {
        let m = &h.state().state.open_chat.as_ref().unwrap().timeline.msgs[i];
        crate::model::attachment::AttachmentMeta::parse(m.data.as_deref().unwrap()).unwrap()
    }

    fn wait_until(h: &mut Harness<'static, MimirDesktop>, what: &str, f: impl Fn(&Harness<'static, MimirDesktop>) -> bool) {
        let end = Instant::now() + Duration::from_secs(5);
        while !f(h) && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
            h.step();
        }
        assert!(f(h), "{what}");
    }

    #[test] fn clip_button_opens_picker_and_stages() {
        let mut h = open_saved();
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("отчёт.pdf");
        std::fs::write(&src, b"%PDF-1.4").unwrap();
        crate::ui::attach_panel::set_picked(vec![src.clone()]);
        h.get_by_label("Файл").click();
        h.run_steps(3);
        wait_staged(&mut h);
        let s = h.state().state.composer.attachment.clone().unwrap();
        assert_eq!((s.path, s.original_name.as_str(), s.mime.as_str(), s.msg_type, s.temp), (src, "отчёт.pdf", "application/pdf", 3, false));
        assert!(h.query_by_label("отчёт.pdf").is_some(), "the panel shows the name");
        assert!(h.query_by_label("8 Б").is_some(), "and the size");
        assert!(h.state().state.open_chat.as_ref().unwrap().timeline.msgs.is_empty(), "picking does not send (AM §1)");
    }

    #[test] fn drop_two_files_stages_first_then_sends_both() {
        let mut h = open_saved();
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = (tmp.path().join("a.txt"), tmp.path().join("b.pdf"));
        std::fs::write(&a, "один").unwrap();
        std::fs::write(&b, "два").unwrap();
        drop_files(&mut h, &[&a, &b]);
        wait_staged(&mut h);
        let c = &h.state().state.composer;
        assert_eq!(c.attachment.as_ref().unwrap().original_name, "a.txt");
        assert_eq!(c.more.iter().map(|s| s.original_name.as_str()).collect::<Vec<_>>(), ["b.pdf"]);
        assert!(h.query_by_label_contains("+1").is_some(), "the second file is announced");
        type_in(&mut h, "подпись");
        h.key_press(Key::Enter);
        h.step();
        let c = &h.state().state.composer;
        assert!(c.attachment.is_none() && c.more.is_empty() && c.text.is_empty(), "panel and field emptied");
        wait_msgs(&mut h, 2);
        let (m0, m1) = (meta(&h, 0), meta(&h, 1));
        assert_eq!((m0.original_name.as_str(), m0.caption.as_str(), m0.mime.as_str()), ("a.txt", "подпись", "text/plain"), "caption on the first");
        assert_eq!((m1.original_name.as_str(), m1.caption.as_str()), ("b.pdf", ""));
        let files = h.state().paths.files_dir.clone();
        assert_eq!(std::fs::read(files.join(&m1.name)).unwrap(), "два".as_bytes(), "the core copied it into files_dir");
        assert!(a.exists() && b.exists(), "dropped originals are never deleted");
    }

    #[test] fn ctrl_v_image_stages_png() {
        use eframe::egui::{Color32, ColorImage, Event};
        let mut h = open_saved();
        h.event(Event::PasteImage(std::sync::Arc::new(ColorImage::new([4, 3], vec![Color32::RED; 12]))));
        h.run_steps(3);
        wait_staged(&mut h);
        let s = h.state().state.composer.attachment.clone().unwrap();
        assert_eq!((s.original_name.as_str(), s.mime.as_str(), s.msg_type, s.temp), ("image.png", "image/png", 1, true));
        assert_eq!(image::image_dimensions(&s.path).unwrap(), (4, 3));
        h.get_by_label("Отправить").click();
        h.step();
        wait_msgs(&mut h, 1);
        assert_eq!((meta(&h, 0).mime.as_str(), h.state().state.open_chat.as_ref().unwrap().timeline.msgs[0].msg_type), ("image/png", 1));
        wait_until(&mut h, "the pasted temp file goes once sent", |_| !s.path.exists());
    }

    #[test] fn attach_panel_cancel_clears() {
        use eframe::egui::{Color32, ColorImage, Event};
        let mut h = open_saved();
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a.txt");
        std::fs::write(&a, "один").unwrap();
        drop_files(&mut h, &[&a]);
        wait_staged(&mut h);
        assert!(h.query_by_label("a.txt").is_some());
        h.get_by_label("Отмена").click();
        h.run_steps(3);
        assert!(h.state().state.composer.attachment.is_none());
        assert!(h.query_by_label("a.txt").is_none(), "panel gone");
        assert!(a.exists(), "a picked file is only let go of");
        // A pasted picture is our temp file: cancelling deletes it.
        h.event(Event::PasteImage(std::sync::Arc::new(ColorImage::new([2, 2], vec![Color32::BLUE; 4]))));
        h.run_steps(3);
        wait_staged(&mut h);
        let pasted = h.state().state.composer.attachment.clone().unwrap().path;
        h.get_by_label("Отмена").click();
        h.run_steps(3);
        assert!(!pasted.exists());
        assert!(h.state().state.open_chat.as_ref().unwrap().timeline.msgs.is_empty(), "nothing sent");
    }

    #[test] fn bad_drop_shows_inline_error_and_sends_nothing() {
        // Review Focus 3, in a group (500 MB limit): the corrected text shows in the panel; a folder is refused too.
        let mut h = open_group();
        let tmp = tempfile::tempdir().unwrap();
        let big = tmp.path().join("big.iso");
        std::fs::File::create(&big).unwrap().set_len(crate::media::stage::MAX_GROUP_FILE + 1).unwrap();
        drop_files(&mut h, &[&big]);
        wait_staged(&mut h);
        assert!(h.state().state.composer.attachment.is_none());
        assert!(h.query_by_label("Файл слишком велик для группового чата. Максимальный размер 500 МБ.").is_some());
        assert!(h.query_by_label("Отправить").is_none(), "nothing staged: nothing to send");
        h.run_steps(3);
        assert!(h.state().core.get_group_messages(7, 0, 10).is_empty(), "nothing sent");
        h.state_mut().state.composer.attach_error = None;
        drop_files(&mut h, &[tmp.path()]);
        wait_staged(&mut h);
        assert!(h.query_by_label("Можно отправить только файл, не папку").is_some());
        h.get_by_label("Отмена").click();
        h.run_steps(3);
        assert!(h.state().state.composer.attach_error.is_none(), "✕ dismisses the error");
    }

    #[test] fn switching_chat_drops_staged_attachment() {
        use eframe::egui::{Color32, ColorImage, Event};
        let mut h = open_saved();
        h.event(Event::PasteImage(std::sync::Arc::new(ColorImage::new([2, 2], vec![Color32::BLUE; 4]))));
        h.run_steps(3);
        wait_staged(&mut h);
        let pasted = h.state().state.composer.attachment.clone().unwrap().path;
        let ann = h.state().core.add_contact(vec![7; 32], "Аня".into()).unwrap();
        h.state_mut().state.selected = Some(ChatKey::Contact(ann));
        h.run_steps(3);
        assert!(h.state().state.composer.attachment.is_none());
        assert!(!pasted.exists());
    }

    #[test] fn offline_attachment_is_queued() {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        let ctx = h.ctx.clone();
        start_app(h.state_mut(), &ctx);
        h.state_mut().state.core_started = true;
        let core = h.state().core.clone();
        let ann = core.add_contact(vec![7; 32], "Аня".into()).unwrap();
        h.state_mut().state.chats = build_chat_list(&core, crate::i18n::Lang::Ru);
        h.state_mut().state.selected = Some(ChatKey::Contact(ann));
        h.run_steps(3);
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a.txt");
        std::fs::write(&a, "один").unwrap();
        drop_files(&mut h, &[&a]);
        wait_staged(&mut h);
        h.get_by_label("Отправить").click();
        h.step();
        wait_msgs(&mut h, 1);
        // The peer is offline: send_contact_message stored the row and threw Connection, which means "queued".
        wait_until(&mut h, "the send job ends", |h| h.state().state.sending.is_empty());
        let m = &h.state().state.open_chat.as_ref().unwrap().timeline.msgs[0];
        assert_eq!((m.msg_type, m.delivered), (3, false));
        assert_eq!(meta(&h, 0).original_name, "a.txt");
        assert!(h.state().state.transfers.is_empty(), "no upload ring left behind");
        assert_eq!(h.state().state.status, None, "queued, not an error");
    }
}
