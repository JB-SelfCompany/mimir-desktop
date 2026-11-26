use eframe::egui::ViewportBuilder;
use mimir_desktop::{app::Startup, logs, paths::DataPaths, platform};

fn main() -> eframe::Result {
    let root = DataPaths::default_for_user().root;
    logs::init(&root);
    let link = platform::link_arg(std::env::args());
    // A relaunch (backup import) waits for the instance that is quitting instead of forwarding to it.
    let wait = if std::env::var_os(platform::RELAUNCHED).is_some() { platform::RELAUNCH_WAIT } else { std::time::Duration::ZERO };
    let instance = match platform::acquire(&root, link.as_deref(), wait) {
        platform::Acquire::Primary(i) => i,
        platform::Acquire::Forwarded => return Ok(()),
    };
    let app_root = root.clone();
    std::thread::spawn(move || {
        platform::register_scheme();
        platform::register_app_id(&app_root);
        platform::update::remove_old();
    });
    eframe::run_native(
        "Mimir",
        eframe::NativeOptions {
            viewport: ViewportBuilder::default()
                .with_title("Mimir")
                .with_inner_size([1100., 720.])
                .with_min_inner_size([720., 480.])
                .with_drag_and_drop(true),
            ..Default::default()
        },
        Box::new(move |cc| Ok(Box::new(Startup::new(cc, instance, link)))),
    )
}
