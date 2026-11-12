//! The two builds sharing this code: the Where Winds Meet guqin player and the Roblox
//! 61-key piano player. They differ only in defaults, window title and the key board.

use crate::mapping::KeyMode;
use crate::settings::Settings;

/// Which game this executable plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edition {
    /// Where Winds Meet guqin (21 / 36 keys).
    Guqin,
    /// Roblox piano, Visual Pianos layout (61 keys, `1`..`m` plus Shift).
    Roblox,
}

impl Edition {
    /// Window and header title.
    pub fn title(self) -> &'static str {
        match self {
            Edition::Guqin => "WWM Guqin Freeplay",
            Edition::Roblox => "Roblox Piano Freeplay",
        }
    }

    /// Initial window size; the piano needs room for its full-width keyboard.
    pub fn window_size(self) -> [f32; 2] {
        match self {
            Edition::Guqin => [980.0, 680.0],
            Edition::Roblox => [1080.0, 820.0],
        }
    }

    /// Settings file name, kept next to the executable.
    pub fn settings_file(self) -> &'static str {
        match self {
            Edition::Guqin => "wwm-guqin.json",
            Edition::Roblox => "roblox-piano.json",
        }
    }

    /// Settings used when the file is missing or corrupt.
    pub fn default_settings(self) -> Settings {
        let mut s = Settings::default();
        if self == Edition::Roblox {
            s.key_mode = KeyMode::Roblox61;
            s.window_keywords = vec!["Roblox".to_string()];
        }
        s
    }

    /// Keep the key mode inside what this edition offers.
    pub fn normalize(self, s: &mut Settings) {
        match self {
            Edition::Roblox => s.key_mode = KeyMode::Roblox61,
            Edition::Guqin if s.key_mode == KeyMode::Roblox61 => s.key_mode = KeyMode::Natural21,
            Edition::Guqin => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_edition_defaults_and_normalize() {
        assert_eq!(Edition::Guqin.default_settings(), Settings::default());
        let roblox = Edition::Roblox.default_settings();
        assert_eq!(roblox.key_mode, KeyMode::Roblox61);
        assert_eq!(roblox.window_keywords, vec!["Roblox"]);

        let mut s = Settings::default();
        Edition::Roblox.normalize(&mut s);
        assert_eq!(s.key_mode, KeyMode::Roblox61);
        Edition::Guqin.normalize(&mut s);
        assert_eq!(s.key_mode, KeyMode::Natural21);
        s.key_mode = KeyMode::Chromatic36;
        Edition::Guqin.normalize(&mut s);
        assert_eq!(s.key_mode, KeyMode::Chromatic36);
        assert_ne!(
            Edition::Guqin.settings_file(),
            Edition::Roblox.settings_file()
        );
    }
}
