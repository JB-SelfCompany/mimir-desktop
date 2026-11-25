//! Create-group dialog (Android GroupChatEditActivity, create mode): name, description, mediator, public switch.
//! `create_group` blocks on the network plus proof-of-work for seconds, so it runs on the worker; the dialog stays
//! open (spinner, no buttons) until the result comes back.
use crate::{
    app::MimirDesktop,
    i18n::{tr, Key},
    media::avatar::GROUP_MAX,
    ui::{avatar::{self, pick_button}, theme::{dialog, dialog_title, field_label, palette, primary_button}},
};
use eframe::egui::{self, Align, Color32, ComboBox, Id, Layout, RichText, Spinner, TextEdit};
use mimir::StoredMediator;

/// Android `MAX_NAME_LENGTH` / `MAX_DESCRIPTION_LENGTH`, counted in characters.
pub(crate) const MAX_NAME: usize = 25;
pub(crate) const MAX_DESCRIPTION: usize = 200;

#[derive(Default)]
pub struct NewGroupDialog {
    pub name: String,
    pub description: String,
    pub public: bool,
    /// Target mediator; the default one until the user picks another.
    pub mediator: Option<Vec<u8>>,
    /// The registry as of opening (stored rows).
    pub mediators: Vec<StoredMediator>,
    /// True while the worker creates the group.
    pub busy: bool,
    pub error: Option<String>,
    /// The picked avatar (≤ 200 KiB), sent with `create_group`.
    pub avatar: crate::ui::avatar::AvatarPick,
    /// The avatar handed to the last `create_group` job, for the tests.
    #[cfg(test)]
    pub submitted_avatar: Option<Vec<u8>>,
}

pub fn open(app: &mut MimirDesktop) {
    let mediators = app.core.get_mediators();
    let default = app.core.get_default_mediator().or_else(|| mediators.first().map(|m| m.pubkey.clone()));
    app.state.new_group = Some(NewGroupDialog { mediators, mediator: default, ..Default::default() });
}

/// Mediator name, or the first 8 hex digits of its key.
fn picker_label(m: &StoredMediator) -> String {
    if m.name.trim().is_empty() { hex::encode_upper(&m.pubkey).chars().take(8).collect() } else { m.name.clone() }
}

fn submit(app: &mut MimirDesktop) {
    let lang = app.lang;
    let Some(d) = app.state.new_group.as_mut() else { return };
    let (name, description) = (d.name.trim().to_string(), d.description.trim().to_string());
    if name.is_empty() {
        d.error = Some(tr(lang, Key::GroupNameRequired).to_string());
        return;
    }
    let Some(mediator) = d.mediator.clone() else { return };
    let (public, avatar) = (d.public, d.avatar.bytes.clone());
    d.busy = true;
    d.error = None;
    #[cfg(test)]
    {
        d.submitted_avatar = avatar.clone();
    }
    // Tests do not run the job: create_group dials and mines, and an unreachable mediator holds the worker for seconds.
    #[cfg(not(test))]
    app.group_worker.submit(Box::new(move |core| match core.create_group(mediator, name, description, avatar, public) {
        Ok(chat_id) => crate::core::worker::WorkResult::GroupCreated { chat_id },
        Err(e) => crate::core::worker::WorkResult::Failed { what: "create_group", err: e.to_string() },
    }));
    #[cfg(test)]
    let _ = (mediator, name, description, public);
}

/// A text field with its «N/max» counter under it; true when the text was edited this frame.
pub(crate) fn counted(ui: &mut egui::Ui, text: &mut String, hint: &str, multiline: bool, max: usize, label: &egui::Response) -> bool {
    let edit = if multiline { TextEdit::multiline(text).desired_rows(3) } else { TextEdit::singleline(text) };
    let changed = ui.add(edit.hint_text(hint).char_limit(max).desired_width(f32::INFINITY)).labelled_by(label.id).changed();
    ui.label(RichText::new(format!("{}/{max}", text.chars().count())).size(12.).weak());
    changed
}

pub fn show(app: &mut MimirDesktop, ctx: &egui::Context) {
    if app.state.new_group.is_none() {
        return;
    }
    let (lang, started) = (app.lang, app.state.core_started);
    if let Some(d) = app.state.new_group.as_mut() {
        if let Some(e) = d.avatar.poll(ctx) {
            d.error = Some(avatar::error_text(e, lang).to_string());
        }
    }
    let (mut go, mut cancel) = (false, false);
    let r = dialog(ctx, Id::unique("new_group")).show(ctx, |ui| {
        ui.set_width(420.);
        let p = palette(ui.visuals().dark_mode);
        let Some(d) = app.state.new_group.as_mut() else { return };
        dialog_title(ui, tr(lang, Key::CreateNewGroup));
        ui.add_enabled_ui(!d.busy, |ui| {
            // The group avatar (≤ 200 KiB), 128 px; no chat id to colour the placeholder yet, so Android's grey.
            ui.vertical_centered(|ui| {
                if pick_button(ui, &mut app.state.thumbs, &d.avatar, None, Color32::from_gray(0xC8), 128., tr(lang, Key::ChangeAvatar)) {
                    d.avatar.start(ui.ctx(), tr(lang, Key::Image), GROUP_MAX);
                }
            });
            let l = field_label(ui, &p, tr(lang, Key::GroupName));
            if counted(ui, &mut d.name, tr(lang, Key::GroupNameHint), false, MAX_NAME, &l) {
                d.error = None; // «name required» (or a create failure) goes once the name is edited
            }
            let l = field_label(ui, &p, tr(lang, Key::GroupDescription));
            counted(ui, &mut d.description, tr(lang, Key::GroupDescriptionHint), true, MAX_DESCRIPTION, &l);
            if d.mediators.len() >= 2 {
                let l = field_label(ui, &p, tr(lang, Key::CreateOnMediator));
                let current = d.mediators.iter().find(|m| Some(&m.pubkey) == d.mediator.as_ref()).map(picker_label).unwrap_or_default();
                ComboBox::from_id_salt("new_group_mediator").selected_text(current).width(ui.available_width()).show_ui(ui, |ui| {
                    for m in &d.mediators {
                        ui.selectable_value(&mut d.mediator, Some(m.pubkey.clone()), picker_label(m));
                    }
                })
                .response
                .labelled_by(l.id);
            }
            ui.add_space(6.);
            ui.checkbox(&mut d.public, tr(lang, Key::GroupPublicLabel));
            if d.public {
                ui.label(RichText::new(tr(lang, Key::GroupPublicHint)).size(12.).color(p.text_dim));
            }
        });
        if d.mediators.is_empty() {
            ui.colored_label(p.danger, tr(lang, Key::NoMediatorConfigured));
        }
        if let Some(e) = &d.error {
            ui.colored_label(p.danger, e);
        }
        if !started {
            ui.weak(tr(lang, Key::NotStarted));
        }
        ui.add_space(8.);
        if d.busy {
            ui.horizontal(|ui| {
                ui.add(Spinner::new());
                ui.label(tr(lang, Key::CreatingChat));
            });
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let enabled = started && !d.busy && d.mediator.is_some();
            go = ui.add_enabled(enabled, primary_button(&p, tr(lang, Key::CreateGroup))).clicked();
            cancel = ui.add_enabled(!d.busy, egui::Button::new(tr(lang, Key::Cancel))).clicked();
        });
    });
    let busy = app.state.new_group.as_ref().is_some_and(|d| d.busy);
    if go {
        submit(app);
    } else if cancel || (r.should_close() && !busy) {
        app.state.new_group = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{core::{events::Event, worker::WorkResult}, model::chat_key::ChatKey, settings::Settings, test_support::{start_app, test_harness}};
    use egui_kittest::{kittest::{NodeT as _, Queryable as _}, Harness};
    use eframe::egui::Role;

    type H = Harness<'static, MimirDesktop>;
    const A: [u8; 32] = [0xA1; 32];
    const B: [u8; 32] = [0xB2; 32];

    fn open_with(mediators: &[([u8; 32], &str)], started: bool) -> H {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        if started {
            let ctx = h.ctx.clone();
            start_app(h.state_mut(), &ctx);
            h.state_mut().state.core_started = true;
        }
        for (k, n) in mediators {
            h.state().core.add_mediator(k.to_vec(), n.to_string()).unwrap();
        }
        open(h.state_mut());
        h.run();
        h
    }

    fn dialog(h: &mut H) -> &mut NewGroupDialog {
        h.state_mut().state.new_group.as_mut().unwrap()
    }

    #[test]
    fn validates_like_android() {
        let mut h = open_with(&[(A, "Семья")], true);
        h.get_by_role_and_label(Role::Button, "Создать группу").click();
        h.run();
        assert!(h.query_by_label("Название группы обязательно!").is_some());
        assert!(!dialog(&mut h).busy);
        h.get_by_label("Имя группы:").click();
        h.get_by_label("Имя группы:").type_text(&"ы".repeat(26));
        h.run();
        assert!(h.query_by_label("Название группы обязательно!").is_none(), "cleared once the name is edited");
        assert_eq!(dialog(&mut h).name.chars().count(), 25);
        assert!(h.query_by_label("25/25").is_some());
        h.get_by_label("Описание группы:").click();
        h.get_by_label("Описание группы:").type_text("abc");
        h.run();
        assert!(h.query_by_label("3/200").is_some());
        assert!(h.query_by_label("0/200").is_none());
    }

    #[test]
    fn mediator_picker_only_with_two() {
        let h = open_with(&[(A, "Семья")], true);
        assert!(h.query_by_label("Создать на медиаторе:").is_none());
        let mut h = open_with(&[(A, "Семья"), (B, "")], true);
        assert!(h.query_by_label("Создать на медиаторе:").is_some());
        assert_eq!(dialog(&mut h).mediator.as_deref(), Some(&A[..]), "default preselected");
        h.get_by_role_and_label(Role::ComboBox, "Создать на медиаторе:").click();
        h.run();
        h.get_by_label("B2B2B2B2").click();
        h.run();
        assert_eq!(dialog(&mut h).mediator.as_deref(), Some(&B[..]));
    }

    #[test]
    fn no_mediator_message() {
        let mut h = open_with(&[], true);
        assert!(h.query_by_label_contains("Медиатор не настроен. Добавьте его через главное меню: Медиаторы.").is_some());
        h.get_by_role_and_label(Role::Button, "Создать группу").click();
        h.run();
        assert!(h.query_by_label("Название группы обязательно!").is_none(), "button is disabled, nothing submitted");
        assert!(!dialog(&mut h).busy);
    }

    fn press_create(h: &mut H) {
        h.get_by_label("Имя группы:").click();
        h.get_by_label("Имя группы:").type_text("Дача");
        h.run();
        h.get_by_role_and_label(Role::Button, "Создать группу").click();
        h.step();
    }

    #[test]
    fn create_shows_progress_then_opens_chat() {
        let mut h = open_with(&[(A, "Семья")], true);
        let (tx, rx) = std::sync::mpsc::channel();
        h.state_mut().rx = rx; // the real job (no mediator is reachable) reports to the old channel, which nobody reads
        press_create(&mut h);
        h.step();
        assert!(dialog(&mut h).busy);
        assert!(h.query_by_label("Майним подтверждение для чата…").is_some());
        h.state().core.save_group_chat(7, "Дача".into(), String::new(), None, A.to_vec(), vec![], vec![3; 32], 0).unwrap();
        tx.send(Event::WorkDone(WorkResult::GroupCreated { chat_id: 7 })).unwrap();
        h.step();
        h.step();
        assert!(h.state().state.new_group.is_none());
        assert_eq!(h.state().state.selected, Some(ChatKey::Group(7)));
        assert!(h.state().state.chats.iter().any(|c| c.group.is_some()));
    }

    #[test]
    fn create_failure_reenables_dialog() {
        let mut h = open_with(&[(A, "Семья")], true);
        let (tx, rx) = std::sync::mpsc::channel();
        h.state_mut().rx = rx;
        press_create(&mut h);
        h.step();
        tx.send(Event::WorkDone(WorkResult::Failed { what: "create_group", err: "no route".into() })).unwrap();
        h.step();
        h.run();
        let d = dialog(&mut h);
        assert!(!d.busy);
        assert_eq!(d.name, "Дача");
        assert!(h.query_by_label_contains("Не получилось создать чат :( no route").is_some());
        assert!(h.query_by_label("Майним подтверждение для чата…").is_none());
        assert!(!h.get_by_role_and_label(Role::Button, "Создать группу").accesskit_node().is_disabled());
    }

    #[test]
    fn public_toggle_hint() {
        let mut h = open_with(&[(A, "Семья")], true);
        let hint = "Войти сможет любой, у кого есть ссылка. Публичная группа не приватна: ключ передаётся в ссылке, поэтому медиатор может читать сообщения.";
        assert!(h.query_by_label(hint).is_none());
        h.get_by_label("Публичная группа").click();
        h.run();
        assert!(dialog(&mut h).public);
        assert!(h.query_by_label(hint).is_some());
    }

    #[test]
    fn toolbar_new_chat_opens_it() {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        h.get_by_label("Новый чат").click();
        h.run();
        assert!(h.state().state.new_group.is_some());
        assert!(!h.state().state.show_add_contact);
        assert!(h.query_by_label("Создать новую группу").is_some());
    }
}
