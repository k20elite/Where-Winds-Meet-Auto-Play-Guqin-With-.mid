//! Neumorphic egui front end. Drawing code never mutates the controller directly: it reads
//! one snapshot per frame and pushes `UiAction`s, applied afterwards under a single lock.

use crate::controller::{Controller, Shared};
use crate::hotkeys::{self, Action, HotkeyThread};
use crate::input::{GameWindow, Output};
use crate::layout::{KeyLayout, LayoutPreset};
use crate::library;
use crate::mapping::{KeyMode, NoteMode};
use crate::neu::{self, NeuButton, Palette};
use crate::player::PlayState;
use crate::settings::{Hotkeys, InputBackend, Repeat, Settings, Theme};
use eframe::egui::{
    self, Align, Align2, CornerRadius, FontId, Frame, Id, Layout, Order, Pos2, Rect, RichText,
    ScrollArea, Sense, Ui, Vec2,
};
use std::sync::atomic::Ordering;
use std::sync::MutexGuard;
use std::time::{Duration, Instant};

const TOAST_TTL: Duration = Duration::from_secs(4);
const SAVE_EVERY: Duration = Duration::from_secs(2);
const PLAYING_REPAINT: Duration = Duration::from_millis(33);
const ROW_HEIGHT: f32 = 38.0;

/// Library tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    All,
    Favorites,
    Queue,
}

/// Everything the UI can ask the controller to do.
#[derive(Debug, Clone)]
enum UiAction {
    Toggle,
    Stop,
    Next,
    Prev,
    Seek(u64),
    CycleMode(i32),
    KeyMode(KeyMode),
    Octave(i8),
    AutoTranspose(bool),
    ManualTranspose(i8),
    Speed(f32),
    Repeat(Repeat),
    Shuffle(bool),
    Track(usize, bool),
    PlayEntry(usize, Vec<usize>),
    ToggleFavorite(usize),
    Rescan,
    ChooseFolder,
    Theme(Theme),
    Layout(LayoutPreset),
    CustomKeys(String),
    Backend(InputBackend),
    ModifierDelay(u16),
    Hold(u16),
    SkipDrums(bool),
    Keywords(Vec<String>),
    ApplyHotkeys,
}

/// Track row shown in the tracks card.
struct TrackRow {
    enabled: bool,
    name: String,
    notes: u32,
    drum: bool,
}

/// Per-frame copy of controller state (one lock per frame).
struct Snapshot {
    title: Option<String>,
    playing_entry: Option<usize>,
    state: PlayState,
    pos_us: u64,
    dur_us: u64,
    active_slots: u32,
    note_mode: NoteMode,
    key_mode: KeyMode,
    octave: i8,
    auto_transpose: bool,
    manual_transpose: i8,
    auto_shift: i8,
    speed: f32,
    repeat: Repeat,
    shuffle: bool,
    theme: Theme,
    backend: InputBackend,
    layout: LayoutPreset,
    keys: [char; 21],
    modifier_delay_ms: u16,
    key_hold_ms: u16,
    skip_drums: bool,
    hotkeys: Hotkeys,
    has_library: bool,
    tracks: Vec<TrackRow>,
}

/// Cached visible library rows; recomputed only when its key changes.
#[derive(Default)]
struct ListCache {
    key: Option<(u64, u8, String)>,
    indices: Vec<usize>,
}

struct Toast {
    text: String,
    born: Instant,
}

/// The application window.
pub struct App {
    controller: Shared<Output>,
    hotkey_thread: Option<HotkeyThread>,
    toasts: Vec<Toast>,
    tab: Tab,
    query: String,
    list: ListCache,
    settings_open: bool,
    tracks_open: bool,
    custom_keys: String,
    keywords_text: String,
    hotkeys_draft: Hotkeys,
    hotkey_errors: [Option<String>; 6],
    last_save: Instant,
    reduced_motion: bool,
    styled_theme: Option<Theme>,
    game: GameWindow,
    game_keywords: Vec<String>,
}

fn lock(shared: &Shared<Output>) -> MutexGuard<'_, Controller<Output>> {
    shared.lock().unwrap_or_else(|e| e.into_inner())
}

/// Format seconds as mm:ss (negative/NaN -> 00:00; minutes may exceed 59).
pub fn format_time(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return "00:00".to_owned();
    }
    let s = seconds.floor() as u64;
    format!("{:02}:{:02}", s / 60, s % 60)
}

/// "1 song" / "N songs".
pub fn song_count(n: usize) -> String {
    if n == 1 {
        "1 song".to_owned()
    } else {
        format!("{n} songs")
    }
}

/// Jianpu label for a key slot: (degree 1..=7, octave -1 low / 0 mid / +1 high).
pub fn jianpu(slot: u8) -> (u8, i8) {
    (slot % 7 + 1, (slot / 7) as i8 - 1)
}

fn tab_code(tab: Tab) -> u8 {
    match tab {
        Tab::All => 0,
        Tab::Favorites => 1,
        Tab::Queue => 2,
    }
}

fn action_index(a: Action) -> usize {
    match a {
        Action::PlayPause => 0,
        Action::Stop => 1,
        Action::Next => 2,
        Action::Prev => 3,
        Action::ModeNext => 4,
        Action::ModePrev => 5,
    }
}

impl App {
    /// Build the app; `notice` and `hotkey_failures` are shown as toasts.
    pub fn new(
        controller: Shared<Output>,
        hotkey_thread: Option<HotkeyThread>,
        notice: Option<String>,
        hotkey_failures: Vec<(Action, String)>,
    ) -> Self {
        let (custom_keys, keywords, hotkeys_draft) = {
            let c = lock(&controller);
            let s = c.settings();
            (
                s.custom_keys.clone(),
                s.window_keywords.clone(),
                s.hotkeys.clone(),
            )
        };
        let mut app = Self {
            controller,
            hotkey_thread,
            toasts: Vec::new(),
            tab: Tab::All,
            query: String::new(),
            list: ListCache::default(),
            settings_open: false,
            tracks_open: true,
            custom_keys,
            keywords_text: keywords.join("\n"),
            hotkeys_draft,
            hotkey_errors: Default::default(),
            last_save: Instant::now(),
            reduced_motion: neu::is_reduced_motion(),
            styled_theme: None,
            game: GameWindow::new(keywords.clone()),
            game_keywords: keywords,
        };
        if let Some(n) = notice {
            app.toast(n);
        }
        for (action, err) in hotkey_failures {
            app.hotkey_errors[action_index(action)] = Some(err.clone());
            app.toast(format!("Hotkey {action:?}: {err}"));
        }
        app
    }

    fn toast(&mut self, text: String) {
        self.toasts.push(Toast {
            text,
            born: Instant::now(),
        });
    }

    /// One lock: snapshot state, refresh the list cache, collect notices.
    fn snapshot(&mut self) -> Snapshot {
        let mut c = lock(&self.controller);
        if let Some(n) = c.take_notice() {
            self.toasts.push(Toast {
                text: n,
                born: Instant::now(),
            });
        }
        let status = c.status();
        let s: &Settings = c.settings();
        let key_layout = s.key_layout();
        let np = c.now_playing();
        let tracks = np
            .map(|np| {
                np.song
                    .tracks
                    .iter()
                    .enumerate()
                    .map(|(i, t)| TrackRow {
                        enabled: np.tracks.get(i).copied().unwrap_or(true),
                        name: t.name.clone(),
                        notes: t.note_count,
                        drum: t.is_drum,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let snap = Snapshot {
            title: np.map(|n| n.title.clone()),
            playing_entry: np.and_then(|n| n.entry),
            state: status.state(),
            pos_us: status.position_us.load(Ordering::Relaxed),
            dur_us: status.duration_us.load(Ordering::Relaxed),
            active_slots: status.active_slots.load(Ordering::Relaxed),
            note_mode: s.note_mode,
            key_mode: s.key_mode,
            octave: s.octave,
            auto_transpose: s.auto_transpose,
            manual_transpose: s.manual_transpose,
            auto_shift: np.map(|n| n.auto_shift).unwrap_or(0),
            speed: s.speed,
            repeat: s.repeat,
            shuffle: s.shuffle,
            theme: s.theme,
            backend: s.backend,
            layout: s.layout,
            keys: *key_layout.keys(),
            modifier_delay_ms: s.modifier_delay_ms,
            key_hold_ms: s.key_hold_ms,
            skip_drums: s.skip_drums,
            hotkeys: s.hotkeys.clone(),
            has_library: !c.entries().is_empty(),
            tracks,
        };

        let key = (c.revision(), tab_code(self.tab), self.query.clone());
        if self.list.key.as_ref() != Some(&key) {
            let base: Vec<usize> = match self.tab {
                Tab::All => (0..c.entries().len()).collect(),
                Tab::Favorites => c.favorite_indices(),
                Tab::Queue => c.queue_items().to_vec(),
            };
            self.list.indices = if self.query.trim().is_empty() {
                base
            } else {
                let mut hit = vec![false; c.entries().len()];
                for i in library::filter(c.entries(), &self.query) {
                    hit[i] = true;
                }
                base.into_iter().filter(|&i| hit[i]).collect()
            };
            self.list.key = Some(key);
        }
        snap
    }

    fn apply(&mut self, actions: Vec<UiAction>, ctx: &egui::Context) {
        if actions.is_empty() {
            return;
        }
        let mut folder = None;
        let mut restart_hotkeys = false;
        let mut toasts = Vec::new();
        {
            let mut c = lock(&self.controller);
            for a in actions {
                match a {
                    UiAction::Toggle => c.toggle(),
                    UiAction::Stop => c.stop(),
                    UiAction::Next => c.next(),
                    UiAction::Prev => c.prev(),
                    UiAction::Seek(us) => c.seek(us),
                    UiAction::CycleMode(d) => c.cycle_mode(d),
                    UiAction::KeyMode(m) => c.set_key_mode(m),
                    UiAction::Octave(o) => c.set_octave(o),
                    UiAction::AutoTranspose(b) => c.set_auto_transpose(b),
                    UiAction::ManualTranspose(t) => c.set_manual_transpose(t),
                    UiAction::Speed(s) => c.set_speed(s),
                    UiAction::Repeat(r) => c.set_repeat(r),
                    UiAction::Shuffle(b) => c.set_shuffle(b),
                    UiAction::Track(i, b) => c.set_track_enabled(i, b),
                    UiAction::PlayEntry(i, q) => c.play_entry(i, q),
                    UiAction::ToggleFavorite(i) => c.toggle_favorite(i),
                    UiAction::Rescan => match c.rescan() {
                        Ok(n) => toasts.push(format!("Found {}", song_count(n))),
                        Err(e) => toasts.push(e),
                    },
                    UiAction::ChooseFolder => folder = Some(()),
                    UiAction::Theme(t) => c.update_settings(|s| s.theme = t),
                    UiAction::Layout(p) => c.update_output_settings(|s| s.layout = p),
                    UiAction::CustomKeys(k) => c.update_output_settings(|s| s.custom_keys = k),
                    UiAction::Backend(b) => c.update_output_settings(|s| s.backend = b),
                    UiAction::ModifierDelay(ms) => {
                        c.update_output_settings(|s| s.modifier_delay_ms = ms)
                    }
                    UiAction::Hold(ms) => c.set_hold_ms(ms),
                    UiAction::SkipDrums(b) => c.update_settings(|s| s.skip_drums = b),
                    UiAction::Keywords(k) => c.update_output_settings(|s| s.window_keywords = k),
                    UiAction::ApplyHotkeys => restart_hotkeys = true,
                }
            }
        }
        for t in toasts {
            self.toast(t);
        }
        // The native dialog blocks this thread, so it runs without the controller lock held.
        if folder.is_some() {
            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                let n = {
                    let mut c = lock(&self.controller);
                    c.set_library_dir(dir);
                    c.entries().len()
                };
                self.toast(format!("Found {}", song_count(n)));
            }
        }
        if restart_hotkeys {
            self.restart_hotkeys(ctx);
        }
    }

    fn restart_hotkeys(&mut self, ctx: &egui::Context) {
        let (specs, errors) = hotkeys::from_settings(&self.hotkeys_draft);
        self.hotkey_errors = Default::default();
        if !errors.is_empty() {
            for (a, e) in errors {
                self.hotkey_errors[action_index(a)] = Some(e);
            }
            self.toast("Fix the hotkey errors first".to_owned());
            return;
        }
        if let Some(old) = self.hotkey_thread.take() {
            old.stop();
        }
        let shared = self.controller.clone();
        let repaint = ctx.clone();
        let (thread, failures) = HotkeyThread::start(specs, move |action| {
            lock(&shared).handle_action(action);
            repaint.request_repaint();
        });
        self.hotkey_thread = Some(thread);
        let draft = self.hotkeys_draft.clone();
        lock(&self.controller).update_settings(|s| s.hotkeys = draft);
        if failures.is_empty() {
            self.toast("Hotkeys updated".to_owned());
        }
        for (a, e) in failures {
            self.hotkey_errors[action_index(a)] = Some(e.clone());
            self.toast(format!("Hotkey {a:?}: {e}"));
        }
    }

    fn sync_game_window(&mut self) -> bool {
        let keywords = lock(&self.controller).settings().window_keywords.clone();
        if keywords != self.game_keywords {
            self.game = GameWindow::new(keywords.clone());
            self.game_keywords = keywords;
        }
        self.game.is_found()
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let snap = self.snapshot();
        let pal = neu::palette(snap.theme);

        if self.styled_theme != Some(snap.theme) {
            // Apply to both egui themes so an OS light/dark switch cannot bring back defaults.
            let visuals = neu::visuals(snap.theme, &pal, self.reduced_motion);
            let animation_time = if self.reduced_motion { 0.0 } else { 0.1 };
            ctx.all_styles_mut(|s| {
                s.visuals = visuals.clone();
                s.animation_time = animation_time;
            });
            self.styled_theme = Some(snap.theme);
        }

        if self.last_save.elapsed() >= SAVE_EVERY {
            self.last_save = Instant::now();
            let save_error = lock(&self.controller).save_if_dirty();
            if let Some(err) = save_error {
                self.toast(format!("Cannot save settings: {err}"));
            }
        }

        let game_found = match snap.backend {
            InputBackend::Window => {
                ctx.request_repaint_after(Duration::from_secs(1));
                self.sync_game_window()
            }
            InputBackend::Global => true,
        };
        if snap.state == PlayState::Playing {
            ctx.request_repaint_after(PLAYING_REPAINT);
        }

        let mut actions = Vec::new();
        egui::CentralPanel::default()
            .frame(Frame::NONE.fill(pal.base).inner_margin(20.0))
            .show(ui, |ui| {
                header(
                    ui,
                    &pal,
                    &snap,
                    game_found,
                    &mut actions,
                    &mut self.settings_open,
                );
                ui.add_space(18.0);
                self.body(ui, &pal, &snap, &mut actions);
            });

        if self.settings_open {
            self.settings_window(&ctx, &pal, &snap, &mut actions);
        }
        self.draw_toasts(&ctx, &pal);
        self.apply(actions, &ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(t) = self.hotkey_thread.take() {
            t.stop();
        }
        let mut c = lock(&self.controller);
        c.stop();
        let _ = c.save_if_dirty();
    }
}

fn header(
    ui: &mut Ui,
    pal: &Palette,
    snap: &Snapshot,
    game_found: bool,
    actions: &mut Vec<UiAction>,
    settings_open: &mut bool,
) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("WWM Guqin Freeplay")
                .font(FontId::proportional(22.0))
                .strong()
                .color(pal.text),
        );
        ui.add_space(14.0);

        let (label, dot) = match snap.backend {
            InputBackend::Global => ("Global input", pal.accent),
            InputBackend::Window if game_found => ("Game found", pal.accent),
            InputBackend::Window => ("Game not found", pal.text_secondary),
        };
        let galley =
            ui.painter()
                .layout_no_wrap(label.to_owned(), FontId::proportional(13.0), pal.text);
        let size = Vec2::new(galley.size().x + 40.0, 32.0);
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        neu::paint_inset(ui.painter(), rect, CornerRadius::same(16), pal, 0.4);
        ui.painter()
            .circle_filled(Pos2::new(rect.left() + 16.0, rect.center().y), 5.0, dot);
        ui.painter().galley(
            Pos2::new(rect.left() + 28.0, rect.center().y - galley.size().y * 0.5),
            galley,
            pal.text,
        );

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let settings_label = if *settings_open {
                "Close settings"
            } else {
                "Settings"
            };
            if ui.add(NeuButton::new(settings_label, *pal)).clicked() {
                *settings_open = !*settings_open;
            }
            ui.add_space(10.0);
            let (glyph, next) = match snap.theme {
                Theme::Light => ("Dark mode", Theme::Dark),
                Theme::Dark => ("Light mode", Theme::Light),
            };
            if ui.add(NeuButton::new(glyph, *pal)).clicked() {
                actions.push(UiAction::Theme(next));
            }
        });
    });
}

impl App {
    fn body(&mut self, ui: &mut Ui, pal: &Palette, snap: &Snapshot, actions: &mut Vec<UiAction>) {
        let avail = ui.available_size();
        let gap = 24.0;
        let left_w = (avail.x * 0.5).clamp(380.0, 560.0);
        let right_w = (avail.x - left_w - gap).max(260.0);
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                Vec2::new(left_w, avail.y),
                Layout::top_down(Align::Min),
                |ui| {
                    ScrollArea::vertical()
                        .id_salt("left")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            Frame::NONE.inner_margin(12.0).show(ui, |ui| {
                                now_playing_card(ui, pal, snap, actions);
                                ui.add_space(22.0);
                                controls_card(ui, pal, snap, actions);
                                ui.add_space(22.0);
                                keyboard_card(ui, pal, snap, self.reduced_motion);
                                if !snap.tracks.is_empty() {
                                    ui.add_space(22.0);
                                    tracks_card(ui, pal, snap, &mut self.tracks_open, actions);
                                }
                            });
                        });
                },
            );
            ui.add_space(gap);
            ui.allocate_ui_with_layout(
                Vec2::new(right_w, avail.y),
                Layout::top_down(Align::Min),
                |ui| {
                    Frame::NONE.inner_margin(12.0).show(ui, |ui| {
                        self.library_card(ui, pal, snap, actions);
                    });
                },
            );
        });
    }

    fn library_card(
        &mut self,
        ui: &mut Ui,
        pal: &Palette,
        snap: &Snapshot,
        actions: &mut Vec<UiAction>,
    ) {
        neu::neu_card(ui, pal, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Library")
                        .font(FontId::proportional(17.0))
                        .strong()
                        .color(pal.text),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add(NeuButton::new("Rescan", *pal)).clicked() {
                        actions.push(UiAction::Rescan);
                    }
                    ui.add_space(8.0);
                    if ui.add(NeuButton::new("Choose folder", *pal)).clicked() {
                        actions.push(UiAction::ChooseFolder);
                    }
                });
            });
            ui.add_space(14.0);
            let width = ui.available_width();
            neu::neu_text_field(
                ui,
                &mut self.query,
                "Search (no accents needed)…",
                pal,
                width,
            );
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                for (tab, label) in [
                    (Tab::All, "All"),
                    (Tab::Favorites, "Favorites"),
                    (Tab::Queue, "Queue"),
                ] {
                    let btn = NeuButton::new(label, *pal).selected(self.tab == tab);
                    if ui.add(btn).clicked() {
                        self.tab = tab;
                    }
                    ui.add_space(6.0);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(song_count(self.list.indices.len()))
                            .color(pal.text_secondary),
                    );
                });
            });
            ui.add_space(12.0);

            if !snap.has_library {
                ui.add_space(24.0);
                ui.label(
                    RichText::new("Choose a folder with .mid files to get started.")
                        .color(pal.text_secondary),
                );
                return;
            }
            if self.list.indices.is_empty() {
                ui.label(RichText::new("No songs match.").color(pal.text_secondary));
                return;
            }

            let indices = &self.list.indices;
            let shared = &self.controller;
            ScrollArea::vertical()
                .id_salt("library")
                .auto_shrink([false, false])
                .show_rows(ui, ROW_HEIGHT, indices.len(), |ui, range| {
                    let rows: Vec<(usize, String, bool)> = {
                        let c = lock(shared);
                        indices[range]
                            .iter()
                            .map(|&i| {
                                let title = c
                                    .entries()
                                    .get(i)
                                    .map(|e| e.title.clone())
                                    .unwrap_or_default();
                                (i, title, c.is_favorite(i))
                            })
                            .collect()
                    };
                    for (idx, title, fav) in rows {
                        library_row(ui, pal, idx, &title, fav, snap, indices, actions);
                    }
                });
        });
    }

    fn settings_window(
        &mut self,
        ctx: &egui::Context,
        pal: &Palette,
        snap: &Snapshot,
        actions: &mut Vec<UiAction>,
    ) {
        let mut open = self.settings_open;
        egui::Window::new(RichText::new("Settings").color(pal.text).strong())
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size(Vec2::new(
                540.0,
                (ctx.content_rect().height() - 60.0).max(420.0),
            ))
            .frame(
                Frame::NONE
                    .fill(pal.base)
                    .corner_radius(CornerRadius::same(20))
                    .inner_margin(20.0)
                    .shadow(ctx.global_style().visuals.window_shadow),
            )
            .show(ctx, |ui| {
                ScrollArea::vertical()
                    .id_salt("settings")
                    .show(ui, |ui| self.settings_body(ui, pal, snap, actions));
            });
        self.settings_open = open;
    }

    fn settings_body(
        &mut self,
        ui: &mut Ui,
        pal: &Palette,
        snap: &Snapshot,
        actions: &mut Vec<UiAction>,
    ) {
        section(ui, pal, "Keyboard layout");
        ui.horizontal_wrapped(|ui| {
            for (p, name) in [
                (LayoutPreset::Qwerty, "QWERTY"),
                (LayoutPreset::Azerty, "AZERTY"),
                (LayoutPreset::Qwertz, "QWERTZ"),
                (LayoutPreset::Custom, "Custom"),
            ] {
                if ui
                    .add(NeuButton::new(name, *pal).selected(snap.layout == p))
                    .clicked()
                    && snap.layout != p
                {
                    actions.push(UiAction::Layout(p));
                }
            }
        });
        if snap.layout == LayoutPreset::Custom {
            ui.add_space(8.0);
            hint(ui, pal, "21 keys, low row first (do re mi fa so la ti × 3)");
            let resp = neu::neu_text_field(ui, &mut self.custom_keys, "", pal, 300.0);
            let chars: Vec<char> = self.custom_keys.chars().collect();
            match KeyLayout::custom(&chars) {
                Ok(_) if resp.changed() => {
                    actions.push(UiAction::CustomKeys(self.custom_keys.to_lowercase()))
                }
                Ok(_) => {}
                Err(e) => error_line(ui, pal, &e.to_string()),
            }
        }

        section(ui, pal, "Input");
        ui.horizontal_wrapped(|ui| {
            for (b, name) in [
                (InputBackend::Global, "Global (SendInput)"),
                (InputBackend::Window, "Background window"),
            ] {
                if ui
                    .add(NeuButton::new(name, *pal).selected(snap.backend == b))
                    .clicked()
                    && snap.backend != b
                {
                    actions.push(UiAction::Backend(b));
                }
            }
        });
        if snap.backend == InputBackend::Window {
            hint(
                ui,
                pal,
                "Background mode posts keys to the game window; Shift/Ctrl may not register.",
            );
        }
        ui.add_space(8.0);
        let mut delay = snap.modifier_delay_ms as f32;
        if labeled_slider(ui, pal, "Modifier delay", &mut delay, 0.0..=50.0, "ms") {
            actions.push(UiAction::ModifierDelay(delay.round() as u16));
        }
        let mut hold = snap.key_hold_ms as f32;
        if labeled_slider(ui, pal, "Key hold", &mut hold, 5.0..=200.0, "ms") {
            actions.push(UiAction::Hold(hold.round() as u16));
        }
        let mut skip = snap.skip_drums;
        if neu::neu_toggle(ui, &mut skip, pal, "Skip drum tracks").changed() {
            actions.push(UiAction::SkipDrums(skip));
        }

        section(ui, pal, "Game window titles (one per line)");
        let resp = neu::neu_text_area(ui, &mut self.keywords_text, pal, 3);
        if resp.lost_focus() {
            let lines = self
                .keywords_text
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_owned)
                .collect();
            actions.push(UiAction::Keywords(lines));
        }

        section(ui, pal, "Global hotkeys");
        hint(
            ui,
            pal,
            "e.g. ScrollLock, F10, Ctrl+Shift+P. Letters need a modifier.",
        );
        let fields: [(&str, Action); 6] = [
            ("Play / pause", Action::PlayPause),
            ("Stop", Action::Stop),
            ("Next", Action::Next),
            ("Previous", Action::Prev),
            ("Next mode", Action::ModeNext),
            ("Previous mode", Action::ModePrev),
        ];
        for (label, action) in fields {
            let i = action_index(action);
            let draft = &mut self.hotkeys_draft;
            let text = match action {
                Action::PlayPause => &mut draft.play_pause,
                Action::Stop => &mut draft.stop,
                Action::Next => &mut draft.next,
                Action::Prev => &mut draft.prev,
                Action::ModeNext => &mut draft.mode_next,
                Action::ModePrev => &mut draft.mode_prev,
            };
            ui.horizontal(|ui| {
                row_label(ui, pal, label, 120.0);
                if neu::neu_text_field(ui, text, "", pal, 180.0).changed() {
                    self.hotkey_errors[i] = hotkeys::parse(text).err().map(|e| e.to_string());
                }
            });
            if let Some(err) = &self.hotkey_errors[i] {
                error_line(ui, pal, err);
            }
            ui.add_space(4.0);
        }
        ui.add_space(6.0);
        let changed = self.hotkeys_draft != snap.hotkeys;
        if ui
            .add(
                NeuButton::new("Apply hotkeys", *pal)
                    .accent(changed)
                    .enabled(changed),
            )
            .clicked()
        {
            actions.push(UiAction::ApplyHotkeys);
        }
    }

    fn draw_toasts(&mut self, ctx: &egui::Context, pal: &Palette) {
        self.toasts.retain(|t| t.born.elapsed() < TOAST_TTL);
        let Some(soonest) = self
            .toasts
            .iter()
            .map(|t| TOAST_TTL - t.born.elapsed())
            .min()
        else {
            return;
        };
        ctx.request_repaint_after(soonest);
        egui::Area::new(Id::new("toasts"))
            .order(Order::Foreground)
            .anchor(Align2::CENTER_BOTTOM, Vec2::new(0.0, -28.0))
            .interactable(false)
            .show(ctx, |ui| {
                for t in self.toasts.iter().rev().take(4) {
                    let galley = ui.painter().layout(
                        t.text.clone(),
                        FontId::proportional(14.0),
                        pal.text,
                        420.0,
                    );
                    let size = galley.size() + Vec2::new(36.0, 22.0);
                    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
                    neu::paint_raised(ui.painter(), rect, CornerRadius::same(16), pal, 0.7);
                    ui.painter().circle_filled(
                        Pos2::new(rect.left() + 12.0, rect.center().y),
                        3.5,
                        pal.accent,
                    );
                    ui.painter()
                        .galley(rect.min + Vec2::new(22.0, 11.0), galley, pal.text);
                    ui.add_space(10.0);
                }
            });
    }
}

/// Fixed-width, left-aligned label so control rows line up.
fn row_label(ui: &mut Ui, pal: &Palette, text: &str, width: f32) {
    ui.allocate_ui_with_layout(
        Vec2::new(width, 34.0),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.set_min_width(width);
            ui.label(RichText::new(text).color(pal.text));
        },
    );
}

fn section(ui: &mut Ui, pal: &Palette, title: &str) {
    ui.add_space(16.0);
    ui.label(
        RichText::new(title)
            .font(FontId::proportional(15.0))
            .strong()
            .color(pal.text),
    );
    ui.add_space(6.0);
}

fn hint(ui: &mut Ui, pal: &Palette, text: &str) {
    ui.label(
        RichText::new(text)
            .font(FontId::proportional(12.0))
            .color(pal.text_secondary),
    );
}

fn error_line(ui: &mut Ui, pal: &Palette, text: &str) {
    ui.label(
        RichText::new(format!("⚠ {text}"))
            .font(FontId::proportional(12.5))
            .strong()
            .color(pal.text),
    );
}

fn labeled_slider(
    ui: &mut Ui,
    pal: &Palette,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    unit: &str,
) -> bool {
    ui.horizontal(|ui| {
        row_label(ui, pal, label, 120.0);
        let changed = neu::neu_slider(ui, value, range, pal, 180.0).changed();
        ui.label(RichText::new(format!("{:.0} {unit}", value)).color(pal.text_secondary));
        changed
    })
    .inner
}

fn card_title(ui: &mut Ui, pal: &Palette, text: &str) {
    ui.label(
        RichText::new(text)
            .font(FontId::proportional(13.0))
            .color(pal.text_secondary),
    );
    ui.add_space(6.0);
}

fn now_playing_card(ui: &mut Ui, pal: &Palette, snap: &Snapshot, actions: &mut Vec<UiAction>) {
    neu::neu_card(ui, pal, |ui| {
        card_title(ui, pal, "NOW PLAYING");
        let title = snap.title.as_deref().unwrap_or("Nothing playing");
        ui.add(
            egui::Label::new(
                RichText::new(title)
                    .font(FontId::proportional(19.0))
                    .strong()
                    .color(pal.text),
            )
            .truncate(),
        );
        ui.add_space(12.0);

        let dur = snap.dur_us.max(1) as f64;
        ui.horizontal(|ui| {
            let mono = |s: String| RichText::new(s).font(FontId::monospace(13.0));
            ui.label(mono(format_time(snap.pos_us as f64 / 1e6)).color(pal.text_secondary));
            let mut t = (snap.pos_us as f64 / dur) as f32;
            let w = ui.available_width() - 56.0;
            if neu::neu_slider(ui, &mut t, 0.0..=1.0, pal, w).changed() && snap.title.is_some() {
                actions.push(UiAction::Seek((t as f64 * dur) as u64));
            }
            ui.label(mono(format_time(snap.dur_us as f64 / 1e6)).color(pal.text_secondary));
        });
        ui.add_space(14.0);

        ui.horizontal(|ui| {
            let small = Vec2::new(64.0, 44.0);
            let total = 64.0 * 3.0 + 84.0 + 3.0 * 14.0;
            ui.add_space(((ui.available_width() - total) * 0.5).max(0.0));
            if ui.add(NeuButton::new("⏮", *pal).min_size(small)).clicked() {
                actions.push(UiAction::Prev);
            }
            ui.add_space(14.0);
            let playing = snap.state == PlayState::Playing;
            let play = NeuButton::new(if playing { "⏸" } else { "▶" }, *pal)
                .accent(true)
                .font_size(20.0)
                .min_size(Vec2::new(84.0, 52.0))
                .corner_radius(CornerRadius::same(26));
            let resp = ui
                .add(play)
                .on_hover_text(if playing { "Pause" } else { "Play" });
            if resp.clicked() {
                actions.push(UiAction::Toggle);
            }
            ui.add_space(14.0);
            let stop = ui.add(NeuButton::new("⏹", *pal).min_size(small));
            if stop.on_hover_text("Stop").clicked() {
                actions.push(UiAction::Stop);
            }
            ui.add_space(14.0);
            let next = ui.add(NeuButton::new("⏭", *pal).min_size(small));
            if next.on_hover_text("Next").clicked() {
                actions.push(UiAction::Next);
            }
        });
        ui.add_space(4.0);
        let state_text = match snap.state {
            PlayState::Playing => "Playing",
            PlayState::Paused => "Paused",
            PlayState::Finished => "Finished",
            PlayState::Idle => "Stopped",
        };
        ui.vertical_centered(|ui| hint(ui, pal, state_text));
    });
}

fn stepper(
    ui: &mut Ui,
    pal: &Palette,
    label: &str,
    value_text: &str,
    enabled: bool,
) -> Option<i32> {
    let mut out = None;
    ui.horizontal(|ui| {
        row_label(ui, pal, label, 86.0);
        let btn = |t| {
            NeuButton::new(t, *pal)
                .enabled(enabled)
                .min_size(Vec2::splat(34.0))
        };
        if ui.add(btn("−")).clicked() {
            out = Some(-1);
        }
        let color = if enabled {
            pal.text
        } else {
            pal.text_secondary
        };
        ui.add_sized(
            Vec2::new(96.0, 34.0),
            egui::Label::new(RichText::new(value_text).strong().color(color)),
        );
        if ui.add(btn("+")).clicked() {
            out = Some(1);
        }
    });
    out
}

fn controls_card(ui: &mut Ui, pal: &Palette, snap: &Snapshot, actions: &mut Vec<UiAction>) {
    neu::neu_card(ui, pal, |ui| {
        card_title(ui, pal, "SOUND");
        if let Some(d) = stepper(ui, pal, "Mode", snap.note_mode.label(), true) {
            actions.push(UiAction::CycleMode(d));
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            row_label(ui, pal, "Keys", 86.0);
            for (mode, name) in [
                (KeyMode::Natural21, "21 keys"),
                (KeyMode::Chromatic36, "36 keys"),
            ] {
                if ui
                    .add(NeuButton::new(name, *pal).selected(snap.key_mode == mode))
                    .clicked()
                    && snap.key_mode != mode
                {
                    actions.push(UiAction::KeyMode(mode));
                }
                ui.add_space(6.0);
            }
        });
        ui.add_space(8.0);
        let octave = format!("{:+}", snap.octave);
        if let Some(d) = stepper(ui, pal, "Octave", &octave, true) {
            actions.push(UiAction::Octave(snap.octave + d as i8));
        }
        ui.add_space(8.0);
        let mut auto = snap.auto_transpose;
        ui.horizontal(|ui| {
            row_label(ui, pal, "Transpose", 86.0);
            if neu::neu_toggle(ui, &mut auto, pal, "Auto").changed() {
                actions.push(UiAction::AutoTranspose(auto));
            }
        });
        let value = if snap.auto_transpose {
            format!("{:+} (auto)", snap.auto_shift)
        } else {
            format!("{:+}", snap.manual_transpose)
        };
        if let Some(d) = stepper(ui, pal, "", &value, !snap.auto_transpose) {
            actions.push(UiAction::ManualTranspose(snap.manual_transpose + d as i8));
        }
        ui.add_space(8.0);
        let mut speed = snap.speed;
        ui.horizontal(|ui| {
            row_label(ui, pal, "Speed", 86.0);
            let w = (ui.available_width() - 60.0).max(80.0);
            if neu::neu_slider(ui, &mut speed, 0.25..=2.0, pal, w).changed() {
                // Snap to 0.05 steps so the value text stays readable.
                actions.push(UiAction::Speed((speed * 20.0).round() / 20.0));
            }
            ui.label(RichText::new(format!("{:.2}×", snap.speed)).color(pal.text_secondary));
        });
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            row_label(ui, pal, "Repeat", 86.0);
            for (r, name) in [
                (Repeat::Off, "Off"),
                (Repeat::One, "One"),
                (Repeat::All, "All"),
            ] {
                if ui
                    .add(NeuButton::new(name, *pal).selected(snap.repeat == r))
                    .clicked()
                    && snap.repeat != r
                {
                    actions.push(UiAction::Repeat(r));
                }
                ui.add_space(6.0);
            }
            ui.add_space(10.0);
            let mut shuffle = snap.shuffle;
            if neu::neu_toggle(ui, &mut shuffle, pal, "Shuffle").changed() {
                actions.push(UiAction::Shuffle(shuffle));
            }
        });
    });
}

fn keyboard_card(ui: &mut Ui, pal: &Palette, snap: &Snapshot, reduced_motion: bool) {
    neu::neu_card(ui, pal, |ui| {
        card_title(ui, pal, "QIN BOARD");
        let gap = 8.0;
        let tile = ((ui.available_width() - 6.0 * gap) / 7.0).clamp(38.0, 58.0);
        let indent = ((ui.available_width() - 7.0 * tile - 6.0 * gap) * 0.5).max(0.0);
        for row in [2u8, 1, 0] {
            ui.horizontal(|ui| {
                ui.add_space(indent);
                ui.spacing_mut().item_spacing.x = gap;
                for degree in 0..7u8 {
                    let slot = row * 7 + degree;
                    let active = snap.active_slots & (1 << slot) != 0;
                    key_tile(
                        ui,
                        pal,
                        snap.keys[slot as usize],
                        slot,
                        active,
                        tile,
                        reduced_motion,
                    );
                }
            });
            ui.add_space(gap);
        }
    });
}

fn key_tile(
    ui: &mut Ui,
    pal: &Palette,
    key: char,
    slot: u8,
    active: bool,
    size: f32,
    reduced_motion: bool,
) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let cr = CornerRadius::same(12);
    // Press animation: a short ease into the inset look, skipped under reduced motion.
    let t = if reduced_motion {
        if active {
            1.0
        } else {
            0.0
        }
    } else {
        ui.ctx()
            .animate_bool_with_time(Id::new(("tile", slot)), active, 0.08)
    };
    let painter = ui.painter();
    if t > 0.5 {
        neu::paint_inset(painter, rect, cr, pal, 0.6);
    } else {
        neu::paint_raised(painter, rect, cr, pal, 0.45 * (1.0 - t));
    }
    if active {
        painter.circle_filled(rect.right_top() + Vec2::new(-8.0, 8.0), 3.5, pal.accent);
    }
    let key_text = RichText::new(key.to_ascii_uppercase().to_string());
    let font = FontId::proportional(size * 0.32);
    painter.text(
        rect.center() - Vec2::new(0.0, size * 0.2),
        Align2::CENTER_CENTER,
        key_text.text(),
        font,
        pal.text,
    );
    let (degree, octave) = jianpu(slot);
    let num_pos = rect.center() + Vec2::new(0.0, size * 0.24);
    let deg_color = if active { pal.text } else { pal.text_secondary };
    painter.text(
        num_pos,
        Align2::CENTER_CENTER,
        degree.to_string(),
        FontId::proportional(size * 0.24),
        deg_color,
    );
    if octave != 0 {
        let dy = size * 0.16 * -(octave as f32);
        painter.circle_filled(num_pos + Vec2::new(0.0, dy), 1.6, deg_color);
    }
}

fn tracks_card(
    ui: &mut Ui,
    pal: &Palette,
    snap: &Snapshot,
    open: &mut bool,
    actions: &mut Vec<UiAction>,
) {
    neu::neu_card(ui, pal, |ui| {
        ui.horizontal(|ui| {
            let arrow = if *open { "Hide tracks" } else { "Show tracks" };
            if ui.add(NeuButton::new(arrow, *pal)).clicked() {
                *open = !*open;
            }
            let on = snap.tracks.iter().filter(|t| t.enabled).count();
            hint(ui, pal, &format!("{on}/{} enabled", snap.tracks.len()));
        });
        if !*open {
            return;
        }
        ui.add_space(10.0);
        for (i, t) in snap.tracks.iter().enumerate() {
            ui.horizontal(|ui| {
                let mut enabled = t.enabled;
                if neu::neu_toggle(ui, &mut enabled, pal, "").changed() {
                    actions.push(UiAction::Track(i, enabled));
                }
                ui.add_space(6.0);
                ui.add(egui::Label::new(RichText::new(&t.name).color(pal.text)).truncate());
                hint(ui, pal, &format!("{} notes", t.notes));
                if t.drum {
                    neu::neu_tag(ui, "drums", pal);
                }
            });
            ui.add_space(6.0);
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn library_row(
    ui: &mut Ui,
    pal: &Palette,
    idx: usize,
    title: &str,
    fav: bool,
    snap: &Snapshot,
    queue: &[usize],
    actions: &mut Vec<UiAction>,
) {
    let size = Vec2::new(ui.available_width(), ROW_HEIGHT - 4.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let star_rect = Rect::from_center_size(
        rect.right_center() - Vec2::new(22.0, 0.0),
        Vec2::splat(30.0),
    );
    let star = ui.interact(star_rect, Id::new(("star", idx)), Sense::click());
    if star.clicked() {
        actions.push(UiAction::ToggleFavorite(idx));
    } else if resp.clicked() {
        actions.push(UiAction::PlayEntry(idx, queue.to_vec()));
    }
    ui.add_space(4.0);
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter();
    let current = snap.playing_entry == Some(idx);
    let cr = CornerRadius::same(12);
    if current || resp.is_pointer_button_down_on() {
        neu::paint_inset(painter, rect, cr, pal, 0.4);
    } else if resp.hovered() {
        neu::paint_raised(painter, rect, cr, pal, 0.3);
    }
    if current {
        let bar = Rect::from_min_size(
            rect.min + Vec2::new(6.0, 9.0),
            Vec2::new(4.0, rect.height() - 18.0),
        );
        painter.rect_filled(bar, CornerRadius::same(2), pal.accent);
    }
    let mut text = RichText::new(title).color(pal.text).size(14.0);
    if current {
        text = text.strong();
    }
    let galley = egui::WidgetText::from(text).into_galley(
        ui,
        Some(egui::TextWrapMode::Truncate),
        rect.width() - 70.0,
        egui::TextStyle::Body,
    );
    painter.galley(
        Pos2::new(rect.left() + 18.0, rect.center().y - galley.size().y * 0.5),
        galley,
        pal.text,
    );
    let (glyph, color) = if fav {
        ("★", pal.accent)
    } else {
        ("☆", pal.text_secondary)
    };
    painter.text(
        star_rect.center(),
        Align2::CENTER_CENTER,
        glyph,
        FontId::proportional(17.0),
        color,
    );
    if resp.has_focus() || star.has_focus() {
        neu::paint_focus_ring(painter, rect, cr, pal);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_time_exact() {
        assert_eq!(format_time(0.0), "00:00");
        assert_eq!(format_time(59.9), "00:59");
        assert_eq!(format_time(60.0), "01:00");
        assert_eq!(format_time(3600.0), "60:00");
        assert_eq!(format_time(-5.0), "00:00");
        assert_eq!(format_time(f64::NAN), "00:00");
    }

    #[test]
    fn song_count_pluralises() {
        assert_eq!(song_count(0), "0 songs");
        assert_eq!(song_count(1), "1 song");
        assert_eq!(song_count(25), "25 songs");
    }

    #[test]
    fn jianpu_for_every_row() {
        assert_eq!(jianpu(0), (1, -1));
        assert_eq!(jianpu(6), (7, -1));
        assert_eq!(jianpu(7), (1, 0));
        assert_eq!(jianpu(13), (7, 0));
        assert_eq!(jianpu(14), (1, 1));
        assert_eq!(jianpu(20), (7, 1));
    }

    #[test]
    fn action_indices_are_unique() {
        let all = [
            Action::PlayPause,
            Action::Stop,
            Action::Next,
            Action::Prev,
            Action::ModeNext,
            Action::ModePrev,
        ];
        let mut seen = [false; 6];
        for a in all {
            let i = action_index(a);
            assert!(!seen[i], "{a:?} reuses index {i}");
            seen[i] = true;
        }
    }
}
