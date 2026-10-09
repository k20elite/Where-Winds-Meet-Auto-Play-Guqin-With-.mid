//! Entry point for Where Winds Meet Guqin auto-player desktop application.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod controller;
mod fonts;
mod hotkeys;
mod input;
mod layout;
mod library;
mod mapping;
mod midi;
mod neu;
mod player;
mod settings;

use app::App;
use controller::{Controller, Shared};
use eframe::egui::{self, ViewportBuilder};
use hotkeys::HotkeyThread;
use input::Output;
use player::PlayerEvent;
use settings::Settings;
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::thread;

fn main() -> eframe::Result<()> {
    let settings_path = Settings::default_path();
    let (settings, load_warning) = Settings::load(&settings_path);

    let native_options = eframe::NativeOptions {
        viewport: ViewportBuilder::default()
            .with_inner_size([980.0, 680.0])
            .with_min_inner_size([760.0, 560.0])
            .with_title("WWM Guqin Freeplay"),
        ..Default::default()
    };

    eframe::run_native(
        "WWM Guqin Freeplay",
        native_options,
        Box::new(move |cc| {
            // System fonts: Segoe UI for Vietnamese, CJK fallback for Chinese/Korean titles.
            let mut fonts = egui::FontDefinitions::default();
            fonts::install_system_fonts(&mut fonts);
            cc.egui_ctx.set_fonts(fonts);

            let (finish_tx, finish_rx) = channel::<()>();
            let ctx_clone = cc.egui_ctx.clone();

            let controller: Shared<Output> = Arc::new(std::sync::Mutex::new(Controller::new(
                settings.clone(),
                settings_path,
                Output::from_settings,
                move |event| {
                    if let PlayerEvent::Finished = event {
                        let _ = finish_tx.send(());
                    }
                    ctx_clone.request_repaint();
                },
            )));

            // Worker thread prevents deadlock: drains PlayerEvent::Finished with Weak handle
            let weak_controller = Arc::downgrade(&controller);
            let ctx_finished = cc.egui_ctx.clone();
            let spawned = thread::Builder::new()
                .name("player-finished-worker".to_string())
                .spawn(move || {
                    let lock_with = |f: &mut dyn FnMut(&mut Controller<Output>)| {
                        let strong = weak_controller.upgrade()?;
                        f(&mut strong.lock().unwrap_or_else(|e| e.into_inner()));
                        Some(())
                    };
                    while finish_rx.recv().is_ok() {
                        let mut plan = None;
                        if lock_with(&mut |c| plan = c.schedule_advance()).is_none() {
                            break;
                        }
                        let Some((delay, token)) = plan else {
                            continue;
                        };
                        ctx_finished.request_repaint();
                        // Wait without the lock so the UI and hotkeys stay responsive.
                        thread::sleep(delay);
                        if lock_with(&mut |c| c.advance_if_current(token)).is_none() {
                            break;
                        }
                        ctx_finished.request_repaint();
                    }
                });
            if spawned.is_err() {
                // Without the worker, songs do not auto-advance; playback still works.
                eprintln!("could not start the auto-advance worker");
            }

            // Start global hotkeys
            let (specs, hotkey_parse_errors) = hotkeys::from_settings(&settings.hotkeys);
            let controller_hotkey = Arc::clone(&controller);
            let ctx_hotkey = cc.egui_ctx.clone();

            let (hotkey_thread, hotkey_reg_failures) = HotkeyThread::start(specs, move |action| {
                controller_hotkey
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .handle_action(action);
                ctx_hotkey.request_repaint();
            });

            let mut all_failures = hotkey_reg_failures;
            for (action, err) in hotkey_parse_errors {
                all_failures.push((action, err));
            }

            Ok(Box::new(App::new(
                controller,
                Some(hotkey_thread),
                load_warning,
                all_failures,
            )))
        }),
    )
}
