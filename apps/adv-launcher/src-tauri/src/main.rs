// Keep a console window from opening alongside the launcher in Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    adv_launcher::run();
}
