//! Application settings and persistence.

use crate::layout::{KeyLayout, LayoutPreset};
use crate::mapping::{KeyMode, Mapper, NoteMode};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Input injection backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum InputBackend {
    /// Send input globally via Windows SendInput.
    #[default]
    Global,
    /// Background mode: posts to the game window; Shift/Ctrl may not register.
    Window,
}

/// Repeat mode for playlist playback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Repeat {
    /// Play through queue once and stop.
    #[default]
    Off,
    /// Loop current song indefinitely.
    One,
    /// Loop entire queue.
    All,
}

/// Library list order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SortOrder {
    /// Title A to Z.
    #[default]
    NameAsc,
    /// Title Z to A.
    NameDesc,
    /// File extension, then title.
    Type,
    /// Last modified, newest first.
    Date,
}

/// UI color theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Theme {
    /// Light theme.
    #[default]
    Light,
    /// Dark theme.
    Dark,
}

/// Hotkey configuration strings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hotkeys {
    /// Hotkey for play / pause toggle.
    pub play_pause: String,
    /// Hotkey to stop playback.
    pub stop: String,
    /// Hotkey to jump to next track.
    pub next: String,
    /// Hotkey to jump to previous track.
    pub prev: String,
    /// Hotkey to switch to next NoteMode.
    pub mode_next: String,
    /// Hotkey to switch to previous NoteMode.
    pub mode_prev: String,
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self {
            play_pause: "ScrollLock".to_string(),
            stop: "End".to_string(),
            next: "F11".to_string(),
            prev: "F10".to_string(),
            mode_next: "PageUp".to_string(),
            mode_prev: "PageDown".to_string(),
        }
    }
}

/// Application settings state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Path to music library directory.
    pub library_dir: Option<PathBuf>,
    /// Selected keyboard layout preset.
    pub layout: LayoutPreset,
    /// Custom key characters string.
    pub custom_keys: String,
    /// Active NoteMode algorithm.
    pub note_mode: NoteMode,
    /// Active KeyMode key range.
    pub key_mode: KeyMode,
    /// Enable automatic transposition.
    pub auto_transpose: bool,
    /// Manual transposition offset (-6..=6).
    pub manual_transpose: i8,
    /// Manual octave shift (-2..=2).
    pub octave: i8,
    /// Playback speed multiplier (0.25..=2.0).
    pub speed: f32,
    /// Milliseconds to delay between modifier and stroke.
    pub modifier_delay_ms: u16,
    /// Milliseconds to hold key pressed.
    pub key_hold_ms: u16,
    /// Input injection backend.
    pub backend: InputBackend,
    /// Window title match keywords.
    pub window_keywords: Vec<String>,
    /// Automatically skip drum tracks during play.
    pub skip_drums: bool,
    /// List of favorite song relative paths.
    pub favorites: Vec<String>,
    /// Playlist repeat mode.
    pub repeat: Repeat,
    /// Enable playlist shuffle.
    pub shuffle: bool,
    /// Start the next song automatically when one ends.
    pub auto_next: bool,
    /// Seconds to wait between songs (0..=60).
    pub next_delay_s: u16,
    /// Library list order.
    pub sort: SortOrder,
    /// UI theme setting.
    pub theme: Theme,
    /// Global hotkey mappings.
    pub hotkeys: Hotkeys,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            library_dir: None,
            layout: LayoutPreset::Qwerty,
            custom_keys: "zxcvbnmasdfghjqwertyu".to_string(),
            note_mode: NoteMode::Nearest,
            key_mode: KeyMode::Natural21,
            auto_transpose: true,
            manual_transpose: 0,
            octave: 0,
            speed: 1.0,
            modifier_delay_ms: 0,
            key_hold_ms: 25,
            backend: InputBackend::Global,
            window_keywords: vec![
                "Where Winds Meet".to_string(),
                "燕云十六声".to_string(),
                "WWM".to_string(),
            ],
            skip_drums: true,
            favorites: Vec::new(),
            repeat: Repeat::Off,
            shuffle: false,
            auto_next: true,
            next_delay_s: 0,
            sort: SortOrder::NameAsc,
            theme: Theme::Light,
            hotkeys: Hotkeys::default(),
        }
    }
}

impl Settings {
    /// Sanitize and clamp all configuration fields to valid limits.
    pub fn sanitize(&mut self) {
        let chars: Vec<char> = self.custom_keys.chars().collect();
        if KeyLayout::custom(&chars).is_err() {
            self.custom_keys = "zxcvbnmasdfghjqwertyu".to_string();
        }

        self.manual_transpose = self.manual_transpose.clamp(-6, 6);
        self.octave = self.octave.clamp(-2, 2);

        if self.speed.is_nan() || self.speed.is_infinite() {
            self.speed = 1.0;
        } else {
            self.speed = self.speed.clamp(0.25, 2.0);
        }

        self.modifier_delay_ms = self.modifier_delay_ms.clamp(0, 50);
        self.key_hold_ms = self.key_hold_ms.clamp(5, 200);
        self.next_delay_s = self.next_delay_s.min(60);

        let mut seen = HashSet::new();
        let mut clean_keywords = Vec::new();
        for kw in self.window_keywords.drain(..) {
            let trimmed = kw.trim().to_string();
            if trimmed.is_empty() {
                continue;
            }
            let lower = trimmed.to_lowercase();
            if seen.insert(lower) {
                clean_keywords.push(trimmed);
                if clean_keywords.len() == 16 {
                    break;
                }
            }
        }
        self.window_keywords = clean_keywords;

        let mut seen_favs = HashSet::new();
        let mut clean_favs = Vec::new();
        for fav in self.favorites.drain(..) {
            let normalized = fav.replace('\\', "/");
            if seen_favs.insert(normalized.clone()) {
                clean_favs.push(normalized);
            }
        }
        self.favorites = clean_favs;
    }

    /// Resolve active key layout, falling back to Qwerty on invalid custom keys.
    pub fn key_layout(&self) -> KeyLayout {
        if self.layout == LayoutPreset::Custom {
            let chars: Vec<char> = self.custom_keys.chars().collect();
            if let Ok(kl) = KeyLayout::custom(&chars) {
                return kl;
            }
        }
        KeyLayout::preset(self.layout).unwrap_or_else(|| {
            KeyLayout::preset(LayoutPreset::Qwerty).expect("Qwerty preset layout is always valid")
        })
    }

    /// Construct a Mapper configured with current settings.
    pub fn mapper(&self, auto_shift: i8) -> Mapper {
        let transpose = if self.auto_transpose {
            auto_shift
        } else {
            self.manual_transpose
        };
        Mapper {
            note_mode: self.note_mode,
            key_mode: self.key_mode,
            transpose,
            octave: self.octave,
        }
    }

    /// Determine default settings path adjacent to current executable or working directory.
    pub fn default_path() -> PathBuf {
        let exe_dir = env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()));
        match exe_dir {
            Some(dir) => dir.join("wwm-guqin.json"),
            None => PathBuf::from("wwm-guqin.json"),
        }
    }

    /// Load settings from path; falls back to default on missing or corrupt files.
    pub fn load(path: &Path) -> (Settings, Option<String>) {
        if !path.exists() {
            return (Settings::default(), None);
        }

        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => {
                let msg = "settings file unreadable, settings reset to default".to_string();
                return (Settings::default(), Some(msg));
            }
        };

        match serde_json::from_str::<Settings>(&content) {
            Ok(mut s) => {
                s.sanitize();
                (s, None)
            }
            Err(_) => {
                let msg = "corrupt JSON detected, settings reset to default".to_string();
                (Settings::default(), Some(msg))
            }
        }
    }

    /// Atomically persist settings as formatted JSON.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let tmp_path = path.with_extension(format!(
            "{}.tmp",
            path.extension().and_then(|s| s.to_str()).unwrap_or("json")
        ));
        let json = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        fs::write(&tmp_path, json)?;
        fs::rename(&tmp_path, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_missing_file_load() {
        let p = Path::new("non_existent_file_path_12345.json");
        let (s, warn) = Settings::load(p);
        assert_eq!(s, Settings::default());
        assert_eq!(warn, None);
    }

    #[test]
    fn test_corrupt_json_load() {
        let unique_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = env::temp_dir().join(format!("wwm_test_corrupt_json_{unique_id}"));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("test_corrupt.json");
        fs::write(&path, "{not json").unwrap();

        let (s, warn) = Settings::load(&path);
        assert_eq!(s, Settings::default());
        let warn_msg = warn.expect("warning expected");
        assert!(warn_msg.contains("settings reset"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_partial_json_load_and_clamping() {
        let unique_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = env::temp_dir().join(format!("wwm_test_partial_json_{unique_id}"));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("partial.json");
        fs::write(&path, "{\"speed\": 9.0}").unwrap();

        let (s, warn) = Settings::load(&path);
        assert_eq!(warn, None);
        assert_eq!(s.speed, 2.0);
        assert_eq!(s.octave, 0);
        assert_eq!(s.manual_transpose, 0);
        assert_eq!(s.layout, LayoutPreset::Qwerty);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_sanitize_clamps_and_resets() {
        let mut s = Settings {
            speed: f32::NAN,
            octave: 5,
            manual_transpose: -100,
            modifier_delay_ms: 100,
            key_hold_ms: 1,
            custom_keys: "invalid_custom_keys".to_string(),
            window_keywords: vec![
                "  ".to_string(),
                "wwm".to_string(),
                "WWM".to_string(),
                " Game ".to_string(),
            ],
            favorites: vec!["a\\b.mid".to_string(), "a/b.mid".to_string()],
            ..Default::default()
        };

        s.sanitize();

        assert_eq!(s.speed, 1.0);
        assert_eq!(s.octave, 2);
        assert_eq!(s.manual_transpose, -6);
        assert_eq!(s.modifier_delay_ms, 50);
        assert_eq!(s.key_hold_ms, 5);
        assert_eq!(s.custom_keys, "zxcvbnmasdfghjqwertyu");
        assert_eq!(s.window_keywords, vec!["wwm", "Game"]);
        assert_eq!(s.favorites, vec!["a/b.mid"]);
    }

    #[test]
    fn test_sanitize_window_keywords_truncates_to_sixteen() {
        let mut s = Settings {
            window_keywords: (0..20).map(|i| format!("Window {i}")).collect(),
            ..Default::default()
        };
        s.sanitize();
        assert_eq!(s.window_keywords.len(), 16);
        let expected: Vec<String> = (0..16).map(|i| format!("Window {i}")).collect();
        assert_eq!(s.window_keywords, expected);
    }

    #[test]
    fn test_atomic_save_roundtrip_and_no_tmp() {
        let unique_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = env::temp_dir().join(format!("wwm_test_save_roundtrip_{unique_id}"));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("test_settings.json");

        let original = Settings {
            speed: 1.5,
            auto_transpose: false,
            manual_transpose: 3,
            ..Default::default()
        };
        original.save(&path).unwrap();

        let tmp_path = dir.join("test_settings.json.tmp");
        assert!(!tmp_path.exists(), "temp file should not remain");

        let entries: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.file_name()))
            .collect();
        assert_eq!(
            entries,
            vec![std::ffi::OsString::from("test_settings.json")]
        );

        let (loaded, warn) = Settings::load(&path);
        assert_eq!(warn, None);
        assert_eq!(original, loaded);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_key_layout_and_mapper_helpers() {
        let s = Settings {
            layout: LayoutPreset::Custom,
            custom_keys: "yxcvbnmasdfghjqwertzu".to_string(),
            ..Default::default()
        };
        let kl = s.key_layout();
        assert_eq!(kl.key(0), Some('y'));

        let s2 = Settings {
            auto_transpose: true,
            ..Default::default()
        };
        let m1 = s2.mapper(2);
        assert_eq!(m1.transpose, 2);

        let s3 = Settings {
            auto_transpose: false,
            manual_transpose: -4,
            ..Default::default()
        };
        let m2 = s3.mapper(2);
        assert_eq!(m2.transpose, -4);
    }

    #[test]
    fn test_default_backend_is_global() {
        assert_eq!(Settings::default().backend, InputBackend::Global);
        assert_eq!(InputBackend::default(), InputBackend::Global);
    }
}
