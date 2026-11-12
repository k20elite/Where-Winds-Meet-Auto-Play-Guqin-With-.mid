//! Where Winds Meet guqin auto-player.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> eframe::Result<()> {
    wwm_guqin::run(wwm_guqin::Edition::Guqin)
}
