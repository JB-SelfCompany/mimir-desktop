use eframe::egui::ViewportBuilder;
use mimir_desktop::{app::Startup, logs, paths::DataPaths};

fn main() -> eframe::Result {
    logs::init(&DataPaths::default_for_user().root);
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
        Box::new(|cc| Ok(Box::new(Startup::new(cc)))),
    )
}
