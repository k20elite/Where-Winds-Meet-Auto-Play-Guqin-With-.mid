//! Roblox 61-key piano auto-player (Visual Pianos layout).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> eframe::Result<()> {
    wwm_guqin::run(wwm_guqin::Edition::Roblox)
}
