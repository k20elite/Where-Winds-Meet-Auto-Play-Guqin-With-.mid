//! Playback engine and player thread for Guqin MIDI playback.

use crate::mapping::{Mapper, Stroke};
use crate::midi::Song;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Output sink trait for keystroke events.
pub trait Out {
    /// Press a stroke.
    fn press(&mut self, s: Stroke);
    /// Release a stroke.
    fn release(&mut self, s: Stroke);
    /// Release all currently held strokes.
    fn release_all(&mut self);
}

impl Out for crate::input::Output {
    fn press(&mut self, s: Stroke) {
        self.press(s);
    }

    fn release(&mut self, s: Stroke) {
        self.release(s);
    }

    fn release_all(&mut self) {
        self.release_all();
    }
}

/// Playback state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PlayState {
    /// Idle state (nothing loaded or playback stopped).
    Idle = 0,
    /// Actively playing.
    Playing = 1,
    /// Playback paused.
    Paused = 2,
    /// Song finished and all releases sent.
    Finished = 3,
}

impl PlayState {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => PlayState::Playing,
            2 => PlayState::Paused,
            3 => PlayState::Finished,
            _ => PlayState::Idle,
        }
    }
}

/// Pure deterministic playback engine state machine.
pub struct Engine<O: Out> {
    out: O,
    state: PlayState,
    song: Option<Arc<Song>>,
    mapper: Mapper,
    track_enabled: Vec<bool>,
    cursor: usize,
    anchor_now: Duration,
    anchor_pos_us: u64,
    speed: f32,
    hold: Duration,
    pending_releases: VecDeque<(Duration, Stroke)>,
    slot_refcounts: [u32; 32],
}

impl<O: Out> Engine<O> {
    /// Create new engine with specified output backend.
    pub fn new(out: O) -> Self {
        Self {
            out,
            state: PlayState::Idle,
            song: None,
            mapper: Mapper {
                note_mode: crate::mapping::NoteMode::Nearest,
                key_mode: crate::mapping::KeyMode::Natural21,
                transpose: 0,
                octave: 0,
            },
            track_enabled: Vec::new(),
            cursor: 0,
            anchor_now: Duration::ZERO,
            anchor_pos_us: 0,
            speed: 1.0,
            hold: Duration::from_millis(25),
            pending_releases: VecDeque::with_capacity(64),
            slot_refcounts: [0; 32],
        }
    }

    /// Load a song. Releases all keys, resets position to 0, sets state to Idle.
    pub fn load(&mut self, song: Arc<Song>, mapper: Mapper, track_enabled: Vec<bool>) {
        self.out.release_all();
        self.pending_releases.clear();
        self.slot_refcounts.fill(0);
        self.song = Some(song);
        self.mapper = mapper;
        self.track_enabled = track_enabled;
        self.cursor = 0;
        self.anchor_now = Duration::ZERO;
        self.anchor_pos_us = 0;
        self.state = PlayState::Idle;
    }

    /// Start or resume playback.
    pub fn play(&mut self, now: Duration) {
        if self.song.is_none() {
            return;
        }

        match self.state {
            PlayState::Playing => {}
            PlayState::Paused => {
                self.anchor_now = now;
                self.state = PlayState::Playing;
            }
            PlayState::Finished => {
                self.seek(now, 0);
                self.anchor_now = now;
                self.state = PlayState::Playing;
            }
            PlayState::Idle => {
                self.anchor_now = now;
                self.state = PlayState::Playing;
            }
        }
    }

    /// Pause playback. Releases all pending keys immediately.
    pub fn pause(&mut self, now: Duration) {
        if self.state == PlayState::Playing {
            let current_pos = self.position_us(now);
            self.anchor_pos_us = current_pos;
            self.anchor_now = now;
            self.state = PlayState::Paused;
            self.out.release_all();
            self.pending_releases.clear();
            self.slot_refcounts.fill(0);
        }
    }

    /// Toggle playback between Playing and Paused/Idle.
    pub fn toggle(&mut self, now: Duration) {
        match self.state {
            PlayState::Playing => self.pause(now),
            PlayState::Paused | PlayState::Idle | PlayState::Finished => self.play(now),
        }
    }

    /// Stop playback. Releases all keys, resets position to 0, enters Idle state.
    pub fn stop(&mut self) {
        self.out.release_all();
        self.pending_releases.clear();
        self.slot_refcounts.fill(0);
        self.cursor = 0;
        self.anchor_now = Duration::ZERO;
        self.anchor_pos_us = 0;
        self.state = PlayState::Idle;
    }

    /// Seek to song position in microseconds. Clamped to song duration.
    pub fn seek(&mut self, now: Duration, pos_us: u64) {
        let Some(song) = &self.song else {
            self.anchor_pos_us = 0;
            self.anchor_now = now;
            return;
        };

        let clamped = pos_us.min(song.duration_us);
        self.out.release_all();
        self.pending_releases.clear();
        self.slot_refcounts.fill(0);

        self.anchor_pos_us = clamped;
        self.anchor_now = now;

        self.cursor = song.events.partition_point(|event| event.time_us < clamped);

        if self.state == PlayState::Finished {
            self.state = PlayState::Paused;
        }
    }

    /// Set playback speed multiplier (clamped 0.25..=2.0). Preserves position continuity.
    pub fn set_speed(&mut self, now: Duration, speed: f32) {
        let valid_speed = if speed.is_nan() {
            1.0
        } else {
            speed.clamp(0.25, 2.0)
        };

        let current_pos = self.position_us(now);
        self.anchor_pos_us = current_pos;
        self.anchor_now = now;
        self.speed = valid_speed;
    }

    /// Set mapper configuration for subsequent notes.
    pub fn set_mapper(&mut self, mapper: Mapper) {
        self.mapper = mapper;
    }

    /// Set per-track enable flags. Missing track flags treated as enabled.
    pub fn set_track_enabled(&mut self, track_enabled: Vec<bool>) {
        self.track_enabled = track_enabled;
    }

    /// Set key hold duration (clamped 5..=200 ms).
    pub fn set_hold(&mut self, hold: Duration) {
        let ms = hold.as_millis().clamp(5, 200) as u64;
        self.hold = Duration::from_millis(ms);
    }

    /// Calculate current song position in microseconds.
    pub fn position_us(&self, now: Duration) -> u64 {
        let Some(song) = &self.song else {
            return 0;
        };

        if self.state == PlayState::Playing {
            let elapsed_real_us = now.saturating_sub(self.anchor_now).as_micros() as f64;
            let song_advance_us = (elapsed_real_us * self.speed as f64).round() as u64;
            (self.anchor_pos_us + song_advance_us).min(song.duration_us)
        } else {
            self.anchor_pos_us.min(song.duration_us)
        }
    }

    /// Current playback state.
    pub fn state(&self) -> PlayState {
        self.state
    }

    /// Bitmask of currently pressed slots (0..=31, bit index = slot).
    pub fn active_slots(&self) -> u32 {
        let mut mask = 0u32;
        for (slot, &count) in self.slot_refcounts.iter().enumerate() {
            if count > 0 && slot < 32 {
                mask |= 1 << slot;
            }
        }
        mask
    }

    /// Playback speed multiplier.
    pub fn speed(&self) -> f32 {
        self.speed
    }

    /// Reference to output backend.
    pub fn out(&self) -> &O {
        &self.out
    }

    /// Mutable reference to output backend.
    pub fn out_mut(&mut self) -> &mut O {
        &mut self.out
    }

    /// Process events due up to `now` and return next wake deadline.
    pub fn tick(&mut self, now: Duration) -> Option<Duration> {
        // 1. Process pending releases due at or before `now`.
        while let Some(&(due, stroke)) = self.pending_releases.front() {
            if due <= now {
                self.pending_releases.pop_front();
                let slot = stroke.slot as usize;
                if slot < self.slot_refcounts.len() && self.slot_refcounts[slot] > 0 {
                    self.slot_refcounts[slot] -= 1;
                }
                self.out.release(stroke);
            } else {
                break;
            }
        }

        // 2. Process note events if playing.
        if self.state == PlayState::Playing {
            if let Some(song) = self.song.clone() {
                let song_pos_us = self.position_us(now);
                let late_threshold_us = song_pos_us.saturating_sub(250_000);

                let mut last_event_time_us = u64::MAX;
                // Fixed bitset for up to 21 slots * 3 modifiers = 63 bits (< 64).
                let mut chord_strokes = 0u64;

                while self.cursor < song.events.len() {
                    let event = song.events[self.cursor];
                    if event.time_us > song_pos_us {
                        break;
                    }

                    self.cursor += 1;

                    // Only Note-On events are played.
                    if !event.on {
                        continue;
                    }

                    // Check track enable flag (missing track treated as enabled).
                    let track_idx = event.track as usize;
                    if track_idx < self.track_enabled.len() && !self.track_enabled[track_idx] {
                        continue;
                    }

                    // Late note check (more than 250 ms song time behind).
                    if event.time_us < late_threshold_us {
                        continue;
                    }

                    // Map note.
                    if let Some(stroke) = self.mapper.map(event.note) {
                        if event.time_us != last_event_time_us {
                            last_event_time_us = event.time_us;
                            chord_strokes = 0;
                        }

                        let mod_idx = match stroke.modifier {
                            crate::mapping::Modifier::None => 0u64,
                            crate::mapping::Modifier::Shift => 1u64,
                            crate::mapping::Modifier::Ctrl => 2u64,
                        };
                        let stroke_key = (stroke.slot as u64) * 3 + mod_idx;
                        let stroke_bit = 1u64 << (stroke_key % 64);
                        if (chord_strokes & stroke_bit) != 0 {
                            // Duplicate identical stroke at same timestamp in chord -> deduplicate.
                            continue;
                        }
                        chord_strokes |= stroke_bit;

                        self.out.press(stroke);
                        let slot = stroke.slot as usize;
                        if slot < self.slot_refcounts.len() {
                            self.slot_refcounts[slot] += 1;
                        }
                        self.pending_releases.push_back((now + self.hold, stroke));
                    }
                }

                // Check if song reached end.
                if self.cursor >= song.events.len() && self.pending_releases.is_empty() {
                    self.anchor_pos_us = song.duration_us;
                    self.state = PlayState::Finished;
                }
            }
        }

        // 3. Compute next wake deadline.
        let mut next_deadline = self.pending_releases.front().map(|(due, _)| *due);

        if self.state == PlayState::Playing {
            if let Some(song) = &self.song {
                if self.cursor < song.events.len() {
                    let next_event_us = song.events[self.cursor].time_us;
                    let current_pos_us = self.position_us(now);
                    let delta_song_us = next_event_us.saturating_sub(current_pos_us);
                    let delta_real_us = (delta_song_us as f64 / self.speed as f64).ceil() as u64;
                    let event_deadline = now + Duration::from_micros(delta_real_us);

                    next_deadline = match next_deadline {
                        Some(d) => Some(d.min(event_deadline)),
                        None => Some(event_deadline),
                    };
                }
            }
        }

        next_deadline
    }
}

/// Atomic status report shared between player thread and UI/callers.
pub struct SharedStatus {
    /// PlayState as u8.
    pub state: AtomicU8,
    /// Playback position in microseconds.
    pub position_us: AtomicU64,
    /// Loaded song duration in microseconds.
    pub duration_us: AtomicU64,
    /// Active slot bitmask.
    pub active_slots: AtomicU32,
    /// Playback speed * 100 as integer.
    pub speed_x100: AtomicU32,
}

impl SharedStatus {
    fn new() -> Self {
        Self {
            state: AtomicU8::new(PlayState::Idle as u8),
            position_us: AtomicU64::new(0),
            duration_us: AtomicU64::new(0),
            active_slots: AtomicU32::new(0),
            speed_x100: AtomicU32::new(100),
        }
    }

    /// Load playback state.
    pub fn state(&self) -> PlayState {
        PlayState::from_u8(self.state.load(Ordering::Relaxed))
    }
}

/// Commands sent to background player thread.
pub enum Cmd<O: Out = crate::input::Output> {
    /// Load song and reset engine.
    Load(Arc<Song>, Mapper, Vec<bool>),
    /// Start or resume playback.
    Play,
    /// Pause playback.
    Pause,
    /// Toggle play/pause.
    Toggle,
    /// Stop playback.
    Stop,
    /// Seek to position in microseconds.
    Seek(u64),
    /// Set speed multiplier.
    Speed(f32),
    /// Set note mapper.
    Mapper(Mapper),
    /// Set track enabled flags.
    Tracks(Vec<bool>),
    /// Set key hold duration in milliseconds.
    Hold(u16),
    /// Swap output sink.
    Output(O),
    /// Terminate player thread.
    Shutdown,
}

/// Events emitted by player to UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerEvent {
    /// Playback state changed.
    StateChanged(PlayState),
    /// Playback reached end of song.
    Finished,
}

/// High-level background playback controller.
pub struct GenericPlayer<O: Out + Send + 'static> {
    tx: Sender<Cmd<O>>,
    status: Arc<SharedStatus>,
    thread: Option<JoinHandle<()>>,
}

/// Standard player with concrete Output sink.
pub type Player = GenericPlayer<crate::input::Output>;

impl<O: Out + Send + 'static> GenericPlayer<O> {
    /// Spawn playback thread with output sink and UI event notification callback.
    pub fn spawn(out: O, notify: impl Fn(PlayerEvent) + Send + 'static) -> Self {
        let (tx, rx) = channel();
        let status = Arc::new(SharedStatus::new());
        let thread_status = Arc::clone(&status);

        let thread = thread::spawn(move || {
            run_player_thread(out, rx, thread_status, notify);
        });

        Self {
            tx,
            status,
            thread: Some(thread),
        }
    }

    /// Send command to player thread.
    pub fn send(&self, cmd: Cmd<O>) {
        let _ = self.tx.send(cmd);
    }

    /// Reference to shared atomic status.
    pub fn status(&self) -> &Arc<SharedStatus> {
        &self.status
    }
}

impl<O: Out + Send + 'static> Drop for GenericPlayer<O> {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Shutdown);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

fn run_player_thread<O: Out + Send + 'static>(
    out: O,
    rx: Receiver<Cmd<O>>,
    status: Arc<SharedStatus>,
    notify: impl Fn(PlayerEvent) + Send + 'static,
) {
    let start_time = Instant::now();
    let mut engine = Engine::new(out);
    let mut high_res_timer_active = false;
    let mut last_reported_state = engine.state();

    let spin_threshold = Duration::from_millis(1);

    publish_status(&engine, &status, start_time.elapsed());

    'main_loop: loop {
        let now = start_time.elapsed();
        let deadline = engine.tick(now);

        // Update high-precision Windows multimedia timer period.
        let is_playing = engine.state() == PlayState::Playing;
        if is_playing != high_res_timer_active {
            if is_playing {
                // SAFETY: timeBeginPeriod requests 1 ms scheduler resolution.
                unsafe {
                    windows_sys::Win32::Media::timeBeginPeriod(1);
                }
                high_res_timer_active = true;
            } else {
                // SAFETY: timeEndPeriod balances previous timeBeginPeriod(1).
                unsafe {
                    windows_sys::Win32::Media::timeEndPeriod(1);
                }
                high_res_timer_active = false;
            }
        }

        let curr_state = engine.state();
        if curr_state != last_reported_state {
            last_reported_state = curr_state;
            notify(PlayerEvent::StateChanged(curr_state));
            if curr_state == PlayState::Finished {
                notify(PlayerEvent::Finished);
            }
        }

        publish_status(&engine, &status, now);

        // Wait or receive commands.
        match deadline {
            None => match rx.recv() {
                Ok(cmd) => {
                    if handle_cmd(&mut engine, cmd, &start_time) {
                        break 'main_loop;
                    }
                }
                Err(_) => break 'main_loop,
            },
            Some(target_deadline) => {
                let now_before_wait = start_time.elapsed();
                if target_deadline > now_before_wait {
                    let wait_rem = target_deadline - now_before_wait;
                    if wait_rem > spin_threshold {
                        let coarse_wait = wait_rem - spin_threshold;
                        match rx.recv_timeout(coarse_wait) {
                            Ok(cmd) => {
                                if handle_cmd(&mut engine, cmd, &start_time) {
                                    break 'main_loop;
                                }
                                continue 'main_loop;
                            }
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                                break 'main_loop;
                            }
                        }
                    }

                    // Precise spin wait for the last ~1 ms.
                    while start_time.elapsed() < target_deadline {
                        std::hint::spin_loop();
                        thread::yield_now();
                    }
                }

                // Check non-blocking incoming commands.
                while let Ok(cmd) = rx.try_recv() {
                    if handle_cmd(&mut engine, cmd, &start_time) {
                        break 'main_loop;
                    }
                }
            }
        }
    }

    // Clean exit
    if high_res_timer_active {
        // SAFETY: timeEndPeriod balances previous timeBeginPeriod(1) on thread exit.
        unsafe {
            windows_sys::Win32::Media::timeEndPeriod(1);
        }
    }
    engine.out_mut().release_all();
}

fn handle_cmd<O: Out>(engine: &mut Engine<O>, cmd: Cmd<O>, start_time: &Instant) -> bool {
    let now = start_time.elapsed();
    match cmd {
        Cmd::Load(song, mapper, tracks) => {
            engine.load(song, mapper, tracks);
        }
        Cmd::Play => {
            engine.play(now);
        }
        Cmd::Pause => {
            engine.pause(now);
        }
        Cmd::Toggle => {
            engine.toggle(now);
        }
        Cmd::Stop => {
            engine.stop();
        }
        Cmd::Seek(pos) => {
            engine.seek(now, pos);
        }
        Cmd::Speed(s) => {
            engine.set_speed(now, s);
        }
        Cmd::Mapper(m) => {
            engine.set_mapper(m);
        }
        Cmd::Tracks(t) => {
            engine.set_track_enabled(t);
        }
        Cmd::Hold(h) => {
            engine.set_hold(Duration::from_millis(h as u64));
        }
        Cmd::Output(mut new_out) => {
            engine.out_mut().release_all();
            std::mem::swap(engine.out_mut(), &mut new_out);
        }
        Cmd::Shutdown => {
            return true;
        }
    }
    false
}

fn publish_status<O: Out>(engine: &Engine<O>, status: &Arc<SharedStatus>, now: Duration) {
    status.state.store(engine.state() as u8, Ordering::Relaxed);
    status
        .position_us
        .store(engine.position_us(now), Ordering::Relaxed);
    let dur = engine.song.as_ref().map_or(0, |s| s.duration_us);
    status.duration_us.store(dur, Ordering::Relaxed);
    status
        .active_slots
        .store(engine.active_slots(), Ordering::Relaxed);
    status
        .speed_x100
        .store((engine.speed() * 100.0).round() as u32, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapping::{KeyMode, Modifier, NoteMode};
    use crate::midi::{NoteEvent, TrackInfo};

    #[derive(Default)]
    struct MockOut {
        events: Vec<(u64, &'static str, Stroke)>,
        current_time_ms: u64,
    }

    impl Out for MockOut {
        fn press(&mut self, s: Stroke) {
            self.events.push((self.current_time_ms, "press", s));
        }

        fn release(&mut self, s: Stroke) {
            self.events.push((self.current_time_ms, "release", s));
        }

        fn release_all(&mut self) {
            self.events.push((
                self.current_time_ms,
                "release_all",
                Stroke {
                    slot: 255,
                    modifier: Modifier::None,
                },
            ));
        }
    }

    fn make_test_song(events: Vec<NoteEvent>, duration_us: u64) -> Arc<Song> {
        Arc::new(Song {
            events,
            tracks: vec![TrackInfo {
                index: 0,
                name: "Track 1".to_string(),
                note_count: 1,
                is_drum: false,
            }],
            duration_us,
        })
    }

    fn default_mapper() -> Mapper {
        Mapper {
            note_mode: NoteMode::Nearest,
            key_mode: KeyMode::Natural21,
            transpose: 0,
            octave: 0,
        }
    }

    #[test]
    fn test_exact_timing_speed_1_0() {
        let mock = MockOut::default();
        let song = make_test_song(
            vec![
                NoteEvent {
                    time_us: 0,
                    note: 60,
                    on: true,
                    track: 0,
                    channel: 0,
                },
                NoteEvent {
                    time_us: 100_000,
                    note: 62,
                    on: true,
                    track: 0,
                    channel: 0,
                },
                NoteEvent {
                    time_us: 250_000,
                    note: 64,
                    on: true,
                    track: 0,
                    channel: 0,
                },
            ],
            300_000,
        );

        let mut engine = Engine::new(mock);
        engine.load(song, default_mapper(), vec![true]);
        engine.play(Duration::ZERO);

        let mut now = Duration::ZERO;
        while let Some(next) = engine.tick(now) {
            now = next;
            engine.out_mut().current_time_ms = now.as_millis() as u64;
        }

        let events = &engine.out().events;
        // Press 60 at 0, rel at 25; press 62 at 100, rel at 125; press 64 at 250, rel at 275
        assert_eq!(
            events,
            &[
                (
                    0,
                    "release_all",
                    Stroke {
                        slot: 255,
                        modifier: Modifier::None
                    }
                ), // from load
                (
                    0,
                    "press",
                    Stroke {
                        slot: 7,
                        modifier: Modifier::None
                    }
                ),
                (
                    25,
                    "release",
                    Stroke {
                        slot: 7,
                        modifier: Modifier::None
                    }
                ),
                (
                    100,
                    "press",
                    Stroke {
                        slot: 8,
                        modifier: Modifier::None
                    }
                ),
                (
                    125,
                    "release",
                    Stroke {
                        slot: 8,
                        modifier: Modifier::None
                    }
                ),
                (
                    250,
                    "press",
                    Stroke {
                        slot: 9,
                        modifier: Modifier::None
                    }
                ),
                (
                    275,
                    "release",
                    Stroke {
                        slot: 9,
                        modifier: Modifier::None
                    }
                ),
            ]
        );
        assert_eq!(engine.state(), PlayState::Finished);
    }

    #[test]
    fn test_speed_2_0_and_continuity() {
        let mock = MockOut::default();
        let song = make_test_song(
            vec![
                NoteEvent {
                    time_us: 0,
                    note: 60,
                    on: true,
                    track: 0,
                    channel: 0,
                },
                NoteEvent {
                    time_us: 100_000,
                    note: 62,
                    on: true,
                    track: 0,
                    channel: 0,
                },
                NoteEvent {
                    time_us: 250_000,
                    note: 64,
                    on: true,
                    track: 0,
                    channel: 0,
                },
            ],
            300_000,
        );

        let mut engine = Engine::new(mock);
        engine.load(song, default_mapper(), vec![true]);
        engine.set_speed(Duration::ZERO, 2.0);
        engine.play(Duration::ZERO);

        let mut now = Duration::ZERO;
        while let Some(next) = engine.tick(now) {
            now = next;
            engine.out_mut().current_time_ms = now.as_millis() as u64;
        }

        let events = &engine.out().events;
        // At speed 2.0: 100ms song -> 50ms real; 250ms song -> 125ms real
        assert_eq!(
            events,
            &[
                (
                    0,
                    "release_all",
                    Stroke {
                        slot: 255,
                        modifier: Modifier::None
                    }
                ),
                (
                    0,
                    "press",
                    Stroke {
                        slot: 7,
                        modifier: Modifier::None
                    }
                ),
                (
                    25,
                    "release",
                    Stroke {
                        slot: 7,
                        modifier: Modifier::None
                    }
                ),
                (
                    50,
                    "press",
                    Stroke {
                        slot: 8,
                        modifier: Modifier::None
                    }
                ),
                (
                    75,
                    "release",
                    Stroke {
                        slot: 8,
                        modifier: Modifier::None
                    }
                ),
                (
                    125,
                    "press",
                    Stroke {
                        slot: 9,
                        modifier: Modifier::None
                    }
                ),
                (
                    150,
                    "release",
                    Stroke {
                        slot: 9,
                        modifier: Modifier::None
                    }
                ),
            ]
        );

        // Test position continuity on mid-song speed change
        let mut engine2 = Engine::new(MockOut::default());
        let song2 = make_test_song(vec![], 500_000);
        engine2.load(song2, default_mapper(), vec![true]);
        engine2.play(Duration::from_millis(100));
        let check_time = Duration::from_millis(200);
        let pos_before = engine2.position_us(check_time);
        engine2.set_speed(check_time, 1.5);
        let pos_after = engine2.position_us(check_time);
        assert_eq!(pos_before, pos_after);
    }

    #[test]
    fn test_pause_and_resume() {
        let mock = MockOut::default();
        let song = make_test_song(
            vec![
                NoteEvent {
                    time_us: 0,
                    note: 60,
                    on: true,
                    track: 0,
                    channel: 0,
                },
                NoteEvent {
                    time_us: 250_000,
                    note: 64,
                    on: true,
                    track: 0,
                    channel: 0,
                },
            ],
            300_000,
        );

        let mut engine = Engine::new(mock);
        engine.load(song, default_mapper(), vec![true]);
        engine.play(Duration::ZERO);

        let _ = engine.tick(Duration::ZERO);
        let _ = engine.tick(Duration::from_millis(25)); // release note 60

        // Pause at 120 ms
        engine.pause(Duration::from_millis(120));
        assert_eq!(engine.state(), PlayState::Paused);

        // Resume at 1120 ms -> note at 250 ms song time pressed at 1250 ms real time
        engine.play(Duration::from_millis(1120));
        let next_wake = engine.tick(Duration::from_millis(1120));
        assert_eq!(next_wake, Some(Duration::from_millis(1250)));

        engine.out_mut().current_time_ms = 1250;
        let _ = engine.tick(Duration::from_millis(1250));
        let last_event = engine.out().events.last().copied();
        assert_eq!(
            last_event,
            Some((
                1250,
                "press",
                Stroke {
                    slot: 9,
                    modifier: Modifier::None
                }
            ))
        );
    }

    #[test]
    fn test_seek_while_playing_and_clamping() {
        let mock = MockOut::default();
        let song = make_test_song(
            vec![
                NoteEvent {
                    time_us: 100_000,
                    note: 60,
                    on: true,
                    track: 0,
                    channel: 0,
                },
                NoteEvent {
                    time_us: 250_000,
                    note: 62,
                    on: true,
                    track: 0,
                    channel: 0,
                },
            ],
            300_000,
        );

        let mut engine = Engine::new(mock);
        engine.load(song, default_mapper(), vec![true]);
        engine.play(Duration::ZERO);

        // Seek to 200 ms at now = 50 ms
        engine.seek(Duration::from_millis(50), 200_000);
        assert_eq!(engine.state(), PlayState::Playing);

        // Next note at 250 ms song time should trigger 50 ms later (at now = 100 ms)
        let next = engine.tick(Duration::from_millis(50));
        assert_eq!(next, Some(Duration::from_millis(100)));

        engine.out_mut().current_time_ms = 100;
        let _ = engine.tick(Duration::from_millis(100));
        let pressed_slots: Vec<u8> = engine
            .out()
            .events
            .iter()
            .filter(|(_, kind, _)| *kind == "press")
            .map(|(_, _, s)| s.slot)
            .collect();
        // 60 should be skipped; only 62 (slot 8) is pressed
        assert_eq!(pressed_slots, vec![8]);

        // Seek beyond duration clamps
        engine.seek(Duration::from_millis(100), 999_999);
        assert_eq!(engine.position_us(Duration::from_millis(100)), 300_000);

        // Seek while paused stays paused
        engine.pause(Duration::from_millis(150));
        engine.seek(Duration::from_millis(150), 50_000);
        assert_eq!(engine.state(), PlayState::Paused);
    }

    #[test]
    fn test_late_notes_skip() {
        let mock = MockOut::default();
        let song = make_test_song(
            vec![
                NoteEvent {
                    time_us: 0,
                    note: 60,
                    on: true,
                    track: 0,
                    channel: 0,
                },
                NoteEvent {
                    time_us: 100_000,
                    note: 62,
                    on: true,
                    track: 0,
                    channel: 0,
                },
                NoteEvent {
                    time_us: 250_000,
                    note: 64,
                    on: true,
                    track: 0,
                    channel: 0,
                },
            ],
            500_000,
        );

        let mut engine = Engine::new(mock);
        engine.load(song, default_mapper(), vec![true]);
        engine.play(Duration::ZERO);

        // First tick called at 400 ms:
        // Notes at 0 and 100 ms are > 250 ms late -> skipped.
        // Note at 250 ms is 150 ms late -> played.
        engine.out_mut().current_time_ms = 400;
        let _ = engine.tick(Duration::from_millis(400));

        let presses: Vec<u8> = engine
            .out()
            .events
            .iter()
            .filter(|(_, kind, _)| *kind == "press")
            .map(|(_, _, s)| s.slot)
            .collect();
        assert_eq!(presses, vec![9]); // only note 64 (slot 9)
    }

    #[test]
    fn test_track_enables_and_mapper_change() {
        let mock = MockOut::default();
        let song = Arc::new(Song {
            events: vec![
                NoteEvent {
                    time_us: 0,
                    note: 60,
                    on: true,
                    track: 0,
                    channel: 0,
                },
                NoteEvent {
                    time_us: 50_000,
                    note: 62,
                    on: true,
                    track: 1,
                    channel: 0,
                },
                NoteEvent {
                    time_us: 100_000,
                    note: 64,
                    on: true,
                    track: 2,
                    channel: 0,
                },
            ],
            tracks: vec![
                TrackInfo {
                    index: 0,
                    name: "T0".to_string(),
                    note_count: 1,
                    is_drum: false,
                },
                TrackInfo {
                    index: 1,
                    name: "T1".to_string(),
                    note_count: 1,
                    is_drum: false,
                },
                TrackInfo {
                    index: 2,
                    name: "T2".to_string(),
                    note_count: 1,
                    is_drum: false,
                },
            ],
            duration_us: 200_000,
        });

        let mut engine = Engine::new(mock);
        // Track 1 disabled, track 2 missing in vector -> defaults to enabled!
        engine.load(song, default_mapper(), vec![true, false]);
        engine.play(Duration::ZERO);

        let _ = engine.tick(Duration::ZERO); // Plays note 60 (track 0)
        let _ = engine.tick(Duration::from_millis(50)); // Track 1 skipped!

        // Change mapper mid-song: transpose +1
        let mut new_mapper = default_mapper();
        new_mapper.transpose = 1;
        engine.set_mapper(new_mapper);

        let _ = engine.tick(Duration::from_millis(100)); // Track 2 plays with new mapper!

        let presses: Vec<u8> = engine
            .out()
            .events
            .iter()
            .filter(|(_, kind, _)| *kind == "press")
            .map(|(_, _, s)| s.slot)
            .collect();
        // 60 -> slot 7; 64 transposed by +1 = 65 (F4) -> slot 10
        assert_eq!(presses, vec![7, 10]);
    }

    #[test]
    fn test_chord_deduplication() {
        let mock = MockOut::default();
        // Chord with two notes mapping to same slot and one different note
        let song = make_test_song(
            vec![
                NoteEvent {
                    time_us: 0,
                    note: 60,
                    on: true,
                    track: 0,
                    channel: 0,
                },
                NoteEvent {
                    time_us: 0,
                    note: 60,
                    on: true,
                    track: 0,
                    channel: 0,
                },
                NoteEvent {
                    time_us: 0,
                    note: 64,
                    on: true,
                    track: 0,
                    channel: 0,
                },
            ],
            100_000,
        );

        let mut engine = Engine::new(mock);
        engine.load(song, default_mapper(), vec![true]);
        engine.play(Duration::ZERO);
        let _ = engine.tick(Duration::ZERO);

        let presses: Vec<u8> = engine
            .out()
            .events
            .iter()
            .filter(|(_, kind, _)| *kind == "press")
            .map(|(_, _, s)| s.slot)
            .collect();
        assert_eq!(presses, vec![7, 9]); // 60 pressed once, 64 pressed once
    }

    #[test]
    fn test_chromatic36_chord_deduplication() {
        let mock = MockOut::default();
        let mapper = Mapper {
            note_mode: NoteMode::Nearest,
            key_mode: KeyMode::Chromatic36,
            transpose: 0,
            octave: 0,
        };

        // Notes 60 (slot 7, None), 61 (slot 7, Shift), and 72-12=60 duplicate at t=0
        let song = make_test_song(
            vec![
                NoteEvent {
                    time_us: 0,
                    note: 60,
                    on: true,
                    track: 0,
                    channel: 0,
                },
                NoteEvent {
                    time_us: 0,
                    note: 61,
                    on: true,
                    track: 0,
                    channel: 0,
                },
                NoteEvent {
                    time_us: 0,
                    note: 60,
                    on: true,
                    track: 0,
                    channel: 0,
                },
            ],
            100_000,
        );

        let mut engine = Engine::new(mock);
        engine.load(song, mapper, vec![true]);
        engine.play(Duration::ZERO);
        let _ = engine.tick(Duration::ZERO);

        let presses: Vec<Stroke> = engine
            .out()
            .events
            .iter()
            .filter(|(_, kind, _)| *kind == "press")
            .map(|(_, _, s)| *s)
            .collect();

        assert_eq!(
            presses,
            vec![
                Stroke {
                    slot: 7,
                    modifier: Modifier::None
                },
                Stroke {
                    slot: 7,
                    modifier: Modifier::Shift
                },
            ]
        );
    }

    #[test]
    fn test_track_enables_empty_and_short_vec() {
        // 2-track song
        let song = Arc::new(Song {
            events: vec![
                NoteEvent {
                    time_us: 0,
                    note: 60,
                    on: true,
                    track: 0,
                    channel: 0,
                },
                NoteEvent {
                    time_us: 0,
                    note: 64,
                    on: true,
                    track: 1,
                    channel: 0,
                },
            ],
            tracks: vec![
                TrackInfo {
                    index: 0,
                    name: "T0".to_string(),
                    note_count: 1,
                    is_drum: false,
                },
                TrackInfo {
                    index: 1,
                    name: "T1".to_string(),
                    note_count: 1,
                    is_drum: false,
                },
            ],
            duration_us: 100_000,
        });

        // Case 1: vec![] -> all tracks enabled
        let mut engine = Engine::new(MockOut::default());
        engine.load(Arc::clone(&song), default_mapper(), vec![]);
        engine.play(Duration::ZERO);
        let _ = engine.tick(Duration::ZERO);

        let presses1: Vec<u8> = engine
            .out()
            .events
            .iter()
            .filter(|(_, kind, _)| *kind == "press")
            .map(|(_, _, s)| s.slot)
            .collect();
        assert_eq!(presses1, vec![7, 9]);

        // Case 2: vec![false] -> track 0 skipped, track 1 (missing index) plays
        let mut engine2 = Engine::new(MockOut::default());
        engine2.load(song, default_mapper(), vec![false]);
        engine2.play(Duration::ZERO);
        let _ = engine2.tick(Duration::ZERO);

        let presses2: Vec<u8> = engine2
            .out()
            .events
            .iter()
            .filter(|(_, kind, _)| *kind == "press")
            .map(|(_, _, s)| s.slot)
            .collect();
        assert_eq!(presses2, vec![9]);
    }

    #[test]
    fn test_same_slot_repressed_policy() {
        // Policy:
        // Output counts down_counts per virtual key / slot.
        // When slot is re-pressed before release:
        // Engine sends press immediately, slot_refcount increments,
        // and pending_releases queues a new release at due time.
        // Active slots bitmask stays 1 as long as refcount > 0.
        let mock = MockOut::default();
        let song = make_test_song(
            vec![
                NoteEvent {
                    time_us: 0,
                    note: 60,
                    on: true,
                    track: 0,
                    channel: 0,
                },
                NoteEvent {
                    time_us: 10_000,
                    note: 60,
                    on: true,
                    track: 0,
                    channel: 0,
                },
            ],
            50_000,
        );

        let mut engine = Engine::new(mock);
        engine.load(song, default_mapper(), vec![true]);
        engine.set_hold(Duration::from_millis(25));
        engine.play(Duration::ZERO);

        engine.out_mut().current_time_ms = 0;
        let _ = engine.tick(Duration::ZERO);
        assert_eq!(engine.active_slots(), 1 << 7);

        engine.out_mut().current_time_ms = 10;
        let _ = engine.tick(Duration::from_millis(10));
        assert_eq!(engine.active_slots(), 1 << 7);

        // At 25ms: first release fires, but second press holds until 35ms
        engine.out_mut().current_time_ms = 25;
        let _ = engine.tick(Duration::from_millis(25));
        assert_eq!(engine.active_slots(), 1 << 7);

        // At 35ms: second release fires, slot bit cleared
        engine.out_mut().current_time_ms = 35;
        let _ = engine.tick(Duration::from_millis(35));
        assert_eq!(engine.active_slots(), 0);

        let events: Vec<(u64, &str, u8)> = engine
            .out()
            .events
            .iter()
            .filter(|(_, k, _)| *k != "release_all")
            .map(|(t, k, s)| (*t, *k, s.slot))
            .collect();

        assert_eq!(
            events,
            vec![
                (0, "press", 7),
                (10, "press", 7),
                (25, "release", 7),
                (35, "release", 7),
            ]
        );
    }

    #[test]
    fn test_stop_and_finished_states() {
        let mock = MockOut::default();
        let song = make_test_song(
            vec![NoteEvent {
                time_us: 0,
                note: 60,
                on: true,
                track: 0,
                channel: 0,
            }],
            50_000,
        );

        let mut engine = Engine::new(mock);
        engine.load(song, default_mapper(), vec![true]);
        engine.play(Duration::ZERO);

        let _ = engine.tick(Duration::ZERO);
        let next = engine.tick(Duration::from_millis(25));
        assert_eq!(engine.state(), PlayState::Finished);
        assert_eq!(next, None);
        assert_eq!(engine.position_us(Duration::from_millis(25)), 50_000);
        assert_eq!(engine.position_us(Duration::from_secs(10)), 50_000);

        // play() from Finished restarts at 0
        engine.play(Duration::from_millis(50));
        assert_eq!(engine.state(), PlayState::Playing);
        assert_eq!(engine.position_us(Duration::from_millis(50)), 0);

        // stop() returns to Idle and position 0
        engine.stop();
        assert_eq!(engine.state(), PlayState::Idle);
        assert_eq!(engine.position_us(Duration::from_millis(100)), 0);
        assert_eq!(engine.tick(Duration::from_millis(100)), None);
    }

    #[test]
    fn test_player_thread_smoke() {
        let (notify_tx, notify_rx) = channel();
        let mock = MockOut::default();

        let song = make_test_song(
            vec![
                NoteEvent {
                    time_us: 0,
                    note: 60,
                    on: true,
                    track: 0,
                    channel: 0,
                },
                NoteEvent {
                    time_us: 10_000,
                    note: 62,
                    on: true,
                    track: 0,
                    channel: 0,
                },
                NoteEvent {
                    time_us: 20_000,
                    note: 64,
                    on: true,
                    track: 0,
                    channel: 0,
                },
            ],
            30_000,
        );

        let player = GenericPlayer::spawn(mock, move |ev| {
            let _ = notify_tx.send(ev);
        });

        player.send(Cmd::Hold(10));
        player.send(Cmd::Load(song, default_mapper(), vec![true]));
        player.send(Cmd::Play);

        let start = Instant::now();
        let mut got_finished = false;
        while start.elapsed() < Duration::from_secs(1) {
            if let Ok(ev) = notify_rx.recv_timeout(Duration::from_millis(100)) {
                if ev == PlayerEvent::Finished {
                    got_finished = true;
                    break;
                }
            }
        }

        assert!(
            got_finished,
            "smoke test must finish playback within 1 second"
        );
        drop(player);
    }
}
