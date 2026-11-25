//! Mediators registry modal (Android MediatorsActivity): the list with connection dots, add / rename /
//! default / delete, and invitations to mediators we do not use yet. Every core call runs on the worker
//! (Android does them on the UI thread); only reads of stored rows happen inline.
use crate::{
    app::MimirDesktop,
    core::worker::WorkResult,
    i18n::{tr, trf, Key},
    model::{chat_list::display_name, Dot},
    ui::{
        shell::dot_color,
        theme::{danger_button, dialog, dialog_title, field_label, palette, primary_button, Palette},
    },
};
use eframe::egui::{self, Align, Color32, CursorIcon, FontFamily, FontId, Id, Layout, Popup, RichText, ScrollArea, Sense, TextEdit};
use mimir::{MimirApp, MimirError, StoredMediator, StoredMediatorInvitation};

/// Add / rename form; renaming keeps the key (shown disabled).
#[derive(Default)]
pub struct Form {
    pub rename_of: Option<Vec<u8>>,
    pub key: String,
    pub name: String,
    pub error: Option<Key>,
}

#[derive(Default)]
pub struct MediatorsUi {
    pub list: Vec<StoredMediator>,
    /// Pending invitations with the inviter's display name.
    pub invitations: Vec<(StoredMediatorInvitation, String)>,
    pub form: Option<Form>,
    /// Mediator key awaiting delete confirmation.
    pub confirm_delete: Option<Vec<u8>>,
    /// Mediator key of the invitation being looked at.
    pub invitation: Option<Vec<u8>>,
}

/// Name of the mediator, or «Unnamed mediator».
pub fn mediator_label(m: &StoredMediator, lang: crate::i18n::Lang) -> String {
    if m.name.trim().is_empty() { tr(lang, Key::MediatorUnnamed).to_string() } else { m.name.clone() }
}

/// Keeps the first `head` and last `tail` characters, with «…» between (egui labels cannot ellipsize in the middle).
pub fn middle_ellipsis(s: &str, head: usize, tail: usize) -> String {
    let n = s.chars().count();
    if n <= head + tail {
        return s.to_string();
    }
    format!("{}…{}", s.chars().take(head).collect::<String>(), s.chars().skip(n - tail).collect::<String>())
}

/// The inviter as the user knows them: the contact's name, or the first 8 hex digits of the key.
fn from_name(core: &MimirApp, pubkey: &[u8]) -> String {
    display_name(&core.get_contact_by_pubkey(pubkey.to_vec()).map(|c| c.name).unwrap_or_default(), pubkey)
}

/// Re-reads the registry and the invitations (stored rows, cheap).
pub fn refresh(app: &mut MimirDesktop) {
    refresh_invitations(app);
    app.state.mediators_ui.list = app.core.get_mediators();
}

/// Re-reads the pending mediator invitations with their inviters' names (also counted in the invites banner).
pub fn refresh_invitations(app: &mut MimirDesktop) {
    let invitations = app.core.get_mediator_invitations();
    app.state.mediators_ui.invitations = invitations.into_iter().map(|i| {
        let name = from_name(&app.core, &i.from_pubkey);
        (i, name)
    }).collect();
}

pub fn open(app: &mut MimirDesktop) {
    app.state.show_mediators = true;
    refresh(app);
}

/// Runs a core call on the worker; `ok` is the status line text on success. A failure shows the core's message.
fn run(app: &mut MimirDesktop, ok: Option<Key>, job: impl FnOnce(&MimirApp) -> Result<(), MimirError> + Send + 'static) {
    app.group_worker.submit(Box::new(move |core| match job(core) {
        Ok(()) => ok.map_or(WorkResult::Done, WorkResult::Notice),
        Err(e) => WorkResult::Failed { what: "mediator", err: e.to_string() },
    }));
}

enum Act {
    Add,
    OpenInvitation(Vec<u8>),
    Rename(Vec<u8>, String),
    SetDefault(Vec<u8>),
    Copy(Vec<u8>),
    Delete(Vec<u8>, i32),
}

fn dot(ui: &mut egui::Ui, color: Color32) {
    let (r, _) = ui.allocate_exact_size(egui::vec2(10., 10.), Sense::hover());
    ui.painter().circle_filled(r.center(), 5., color);
}

/// Dot, then lines of text, as one clickable block; returns its response.
fn block(ui: &mut egui::Ui, id: impl std::hash::Hash + std::fmt::Debug, color: Color32, lines: impl FnOnce(&mut egui::Ui)) -> egui::Response {
    let rect = ui
        .scope(|ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                dot(ui, color);
                ui.vertical(lines);
            });
        })
        .response
        .rect;
    ui.interact(rect, ui.scope_id().with(id), Sense::click()).on_hover_cursor(CursorIcon::PointingHand)
}

fn small(p: &Palette, text: impl Into<String>) -> RichText {
    RichText::new(text).size(11.).color(p.text_dim)
}

fn mediator_row(ui: &mut egui::Ui, p: &Palette, app: &MimirDesktop, m: &StoredMediator) -> Option<Act> {
    let lang = app.lang;
    let key = hex::encode_upper(&m.pubkey);
    let connected = app.state.connected_mediators.contains(&m.pubkey);
    let grey = Color32::from_gray(0x80);
    let resp = block(ui, &key, if connected { dot_color(Dot::Green) } else { grey }, |ui| {
        ui.label(RichText::new(mediator_label(m, lang)).size(14.).family(FontFamily::Name("bold".into())));
        ui.label(RichText::new(middle_ellipsis(&key, 20, 20)).font(FontId::monospace(11.)).color(p.text_dim));
        let mut sub = trf(lang, Key::MediatorChats, &[&m.chat_count.to_string()]);
        if m.is_default {
            sub = format!("{sub} · {}", tr(lang, Key::MediatorDefaultBadge));
        }
        ui.label(small(p, sub));
    });
    let mut act = None;
    Popup::menu(&resp).show(|ui| {
        ui.set_min_width(200.);
        if !m.is_default && ui.button(tr(lang, Key::MediatorUseForNewChats)).clicked() {
            act = Some(Act::SetDefault(m.pubkey.clone()));
        }
        if ui.button(tr(lang, Key::RenameMediator)).clicked() {
            act = Some(Act::Rename(m.pubkey.clone(), m.name.clone()));
        }
        if ui.button(tr(lang, Key::MenuCopy)).clicked() {
            act = Some(Act::Copy(m.pubkey.clone()));
        }
        if ui.button(tr(lang, Key::Delete)).clicked() {
            act = Some(Act::Delete(m.pubkey.clone(), m.chat_count));
        }
    });
    act
}

fn registry(app: &mut MimirDesktop, ctx: &egui::Context) {
    let (lang, started) = (app.lang, app.state.core_started);
    let (mut act, mut close) = (None, false);
    let r = dialog(ctx, Id::unique("mediators")).show(ctx, |ui| {
        ui.set_width(420.);
        let p = palette(ui.visuals().dark_mode);
        ui.horizontal(|ui| {
            ui.label(RichText::new(tr(lang, Key::Mediators)).size(16.).family(FontFamily::Name("bold".into())));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("+").on_hover_text(tr(lang, Key::AddMediator)).clicked() {
                    act = Some(Act::Add);
                }
            });
        });
        ui.add_space(8.);
        ui.label(tr(lang, Key::MediatorsDesc));
        if !started {
            ui.weak(tr(lang, Key::NotStarted));
        }
        ui.add_space(8.);
        ScrollArea::vertical().max_height(360.).show(ui, |ui| {
            ui.set_width(ui.available_width());
            for (inv, name) in &app.state.mediators_ui.invitations {
                let id = ("invitation", &inv.mediator_pubkey);
                let sub = trf(lang, Key::MediatorInvitationRow, &[name]);
                let resp = block(ui, id, Color32::from_gray(0x80), |ui| {
                    ui.label(RichText::new(tr(lang, Key::MediatorInvitation)).size(14.).family(FontFamily::Name("bold".into())));
                    ui.label(small(&p, sub));
                });
                if resp.clicked() {
                    act = Some(Act::OpenInvitation(inv.mediator_pubkey.clone()));
                }
                ui.add_space(6.);
            }
            for m in &app.state.mediators_ui.list {
                act = mediator_row(ui, &p, app, m).or(act.take());
                ui.add_space(6.);
            }
        });
        ui.add_space(8.);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            close = ui.button(tr(lang, Key::Close)).clicked();
        });
    });
    match act {
        Some(Act::Add) => app.state.mediators_ui.form = Some(Form::default()),
        Some(Act::OpenInvitation(pk)) => app.state.mediators_ui.invitation = Some(pk),
        Some(Act::Rename(pk, name)) => {
            let key = hex::encode_upper(&pk);
            app.state.mediators_ui.form = Some(Form { rename_of: Some(pk), key, name, error: None });
        }
        Some(Act::SetDefault(pk)) => run(app, None, move |c| c.set_default_mediator(pk)),
        Some(Act::Copy(pk)) => {
            ctx.copy_text(hex::encode_upper(pk));
            app.notice(Key::CopiedToClipboard);
        }
        Some(Act::Delete(pk, 0)) => app.state.mediators_ui.confirm_delete = Some(pk),
        Some(Act::Delete(_, n)) => {
            let text = trf(lang, Key::MediatorHasChats, &[&n.to_string()]);
            app.error_text(text);
        }
        None => {}
    }
    if close || r.should_close() {
        app.state.show_mediators = false;
    }
}

fn submit_form(app: &mut MimirDesktop) {
    let Some(f) = app.state.mediators_ui.form.as_mut() else { return };
    let name = f.name.trim().to_string();
    if let Some(pk) = f.rename_of.clone() {
        app.state.mediators_ui.form = None;
        run(app, None, move |c| c.rename_mediator(pk, name));
        return;
    }
    match hex::decode(f.key.trim()).ok().filter(|k| k.len() == 32) {
        None => f.error = Some(Key::MediatorInvalidPubkey),
        Some(pk) => {
            app.state.mediators_ui.form = None;
            run(app, Some(Key::MediatorAdded), move |c| c.add_mediator(pk, name));
        }
    }
}

fn form_dialog(app: &mut MimirDesktop, ctx: &egui::Context) {
    let (lang, started) = (app.lang, app.state.core_started);
    let (mut go, mut cancel) = (false, false);
    let r = dialog(ctx, Id::unique("mediator_form")).show(ctx, |ui| {
        ui.set_width(420.);
        let p = palette(ui.visuals().dark_mode);
        let Some(f) = app.state.mediators_ui.form.as_mut() else { return };
        let renaming = f.rename_of.is_some();
        dialog_title(ui, tr(lang, if renaming { Key::RenameMediator } else { Key::AddMediator }));
        let l = field_label(ui, &p, tr(lang, Key::MediatorPubkeyLabel));
        let key = ui
            .add_enabled(
                !renaming,
                TextEdit::singleline(&mut f.key).font(FontId::monospace(12.)).hint_text(tr(lang, Key::MediatorPubkeyHint)).desired_width(f32::INFINITY),
            )
            .labelled_by(l.id);
        let l = field_label(ui, &p, tr(lang, Key::MediatorNameLabel));
        let name = ui.add(TextEdit::singleline(&mut f.name).hint_text(tr(lang, Key::MediatorNameHint)).desired_width(f32::INFINITY)).labelled_by(l.id);
        if key.changed() || name.changed() {
            f.error = None; // the «invalid key» error goes once the form is edited
        }
        if let Some(e) = f.error {
            ui.colored_label(p.danger, tr(lang, e));
        }
        // Renaming is storage only; adding dials the mediator.
        let enabled = started || renaming;
        if !enabled {
            ui.weak(tr(lang, Key::NotStarted));
        }
        ui.add_space(8.);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            // Enter in either field submits, as the button does.
            let enter = (key.lost_focus() || name.lost_focus()) && ui.input(|i| i.key_pressed(egui::Key::Enter));
            go = ui.add_enabled(enabled, primary_button(&p, tr(lang, if renaming { Key::Save } else { Key::AddButton }))).clicked() || (enter && enabled);
            cancel = ui.button(tr(lang, Key::Cancel)).clicked();
        });
    });
    if go {
        submit_form(app);
    } else if cancel || r.should_close() {
        app.state.mediators_ui.form = None;
    }
}

fn delete_dialog(app: &mut MimirDesktop, ctx: &egui::Context, pk: Vec<u8>) {
    let lang = app.lang;
    let (mut go, mut cancel) = (false, false);
    let r = dialog(ctx, Id::unique("mediator_delete")).show(ctx, |ui| {
        ui.set_width(380.);
        let p = palette(ui.visuals().dark_mode);
        dialog_title(ui, tr(lang, Key::Delete));
        ui.label(tr(lang, Key::MediatorDeleteConfirm));
        ui.add_space(8.);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            go = ui.add(danger_button(&p, tr(lang, Key::Delete))).clicked();
            cancel = ui.button(tr(lang, Key::Cancel)).clicked();
        });
    });
    if go {
        app.state.mediators_ui.confirm_delete = None;
        run(app, None, move |c| c.delete_mediator(pk));
    } else if cancel || r.should_close() {
        app.state.mediators_ui.confirm_delete = None;
    }
}

fn invitation_dialog(app: &mut MimirDesktop, ctx: &egui::Context, pk: Vec<u8>) {
    let (lang, started) = (app.lang, app.state.core_started);
    let from = app.state.mediators_ui.invitations.iter().find(|(i, _)| i.mediator_pubkey == pk).map(|(_, n)| n.clone());
    let Some(from) = from else {
        // Answered meanwhile (or from another device): nothing to show.
        app.state.mediators_ui.invitation = None;
        return;
    };
    let (mut accept, mut decline, mut cancel) = (false, false, false);
    let r = dialog(ctx, Id::unique("mediator_invitation")).show(ctx, |ui| {
        ui.set_width(420.);
        let p = palette(ui.visuals().dark_mode);
        dialog_title(ui, tr(lang, Key::MediatorInvitation));
        ui.label(trf(lang, Key::MediatorInvitationMessage, &[&from, &hex::encode_upper(&pk)]));
        if !started {
            ui.weak(tr(lang, Key::NotStarted));
        }
        ui.add_space(8.);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            accept = ui.add_enabled(started, primary_button(&p, tr(lang, Key::MediatorConnect))).clicked();
            decline = ui.add(danger_button(&p, tr(lang, Key::Delete))).clicked();
            cancel = ui.button(tr(lang, Key::Cancel)).clicked();
        });
    });
    if accept || decline {
        // Hide it right away so it cannot be answered twice; WorkDone reloads the real list.
        app.state.mediators_ui.invitations.retain(|(i, _)| i.mediator_pubkey != pk);
        app.state.mediators_ui.invitation = None;
        if accept {
            run(app, None, move |c| c.accept_mediator_invitation(pk));
        } else {
            run(app, Some(Key::MediatorInvitationDeclined), move |c| c.decline_mediator_invitation(pk));
        }
    } else if cancel || r.should_close() {
        app.state.mediators_ui.invitation = None;
    }
}

/// One dialog at a time: a sub-dialog replaces the registry (like the profile's QR dialog) and returns to it.
pub fn show(app: &mut MimirDesktop, ctx: &egui::Context) {
    if !app.state.show_mediators {
        return;
    }
    let m = &app.state.mediators_ui;
    if m.form.is_some() {
        form_dialog(app, ctx);
    } else if let Some(pk) = m.confirm_delete.clone() {
        delete_dialog(app, ctx, pk);
    } else if let Some(pk) = m.invitation.clone() {
        invitation_dialog(app, ctx, pk);
    } else {
        registry(app, ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{settings::Settings, test_support::{start_app, test_harness}};
    use eframe::egui::{epaint::Shape, Role};
    use egui_kittest::{kittest::Queryable as _, Harness};
    use std::{sync::mpsc::channel, time::{Duration, Instant}};

    const A: [u8; 32] = [0xA1; 32];
    const B: [u8; 32] = [0xB2; 32];
    const HEX: &str = "f9a4e8d77c34ce192aebc553d9815709c28d35c0a773160508de91a0dac43c22";

    type H = Harness<'static, MimirDesktop>;

    /// Dialog open over a core holding A «Семья» (default, first added) and B (unnamed).
    fn open_dialog(started: bool) -> H {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        if started {
            let ctx = h.ctx.clone();
            start_app(h.state_mut(), &ctx);
            h.state_mut().state.core_started = true;
        }
        let core = h.state().core.clone();
        core.add_mediator(A.to_vec(), "Семья".into()).unwrap();
        core.add_mediator(B.to_vec(), String::new()).unwrap();
        open(h.state_mut());
        h.run();
        h
    }

    fn wait(h: &mut H, what: &str, done: impl Fn(&H) -> bool) {
        let end = Instant::now() + Duration::from_secs(5);
        while !done(h) && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
            h.step(); // the status line keeps asking for repaints, so `run` would not settle
        }
        assert!(done(h), "timed out waiting for {what}");
    }

    fn status(h: &H) -> Option<String> {
        h.state().state.status.as_ref().map(|s| s.0.clone())
    }

    /// (centre, radius, fill) of every filled circle drawn last frame.
    fn circles(h: &H) -> Vec<(egui::Pos2, f32, Color32)> {
        fn walk(s: &Shape, out: &mut Vec<(egui::Pos2, f32, Color32)>) {
            match s {
                Shape::Circle(c) => out.push((c.center, c.radius, c.fill)),
                Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut out = vec![];
        h.output().shapes.iter().for_each(|cs| walk(&cs.shape, &mut out));
        out
    }

    fn row_dots(h: &H) -> Vec<Color32> {
        let mut d: Vec<_> = circles(h).into_iter().filter(|c| c.1 == 5.).collect();
        d.sort_by(|a, b| a.0.y.total_cmp(&b.0.y));
        d.into_iter().map(|c| c.2).collect()
    }

    fn names(h: &H) -> Vec<Vec<u8>> {
        h.state().core.get_mediators().into_iter().map(|m| m.pubkey).collect()
    }

    #[test]
    fn lists_mediators_default_first() {
        let mut h = open_dialog(false);
        h.state().core.set_default_mediator(B.to_vec()).unwrap();
        open(h.state_mut());
        h.run();
        assert_eq!(names(&h), vec![B.to_vec(), A.to_vec()], "core order");
        let (unnamed, family) = (h.get_by_label("Без названия").rect(), h.get_by_label("Семья").rect());
        assert!(unnamed.top() < family.top(), "default first");
        let short = |k: &[u8; 32]| middle_ellipsis(&hex::encode_upper(k), 20, 20);
        assert!(short(&B).contains('…') && short(&B).starts_with("B2B2"));
        assert!(h.query_by_label(&short(&B)).is_some() && h.query_by_label(&short(&A)).is_some());
        assert!(h.query_by_label("Чатов: 0 · По умолчанию для новых чатов").is_some());
        assert!(h.query_by_label("Чатов: 0").is_some());
        assert!(h.query_by_label("Серверы, на которых живут ваши групповые чаты. Выберите, где создавать новые.").is_some());
    }

    #[test]
    fn middle_ellipsis_keeps_both_ends() {
        assert_eq!(middle_ellipsis("0123456789", 2, 3), "01…789");
        assert_eq!(middle_ellipsis("0123", 2, 2), "0123");
        assert_eq!(middle_ellipsis(&"A".repeat(64), 20, 20).chars().count(), 41);
    }

    #[test]
    fn status_dot_follows_events() {
        use crate::core::events::Event;
        let mut h = open_dialog(false);
        let (tx, rx) = channel();
        h.state_mut().rx = rx;
        let grey = row_dots(&h);
        assert_eq!(grey.len(), 2);
        assert!(grey.iter().all(|c| *c != dot_color(Dot::Green)));
        // Registry order: A (default), B.
        tx.send(Event::MediatorAuthenticated { mediator_pubkey: B.to_vec() }).unwrap();
        h.run();
        assert_eq!(row_dots(&h), vec![grey[0], dot_color(Dot::Green)]);
        assert_eq!(h.state().state.net.mediator, Dot::Green);
        let toolbar: Vec<_> = {
            let mut t: Vec<_> = circles(&h).into_iter().filter(|c| c.1 == 4.).collect();
            t.sort_by(|a, b| a.0.x.total_cmp(&b.0.x));
            t.into_iter().map(|c| c.2).collect()
        };
        assert_eq!(toolbar[2], dot_color(Dot::Green), "toolbar mediator dot");
        tx.send(Event::MediatorDisconnected { mediator_pubkey: B.to_vec() }).unwrap();
        h.run();
        assert_eq!(row_dots(&h), grey);
        assert_eq!(h.state().state.net.mediator, Dot::Red);
    }

    #[test]
    fn add_validates_hex() {
        let mut h = open_dialog(true);
        let before = names(&h);
        h.get_by_label("+").click();
        h.run();
        h.state_mut().state.mediators_ui.form.as_mut().unwrap().key = HEX[..63].into();
        h.run();
        h.get_by_label("Добавить").click();
        h.run();
        assert!(h.query_by_label("Публичный ключ должен состоять из 64 шестнадцатеричных символов").is_some());
        h.run();
        assert_eq!(names(&h), before, "nothing added");
        {
            let f = h.state_mut().state.mediators_ui.form.as_mut().unwrap();
            (f.key, f.name) = (HEX.into(), "  Работа  ".into());
        }
        h.run();
        h.get_by_label("Добавить").click();
        h.step();
        let pk = hex::decode(HEX).unwrap();
        wait(&mut h, "mediator added", |h| names(h).contains(&pk) && status(h).as_deref() == Some("Медиатор добавлен"));
        assert_eq!(h.state().core.get_mediators().into_iter().find(|m| m.pubkey == pk).unwrap().name, "Работа");
        assert!(h.state().state.mediators_ui.form.is_none());
        assert!(h.state().state.status_ok);
        wait(&mut h, "list refreshed", |h| h.query_by_label("Работа").is_some());
    }

    #[test]
    fn add_is_disabled_until_core_started() {
        let mut h = open_dialog(false);
        h.get_by_label("+").click();
        h.run();
        {
            let f = h.state_mut().state.mediators_ui.form.as_mut().unwrap();
            f.key = HEX.into();
        }
        h.run();
        h.get_by_label("Добавить").click();
        h.run();
        assert!(h.state().state.mediators_ui.form.is_some(), "stays open");
        assert!(h.query_by_label("Сеть ещё не запущена, подождите").is_some());
        assert!(!names(&h).contains(&hex::decode(HEX).unwrap()));
    }

    #[test]
    fn delete_refused_with_chats() {
        let mut h = open_dialog(false);
        h.state().core.save_group_chat(7, "Дача".into(), String::new(), None, B.to_vec(), vec![], vec![3; 32], 0).unwrap();
        open(h.state_mut());
        h.run();
        h.get_by_label("Чатов: 1").click();
        h.run();
        h.get_by_label("Удалить").click();
        h.run();
        assert_eq!(status(&h).as_deref(), Some("На этом медиаторе ещё есть чаты (1). Сначала выйдите из них."));
        assert!(!h.state().state.status_ok);
        assert!(h.state().state.mediators_ui.confirm_delete.is_none());
        assert!(names(&h).contains(&B.to_vec()));
    }

    #[test]
    fn delete_confirmed_without_chats() {
        let mut h = open_dialog(false);
        h.get_by_label("Без названия").click();
        h.run();
        h.get_by_label("Удалить").click();
        h.run();
        assert!(h.query_by_label("Удалить этот медиатор? Новые групповые чаты больше не будут создаваться на нём.").is_some());
        assert!(names(&h).contains(&B.to_vec()), "not yet");
        h.get_by_role_and_label(Role::Button, "Отмена").click();
        h.run();
        assert!(names(&h).contains(&B.to_vec()), "cancelled");
        h.get_by_label("Без названия").click();
        h.run();
        h.get_by_label("Удалить").click();
        h.run();
        h.get_by_role_and_label(Role::Button, "Удалить").click();
        h.step();
        wait(&mut h, "mediator removed", |h| !names(h).contains(&B.to_vec()));
        wait(&mut h, "list refreshed", |h| h.query_by_label("Без названия").is_none());
    }

    #[test]
    fn rename_and_set_default() {
        let mut h = open_dialog(false);
        // A is the default: no «use for new chats» in its menu.
        h.get_by_label("Семья").click();
        h.run();
        assert!(h.query_by_label("Использовать для новых чатов").is_none());
        h.key_press(egui::Key::Escape);
        h.run();
        h.get_by_label("Без названия").click();
        h.run();
        h.get_by_label("Использовать для новых чатов").click();
        h.step();
        wait(&mut h, "default moved", |h| h.state().core.get_default_mediator().as_deref() == Some(&B[..]));
        wait(&mut h, "list refreshed", |h| h.state().state.mediators_ui.list.first().is_some_and(|m| m.pubkey == B));
        h.run();
        h.get_by_label("Без названия").click();
        h.run();
        h.get_by_label("Переименовать медиатор").click();
        h.run();
        let f = h.state().state.mediators_ui.form.as_ref().unwrap();
        assert_eq!((f.rename_of.as_deref(), f.key.as_str()), (Some(&B[..]), hex::encode_upper(B).as_str()), "the key is kept, not edited");
        h.state_mut().state.mediators_ui.form.as_mut().unwrap().name = "  Дача  ".into();
        h.run();
        h.get_by_label("Сохранить").click();
        h.step();
        wait(&mut h, "renamed", |h| h.state().core.get_mediators().iter().any(|m| m.pubkey == B && m.name == "Дача"));
        wait(&mut h, "list refreshed", |h| h.query_by_label("Дача").is_some());
    }

    fn with_invitation(started: bool) -> H {
        let mut h = open_dialog(started);
        let inv = StoredMediatorInvitation { mediator_pubkey: hex_pk(), from_pubkey: vec![7; 32], time: 0 };
        h.state_mut().state.mediators_ui.invitations = vec![(inv, "Аня".into())];
        h.run();
        h
    }

    fn hex_pk() -> Vec<u8> {
        hex::decode(HEX).unwrap()
    }

    #[test]
    fn mediator_invitations_listed() {
        let mut h = with_invitation(true);
        let title = h.get_by_label("Ожидает приглашение в группу").rect();
        assert!(title.top() < h.get_by_label("Семья").rect().top(), "above the mediators");
        h.get_by_label("Приглашение от Аня · нажмите, чтобы подключиться").click();
        h.run();
        let full = HEX.to_uppercase();
        assert!(h.query_by_label_contains(&full).is_some(), "full uppercase key");
        assert!(h.query_by_label_contains("Аня приглашает вас в групповой чат на этом медиаторе").is_some());
        for b in ["Подключиться", "Удалить", "Отмена"] {
            assert!(h.query_by_role_and_label(Role::Button, b).is_some(), "{b}");
        }
        h.get_by_role_and_label(Role::Button, "Отмена").click();
        h.run();
        assert!(h.state().state.mediators_ui.invitation.is_none());
        assert!(h.query_by_label("Ожидает приглашение в группу").is_some(), "cancel keeps it");
    }

    #[test]
    fn invitation_accept_registers_the_mediator() {
        let mut h = with_invitation(true);
        h.get_by_label("Приглашение от Аня · нажмите, чтобы подключиться").click();
        h.run();
        h.get_by_role_and_label(Role::Button, "Подключиться").click();
        h.step();
        assert!(h.state().state.mediators_ui.invitations.is_empty(), "hidden at once");
        wait(&mut h, "mediator registered", |h| names(h).contains(&hex_pk()));
    }

    #[test]
    fn invitation_decline_is_local_and_reported() {
        let mut h = with_invitation(true);
        let before = names(&h);
        h.get_by_label("Приглашение от Аня · нажмите, чтобы подключиться").click();
        h.run();
        h.get_by_role_and_label(Role::Button, "Удалить").click();
        h.step();
        wait(&mut h, "decline reported", |h| status(h).as_deref() == Some("Приглашение отклонено"));
        assert_eq!(names(&h), before);
    }

    #[test]
    fn invitation_sender_is_contact_name_or_hex_prefix() {
        let (_t, core) = crate::test_support::test_app();
        core.add_contact(vec![7; 32], "Аня".into()).unwrap();
        assert_eq!(from_name(&core, &[7; 32]), "Аня");
        assert_eq!(from_name(&core, &[0xAB; 32]), "abababab");
    }

    #[test]
    fn main_menu_opens_mediators() {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        h.run();
        assert!(!h.state().state.show_mediators);
        h.get_by_label("Главное меню").click();
        h.run();
        h.get_by_label("Медиаторы").click();
        h.run();
        assert!(h.state().state.show_mediators);
        assert!(h.query_by_label("Серверы, на которых живут ваши групповые чаты. Выберите, где создавать новые.").is_some());
        h.get_by_label("Закрыть").click();
        h.run();
        assert!(!h.state().state.show_mediators);
    }

    #[test]
    fn row_menu_copy_uses_menu_copy_text() {
        let mut h = open_dialog(false);
        h.get_by_label("Без названия").click();
        h.run();
        assert!(h.query_by_label("Копировать").is_none());
        h.get_by_label("Скопировать").click();
        h.run();
        assert!(h.state().state.status_ok && status(&h).is_some(), "copied notice");
    }

    #[test]
    fn enter_submits_the_form() {
        let mut h = open_dialog(true);
        h.get_by_label("+").click();
        h.run();
        h.get_by_label("Публичный ключ:").click();
        h.get_by_label("Публичный ключ:").type_text("zz");
        h.run();
        h.key_press(egui::Key::Enter);
        h.run();
        assert!(h.query_by_label("Публичный ключ должен состоять из 64 шестнадцатеричных символов").is_some(), "Enter ran the submit");
    }

    #[test]
    fn form_error_cleared_on_edit() {
        let mut h = open_dialog(true);
        h.get_by_label("+").click();
        h.run();
        h.get_by_label("Добавить").click();
        h.run();
        let error = "Публичный ключ должен состоять из 64 шестнадцатеричных символов";
        assert!(h.query_by_label(error).is_some());
        h.get_by_label("Публичный ключ:").click();
        h.get_by_label("Публичный ключ:").type_text("a");
        h.run();
        assert!(h.query_by_label(error).is_none(), "cleared once the key is edited");
    }

    #[test]
    fn labels_fall_back_to_unnamed() {
        let m = StoredMediator { pubkey: vec![1], name: "  ".into(), added_at: 0, is_default: false, chat_count: 0 };
        assert_eq!(mediator_label(&m, crate::i18n::Lang::Ru), "Без названия");
        assert_eq!(mediator_label(&m, crate::i18n::Lang::En), "Unnamed mediator");
    }
}
