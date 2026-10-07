//! Runtime font setup. Fonts are read from C:\Windows\Fonts at startup, never embedded,
//! so the exe stays small.
//!
//! egui's bundled font lacks Vietnamese letters with stacked marks (ờ, ạ, ữ ...), so Segoe UI
//! becomes the primary UI font when present; a CJK font is appended as the last fallback.

use eframe::egui::{FontData, FontDefinitions, FontFamily};
use std::path::Path;

/// Primary UI font candidates (full Latin incl. Vietnamese).
const UI_FONTS: &[&str] = &[
    r"C:\Windows\Fonts\segoeui.ttf",
    r"C:\Windows\Fonts\arial.ttf",
];

/// CJK fallback candidates for Chinese/Korean song titles.
const CJK_FONTS: &[&str] = &[
    r"C:\Windows\Fonts\msyh.ttc",
    r"C:\Windows\Fonts\msyh.ttf",
    r"C:\Windows\Fonts\simsun.ttc",
    r"C:\Windows\Fonts\malgun.ttf",
];

fn first_readable(candidates: &[&str]) -> Option<Vec<u8>> {
    candidates
        .iter()
        .map(Path::new)
        .filter(|p| p.exists())
        .find_map(|p| std::fs::read(p).ok())
}

/// Install system fonts into `fonts`: UI font first, egui defaults next, CJK last.
pub fn install_system_fonts(fonts: &mut FontDefinitions) {
    if let Some(bytes) = first_readable(UI_FONTS) {
        fonts
            .font_data
            .insert("system_ui".to_owned(), FontData::from_owned(bytes).into());
        if let Some(prop) = fonts.families.get_mut(&FontFamily::Proportional) {
            prop.insert(0, "system_ui".to_owned());
        }
        if let Some(mono) = fonts.families.get_mut(&FontFamily::Monospace) {
            mono.push("system_ui".to_owned());
        }
    }
    if let Some(bytes) = first_readable(CJK_FONTS) {
        fonts
            .font_data
            .insert("system_cjk".to_owned(), FontData::from_owned(bytes).into());
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            if let Some(list) = fonts.families.get_mut(&family) {
                list.push("system_cjk".to_owned());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_readable_skips_missing_paths() {
        assert_eq!(first_readable(&[r"Z:\definitely\missing\font.ttf"]), None);
    }

    #[test]
    fn ui_font_goes_first_and_defaults_stay() {
        let mut fonts = FontDefinitions::default();
        let default_first = fonts.families[&FontFamily::Proportional][0].clone();
        install_system_fonts(&mut fonts);
        let prop = &fonts.families[&FontFamily::Proportional];
        assert!(
            prop.contains(&default_first),
            "egui default font must remain as fallback"
        );
        if fonts.font_data.contains_key("system_ui") {
            assert_eq!(prop[0], "system_ui");
        }
        if fonts.font_data.contains_key("system_cjk") {
            assert_eq!(prop.last().map(String::as_str), Some("system_cjk"));
        }
    }
}
