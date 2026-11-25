//! Profile modal (Android AccountActivity): name, description, public key, copy buttons, QR dialog.
use crate::{
    app::MimirDesktop,
    i18n::{tr, Key},
    model::links::contact_link,
    core::worker::WorkResult,
    media::avatar::{resolve, AvatarError, ACCOUNT_MAX},
    ui::{avatar::pick_button, qr, theme::{dialog, dialog_title, field_label, palette, primary_button}},
};
use eframe::egui::{self, vec2, Align, Button, Color32, FontId, Id, Layout, RichText, TextEdit};

/// The single local account (Android uses id 1 too).
pub const ACCOUNT: i32 = 1;

/// Fields edited in the modal; saved on close only if changed.
#[derive(Default)]
pub struct Profile {
    loaded: bool,
    name: String,
    info: String,
    orig_name: String,
    orig_info: String,
    qr: Option<egui::TextureHandle>,
    /// Stored account avatar path (relative to the core's data dir; empty = none).
    avatar: String,
    /// A new avatar being picked; saved by the worker as soon as it is prepared.
    pub pick: crate::ui::avatar::AvatarPick,
    /// Why the picked picture could not be used.
    avatar_error: Option<String>,
}

/// `update_account_avatar` stored our avatar: the profile shows the stored file from now on.
pub fn avatar_saved(app: &mut MimirDesktop, path: &str) {
    app.state.own_avatar = Some(path.to_string()); // the main toolbar's
    // The core rewrites the same file; its mtime may not move within one tick.
    if let Some(p) = resolve(&app.paths.core_dir, path) {
        app.state.thumbs.invalidate(&p);
    }
    let p = &mut app.state.profile;
    if p.loaded {
        p.avatar = path.to_string();
        p.pick.preview = None;
    }
}

/// A picked picture: prepared (≤ 48 KiB, contacts drop more, CA §5), then stored by the worker (P3-R7: change only).
fn poll_avatar(app: &mut MimirDesktop, ctx: &egui::Context) {
    let p = &mut app.state.profile;
    if let Some(e) = p.pick.poll(ctx) {
        let key = if e == AvatarError::TooLarge { Key::AvatarTooLarge } else { Key::ErrorLoadingAvatarPicture };
        p.avatar_error = Some(tr(app.lang, key).to_string());
    }
    if let Some(bytes) = p.pick.bytes.take() {
        p.avatar_error = None;
        app.worker.submit(Box::new(move |core| match core.update_account_avatar(ACCOUNT, bytes) {
            Ok(path) => WorkResult::AccountAvatar { path },
            Err(e) => WorkResult::Failed { what: "update_account_avatar", err: e.to_string() },
        }));
    }
}

fn close(app: &mut MimirDesktop) {
    let p = std::mem::take(&mut app.state.profile);
    // ponytail: storage write errors are ignored; the field reverts to the stored value next open.
    if p.name != p.orig_name {
        let _ = app.core.update_account_name(ACCOUNT, p.name);
    }
    if p.info != p.orig_info {
        let _ = app.core.update_account_info(ACCOUNT, p.info);
    }
    app.state.show_profile = false;
}

pub fn show(app: &mut MimirDesktop, ctx: &egui::Context) {
    if !app.state.show_profile {
        return;
    }
    let lang = app.lang;
    let pubkey = app.core.public_key();
    if !app.state.profile.loaded {
        let a = app.core.get_account(ACCOUNT);
        let p = &mut app.state.profile;
        p.loaded = true;
        p.name = a.as_ref().map(|a| a.name.clone()).unwrap_or_default();
        p.avatar = a.as_ref().map(|a| a.avatar.clone()).unwrap_or_default();
        p.info = a.map(|a| a.info).unwrap_or_default();
        (p.orig_name, p.orig_info) = (p.name.clone(), p.info.clone());
    }
    poll_avatar(app, ctx);
    let link = contact_link(&pubkey, &app.state.profile.name);
    // ponytail: the QR dialog replaces the profile modal instead of stacking on it.
    if let Some(tex) = app.state.profile.qr.clone() {
        let mut ok = false;
        let r = dialog(ctx, Id::unique("profile_qr")).show(ctx, |ui| {
            ui.set_width(260.); // the code's width; OK sits under its right edge
            let pal = palette(ui.visuals().dark_mode);
            dialog_title(ui, tr(lang, Key::MyQr));
            ui.image((tex.id(), vec2(260., 260.)));
            ui.add_space(8.);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ok = ui.add(primary_button(&pal, tr(lang, Key::Ok))).clicked();
            });
        });
        if ok || r.should_close() {
            app.state.profile.qr = None;
        }
        return;
    }
    let (mut copy_id, mut copy_link, mut show_qr, mut done) = (false, false, false, false);
    let r = dialog(ctx, Id::unique("profile")).show(ctx, |ui| {
        ui.set_width(380.);
        let pal = palette(ui.visuals().dark_mode);
        dialog_title(ui, tr(lang, Key::Profile));
        let p = &mut app.state.profile;
        ui.vertical_centered(|ui| {
            // AM §9: 128 px, corner 8; the placeholder is Android's white-to-grey button_rounded_white.
            let path = resolve(&app.paths.core_dir, &p.avatar);
            if pick_button(ui, &mut app.state.thumbs, &p.pick, path.as_deref(), Color32::from_gray(0xC8), 128., tr(lang, Key::ChangeAvatar)) {
                p.pick.start(ui.ctx(), tr(lang, Key::Image), ACCOUNT_MAX);
            }
            if let Some(e) = &p.avatar_error {
                ui.colored_label(pal.danger, e);
            }
        });
        let l = field_label(ui, &pal, tr(lang, Key::MyName));
        ui.add(TextEdit::singleline(&mut p.name).hint_text(tr(lang, Key::NameHint)).desired_width(f32::INFINITY)).labelled_by(l.id);
        let l = field_label(ui, &pal, tr(lang, Key::Description));
        ui.add(TextEdit::singleline(&mut p.info).hint_text(tr(lang, Key::Description)).desired_width(f32::INFINITY)).labelled_by(l.id);
        field_label(ui, &pal, tr(lang, Key::MyPublicKey));
        let key = hex::encode_upper(&pubkey);
        let (a, b) = key.split_at(key.len() / 2);
        ui.label(RichText::new(format!("{a}\n{b}")).font(FontId::monospace(14.)));
        field_label(ui, &pal, tr(lang, Key::ShareContact));
        let w = ui.available_width();
        copy_id = ui.add_sized([w, 30.], Button::new(tr(lang, Key::CopyId))).clicked();
        copy_link = ui.add_sized([w, 30.], Button::new(tr(lang, Key::CopyLink))).clicked();
        show_qr = ui.add_sized([w, 30.], Button::new(tr(lang, Key::ShowQr))).clicked();
        ui.add_space(8.);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            done = ui.button(tr(lang, Key::Close)).clicked();
        });
    });
    if copy_id {
        ctx.copy_text(hex::encode(&pubkey));
    }
    if copy_link {
        ctx.copy_text(link.clone());
    }
    if show_qr {
        app.state.profile.qr = Some(qr::texture(ctx, &link));
    }
    if done || r.should_close() {
        close(app);
    }
}

#[cfg(test)]
mod tests {
    use crate::{settings::Settings, test_support::test_harness};
    use egui_kittest::kittest::{NodeT as _, Queryable as _};

    #[test]
    fn avatar_button_waits_for_the_open_picker() {
        // M7: a second click while a pick runs must not open another picker (it dropped the first answer).
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        h.state_mut().state.show_profile = true;
        h.run();
        let (tx, rx) = std::sync::mpsc::channel();
        h.state_mut().state.profile.pick.picking = Some(rx);
        crate::media::avatar::set_picked(std::path::PathBuf::from("second.png"));
        h.get_by_label("Изменить аватар").click();
        h.run();
        assert!(tx.send(Err(crate::media::avatar::AvatarError::Unreadable)).is_ok(), "the first pick is still awaited");
    }

    #[test]
    fn profile_change_avatar() {
        use crate::{media::avatar::{resolve, set_picked, ACCOUNT_MAX}, test_support::{run_until, textured}};
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("me.png");
        std::fs::write(&src, crate::test_support::png_bytes(900, 600, [40, 90, 200])).unwrap();
        h.state_mut().state.show_profile = true;
        h.run();
        // P3-R7: the core cannot clear an account avatar, so there is nothing to remove it with.
        assert!(h.query_all_by_role(eframe::egui::Role::Button).all(|n| !n.accesskit_node().label().unwrap_or_default().to_lowercase().contains("удал")));
        set_picked(src);
        h.get_by_label("Изменить аватар").click();
        let core = h.state().core.clone();
        run_until(&mut h, "update_account_avatar", |_| !core.get_account(1).unwrap().avatar.is_empty());
        let path = resolve(&h.state().paths.core_dir, &core.get_account(1).unwrap().avatar).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.len() <= ACCOUNT_MAX, "{}", bytes.len());
        assert_eq!(image::load_from_memory(&bytes).unwrap().width(), 256);
        // The stored picture (not the picker's preview) is what the profile shows, 128 px (AM §9).
        run_until(&mut h, "the stored avatar", |h| h.state().state.profile.pick.preview.is_none() && textured(h).contains(&eframe::egui::vec2(128., 128.)));
    }

    #[test]
    fn profile_name_persists() {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        h.state_mut().state.show_profile = true;
        h.run();
        h.get_by_label("Моё имя:").click();
        h.get_by_label("Моё имя:").type_text("Ёжик");
        h.run();
        h.get_by_label("Закрыть").click();
        h.run();
        assert!(!h.state().state.show_profile);
        assert_eq!(h.state().core.get_account(1).unwrap().name, "Ёжик");
    }

    #[test]
    fn profile_info_persists() {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        h.state_mut().state.show_profile = true;
        h.run();
        h.get_by_label("Описание:").click();
        h.get_by_label("Описание:").type_text("о себе");
        h.run();
        h.get_by_label("Закрыть").click();
        h.run();
        assert_eq!(h.state().core.get_account(1).unwrap().info, "о себе");
    }

    #[test]
    fn unchanged_fields_are_not_written() {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        h.state_mut().state.show_profile = true;
        h.run();
        let before = h.state().core.get_account_updated_at(1);
        assert_ne!(crate::test_support::button_fill(&h, "Закрыть"), Some(crate::ui::theme::palette(true).accent), "dismiss is plain");
        std::thread::sleep(std::time::Duration::from_millis(1100)); // updated_at may have second resolution
        h.get_by_label("Закрыть").click();
        h.run();
        assert!(!h.state().state.show_profile);
        assert_eq!(h.state().core.get_account_updated_at(1), before);
    }

    #[test]
    fn qr_dialog_is_as_wide_as_the_code() {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        h.set_size(eframe::egui::vec2(1200., 800.));
        h.state_mut().state.show_profile = true;
        h.run();
        h.get_by_label("Показать QR").click();
        h.run();
        let (title, ok) = (h.get_by_label("Мой QR-код").rect(), h.get_by_label("OK").rect());
        let w = ok.right() - title.left();
        assert!((250. ..=270.).contains(&w), "OK sits under the 260 px code: {w}");
    }

    #[test]
    fn escape_closes_modal() {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        h.state_mut().state.show_profile = true;
        h.run();
        h.key_press(eframe::egui::Key::Escape);
        h.run();
        assert!(!h.state().state.show_profile);
    }

    #[test]
    fn qr_dialog_opens_and_closes() {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        h.state_mut().state.show_profile = true;
        h.run();
        h.get_by_label("Показать QR").click();
        h.run();
        assert!(h.query_by_label("Мой QR-код").is_some());
        h.get_by_label("OK").click();
        h.run();
        assert!(h.query_by_label("Мой QR-код").is_none());
        assert!(h.query_by_label("Моё имя:").is_some());
    }
}
