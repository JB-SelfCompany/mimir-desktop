//! Contact requests modal (Android ContactRequestsActivity): accept or decline each request.
use crate::{
    app::MimirDesktop,
    core::worker::WorkResult,
    i18n::{tr, Key},
};
use crate::{media::avatar::resolve, ui::{avatar::{self, avatar_color}, theme::{dialog, dialog_title, palette, primary_button}}};
use eframe::egui::{self, Align, Button, Id, Layout, ScrollArea};
use mimir::MimirError;

/// Android onAccept: accept in storage, then send the response (the core does not send it itself).
fn answer(app: &mut MimirDesktop, id: i64, pubkey: Vec<u8>, accepted: bool) {
    // Hide it right away so it cannot be answered twice; WorkDone reloads the real list.
    app.state.requests.retain(|r| r.id != id);
    app.state.requests_count = app.state.requests.len();
    app.worker.submit(Box::new(move |core| {
        // An offline requester is not an error (like an offline send, no status line).
        // ponytail: the core does not queue the response; the requester learns of it when we next connect or write.
        let respond = |accepted| match core.send_contact_response(pubkey, accepted) {
            Err(MimirError::Connection(_)) => Ok(()),
            r => r,
        };
        let done = if accepted {
            core.accept_contact_request(id).and_then(|contact_id| respond(true).map(|_| contact_id))
        } else {
            core.delete_contact_request(id).and_then(|_| respond(false)).map(|_| 0)
        };
        match (done, accepted) {
            (Ok(contact_id), true) => WorkResult::RequestAccepted { contact_id },
            (Ok(_), false) => WorkResult::RequestDeclined,
            (Err(e), _) => WorkResult::Failed { what: "contact request", err: e.to_string() },
        }
    }));
}

pub fn show(app: &mut MimirDesktop, ctx: &egui::Context) {
    if !app.state.show_requests {
        return;
    }
    let (lang, started) = (app.lang, app.state.core_started);
    let mut chosen = None;
    let mut close = false;
    let r = dialog(ctx, Id::unique("contact_requests")).show(ctx, |ui| {
        ui.set_width(380.);
        let p = palette(ui.visuals().dark_mode);
        dialog_title(ui, tr(lang, Key::RequestsTitle));
        if app.state.requests.is_empty() {
            ui.label(tr(lang, Key::NoRequests));
        }
        if !started {
            ui.weak(tr(lang, Key::NotStarted));
        }
        ScrollArea::vertical().max_height(360.).show(ui, |ui| {
            for q in &app.state.requests {
                ui.separator();
                let name = crate::model::chat_list::display_name(&q.nickname, &q.pubkey);
                ui.horizontal(|ui| {
                    // 48 px in contact requests.
                    avatar::show(ui, &mut app.state.thumbs, resolve(&app.paths.core_dir, &q.avatar).as_deref(), avatar_color(&q.pubkey), 48., 3.);
                    ui.vertical(|ui| {
                        ui.strong(name);
                        if !q.message.is_empty() {
                            ui.label(&q.message);
                        }
                    });
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add_enabled(started, primary_button(&p, tr(lang, Key::Accept))).clicked() {
                        chosen = Some((q.id, q.pubkey.clone(), true));
                    }
                    if ui.add_enabled(started, Button::new(tr(lang, Key::Decline))).clicked() {
                        chosen = Some((q.id, q.pubkey.clone(), false));
                    }
                });
            }
        });
        ui.add_space(8.);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            close = ui.button(tr(lang, Key::Close)).clicked();
        });
    });
    if let Some((id, pubkey, accepted)) = chosen {
        answer(app, id, pubkey, accepted);
    } else if close || r.should_close() {
        app.state.show_requests = false;
    }
}

#[cfg(test)]
mod tests {
    use crate::{app::MimirDesktop, settings::Settings, test_support::{start_app, test_harness}};
    use egui_kittest::{kittest::Queryable as _, Harness};
    use std::time::{Duration, Instant};

    // The started core's network events and worker results repaint at any time, so run() can exceed max_steps
    // and panic (the flake): these tests settle with run_ok() and wait for the state they need.

    const KEY: [u8; 32] = [9; 32];

    fn open_requests(started: bool) -> Harness<'static, MimirDesktop> {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        if started {
            let ctx = h.ctx.clone();
            start_app(h.state_mut(), &ctx);
            h.state_mut().state.core_started = true;
        }
        h.state().core.add_contact_request(KEY.to_vec(), "привет, я Боб".into(), "Боб".into(), String::new(), None).unwrap();
        h.state_mut().refresh_requests();
        h.run_ok();
        h.get_by_label("Запросы в контакты: 1").click();
        h.run_ok();
        h
    }

    fn wait_for_count(h: &mut Harness<'static, MimirDesktop>, n: usize) {
        let end = Instant::now() + Duration::from_secs(5);
        while h.state().state.requests_count != n && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
            h.run_ok();
        }
        assert_eq!(h.state().state.requests_count, n);
    }

    #[test] fn banner_opens_modal_and_accept_adds_contact() {
        let mut h = open_requests(true);
        assert!(h.state().state.show_requests);
        assert!(h.query_by_label("привет, я Боб").is_some());
        h.get_by_label("Принять").click();
        h.run_ok();
        wait_for_count(&mut h, 0);
        assert!(h.state().core.get_contact_by_pubkey(KEY.to_vec()).is_some());
        // The chat list rebuild lands after the request count drops.
        crate::test_support::run_until(&mut h, "chat for the new contact", |h| h.state().state.chats.iter().any(|c| c.pubkey == KEY));
    }

    #[test] fn accepting_offline_requester_shows_no_status_line() {
        let mut h = open_requests(true);
        h.get_by_label("Принять").click();
        h.run_ok();
        // The job ends with a chat list rebuild (success) or a status line (failure).
        let end = Instant::now() + Duration::from_secs(5);
        while !h.state().state.chats.iter().any(|c| c.pubkey == KEY) && h.state().state.status.is_none() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
            h.run_ok();
        }
        assert_eq!(h.state().state.status, None);
        assert!(h.state().state.chats.iter().any(|c| c.pubkey == KEY));
    }

    #[test] fn failed_job_restores_request_list() {
        use crate::core::worker::WorkResult;
        let mut h = open_requests(true);
        // What `answer` does optimistically, then a job that fails leaves the request in storage.
        h.state_mut().state.requests.clear();
        h.state_mut().state.requests_count = 0;
        h.state().worker.submit(Box::new(|_| WorkResult::Failed { what: "contact request", err: "boom".into() }));
        wait_for_count(&mut h, 1);
        assert_eq!(h.state().state.requests.len(), 1);
    }

    #[test] fn decline_removes_without_adding() {
        let mut h = open_requests(true);
        h.get_by_label("Отклонить").click();
        h.run_ok();
        wait_for_count(&mut h, 0);
        assert!(h.state().core.get_contact_by_pubkey(KEY.to_vec()).is_none());
    }

    #[test] fn buttons_disabled_until_core_started() {
        let mut h = open_requests(false);
        h.get_by_label("Принять").click();
        h.get_by_label("Отклонить").click();
        h.run_ok();
        assert_eq!(h.state().state.requests_count, 1);
        assert!(h.query_by_label("Сеть ещё не запущена, подождите").is_some());
    }
}
