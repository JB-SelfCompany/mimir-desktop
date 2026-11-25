//! The tray icon (menu: open, exit; left click opens) and the window it brings back: closing the window only hides it
//! while the icon is up.
use crate::{
    core::events::Event,
    i18n::{tr, Key, Lang},
    platform::Ext,
};
use eframe::egui::{self, ViewportCommand};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::Sender,
    Arc,
};

pub(crate) const ICON: &[u8] = include_bytes!("../../assets/icons/app.png");

/// Starts the icon on its own thread (it needs an event loop there: a Win32 message pump, or GTK on Linux). The flag
/// turns true once the icon is up; it stays false when there is no tray (no AppIndicator host), and closing then quits.
pub fn spawn(tx: Sender<Event>, ctx: egui::Context, lang: Lang) -> Arc<AtomicBool> {
    let active = Arc::new(AtomicBool::new(false));
    let up = active.clone();
    std::thread::spawn(move || {
        #[cfg(target_os = "linux")]
        if let Err(e) = gtk::init() {
            tracing::warn!("no tray (gtk: {e}); closing the window quits");
            return;
        }
        // Dropping it removes the icon: it lives as long as the loop below.
        let _icon = match build(tx, ctx, lang) {
            Ok(icon) => icon,
            Err(e) => {
                tracing::warn!("no tray ({e}); closing the window quits");
                return;
            }
        };
        up.store(true, Ordering::Relaxed);
        run_loop();
    });
    active
}

fn build(tx: Sender<Event>, ctx: egui::Context, lang: Lang) -> Result<tray_icon::TrayIcon, Box<dyn std::error::Error>> {
    use tray_icon::{
        menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
        MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent,
    };
    let img = image::load_from_memory(ICON)?.into_rgba8();
    let (w, h) = img.dimensions();
    let icon = tray_icon::Icon::from_rgba(img.into_raw(), w, h)?;
    let open = MenuItem::new(tr(lang, Key::TrayOpen), true, None);
    let exit = MenuItem::new(tr(lang, Key::ExitMimir), true, None);
    let menu = Menu::new();
    menu.append_items(&[&open, &PredefinedMenuItem::separator(), &exit])?;
    let (open_id, exit_id) = (open.id().clone(), exit.id().clone());
    // The handlers run on this thread, inside its loop.
    let (tx2, ctx2) = (tx.clone(), ctx.clone());
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        if e.id == open_id {
            show_from_tray(&tx2, &ctx2);
        } else if e.id == exit_id {
            let _ = tx2.send(Event::Quit);
            ctx2.request_repaint();
        }
    }));
    TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
        if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
            show_from_tray(&tx, &ctx);
        }
    }));
    let icon = TrayIconBuilder::new().with_tooltip("Mimir").with_icon(icon).with_menu(Box::new(menu)).with_menu_on_left_click(false).build()?;
    Ok(icon)
}

/// The window comes up even if eframe runs no frame while it is hidden; the app then raises it (and repaints) too.
/// Also what a click on a notification does.
pub(crate) fn show_from_tray(tx: &Sender<Event>, ctx: &egui::Context) {
    raise_native();
    let _ = tx.send(Event::External(Ext::Show));
    ctx.request_repaint();
}

pub fn show_window(ctx: &egui::Context) {
    ctx.send_viewport_cmd(ViewportCommand::Visible(true));
    ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
    ctx.send_viewport_cmd(ViewportCommand::Focus);
    raise_native();
}

/// The window close (title bar, Alt+F4) hides it while the tray is up; quitting (tray Exit, settings Exit) lets it go.
/// winit cannot hide a Wayland window, so there it is minimized instead. Returns whether it hid the window.
pub fn on_close_request(ctx: &egui::Context, tray_up: bool, quitting: bool) -> bool {
    let hide = tray_up && !quitting && ctx.input(|i| i.viewport().close_requested());
    if hide {
        ctx.send_viewport_cmd(ViewportCommand::CancelClose);
        let wayland = WAYLAND.load(Ordering::Relaxed);
        ctx.send_viewport_cmd(if wayland { ViewportCommand::Minimized(true) } else { ViewportCommand::Visible(false) });
    }
    hide
}

/// The window is a Wayland surface (set by `remember_window`).
static WAYLAND: AtomicBool = AtomicBool::new(false);

/// The main window's HWND (0 until `remember_window`).
#[cfg(windows)]
static HWND: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

/// Keeps what the window handle tells: the HWND for `raise_native`, or that the window is on Wayland.
pub fn remember_window(cc: &eframe::CreationContext<'_>) {
    use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
    match cc.window_handle().map(|h| h.as_raw()) {
        #[cfg(windows)]
        Ok(RawWindowHandle::Win32(w)) => HWND.store(w.hwnd.get(), Ordering::Relaxed),
        Ok(RawWindowHandle::Wayland(_)) => WAYLAND.store(true, Ordering::Relaxed),
        _ => {}
    }
}

#[cfg(windows)]
fn raise_native() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SetForegroundWindow, ShowWindow, SW_SHOW};
    let hwnd = HWND.load(Ordering::Relaxed);
    if hwnd != 0 {
        // SAFETY: plain Win32 calls on a window handle; a stale one only makes them fail.
        unsafe {
            ShowWindow(hwnd as _, SW_SHOW);
            SetForegroundWindow(hwnd as _);
        }
    }
}

#[cfg(not(windows))]
fn raise_native() {}

#[cfg(windows)]
fn run_loop() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{DispatchMessageW, GetMessageW, TranslateMessage, MSG};
    // SAFETY: the standard message pump of this thread.
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

#[cfg(target_os = "linux")]
fn run_loop() {
    gtk::main();
}

// ponytail: tray-icon on macOS needs the main thread; the icon is skipped and closing quits.
#[cfg(not(any(windows, target_os = "linux")))]
fn run_loop() {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{app::MimirDesktop, settings::Settings, test_support::test_harness, ui::settings::{open, Section}};
    use egui::{ViewportEvent, ViewportId};
    use egui_kittest::{kittest::Queryable as _, Harness};

    type H = Harness<'static, MimirDesktop>;

    /// The app with the tray up; the sender feeds its event channel.
    fn app() -> (H, Sender<Event>) {
        let mut h = test_harness(Settings { intro_done: true, ..Default::default() });
        h.set_size(egui::vec2(1100., 720.));
        let (tx, rx) = std::sync::mpsc::channel();
        h.state_mut().rx = rx;
        h.state_mut().state.tray.store(true, Ordering::Relaxed);
        h.run();
        (h, tx)
    }

    fn commands(h: &H) -> &[ViewportCommand] {
        &h.output().viewport_output[&ViewportId::ROOT].commands
    }

    /// One frame with the OS asking the window to close.
    fn close_request(h: &mut H) {
        h.input_mut().viewports.entry(ViewportId::ROOT).or_default().events.push(ViewportEvent::Close);
        h.step();
    }

    fn click(h: &mut H, label: &str) {
        h.get_by_label(label).scroll_to_me();
        h.run();
        h.get_by_label(label).click();
        h.run();
    }

    #[test]
    fn close_hides_exit_quits() {
        let (mut h, _tx) = app();
        close_request(&mut h);
        assert!(commands(&h).contains(&ViewportCommand::CancelClose) && commands(&h).contains(&ViewportCommand::Visible(false)), "{:?}", commands(&h));
        h.state_mut().state.tray.store(false, Ordering::Relaxed);
        close_request(&mut h);
        assert!(!commands(&h).contains(&ViewportCommand::CancelClose), "without a tray closing quits");
        h.state_mut().state.tray.store(true, Ordering::Relaxed);

        open(h.state_mut());
        h.state_mut().state.settings_ui.section = Section::Advanced;
        h.run();
        click(&mut h, "Закрыть Мимир");
        h.get_by_label("OK").click();
        h.step();
        assert!(commands(&h).contains(&ViewportCommand::Close));
        close_request(&mut h);
        assert!(!commands(&h).contains(&ViewportCommand::CancelClose), "settings Exit really quits");
    }

    #[test]
    fn hidden_window_notifies_for_the_open_chat() {
        use crate::{model::{chat_key::ChatKey, chat_list::build_chat_list}, platform::notify::decide};
        let (mut h, tx) = app();
        let core = h.state().core.clone();
        let c = core.add_contact(vec![7; 32], "Аня".into()).unwrap();
        h.state_mut().state.chats = build_chat_list(&core, Lang::Ru);
        h.state_mut().state.selected = Some(ChatKey::Contact(c));
        h.run();
        let hi = core.add_local_message(c, 1, 0, true, false, 1000, 0, 0, "привет".as_bytes().to_vec()).unwrap();
        let ev = Event::MessageReceived { contact_id: c, message_id: hi, msg_type: 0, reply_to: 0 };
        let focus = |h: &H| *h.state().focus.lock().unwrap();
        assert_eq!(decide(&core, &ev, &focus(&h)), None, "open chat, focused window");
        close_request(&mut h);
        // Another pass while hidden: egui's own `focused` still says true (the last shown frame's).
        h.step();
        assert!(!focus(&h).focused);
        assert_eq!(decide(&core, &ev, &focus(&h)), Some(("Аня".into(), "привет".into())), "closed to the tray: notifies");
        tx.send(Event::External(Ext::Show)).unwrap();
        h.step();
        assert!(focus(&h).focused, "shown again");
    }

    #[test]
    fn tray_quit_event_quits() {
        let (mut h, tx) = app();
        tx.send(Event::Quit).unwrap();
        h.step();
        assert!(h.state().state.quitting);
        assert!(commands(&h).contains(&ViewportCommand::Close));
        close_request(&mut h);
        assert!(!commands(&h).contains(&ViewportCommand::CancelClose));
    }
}
