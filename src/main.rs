#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // no console window in release

slint::include_modules!();

mod app;
mod logo;
mod optimizer;
mod taskbar;
mod winicon;

fn main() {
    if let Err(e) = app::run() {
        rfd::MessageDialog::new()
            .set_title("Gravingine Image Optimizer")
            .set_description(format!("Could not start: {e}"))
            .set_level(rfd::MessageLevel::Error)
            .show();
    }
}
