//! Application controller owning state and bridging hotkeys/UI to playback engine.

use crate::hotkeys::Action;
use crate::library::{self, Entry, Queue, Rng};
use crate::mapping::{self, KeyMode, Mapper, NoteMode};
use crate::midi::{self, Song};
use crate::player::{Cmd, GenericPlayer, Out, PlayerEvent, SharedStatus};
use crate::settings::{Repeat, Settings};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Position threshold (3 seconds) for restarting vs previous track.
pub const PREV_RESTART_THRESHOLD_US: u64 = 3_000_000;

/// Currently loaded and active playback item.
pub struct NowPlaying {
    /// Index into Controller's library entries; None once the file left the library.
    pub entry: Option<usize>,
    /// File path, used to re-locate the entry after a rescan.
    pub path: PathBuf,
    /// Song title.
    pub title: String,
    /// Parsed MIDI song structure.
    pub song: Arc<Song>,
    /// Recommended auto-transposition shift for enabled tracks.
    pub auto_shift: i8,
    /// Per-track enabled flags.
    pub tracks: Vec<bool>,
}

/// Central application controller.
pub struct Controller<O: Out + Send + 'static> {
    settings: Settings,
    settings_path: PathBuf,
    dirty: bool,
    entries: Vec<Entry>,
    fav_mask: Vec<bool>,
    revision: u64,
    queue: Queue,
    queue_items: Vec<usize>,
    rng: Rng,
    now_playing: Option<NowPlaying>,
    player: GenericPlayer<O>,
    notice: Option<String>,
    make_output: Box<dyn Fn(&Settings) -> O + Send>,
}

/// Shared handle for controller across UI and hotkey threads.
pub type Shared<O> = Arc<Mutex<Controller<O>>>;

impl<O: Out + Send + 'static> Controller<O> {
    /// Create new controller and spawn player thread.
    pub fn new(
        mut settings: Settings,
        settings_path: PathBuf,
        make_output: impl Fn(&Settings) -> O + Send + 'static,
        notify: impl Fn(PlayerEvent) + Send + 'static,
    ) -> Self {
        settings.sanitize();
        let out = make_output(&settings);
        let player = GenericPlayer::spawn(out, notify);
        player.send(Cmd::Hold(settings.key_hold_ms));
        player.send(Cmd::Speed(settings.speed));

        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x853c49e6748fea9b);

        let mut controller = Self {
            settings,
            settings_path,
            dirty: false,
            entries: Vec::new(),
            fav_mask: Vec::new(),
            revision: 0,
            queue: Queue::new(),
            queue_items: Vec::new(),
            rng: Rng::new(seed),
            now_playing: None,
            player,
            notice: None,
            make_output: Box::new(make_output),
        };

        let _ = controller.rescan();
        controller
    }

    fn rebuild_fav_mask(&mut self) {
        let fav_set: HashSet<&str> = self.settings.favorites.iter().map(|f| f.as_str()).collect();
        self.fav_mask = self
            .entries
            .iter()
            .map(|e| fav_set.contains(e.rel.as_str()))
            .collect();
        self.revision = self.revision.wrapping_add(1);
    }

    /// Counter bumped whenever entries, favorites, or the queue change (for UI caches).
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Rescan current library directory.
    pub fn rescan(&mut self) -> Result<usize, String> {
        let Some(ref dir) = self.settings.library_dir else {
            return Err("choose a MIDI folder first".to_string());
        };

        match library::scan(dir) {
            Ok(scanned) => {
                let count = scanned.len();
                let old = std::mem::replace(&mut self.entries, scanned);
                self.remap_indices(&old);
                self.rebuild_fav_mask();
                Ok(count)
            }
            Err(e) => Err(format!("failed to scan library: {e}")),
        }
    }

    /// Re-point now-playing and queue indices from `old` entries to the rescanned ones by path.
    fn remap_indices(&mut self, old: &[Entry]) {
        let by_path: HashMap<&Path, usize> = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.path.as_path(), i))
            .collect();

        if let Some(np) = &mut self.now_playing {
            np.entry = by_path.get(np.path.as_path()).copied();
        }

        let old_pos = self.queue.position();
        let mut new_pos = None;
        let mut kept_before = 0usize;
        let mut items = Vec::with_capacity(self.queue_items.len());
        for (pos, &idx) in self.queue_items.iter().enumerate() {
            let Some(&new_idx) = old.get(idx).and_then(|e| by_path.get(e.path.as_path())) else {
                continue;
            };
            if Some(pos) == old_pos {
                new_pos = Some(items.len());
            } else if old_pos.is_some_and(|p| pos < p) {
                kept_before += 1;
            }
            items.push(new_idx);
        }
        // Current item gone: park on the item before it so next() continues in order.
        let start = new_pos.unwrap_or(kept_before.saturating_sub(1));
        self.queue.set(items.clone(), start);
        self.queue_items = items;
    }

    /// Set new library directory, mark dirty, clear queue, and rescan.
    pub fn set_library_dir(&mut self, dir: PathBuf) {
        self.settings.library_dir = Some(dir);
        self.dirty = true;
        self.queue.clear();
        self.queue_items.clear();
        self.revision = self.revision.wrapping_add(1);
        let _ = self.rescan();
    }

    /// Borrow scanned library entries.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Check if entry at index is in favorites.
    pub fn is_favorite(&self, idx: usize) -> bool {
        self.fav_mask.get(idx).copied().unwrap_or(false)
    }

    /// Return indices of all favorite library entries.
    pub fn favorite_indices(&self) -> Vec<usize> {
        self.fav_mask
            .iter()
            .enumerate()
            .filter_map(|(idx, &fav)| if fav { Some(idx) } else { None })
            .collect()
    }

    /// Toggle favorite status of entry at index.
    pub fn toggle_favorite(&mut self, idx: usize) {
        let Some(entry) = self.entries.get(idx) else {
            return;
        };
        let rel = entry.rel.clone();
        library::toggle_favorite(&mut self.settings.favorites, &rel);
        self.dirty = true;
        self.rebuild_fav_mask();
    }

    /// Play an entry with the provided queue items.
    pub fn play_entry(&mut self, idx: usize, mut queue_items: Vec<usize>) {
        if self.entries.is_empty() {
            return;
        }

        if !queue_items.contains(&idx) {
            queue_items = vec![idx];
        }

        let start_pos = queue_items
            .iter()
            .position(|&item| item == idx)
            .unwrap_or(0);
        self.queue.set(queue_items.clone(), start_pos);
        self.queue_items = queue_items;
        self.revision = self.revision.wrapping_add(1);

        self.load_and_play_current();
    }

    fn compute_auto_shift(song: &Song, tracks: &[bool], key_mode: KeyMode) -> i8 {
        let filtered_events: Vec<midi::NoteEvent> = song
            .events
            .iter()
            .filter(|e| {
                let trk = e.track as usize;
                trk < tracks.len() && tracks[trk]
            })
            .copied()
            .collect();
        mapping::auto_transpose(&filtered_events, key_mode)
    }

    fn load_and_play_current(&mut self) {
        let max_attempts = self.queue.len();
        if max_attempts == 0 {
            return;
        }

        let skip_repeat = if self.settings.repeat == Repeat::All {
            Repeat::All
        } else {
            Repeat::Off
        };

        for _ in 0..max_attempts {
            let Some(entry_idx) = self.queue.current() else {
                break;
            };

            let Some(entry) = self.entries.get(entry_idx) else {
                self.queue.next(skip_repeat, None);
                continue;
            };

            let title = entry.title.clone();
            let path = entry.path.clone();
            match midi::load_file(&path) {
                Ok(song) => {
                    let tracks: Vec<bool> = song
                        .tracks
                        .iter()
                        .map(|t| !(self.settings.skip_drums && t.is_drum))
                        .collect();

                    let auto_shift =
                        Self::compute_auto_shift(&song, &tracks, self.settings.key_mode);
                    let song_arc = Arc::new(song);
                    let mapper = self.settings.mapper(auto_shift);

                    self.player
                        .send(Cmd::Load(Arc::clone(&song_arc), mapper, tracks.clone()));
                    self.player.send(Cmd::Play);

                    self.now_playing = Some(NowPlaying {
                        entry: Some(entry_idx),
                        path,
                        title,
                        song: song_arc,
                        auto_shift,
                        tracks,
                    });
                    return;
                }
                Err(e) => {
                    self.notice = Some(format!("Cannot play '{title}': {e}"));
                    self.queue.next(skip_repeat, None);
                }
            }
        }

        self.now_playing = None;
        self.stop();
    }

    /// Toggle play / pause, or play entry 0 if nothing loaded.
    pub fn toggle(&mut self) {
        if self.now_playing.is_none() {
            if !self.entries.is_empty() {
                let all_items: Vec<usize> = (0..self.entries.len()).collect();
                self.play_entry(0, all_items);
            }
        } else {
            self.player.send(Cmd::Toggle);
        }
    }

    /// Stop playback and release keys.
    pub fn stop(&mut self) {
        self.player.send(Cmd::Stop);
    }

    /// Jump to next track in queue.
    pub fn next(&mut self) {
        let shuffle_arg = if self.settings.shuffle {
            Some(&mut self.rng)
        } else {
            None
        };
        if self.queue.next(self.settings.repeat, shuffle_arg).is_some() {
            self.load_and_play_current();
        } else {
            self.stop();
        }
    }

    /// Jump to previous track or restart current song if position > 3 seconds.
    pub fn prev(&mut self) {
        let pos_us = self
            .player
            .status()
            .position_us
            .load(std::sync::atomic::Ordering::Relaxed);
        if pos_us > PREV_RESTART_THRESHOLD_US {
            self.player.send(Cmd::Seek(0));
        } else if self.queue.prev().is_some() {
            self.load_and_play_current();
        }
    }

    /// Seek playback position in microseconds.
    pub fn seek(&mut self, pos_us: u64) {
        self.player.send(Cmd::Seek(pos_us));
    }

    /// Advance song when player thread reports playback finished.
    pub fn on_finished(&mut self) {
        let shuffle_arg = if self.settings.shuffle {
            Some(&mut self.rng)
        } else {
            None
        };
        if self.queue.next(self.settings.repeat, shuffle_arg).is_some() {
            self.load_and_play_current();
        } else {
            self.stop();
        }
    }

    /// Set note mapping mode.
    pub fn set_note_mode(&mut self, mode: NoteMode) {
        self.settings.note_mode = mode;
        self.dirty = true;
        self.resend_mapper();
    }

    /// Cycle note mapping mode forward (+1) or backward (-1).
    pub fn cycle_mode(&mut self, delta: i32) {
        let new_mode = if delta > 0 {
            self.settings.note_mode.next()
        } else {
            self.settings.note_mode.prev()
        };
        self.set_note_mode(new_mode);
    }

    /// Set keyboard mode (Natural21 / Chromatic36).
    pub fn set_key_mode(&mut self, mode: KeyMode) {
        self.settings.key_mode = mode;
        self.dirty = true;
        if let Some(np) = &mut self.now_playing {
            np.auto_shift = Self::compute_auto_shift(&np.song, &np.tracks, self.settings.key_mode);
        }
        self.resend_mapper();
    }

    /// Set octave shift clamped to -2..=2.
    pub fn set_octave(&mut self, octave: i8) {
        self.settings.octave = octave.clamp(-2, 2);
        self.dirty = true;
        self.resend_mapper();
    }

    /// Set automatic transposition enable flag.
    pub fn set_auto_transpose(&mut self, enabled: bool) {
        self.settings.auto_transpose = enabled;
        self.dirty = true;
        self.resend_mapper();
    }

    /// Set manual transposition offset clamped to -6..=6.
    pub fn set_manual_transpose(&mut self, transpose: i8) {
        self.settings.manual_transpose = transpose.clamp(-6, 6);
        self.dirty = true;
        self.resend_mapper();
    }

    /// Set playback speed multiplier clamped to 0.25..=2.0.
    pub fn set_speed(&mut self, speed: f32) {
        self.settings.speed = if speed.is_nan() {
            1.0
        } else {
            speed.clamp(0.25, 2.0)
        };
        self.dirty = true;
        self.player.send(Cmd::Speed(self.settings.speed));
    }

    /// Set whether a specific track index is enabled.
    pub fn set_track_enabled(&mut self, track: usize, enabled: bool) {
        let Some(np) = &mut self.now_playing else {
            return;
        };
        if track >= np.tracks.len() {
            return;
        }
        np.tracks[track] = enabled;
        self.player.send(Cmd::Tracks(np.tracks.clone()));

        np.auto_shift = Self::compute_auto_shift(&np.song, &np.tracks, self.settings.key_mode);
        let mapper = self.settings.mapper(np.auto_shift);
        self.player.send(Cmd::Mapper(mapper));
    }

    /// Set key hold duration in milliseconds (clamped 5..=200).
    pub fn set_hold_ms(&mut self, hold_ms: u16) {
        self.settings.key_hold_ms = hold_ms.clamp(5, 200);
        self.dirty = true;
        self.player.send(Cmd::Hold(self.settings.key_hold_ms));
    }

    /// Set playlist repeat mode.
    pub fn set_repeat(&mut self, repeat: Repeat) {
        self.settings.repeat = repeat;
        self.dirty = true;
    }

    /// Set playlist shuffle flag.
    pub fn set_shuffle(&mut self, shuffle: bool) {
        self.settings.shuffle = shuffle;
        self.dirty = true;
    }

    /// Update settings that do not affect key output (theme, hotkeys, skip drums, ...).
    pub fn update_settings(&mut self, f: impl FnOnce(&mut Settings)) {
        f(&mut self.settings);
        self.settings.sanitize();
        self.dirty = true;
    }

    /// Update output-affecting settings and swap player Output backend.
    pub fn update_output_settings(&mut self, f: impl FnOnce(&mut Settings)) {
        f(&mut self.settings);
        self.settings.sanitize();
        self.dirty = true;
        let new_out = (self.make_output)(&self.settings);
        self.player.send(Cmd::Output(new_out));
    }

    /// Handle global hotkey action.
    pub fn handle_action(&mut self, a: Action) {
        match a {
            Action::PlayPause => self.toggle(),
            Action::Stop => self.stop(),
            Action::Next => self.next(),
            Action::Prev => self.prev(),
            Action::ModeNext => self.cycle_mode(1),
            Action::ModePrev => self.cycle_mode(-1),
        }
    }

    /// Persist settings to disk if modified.
    pub fn save_if_dirty(&mut self) -> Option<String> {
        if !self.dirty {
            return None;
        }
        match self.settings.save(&self.settings_path) {
            Ok(()) => {
                self.dirty = false;
                None
            }
            Err(e) => Some(e.to_string()),
        }
    }

    /// Read-only access to current settings.
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Reference to shared atomic player status.
    pub fn status(&self) -> &Arc<SharedStatus> {
        self.player.status()
    }

    /// Read-only access to currently playing item.
    pub fn now_playing(&self) -> Option<&NowPlaying> {
        self.now_playing.as_ref()
    }

    /// Cloned snapshot of current queue item indices.
    pub fn queue_items(&self) -> &[usize] {
        &self.queue_items
    }

    /// Take last user-facing message notice.
    pub fn take_notice(&mut self) -> Option<String> {
        self.notice.take()
    }

    /// Compute current note Mapper.
    pub fn current_mapper(&self) -> Mapper {
        let shift = self
            .now_playing
            .as_ref()
            .map(|np| np.auto_shift)
            .unwrap_or(0);
        self.settings.mapper(shift)
    }

    fn resend_mapper(&mut self) {
        let mapper = self.current_mapper();
        self.player.send(Cmd::Mapper(mapper));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapping::Stroke;
    use crate::player::PlayState;
    use std::env;
    use std::fs;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(1);

    struct TestTempDir {
        path: PathBuf,
    }

    impl TestTempDir {
        fn new() -> Self {
            let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let dir = env::temp_dir().join(format!("wwm_controller_test_{nanos}_{id}"));
            fs::create_dir_all(&dir).unwrap();
            Self { path: dir }
        }

        fn path(&self) -> &PathBuf {
            &self.path
        }
    }

    impl Drop for TestTempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[derive(Clone, Default)]
    struct MockRecorder {
        events: Arc<Mutex<Vec<String>>>,
    }

    #[derive(Clone)]
    struct MockOut {
        recorder: MockRecorder,
    }

    impl Out for MockOut {
        fn press(&mut self, s: Stroke) {
            let mut ev = self
                .recorder
                .events
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            ev.push(format!("press:{}", s.slot));
        }

        fn release(&mut self, s: Stroke) {
            let mut ev = self
                .recorder
                .events
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            ev.push(format!("release:{}", s.slot));
        }

        fn release_all(&mut self) {
            let mut ev = self
                .recorder
                .events
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            ev.push("release_all".to_string());
        }
    }

    fn write_test_midi(path: &PathBuf, events_data: &[(u8, u8, u8, u8)]) {
        // Build valid SMF Type 0 file
        // Header chunk: MThd, len 6, format 0, 1 track, division 480
        let mut smf = Vec::new();
        smf.extend_from_slice(b"MThd");
        smf.extend_from_slice(&6u32.to_be_bytes());
        smf.extend_from_slice(&0u16.to_be_bytes()); // format 0
        smf.extend_from_slice(&1u16.to_be_bytes()); // 1 track
        smf.extend_from_slice(&480u16.to_be_bytes()); // division

        // Track chunk
        let mut track_bytes = Vec::new();
        for &(delta, status, note, vel) in events_data {
            track_bytes.push(delta); // delta time
            track_bytes.push(status);
            track_bytes.push(note);
            track_bytes.push(vel);
        }
        // End of track meta event: 00 FF 2F 00
        track_bytes.extend_from_slice(&[0x00, 0xFF, 0x2F, 0x00]);

        smf.extend_from_slice(b"MTrk");
        smf.extend_from_slice(&(track_bytes.len() as u32).to_be_bytes());
        smf.extend_from_slice(&track_bytes);

        fs::write(path, smf).unwrap();
    }

    fn write_two_track_midi(path: &PathBuf, trk1: &[(u8, u8, u8, u8)], trk2: &[(u8, u8, u8, u8)]) {
        // SMF Type 1 file (2 tracks)
        let mut smf = Vec::new();
        smf.extend_from_slice(b"MThd");
        smf.extend_from_slice(&6u32.to_be_bytes());
        smf.extend_from_slice(&1u16.to_be_bytes()); // format 1
        smf.extend_from_slice(&2u16.to_be_bytes()); // 2 tracks
        smf.extend_from_slice(&480u16.to_be_bytes());

        for trk_events in [trk1, trk2] {
            let mut track_bytes = Vec::new();
            for &(delta, status, note, vel) in trk_events {
                track_bytes.push(delta);
                track_bytes.push(status);
                track_bytes.push(note);
                track_bytes.push(vel);
            }
            track_bytes.extend_from_slice(&[0x00, 0xFF, 0x2F, 0x00]);

            smf.extend_from_slice(b"MTrk");
            smf.extend_from_slice(&(track_bytes.len() as u32).to_be_bytes());
            smf.extend_from_slice(&track_bytes);
        }

        fs::write(path, smf).unwrap();
    }

    fn wait_for_state(status: &SharedStatus, target: PlayState, timeout: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if status.state() == target {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        status.state() == target
    }

    #[test]
    fn test_rescan_without_library_dir() {
        let temp_dir = TestTempDir::new();
        let settings_path = temp_dir.path().join("settings.json");
        let settings = Settings::default(); // library_dir is None

        let mut controller = Controller::new(
            settings,
            settings_path,
            |_| MockOut {
                recorder: MockRecorder::default(),
            },
            |_| {},
        );

        let res = controller.rescan();
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("choose a MIDI folder"));
    }

    #[test]
    fn test_broken_file_followed_by_good_and_all_broken() {
        let temp_dir = TestTempDir::new();
        let lib_dir = temp_dir.path().join("midi");
        fs::create_dir_all(&lib_dir).unwrap();

        let broken_path = lib_dir.join("01_broken.mid");
        // Valid MThd header so scan() sees it, but invalid chunk data so load_file fails
        fs::write(
            &broken_path,
            b"MThd\x00\x00\x00\x06\x00\x00\x00\x01\x01\xe0corrupt",
        )
        .unwrap();

        let good_path = lib_dir.join("02_good.mid");
        // D major melody note (D4 = 62)
        write_test_midi(&good_path, &[(0, 0x90, 62, 64), (100, 0x80, 62, 0)]);

        let settings = Settings {
            library_dir: Some(lib_dir.clone()),
            ..Default::default()
        };
        let settings_path = temp_dir.path().join("settings.json");

        let mut controller = Controller::new(
            settings,
            settings_path,
            |_| MockOut {
                recorder: MockRecorder::default(),
            },
            |_| {},
        );

        assert_eq!(controller.entries().len(), 2);

        // Try playing broken song first (index 0) with queue [0, 1]
        controller.play_entry(0, vec![0, 1]);

        // Notice should mention broken title
        let notice = controller.take_notice().expect("notice should be set");
        assert!(
            notice.contains("Cannot play '01_broken'"),
            "notice: {notice}"
        );

        // Good song (index 1) should now be playing
        assert_eq!(controller.now_playing().and_then(|np| np.entry), Some(1));
        assert!(wait_for_state(
            controller.status(),
            PlayState::Playing,
            Duration::from_millis(500)
        ));

        // Test Defect 2: play good song, then play_entry on queue of only broken files ->
        // now_playing() is None and notice is set
        fs::remove_file(&good_path).unwrap();
        let broken2_path = lib_dir.join("02_broken2.mid");
        fs::write(
            &broken2_path,
            b"MThd\x00\x00\x00\x06\x00\x00\x00\x01\x01\xe0corrupt2",
        )
        .unwrap();
        controller.rescan().unwrap();

        assert!(controller.now_playing().is_some());
        controller.play_entry(0, vec![0, 1]);
        let notice2 = controller.take_notice().expect("notice should be set");
        assert!(notice2.contains("Cannot play"));
        assert!(controller.now_playing().is_none());
        assert!(wait_for_state(
            controller.status(),
            PlayState::Idle,
            Duration::from_millis(500)
        ));
    }

    #[test]
    fn test_rescan_remaps_now_playing_and_queue() {
        let temp_dir = TestTempDir::new();
        let lib_dir = temp_dir.path().join("midi");
        fs::create_dir_all(&lib_dir).unwrap();
        write_test_midi(&lib_dir.join("b.mid"), &[(0, 0x90, 60, 64)]);
        write_test_midi(&lib_dir.join("c.mid"), &[(0, 0x90, 62, 64)]);

        let settings = Settings {
            library_dir: Some(lib_dir.clone()),
            ..Default::default()
        };
        let mut controller = Controller::new(
            settings,
            temp_dir.path().join("settings.json"),
            |_| MockOut {
                recorder: MockRecorder::default(),
            },
            |_| {},
        );

        controller.play_entry(1, vec![0, 1]);
        assert_eq!(controller.now_playing().and_then(|np| np.entry), Some(1));

        // New file sorts first: every index shifts by one
        write_test_midi(&lib_dir.join("a.mid"), &[(0, 0x90, 64, 64)]);
        controller.rescan().unwrap();
        let np_entry = controller.now_playing().and_then(|np| np.entry);
        assert_eq!(np_entry, Some(2));
        assert_eq!(controller.entries()[2].title, "c");
        assert_eq!(controller.queue_items(), &[1, 2]);

        // Different folder: playing song is no longer in the library
        let other_dir = temp_dir.path().join("other");
        fs::create_dir_all(&other_dir).unwrap();
        write_test_midi(&other_dir.join("x.mid"), &[(0, 0x90, 60, 64)]);
        controller.set_library_dir(other_dir);
        assert!(controller.now_playing().is_some());
        assert_eq!(controller.now_playing().and_then(|np| np.entry), None);
        assert!(controller.queue_items().is_empty());
    }

    #[test]
    fn test_skip_on_error_repeat_one() {
        let temp_dir = TestTempDir::new();
        let lib_dir = temp_dir.path().join("midi");
        fs::create_dir_all(&lib_dir).unwrap();

        let broken_path = lib_dir.join("01_broken.mid");
        fs::write(
            &broken_path,
            b"MThd\x00\x00\x00\x06\x00\x00\x00\x01\x01\xe0corrupt",
        )
        .unwrap();

        let good_path = lib_dir.join("02_good.mid");
        write_test_midi(&good_path, &[(0, 0x90, 62, 64), (100, 0x80, 62, 0)]);

        let settings = Settings {
            library_dir: Some(lib_dir),
            repeat: Repeat::One,
            ..Default::default()
        };
        let settings_path = temp_dir.path().join("settings.json");

        let mut controller = Controller::new(
            settings,
            settings_path,
            |_| MockOut {
                recorder: MockRecorder::default(),
            },
            |_| {},
        );

        // Queue [broken (0), good (1)] starting at broken with Repeat::One
        controller.play_entry(0, vec![0, 1]);
        assert_eq!(controller.now_playing().and_then(|np| np.entry), Some(1));
        assert!(wait_for_state(
            controller.status(),
            PlayState::Playing,
            Duration::from_millis(500)
        ));
    }

    #[test]
    fn test_favorites_mask_and_indices() {
        let temp_dir = TestTempDir::new();
        let lib_dir = temp_dir.path().join("midi");
        fs::create_dir_all(&lib_dir).unwrap();
        let f0 = lib_dir.join("00_song.mid");
        write_test_midi(&f0, &[(0, 0x90, 60, 64)]);
        let f1 = lib_dir.join("01_song.mid");
        write_test_midi(&f1, &[(0, 0x90, 62, 64)]);

        let settings = Settings {
            library_dir: Some(lib_dir.clone()),
            favorites: vec!["01_song.mid".to_string()],
            ..Default::default()
        };
        let settings_path = temp_dir.path().join("settings.json");

        let mut controller = Controller::new(
            settings,
            settings_path,
            |_| MockOut {
                recorder: MockRecorder::default(),
            },
            |_| {},
        );

        assert_eq!(controller.entries().len(), 2);
        assert!(!controller.is_favorite(0));
        assert!(controller.is_favorite(1));
        assert_eq!(controller.favorite_indices(), vec![1]);

        // Toggle index 0 on
        controller.toggle_favorite(0);
        assert!(controller.is_favorite(0));
        assert!(controller.is_favorite(1));
        assert_eq!(controller.favorite_indices(), vec![0, 1]);

        // Toggle index 1 off
        controller.toggle_favorite(1);
        assert!(controller.is_favorite(0));
        assert!(!controller.is_favorite(1));
        assert_eq!(controller.favorite_indices(), vec![0]);

        // Rescan preserves favorite status correctly
        controller.rescan().unwrap();
        assert!(controller.is_favorite(0));
        assert!(!controller.is_favorite(1));
        assert_eq!(controller.favorite_indices(), vec![0]);
    }

    #[test]
    fn test_play_entry_queue_fallback() {
        let temp_dir = TestTempDir::new();
        let lib_dir = temp_dir.path().join("midi");
        fs::create_dir_all(&lib_dir).unwrap();
        let f1 = lib_dir.join("song1.mid");
        write_test_midi(&f1, &[(0, 0x90, 60, 64)]);

        let settings = Settings {
            library_dir: Some(lib_dir),
            ..Default::default()
        };
        let settings_path = temp_dir.path().join("settings.json");

        let mut controller = Controller::new(
            settings,
            settings_path,
            |_| MockOut {
                recorder: MockRecorder::default(),
            },
            |_| {},
        );

        // queue_items doesn't contain idx 0
        controller.play_entry(0, vec![1, 2, 3]);
        assert_eq!(controller.queue_items(), &[0]);
    }

    #[test]
    fn test_drum_track_exclusion_and_auto_shift() {
        let temp_dir = TestTempDir::new();
        let lib_dir = temp_dir.path().join("midi");
        fs::create_dir_all(&lib_dir).unwrap();

        let song_path = lib_dir.join("d_major_with_drum.mid");
        // Track 1: D major melody on channel 0 (D4 = 62, F#4 = 66, A4 = 69)
        let trk1 = [
            (0, 0x90, 62, 64),
            (50, 0x90, 66, 64),
            (50, 0x90, 69, 64),
            (50, 0x80, 62, 0),
        ];
        // Track 2: loud drum on channel 9 (channel 9 status = 0x99, notes 35, 36, 38)
        let trk2 = [
            (0, 0x99, 35, 120),
            (10, 0x99, 36, 120),
            (10, 0x99, 38, 120),
            (10, 0x99, 42, 120),
        ];
        write_two_track_midi(&song_path, &trk1, &trk2);

        let settings = Settings {
            library_dir: Some(lib_dir),
            skip_drums: true,
            key_mode: KeyMode::Natural21,
            ..Default::default()
        };
        let settings_path = temp_dir.path().join("settings.json");

        let mut controller = Controller::new(
            settings,
            settings_path,
            |_| MockOut {
                recorder: MockRecorder::default(),
            },
            |_| {},
        );

        controller.play_entry(0, vec![0]);
        let np = controller.now_playing().expect("song loaded");
        assert_eq!(np.tracks.len(), 2);
        assert!(np.tracks[0], "melody track enabled");
        assert!(!np.tracks[1], "drum track disabled when skip_drums");
        // D major melody auto_shift should be -2 (shifts D into C natural heptatonic)
        assert_eq!(np.auto_shift, -2);
    }

    #[test]
    fn test_toggle_with_nothing_loaded_starts_entry_zero() {
        let temp_dir = TestTempDir::new();
        let lib_dir = temp_dir.path().join("midi");
        fs::create_dir_all(&lib_dir).unwrap();
        let f1 = lib_dir.join("01_song.mid");
        write_test_midi(&f1, &[(0, 0x90, 60, 64)]);
        let f2 = lib_dir.join("02_song.mid");
        write_test_midi(&f2, &[(0, 0x90, 62, 64)]);

        let settings = Settings {
            library_dir: Some(lib_dir),
            ..Default::default()
        };
        let settings_path = temp_dir.path().join("settings.json");

        let mut controller = Controller::new(
            settings,
            settings_path,
            |_| MockOut {
                recorder: MockRecorder::default(),
            },
            |_| {},
        );

        assert!(controller.now_playing().is_none());
        controller.toggle();
        assert_eq!(controller.now_playing().and_then(|np| np.entry), Some(0));
        assert_eq!(controller.queue_items(), &[0, 1]);
    }

    #[test]
    fn test_settings_clamping_and_dirty() {
        let temp_dir = TestTempDir::new();
        let settings_path = temp_dir.path().join("settings.json");
        let settings = Settings::default();

        let mut controller = Controller::new(
            settings,
            settings_path,
            |_| MockOut {
                recorder: MockRecorder::default(),
            },
            |_| {},
        );

        assert!(!controller.dirty);

        // Octave clamp
        controller.set_octave(5);
        assert_eq!(controller.settings().octave, 2);
        assert!(controller.dirty);

        // Speed clamp
        controller.set_speed(9.0);
        assert_eq!(controller.settings().speed, 2.0);

        // Mode cycling wraps
        controller.set_note_mode(NoteMode::Melody);
        controller.cycle_mode(1);
        assert_eq!(controller.settings().note_mode, NoteMode::Nearest);
    }

    #[test]
    fn test_on_finished_repeat_off() {
        let temp_dir = TestTempDir::new();
        let lib_dir = temp_dir.path().join("midi");
        fs::create_dir_all(&lib_dir).unwrap();
        let f1 = lib_dir.join("song.mid");
        write_test_midi(&f1, &[(0, 0x90, 60, 64)]);

        let settings = Settings {
            library_dir: Some(lib_dir),
            repeat: Repeat::Off,
            ..Default::default()
        };
        let settings_path = temp_dir.path().join("settings.json");

        let mut controller = Controller::new(
            settings,
            settings_path,
            |_| MockOut {
                recorder: MockRecorder::default(),
            },
            |_| {},
        );

        controller.play_entry(0, vec![0]);
        assert!(wait_for_state(
            controller.status(),
            PlayState::Playing,
            Duration::from_millis(500)
        ));

        controller.on_finished();
        assert!(wait_for_state(
            controller.status(),
            PlayState::Idle,
            Duration::from_millis(500)
        ));
    }

    #[test]
    fn test_handle_action_next_moves_to_item_2() {
        let temp_dir = TestTempDir::new();
        let lib_dir = temp_dir.path().join("midi");
        fs::create_dir_all(&lib_dir).unwrap();
        for i in 0..3 {
            let f = lib_dir.join(format!("{i:02}_song.mid"));
            write_test_midi(&f, &[(0, 0x90, 60 + i as u8, 64)]);
        }

        let settings = Settings {
            library_dir: Some(lib_dir),
            ..Default::default()
        };
        let settings_path = temp_dir.path().join("settings.json");

        let mut controller = Controller::new(
            settings,
            settings_path,
            |_| MockOut {
                recorder: MockRecorder::default(),
            },
            |_| {},
        );

        controller.play_entry(0, vec![0, 1, 2]);
        assert_eq!(controller.now_playing().and_then(|np| np.entry), Some(0));

        controller.handle_action(Action::Next);
        assert_eq!(controller.now_playing().and_then(|np| np.entry), Some(1));

        controller.handle_action(Action::Next);
        assert_eq!(controller.now_playing().and_then(|np| np.entry), Some(2));
    }

    #[test]
    fn test_prev_within_and_after_three_seconds() {
        let temp_dir = TestTempDir::new();
        let lib_dir = temp_dir.path().join("midi");
        fs::create_dir_all(&lib_dir).unwrap();
        let f0 = lib_dir.join("00_song.mid");
        write_test_midi(&f0, &[(0, 0x90, 60, 64)]);
        let f1 = lib_dir.join("01_song.mid");
        // Longer song for seeking past 3 seconds (duration ~ 5 seconds)
        write_test_midi(
            &f1,
            &[(0, 0x90, 62, 64), (200, 0x90, 64, 64), (200, 0x80, 62, 0)],
        );

        let settings = Settings {
            library_dir: Some(lib_dir),
            ..Default::default()
        };
        let settings_path = temp_dir.path().join("settings.json");

        let mut controller = Controller::new(
            settings,
            settings_path,
            |_| MockOut {
                recorder: MockRecorder::default(),
            },
            |_| {},
        );

        controller.play_entry(1, vec![0, 1]);
        assert_eq!(controller.now_playing().and_then(|np| np.entry), Some(1));

        // When pos <= 3s, prev() jumps to item 0
        controller.prev();
        assert_eq!(controller.now_playing().and_then(|np| np.entry), Some(0));

        // Move to item 1 again
        controller.next();
        assert_eq!(controller.now_playing().and_then(|np| np.entry), Some(1));

        // Simulate position > 3 seconds via atomic status update directly
        controller
            .status()
            .position_us
            .store(4_000_000, Ordering::SeqCst);
        controller.prev();
        // Should NOT jump to item 0, stays on item 1
        assert_eq!(controller.now_playing().and_then(|np| np.entry), Some(1));
    }

    #[test]
    fn test_save_if_dirty_and_clean() {
        let temp_dir = TestTempDir::new();
        let settings_path = temp_dir.path().join("settings.json");
        let settings = Settings::default();

        let mut controller = Controller::new(
            settings,
            settings_path.clone(),
            |_| MockOut {
                recorder: MockRecorder::default(),
            },
            |_| {},
        );

        // Clean: no-op returning None
        assert_eq!(controller.save_if_dirty(), None);
        assert!(!settings_path.exists());

        // Dirty
        controller.set_octave(1);
        assert_eq!(controller.save_if_dirty(), None);
        assert!(settings_path.exists());

        // Second call: no-op returning None
        assert_eq!(controller.save_if_dirty(), None);
    }

    #[test]
    fn test_update_output_settings_invokes_make_output() {
        let temp_dir = TestTempDir::new();
        let settings_path = temp_dir.path().join("settings.json");
        let settings = Settings::default();
        let call_count = Arc::new(AtomicUsize::new(0));
        let cc = Arc::clone(&call_count);

        let mut controller = Controller::new(
            settings,
            settings_path,
            move |_| {
                cc.fetch_add(1, Ordering::SeqCst);
                MockOut {
                    recorder: MockRecorder::default(),
                }
            },
            |_| {},
        );

        // Constructor makes 1 output
        assert_eq!(call_count.load(Ordering::SeqCst), 1);

        controller.update_output_settings(|s| {
            s.modifier_delay_ms = 50;
        });

        assert_eq!(call_count.load(Ordering::SeqCst), 2);
        assert_eq!(controller.settings().modifier_delay_ms, 50);
    }

    #[test]
    fn test_update_settings_keeps_output_and_marks_dirty() {
        let temp_dir = TestTempDir::new();
        let settings_path = temp_dir.path().join("settings.json");
        let call_count = Arc::new(AtomicUsize::new(0));
        let cc = Arc::clone(&call_count);

        let mut controller = Controller::new(
            Settings::default(),
            settings_path.clone(),
            move |_| {
                cc.fetch_add(1, Ordering::SeqCst);
                MockOut {
                    recorder: MockRecorder::default(),
                }
            },
            |_| {},
        );

        controller.update_settings(|s| s.theme = crate::settings::Theme::Dark);

        assert_eq!(call_count.load(Ordering::SeqCst), 1);
        assert_eq!(controller.settings().theme, crate::settings::Theme::Dark);
        assert_eq!(controller.save_if_dirty(), None);
        assert!(settings_path.exists());
    }

    #[test]
    fn test_revision_bumps_on_library_and_queue_changes() {
        let temp_dir = TestTempDir::new();
        let lib = temp_dir.path().join("lib");
        fs::create_dir_all(&lib).unwrap();
        write_test_midi(&lib.join("a.mid"), &[(0, 0x90, 60, 100), (10, 0x80, 60, 0)]);
        write_test_midi(&lib.join("b.mid"), &[(0, 0x90, 62, 100), (10, 0x80, 62, 0)]);

        let mut controller = Controller::new(
            Settings::default(),
            temp_dir.path().join("settings.json"),
            |_| MockOut {
                recorder: MockRecorder::default(),
            },
            |_| {},
        );

        let r0 = controller.revision();
        controller.set_library_dir(lib);
        let r1 = controller.revision();
        assert!(r1 > r0, "set_library_dir must bump revision");

        controller.toggle_favorite(0);
        let r2 = controller.revision();
        assert!(r2 > r1, "toggle_favorite must bump revision");

        controller.play_entry(1, vec![0, 1]);
        assert!(controller.revision() > r2, "play_entry must bump revision");

        let r3 = controller.revision();
        controller.set_speed(1.5);
        assert_eq!(
            controller.revision(),
            r3,
            "speed change must not bump revision"
        );
    }
}
