//! Keyboard layout presets and custom key mapping.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Predefined and custom layout options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LayoutPreset {
    /// Standard QWERTY layout.
    Qwerty,
    /// French AZERTY layout.
    Azerty,
    /// German QWERTZ layout.
    Qwertz,
    /// User-defined layout.
    Custom,
}

/// Key layout error variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutError {
    /// Key count is not 21.
    WrongCount(usize),
    /// Key character is not allowed.
    BadKey(char),
    /// Key character is duplicated.
    Duplicate(char),
}

impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LayoutError::WrongCount(n) => write!(f, "layout needs 21 keys, got {n}"),
            LayoutError::BadKey(c) => write!(f, "key '{c}' is not allowed"),
            LayoutError::Duplicate(c) => write!(f, "key '{c}' is used twice"),
        }
    }
}

impl std::error::Error for LayoutError {}

/// An instrument key layout containing exactly 21 unique allowed keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyLayout {
    keys: [char; 21],
}

impl KeyLayout {
    /// Returns predefined key layout for preset, or None for Custom.
    pub fn preset(p: LayoutPreset) -> Option<KeyLayout> {
        let chars = match p {
            LayoutPreset::Qwerty => "zxcvbnmasdfghjqwertyu",
            LayoutPreset::Azerty => "wxcvbn,qsdfghjazertyu",
            LayoutPreset::Qwertz => "yxcvbnmasdfghjqwertzu",
            LayoutPreset::Custom => return None,
        };
        let mut keys = ['\0'; 21];
        for (i, c) in chars.chars().enumerate() {
            keys[i] = c;
        }
        Some(KeyLayout { keys })
    }

    /// Parse and validate custom key slice (case-insensitive ASCII).
    pub fn custom(keys: &[char]) -> Result<KeyLayout, LayoutError> {
        if keys.len() != 21 {
            return Err(LayoutError::WrongCount(keys.len()));
        }
        let mut out = ['\0'; 21];
        let mut seen = [false; 128];

        for (i, &raw_c) in keys.iter().enumerate() {
            let c = raw_c.to_ascii_lowercase();
            if !is_allowed_key(c) {
                return Err(LayoutError::BadKey(raw_c));
            }
            let idx = c as usize;
            if idx >= 128 || seen[idx] {
                return Err(LayoutError::Duplicate(c));
            }
            seen[idx] = true;
            out[i] = c;
        }
        Ok(KeyLayout { keys: out })
    }

    /// Returns character for slot (0..=20), or None if slot is out of bounds.
    pub fn key(&self, slot: u8) -> Option<char> {
        self.keys.get(slot as usize).copied()
    }

    /// Returns slice of all 21 keys ordered by slot.
    pub fn keys(&self) -> &[char; 21] {
        &self.keys
    }
}

fn is_allowed_key(c: char) -> bool {
    matches!(
        c,
        'a'..='z'
            | '0'..='9'
            | ','
            | '.'
            | '/'
            | ';'
            | '\''
            | '['
            | ']'
            | '-'
            | '='
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_presets_validity() {
        for preset in [
            LayoutPreset::Qwerty,
            LayoutPreset::Azerty,
            LayoutPreset::Qwertz,
        ] {
            let layout = KeyLayout::preset(preset).expect("preset should exist");
            assert_eq!(layout.keys().len(), 21);
            let parsed = KeyLayout::custom(layout.keys()).expect("preset keys valid");
            assert_eq!(parsed, layout);
        }
        assert_eq!(KeyLayout::preset(LayoutPreset::Custom), None);
    }

    #[test]
    fn test_key_slot_bounds() {
        let q = KeyLayout::preset(LayoutPreset::Qwerty).unwrap();
        assert_eq!(q.key(0), Some('z'));
        assert_eq!(q.key(20), Some('u'));
        assert_eq!(q.key(21), None);
    }

    #[test]
    fn test_custom_count_error() {
        let err20 = KeyLayout::custom(&['a'; 20]).unwrap_err();
        assert!(matches!(err20, LayoutError::WrongCount(20)));
        assert_eq!(err20.to_string(), "layout needs 21 keys, got 20");

        let err22 = KeyLayout::custom(&['a'; 22]).unwrap_err();
        assert!(matches!(err22, LayoutError::WrongCount(22)));
        assert_eq!(err22.to_string(), "layout needs 21 keys, got 22");
    }

    #[test]
    fn test_custom_bad_char() {
        let mut chars: Vec<char> = "zxcvbnmasdfghjqwertyu".chars().collect();
        chars[0] = 'é';
        let err = KeyLayout::custom(&chars).unwrap_err();
        assert!(matches!(err, LayoutError::BadKey('é')));
        assert_eq!(err.to_string(), "key 'é' is not allowed");
    }

    #[test]
    fn test_custom_duplicate() {
        let mut chars: Vec<char> = "zxcvbnmasdfghjqwertyu".chars().collect();
        chars[1] = 'z';
        let err = KeyLayout::custom(&chars).unwrap_err();
        assert!(matches!(err, LayoutError::Duplicate('z')));
        assert_eq!(err.to_string(), "key 'z' is used twice");
    }

    #[test]
    fn test_uppercase_lowercased() {
        let mut chars: Vec<char> = "zxcvbnmasdfghjqwertyu".chars().collect();
        chars[0] = 'Z';
        let layout = KeyLayout::custom(&chars).expect("uppercase accepted");
        assert_eq!(layout.key(0), Some('z'));
    }
}
