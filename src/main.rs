#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // no console window in release

mod app;
mod draw;
mod geom;
mod droptarget;
mod icons;
mod optimizer;
mod skin;
mod taskbar;
mod theme;
mod tiles;
mod ui;

fn main() {
    if let Err(e) = app::run() {
        native_windows_gui::fatal_message("Gravingine Image Optimizer", &format!("Could not start: {e}"));
    }
}
