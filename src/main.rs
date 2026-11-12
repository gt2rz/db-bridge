// En Windows, sin esto el .exe de release abre también una ventana de consola.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod db;
mod diff;
mod favorites;
mod mobile;
mod model;
mod sql_highlight;
mod tasks;
mod theme;
mod update_check;
mod value_view;

use app::DbBridgeApp;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 780.0])
            .with_min_inner_size([900.0, 560.0])
            .with_title("DB Bridge · inspector SQLite offline-first"),
        ..Default::default()
    };

    eframe::run_native(
        "db-bridge",
        options,
        Box::new(|cc| Ok(Box::new(DbBridgeApp::new(cc)))),
    )
}
