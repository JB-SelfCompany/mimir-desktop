//! Window shell (mockup option B): welcome, or toolbar + chat list + chat area.
use crate::{
    app::MimirDesktop,
    i18n::{tr, Key},
    model::Dot,
    ui::{add_contact, chat_list, info_panel, invites, join_group, mediators, new_group, profile, requests, settings, chat_view, theme::{palette, Palette}, welcome},
};
use eframe::egui::{
    self, containers::panel::PanelState, vec2, Align, CentralPanel, Color32, Frame, Layout, Panel, Response, RichText, Sense, Shape, Stroke,
    Ui, Role, WidgetInfo,
};

#[derive(Clone, Copy)]
enum Icon {
    Profile,
    AddContact,
    NewChat,
    More,
}

pub fn dot_color(d: Dot) -> Color32 {
    match d {
        Dot::Red => Color32::from_rgb(0xF4, 0x43, 0x36),
        Dot::Yellow => Color32::from_rgb(0xFF, 0xC1, 0x07),
        Dot::Green => Color32::from_rgb(0x4C, 0xAF, 0x50),
    }
}

/// 24x24-unit white line icon (same shapes as the mockup) centred in a 30px click target.
fn icon_button(ui: &mut Ui, icon: Icon, label: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(30., 30.), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, label));
    let g = ui.painter();
    if resp.hovered() {
        g.rect_filled(rect, 3., Color32::from_white_alpha(24));
    }
    let o = rect.center() - vec2(12., 12.);
    let s = Stroke::new(1.8, Color32::WHITE);
    let pt = |x: f32, y: f32| o + vec2(x, y);
    let arc = |cx: f32, cy: f32, r: f32, from: f32, to: f32| -> Vec<egui::Pos2> {
        (0..=10).map(|i| {
            let a = (from + (to - from) * i as f32 / 10.).to_radians();
            pt(cx + r * a.cos(), cy + r * a.sin())
        }).collect()
    };
    match icon {
        Icon::Profile => {
            g.circle_stroke(pt(12., 12.), 9.5, s);
            g.circle_stroke(pt(12., 10.), 3.2, s);
            g.add(Shape::line(arc(12., 20., 5.5, 200., 340.), s));
        }
        Icon::AddContact => {
            g.circle_stroke(pt(14., 8.), 3.5, s);
            g.add(Shape::line(arc(14., 21., 6.5, 205., 335.), s));
            g.line_segment([pt(2.5, 9.5), pt(8.5, 9.5)], s);
            g.line_segment([pt(5.5, 6.5), pt(5.5, 12.5)], s);
        }
        Icon::NewChat => {
            g.add(Shape::closed_line([(4., 4.), (20., 4.), (20., 16.), (8., 16.), (4., 20.)].map(|(x, y)| pt(x, y)).to_vec(), s));
            g.line_segment([pt(12., 7.), pt(12., 13.)], s);
            g.line_segment([pt(9., 10.), pt(15., 10.)], s);
        }
        Icon::More => {
            for y in [5., 12., 19.] {
                g.circle_filled(pt(12., y), 1.8, Color32::WHITE);
            }
        }
    }
    resp.on_hover_text(label)
}

fn toolbar(app: &mut MimirDesktop, ui: &mut Ui, p: &Palette) {
    let lang = app.lang;
    Frame::NONE.fill(p.toolbar).inner_margin(egui::Margin::symmetric(8, 0)).show(ui, |ui| {
        ui.set_height(44.);
        ui.set_width(ui.available_width());
        ui.horizontal_centered(|ui| {
            // Our avatar (32/6) is the profile button once we have one.
            let stored = app.state.own_avatar.get_or_insert_with(|| app.core.get_account(profile::ACCOUNT).map(|a| a.avatar).unwrap_or_default());
            let profile = match crate::media::avatar::resolve(&app.paths.core_dir, stored) {
                Some(path) => {
                    let (rect, resp) = ui.allocate_exact_size(vec2(32., 32.), Sense::click());
                    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, tr(lang, Key::Profile)));
                    crate::ui::avatar::paint(ui, &mut app.state.thumbs, rect, Some(&path), p.accent, 3.);
                    resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tr(lang, Key::Profile))
                }
                None => icon_button(ui, Icon::Profile, tr(lang, Key::Profile)),
            };
            if profile.clicked() {
                app.state.show_profile = true;
            }
            ui.label(RichText::new("Mimir").size(16.).color(Color32::WHITE));
            let n = &app.state.net;
            ui.add_space(6.);
            for (i, d) in [n.ygg, n.tracker, n.mediator].into_iter().enumerate() {
                if i > 0 {
                    ui.add_space(4.);
                }
                let (r, _) = ui.allocate_exact_size(vec2(8., 8.), Sense::hover());
                ui.painter().circle_filled(r.center(), 4., dot_color(d));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let more = icon_button(ui, Icon::More, tr(lang, Key::MainMenu));
                egui::Popup::menu(&more).show(|ui| {
                    ui.set_min_width(170.);
                    if ui.button(tr(lang, Key::Settings)).clicked() {
                        settings::open(app);
                    }
                    if ui.button(tr(lang, Key::Mediators)).clicked() {
                        mediators::open(app);
                    }
                });
                if icon_button(ui, Icon::NewChat, tr(lang, Key::NewChat)).clicked() {
                    new_group::open(app);
                }
                if icon_button(ui, Icon::AddContact, tr(lang, Key::AddContact)).clicked() {
                    app.state.show_add_contact = true;
                }
            });
        });
    });
}

/// A clickable accent-coloured line under the toolbar (contact requests, group invites); true when clicked.
fn banner(ui: &mut Ui, p: &Palette, text: String) -> bool {
    Frame::NONE
        .fill(p.field)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(egui::Label::new(RichText::new(text).color(p.accent)).sense(Sense::click())).on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
        })
        .inner
}

/// Last job failure (core start, send, …) under the toolbar; non-start failures disappear after a few seconds.
fn status_line(app: &mut MimirDesktop, ui: &mut Ui, p: &Palette) {
    let Some((text, until)) = &app.state.status else { return };
    let left = until.map(|t| t.saturating_duration_since(std::time::Instant::now()));
    if left == Some(std::time::Duration::ZERO) {
        app.state.status = None;
        return;
    }
    if let Some(d) = left {
        ui.ctx().request_repaint_after(d);
    }
    Frame::NONE.fill(p.field).inner_margin(egui::Margin::symmetric(12, 6)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let color = if app.state.status_ok { p.accent } else { dot_color(Dot::Red) };
        ui.label(RichText::new(text).size(11.).color(color));
    });
}

pub fn show(app: &mut MimirDesktop, ui: &mut Ui) {
    let p = palette(ui.visuals().dark_mode);
    if !app.settings.intro_done {
        let import = match &app.state.settings_ui.job {
            Some((settings::Job::Import, text)) => Some(text.as_deref()),
            _ => None,
        };
        let choice = welcome::show(ui, app.lang, &p, &mut app.state.welcome_name, import);
        if choice == Some(welcome::Choice::Import) {
            settings::pick_import(app);
        }
        settings::welcome_confirm(app, &ui.ctx().clone());
        if choice == Some(welcome::Choice::Start) {
            let name = std::mem::take(&mut app.state.welcome_name).trim().to_string();
            if !name.is_empty() {
                // ponytail: a failed write leaves the default name; it can be set in the profile.
                let _ = app.core.update_account_name(profile::ACCOUNT, name);
            }
            app.settings.intro_done = true;
            // ponytail: a failed settings write is ignored; the intro just shows again next launch.
            let _ = app.settings.save(&app.paths.settings_file);
        }
        return;
    }
    // 30% of the window at first show, 260..=360; the user can drag it within 260..=420.
    let list_w = (ui.ctx().content_rect().width() * 0.3).clamp(260., 360.);
    // Follows window resizes until the user drags the divider: then the stored width differs from the last automatic one.
    // Must equal the PanelState key Panel::left("chat_list") uses inside egui: in egui 0.37 that is
    // `parent_ui.scope_id().with_salt(id_salt)` (containers/panel.rs, Panel::id). Recheck on an egui upgrade;
    // `list_width_follows_window_resize_until_dragged` fails if the ids drift apart.
    let list_id = ui.scope_id().with("chat_list");
    if let Some(state) = PanelState::load(ui.ctx(), list_id) {
        let last = app.state.list_auto;
        if (state.outer_rect.width() - last).abs() < 2. && (list_w - last).abs() >= 0.5 {
            ui.data_mut(|d| d.remove::<PanelState>(list_id));
            app.state.list_auto = list_w;
        }
    } else {
        app.state.list_auto = list_w;
    }
    Panel::left("chat_list")
        .default_size(list_w)
        .size_range(260.0..=420.0)
        .frame(Frame::NONE.fill(p.list_bg))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(0., 0.);
            toolbar(app, ui, &p);
            if app.state.requests_count > 0 && banner(ui, &p, format!("{}{}", tr(app.lang, Key::ContactRequests), app.state.requests_count)) {
                app.state.show_requests = true;
            }
            let n = invites::count(app);
            if n > 0 && banner(ui, &p, format!("{}: {n}", tr(app.lang, Key::GroupInvites))) {
                invites::open_banner(app);
            }
            status_line(app, ui, &p);
            chat_list::show(ui, &app.state.chats, &mut app.state.selected, &p, &mut app.state.thumbs, &app.paths.core_dir);
        });
    let ctx = ui.ctx().clone();
    // Below 1000 px the info panel floats over the chat instead of narrowing it.
    let wide = ctx.content_rect().width() >= 1000.;
    let info = info_panel::visible(app);
    if info && wide {
        Panel::right("contact_info")
            .exact_size(info_panel::WIDTH)
            .resizable(false)
            .frame(Frame::NONE.fill(p.list_bg))
            .show(ui, |ui| info_panel::contents(app, ui, &p));
    }
    CentralPanel::default().frame(Frame::NONE.fill(p.canvas)).show(ui, |ui| chat_view::show(app, ui, &p));
    if info && !wide {
        info_panel::overlay(app, &ctx, &p);
    }
    add_contact::show(app, &ctx);
    requests::show(app, &ctx);
    invites::show(app, &ctx);
    join_group::show(app, &ctx);
    new_group::show(app, &ctx);
    profile::show(app, &ctx);
    settings::show(app, &ctx);
    // After settings: «Медиаторы» in its Advanced section opens this over it (the last modal shown is on top).
    mediators::show(app, &ctx);
}

#[cfg(test)]
mod tests {
    use crate::model::chat_key::ChatKey;
    use crate::{settings::Settings, test_support::test_harness};
    use eframe::egui::Role;
    use egui_kittest::kittest::Queryable as _;

    #[test]
    fn welcome_has_import_button() {
        use crate::{backup, paths::DataPaths, test_support::{run_until, test_app}, ui::settings::set_backup_picked};
        let mut h = test_harness(Settings::default());
        h.run();
        let tmp = tempfile::tempdir().unwrap();
        // A file that is no backup: Android's refusal under the button; the welcome stays.
        let bad = tmp.path().join("x.zip");
        std::fs::write(&bad, "nope").unwrap();
        set_backup_picked(bad);
        h.get_by_label("Импортировать с телефона").click();
        h.run_steps(2);
        run_until(&mut h, "the refusal", |h| h.query_by_label("Это не файл бэкапа Mimir!").is_some());
        // A real backup is staged without a question (there is nothing to lose yet) and the app relaunches.
        let (src_tmp, src) = test_app();
        let zip = tmp.path().join("b.zip");
        backup::export(&src, &DataPaths::under(src_tmp.path()), &Settings::default(), &zip).unwrap();
        set_backup_picked(zip);
        h.get_by_label("Импортировать с телефона").click();
        h.run_steps(2);
        run_until(&mut h, "the staging", |h| h.state().state.settings_ui.restarting);
        assert!(h.state().paths.root.join("restore_pending.zip").is_file());
        assert!(!h.state().settings.intro_done, "applied by the next start, not here");
    }

    #[test]
    fn welcome_import_warns_over_existing_chats() {
        // settings.json lost (intro_done false) while mimir.db holds chats: Android's overwrite warning first.
        use crate::{backup, paths::DataPaths, test_support::{run_until, test_app}, ui::settings::set_backup_picked};
        let mut h = test_harness(Settings::default());
        let core = h.state().core.clone();
        core.add_contact(vec![7; 32], "Аня".into()).unwrap();
        h.state_mut().state.chats = crate::model::chat_list::build_chat_list(&core, crate::i18n::Lang::Ru);
        h.run();
        let tmp = tempfile::tempdir().unwrap();
        let (src_tmp, src) = test_app();
        let zip = tmp.path().join("b.zip");
        backup::export(&src, &DataPaths::under(src_tmp.path()), &Settings::default(), &zip).unwrap();
        set_backup_picked(zip);
        h.get_by_label("Импортировать с телефона").click();
        h.run_steps(2);
        run_until(&mut h, "the warning", |h| h.query_by_label("Импорт бэкапа?").is_some());
        let pending = h.state().paths.root.join("restore_pending.zip");
        assert!(!pending.exists(), "nothing staged before «Импорт»");
        h.get_by_label("Импорт").click();
        h.run_steps(2);
        run_until(&mut h, "the staging", |h| h.state().state.settings_ui.restarting);
        assert!(pending.is_file());
    }

    #[test]
    fn welcome_then_main() {
        let mut h = test_harness(Settings::default());
        h.get_by_label("Начать").click();
        h.run();
        assert!(h.query_by_label("Избранное").is_some());
        assert!(h.query_by_label("Mimir").is_some());
        assert!(h.state().settings.intro_done);
        assert!(Settings::load(&h.state().paths.settings_file).intro_done, "saved to disk, not only in memory");
    }

    #[test]
    fn welcome_name_is_saved_as_account_name() {
        let mut h = test_harness(Settings::default());
        let before = h.state().core.get_account(1).map(|a| a.name);
        let f = h.get_by_role(Role::TextInput);
        f.focus();
        f.type_text("  Боб  ");
        h.run();
        h.get_by_label("Начать").click();
        h.run();
        assert!(h.state().settings.intro_done);
        assert_eq!(h.state().core.get_account(1).map(|a| a.name).as_deref(), Some("Боб"));
        assert_ne!(before.as_deref(), Some("Боб"));
    }

    #[test]
    fn welcome_name_field_is_focused_once() {
        let mut h = test_harness(Settings::default());
        h.run();
        assert!(h.get_by_role(Role::TextInput).is_focused(), "typing goes straight into the name");
        h.key_press(eframe::egui::Key::Escape); // surrender focus
        h.run();
        assert!(!h.get_by_role(Role::TextInput).is_focused(), "not grabbed back every frame");
        h.get_by_role(Role::TextInput).type_text("Боб");
        h.run();
        assert_eq!(h.state().state.welcome_name, "", "unfocused field ignores typing");
    }

    #[test]
    fn enter_in_welcome_name_starts() {
        let mut h = test_harness(Settings::default());
        let f = h.get_by_role(Role::TextInput);
        f.focus();
        f.type_text("Боб");
        h.run();
        h.key_press(eframe::egui::Key::Enter);
        h.run();
        assert!(h.state().settings.intro_done);
        assert_eq!(h.state().core.get_account(1).map(|a| a.name).as_deref(), Some("Боб"));
    }

    #[test]
    fn welcome_block_is_vertically_centred() {
        for hgt in [600., 900.] {
            let mut h = test_harness(Settings::default());
            h.set_size(eframe::egui::vec2(800., hgt));
            h.run();
            let top = h.get_by_label("Добро пожаловать в Mimir").rect().top() - 123.; // rune 96, item spacing 3, space 24 above the title
            let bottom = h.get_by_label("Импортировать с телефона").rect().bottom();
            let (above, below) = (top, hgt - bottom);
            assert!((above - below).abs() < 3., "height {hgt}: {above} above vs {below} below");
        }
    }

    #[test]
    fn empty_welcome_name_keeps_account_name() {
        let mut h = test_harness(Settings::default());
        let before = h.state().core.get_account(1).map(|a| a.name);
        assert!(h.query_by_role(Role::TextInput).is_some(), "name field shown");
        let f = h.get_by_role(Role::TextInput);
        f.focus();
        f.type_text("   ");
        h.run();
        h.get_by_label("Начать").click();
        h.run();
        assert!(h.state().settings.intro_done);
        assert_eq!(h.state().core.get_account(1).map(|a| a.name), before);
    }

    #[test]
    fn clicking_row_selects_chat() {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        h.get_by_label("Избранное").click();
        h.run();
        assert_eq!(h.state().state.selected, Some(ChatKey::SAVED));
    }

    #[test]
    fn clicking_group_row_opens_group_and_keeps_its_draft() {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        let core = h.state().core.clone();
        core.save_group_chat(7, "Семья".into(), String::new(), None, vec![9; 32], vec![], vec![3; 32], 0).unwrap();
        core.set_group_chat_muted(7, true).unwrap();
        h.state_mut().state.chats = crate::model::chat_list::build_chat_list(&core, crate::i18n::Lang::Ru);
        h.run();
        assert!(h.state().state.chats[1].group.is_some_and(|g| g.muted));
        h.get_by_label("Семья").click();
        h.run();
        assert_eq!(h.state().state.selected, Some(ChatKey::Group(7)));
        let chat = h.state().state.open_chat.as_ref().unwrap();
        assert_eq!((chat.key, chat.pubkey.is_empty()), (ChatKey::Group(7), true));
        h.state_mut().state.core_started = true;
        h.state_mut().state.composer.text = "черновик".into();
        h.run();
        h.state_mut().state.selected = Some(ChatKey::SAVED);
        h.run();
        assert_eq!(core.get_draft(1, 7).and_then(|d| d.text).as_deref(), Some("черновик"));
        assert!(core.get_draft(0, 7).is_none(), "not stored as contact 7's draft");
        assert_eq!(h.state().state.chats[1].preview, "Черновик: черновик");
    }

    #[test]
    fn toolbar_buttons_set_flags() {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        h.get_by_label("Профиль").click();
        h.run();
        assert!(h.state().state.show_profile);
        h.state_mut().state.show_profile = false; // the profile modal covers the toolbar while open
        h.run();
        h.get_by_label("Добавить контакт").click();
        h.run();
        assert!(h.state().state.show_add_contact);
    }

    fn list_width(window: f32) -> f32 {
        let mut h = test_harness(Settings::default());
        h.set_size(eframe::egui::vec2(window, 700.));
        h.run();
        h.get_by_label("Начать").click();
        h.run();
        h.run();
        h.get_by_label("Избранное").rect().width()
    }

    #[test]
    fn list_width_is_30_percent_clamped() {
        assert!((list_width(1000.) - 300.).abs() < 3.);
        assert!((list_width(600.) - 260.).abs() < 3.);
        assert!((list_width(1800.) - 360.).abs() < 3.);
    }

    #[test]
    fn list_width_follows_window_resize_until_dragged() {
        let mut h = test_harness(Settings::default());
        h.set_size(eframe::egui::vec2(1200., 700.));
        h.run();
        h.get_by_label("Начать").click();
        h.run();
        h.run();
        let width = |h: &egui_kittest::Harness<'static, crate::app::MimirDesktop>| h.get_by_label("Избранное").rect().width();
        assert!((width(&h) - 360.).abs() < 3.);
        h.set_size(eframe::egui::vec2(700., 700.));
        h.run();
        h.run();
        assert!((width(&h) - 260.).abs() < 3., "narrow: {}", width(&h));
        h.set_size(eframe::egui::vec2(1000., 700.));
        h.run();
        h.run();
        assert!((width(&h) - 300.).abs() < 3., "mid: {}", width(&h));
        // A user drag of the divider wins over later resizes.
        use eframe::egui::{pos2, Event, Modifiers, PointerButton};
        let edge = pos2(width(&h), 300.);
        let button = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        h.event(Event::PointerMoved(edge));
        h.run();
        h.event(button(edge, true));
        h.run();
        for dx in [10., 30., 60.] {
            h.event(Event::PointerMoved(edge + eframe::egui::vec2(dx, 0.)));
            h.run();
        }
        h.event(button(edge + eframe::egui::vec2(60., 0.), false));
        h.run();
        assert!((width(&h) - 360.).abs() < 4., "dragged: {}", width(&h));
        h.set_size(eframe::egui::vec2(700., 700.));
        h.run();
        h.run();
        assert!((width(&h) - 360.).abs() < 4., "after resize: {}", width(&h));
    }

    #[test]
    fn empty_pane_shows_hint() {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        h.run();
        assert!(h.query_by_label("Выберите чат, чтобы начать переписку").is_some());
        h.get_by_label("Избранное").click();
        h.run();
        assert!(h.query_by_label("Выберите чат, чтобы начать переписку").is_none());
    }
}
