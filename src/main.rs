// Release builds are tray apps: no console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> eframe::Result {
    gifly::run()
}
