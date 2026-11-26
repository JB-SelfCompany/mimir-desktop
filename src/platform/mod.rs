//! OS integration: one instance per data dir, `mimir://` links from the OS, raising the window, the tray icon,
//! notifications.
mod instance;
pub mod notify;
pub mod tray;
pub mod update;

pub use instance::{acquire, link_arg, Acquire, Instance, RELAUNCHED, RELAUNCH_WAIT};
pub use tray::{file_dialog, show_window};

use crate::{
    app::MimirDesktop,
    core::events::Event,
    i18n::{tr, Key},
    model::{group_link::parse_group_link, links::parse_contact},
    ui::{add_contact, call, join_group},
};
use eframe::egui;
use std::sync::mpsc::{Receiver, Sender};

/// What another launch asked of us.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ext {
    Show,
    Link(String),
}

/// Moves `ext` into the app's event channel once the app exists (links that came during the restore screen wait).
pub fn relay(ext: Receiver<Ext>, events: Sender<Event>, ctx: egui::Context) {
    std::thread::spawn(move || {
        for x in ext {
            if events.send(Event::External(x)).is_err() {
                break;
            }
            ctx.request_repaint();
        }
    });
}

/// Raises the window and opens the dialog the link is for (Android UriHandlerActivity).
pub fn handle(app: &mut MimirDesktop, ctx: &egui::Context, ext: &Ext) {
    show_window(ctx);
    app.state.hidden = false;
    let Ext::Link(link) = ext else { return };
    if link.trim().starts_with("mimir://gc/") {
        call::ask_join(app, link);
    } else if let Some(g) = parse_group_link(link) {
        join_group::open(app, g);
    } else if let Some((key, name)) = parse_contact(link) {
        add_contact::open_prefilled(app, &key, name);
    } else {
        app.error_text(tr(app.lang, Key::LinkNotRecognized).to_string());
    }
}

/// Makes `mimir://` links open this executable, for the current user only; failures are only logged.
#[cfg(windows)]
pub fn register_scheme() {
    let Ok(exe) = std::env::current_exe() else { return };
    let exe = exe.display();
    let key = r"HKCU\Software\Classes\mimir";
    let values = [
        (key.to_string(), None, "URL:Mimir link".to_string()),
        (key.to_string(), Some("URL Protocol"), String::new()),
        (format!(r"{key}\DefaultIcon"), None, format!("\"{exe}\",0")),
        (format!(r"{key}\shell\open\command"), None, format!("\"{exe}\" \"%1\"")),
    ];
    for (k, name, data) in values {
        if !reg_add(&k, name, &data) {
            return;
        }
    }
}

/// `reg add <key> [/v name | /ve] /d data /f`, windowless; a failure is logged.
#[cfg(windows)]
fn reg_add(key: &str, name: Option<&str>, data: &str) -> bool {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut reg = std::process::Command::new("reg");
    reg.args(["add", key, "/f", "/d", data]);
    match name {
        Some(n) => reg.args(["/v", n]),
        None => reg.arg("/ve"),
    };
    let status = reg.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).creation_flags(CREATE_NO_WINDOW).status();
    let ok = status.as_ref().is_ok_and(|s| s.success());
    if !ok {
        tracing::warn!("registry write failed at {key}: {status:?}");
    }
    ok
}

/// Names our toasts' AppUserModelID «Mimir», with the app icon (written to `<root>/mimir.png`), for the current user.
/// ponytail: an id registered only under HKCU\Software\Classes\AppUserModelId (no Start-menu shortcut); Windows 10+
/// shows such toasts, but they get no Action Center settings entry of their own. Best-effort: failures are only logged.
#[cfg(windows)]
pub fn register_app_id(root: &std::path::Path) {
    let png = root.join("mimir.png");
    if let Err(e) = std::fs::write(&png, tray::ICON) {
        tracing::warn!("app icon not written: {e}");
        return;
    }
    let key = format!(r"HKCU\Software\Classes\AppUserModelId\{}", notify::APP_ID);
    let _ = reg_add(&key, Some("DisplayName"), "Mimir") && reg_add(&key, Some("IconUri"), &png.display().to_string());
}

#[cfg(not(windows))]
pub fn register_app_id(_root: &std::path::Path) {}

/// Makes `mimir://` links open this executable, for the current user only; failures are only logged.
/// ponytail: `exe` goes into Exec= quoted but unescaped; a path with `"`, `$`, `\` or `%` breaks the entry.
#[cfg(target_os = "linux")]
pub fn register_scheme() {
    if !update::registers_scheme(update::this_install()) {
        return;
    }
    let (Ok(exe), Some(dirs)) = (std::env::current_exe(), directories::BaseDirs::new()) else { return };
    let apps = dirs.data_dir().join("applications");
    let entry = format!(
        "[Desktop Entry]\nType=Application\nName=Mimir\nExec=\"{}\" %u\nTerminal=false\nCategories=Network;InstantMessaging;\nMimeType=x-scheme-handler/mimir;\n",
        exe.display()
    );
    if let Err(e) = std::fs::create_dir_all(&apps).and_then(|_| std::fs::write(apps.join("mimir-desktop.desktop"), entry)) {
        tracing::warn!("mimir:// registration failed: {e}");
        return;
    }
    let status = std::process::Command::new("xdg-mime").args(["default", "mimir-desktop.desktop", "x-scheme-handler/mimir"]).status();
    if !status.as_ref().is_ok_and(|s| s.success()) {
        tracing::warn!("xdg-mime failed: {status:?}");
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn register_scheme() {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{settings::Settings, test_support::test_harness};
    use egui_kittest::{kittest::Queryable as _, Harness};

    const HEX: &str = "f9a4e8d77c34ce192aebc553d9815709c28d35c0a773160508de91a0dac43c22";
    const MED: &str = "000096390fde19f30d3ba042d77f5f5ce0e6c23bd62ff0afc2e6c29a6fe96b33";
    const KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8";

    /// The app, with `tx` feeding its event channel (core jobs report to a channel nobody reads).
    fn app() -> (Harness<'static, MimirDesktop>, Sender<Event>) {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        let (tx, rx) = std::sync::mpsc::channel();
        h.state_mut().rx = rx;
        h.run();
        (h, tx)
    }

    #[test]
    fn contact_link_opens_prefilled_dialog() {
        let (mut h, tx) = app();
        tx.send(Event::External(Ext::Link(format!("mimir://mm/u/{HEX}/Alice%20B")))).unwrap();
        h.run();
        let s = &h.state().state;
        assert!(s.show_add_contact);
        assert_eq!((s.add_dialog.name.as_str(), s.add_dialog.key.as_str()), ("Alice B", HEX));
        assert!(h.query_by_label("Публичный ключ или ссылка:").is_some(), "the dialog is on screen");
    }

    #[test]
    fn group_link_opens_join() {
        let (mut h, tx) = app();
        tx.send(Event::External(Ext::Link(format!("https://go.mimir.im/g/{MED}/123456#{KEY}")))).unwrap();
        // The lookup spinner keeps repainting: a few frames, not `run`.
        for _ in 0..3 {
            h.step();
        }
        assert_eq!(h.state().state.join.as_ref().map(|j| j.link.chat_id), Some(123456));
        assert!(h.query_by_label("Приглашение в группу").is_some());
        assert!(!h.state().state.show_add_contact);
    }

    #[test]
    fn unknown_link_and_show_open_nothing() {
        let (mut h, tx) = app();
        tx.send(Event::External(Ext::Show)).unwrap();
        tx.send(Event::External(Ext::Link("mimir://zz/what".into()))).unwrap();
        h.run();
        let s = &h.state().state;
        assert!(!s.show_add_contact && s.join.is_none());
        assert_eq!(s.status.as_ref().map(|s| s.0.as_str()), Some("Ссылка не распознана"));
    }
}
