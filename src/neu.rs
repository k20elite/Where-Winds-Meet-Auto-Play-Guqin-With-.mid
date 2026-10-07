//! Neumorphic (Soft UI) design system: tokens, shadow painters, and small widgets.
//!
//! Rules: every surface shares the base colour, no borders, raised = dual outer shadows,
//! pressed/input = dual inner shadows, one accent for the primary action and state cues.
//! Shadows alone are not a state cue, so every widget also shows a dot, text, or ring.

use crate::settings::Theme;
use eframe::egui::{
    self, epaint, Color32, CornerRadius, Key, Painter, Pos2, Rect, Response, Sense, Stroke, Ui,
    Vec2, Widget,
};

/// Colour tokens for one theme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    /// Matte base shared by the window and every control.
    pub base: Color32,
    /// Top-left highlight shadow.
    pub light_shadow: Color32,
    /// Bottom-right shade shadow.
    pub dark_shadow: Color32,
    /// Primary text (>= 7:1 on base).
    pub text: Color32,
    /// Secondary text (>= 4.5:1 on base).
    pub text_secondary: Color32,
    /// Single saturated accent (>= 3:1 on base) for the primary action and state cues.
    pub accent: Color32,
    /// Text drawn on top of an accent fill (>= 4.5:1 on accent).
    pub on_accent: Color32,
}

/// WCAG relative luminance of an sRGB colour (0.0..=1.0).
#[cfg(test)]
pub fn relative_luminance(c: Color32) -> f32 {
    fn channel(v: u8) -> f32 {
        let s = v as f32 / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * channel(c.r()) + 0.7152 * channel(c.g()) + 0.0722 * channel(c.b())
}

/// WCAG contrast ratio between two opaque colours (1.0..=21.0).
#[cfg(test)]
pub fn contrast_ratio(a: Color32, b: Color32) -> f32 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// Palette for a theme.
///
/// The light accent is #c8452a instead of the classic coral #ff6b4a: coral reaches only
/// ~2.2:1 on #e0e5ec (non-text minimum is 3:1) and white labels on it fail 4.5:1.
pub fn palette(theme: Theme) -> Palette {
    match theme {
        Theme::Light => Palette {
            base: Color32::from_rgb(0xe0, 0xe5, 0xec),
            light_shadow: Color32::from_rgba_unmultiplied(255, 255, 255, 217),
            dark_shadow: Color32::from_rgba_unmultiplied(163, 177, 198, 153),
            text: Color32::from_rgb(0x2f, 0x3a, 0x4a),
            text_secondary: Color32::from_rgb(0x5b, 0x65, 0x77),
            accent: Color32::from_rgb(0xc8, 0x45, 0x2a),
            on_accent: Color32::WHITE,
        },
        Theme::Dark => Palette {
            base: Color32::from_rgb(0x2b, 0x2f, 0x36),
            light_shadow: Color32::from_rgba_unmultiplied(255, 255, 255, 15),
            dark_shadow: Color32::from_rgba_unmultiplied(0, 0, 0, 140),
            text: Color32::from_rgb(0xe6, 0xe9, 0xef),
            text_secondary: Color32::from_rgb(0xa9, 0xb1, 0xbf),
            accent: Color32::from_rgb(0xff, 0x6b, 0x4a),
            on_accent: Color32::from_rgb(0x1b, 0x1e, 0x23),
        },
    }
}

/// egui visuals matching the palette: base everywhere, no strokes, accent selection.
pub fn visuals(theme: Theme, pal: &Palette, reduced_motion: bool) -> egui::Visuals {
    let mut v = match theme {
        Theme::Light => egui::Visuals::light(),
        Theme::Dark => egui::Visuals::dark(),
    };
    v.override_text_color = Some(pal.text);
    v.window_fill = pal.base;
    v.panel_fill = pal.base;
    v.extreme_bg_color = pal.base;
    v.faint_bg_color = pal.base;
    v.window_stroke = Stroke::NONE;
    v.window_shadow = epaint::Shadow {
        offset: [8, 8],
        blur: 24,
        spread: 0,
        color: pal.dark_shadow,
    };
    v.popup_shadow = v.window_shadow;
    v.window_corner_radius = CornerRadius::same(20);
    v.menu_corner_radius = CornerRadius::same(14);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.bg_fill = pal.base;
        w.weak_bg_fill = pal.base;
        w.bg_stroke = Stroke::NONE;
        w.fg_stroke.color = pal.text;
        w.corner_radius = CornerRadius::same(14);
        w.expansion = 0.0;
    }
    v.selection.bg_fill = pal.accent;
    v.selection.stroke = Stroke::new(1.0, pal.on_accent);
    v.text_cursor.stroke = Stroke::new(2.0, pal.accent);
    if reduced_motion {
        v.text_cursor.blink = false;
    }
    v
}

fn shadow(offset: f32, blur: f32, color: Color32) -> epaint::Shadow {
    let o = offset.round().clamp(-127.0, 127.0) as i8;
    epaint::Shadow {
        offset: [o, o],
        blur: blur.round().clamp(0.0, 255.0) as u8,
        spread: 0,
        color,
    }
}

/// Raised surface: light shadow up-left, dark shadow down-right, then the base fill.
/// `scale` 1.0 = cards (6 px / 12 px blur); small controls use ~0.5.
pub fn paint_raised(painter: &Painter, rect: Rect, cr: CornerRadius, pal: &Palette, scale: f32) {
    let (o, b) = (6.0 * scale, 12.0 * scale);
    painter.add(shadow(-o, b, pal.light_shadow).as_shape(rect, cr));
    painter.add(shadow(o, b, pal.dark_shadow).as_shape(rect, cr));
    painter.rect_filled(rect, cr, pal.base);
}

/// Pressed/input surface: base fill plus dual INNER shadows (dark up-left, light down-right).
///
/// egui has no inset shadow, so this emulates CSS `inset` geometry: the shadow region is the
/// part of `rect` outside a copy of `rect` shifted by the offset. It is drawn as 1 px rings
/// around the shifted copy, fading in over `blur` px, clipped to `rect`.
pub fn paint_inset(painter: &Painter, rect: Rect, cr: CornerRadius, pal: &Palette, scale: f32) {
    painter.rect_filled(rect, cr, pal.base);
    let clipped = painter.with_clip_rect(rect.intersect(painter.clip_rect()));
    let o = (4.0 * scale).max(1.0);
    let blur = (8.0 * scale).max(2.0);
    inner_band(&clipped, rect, cr, Vec2::splat(o), blur, pal.dark_shadow);
    inner_band(&clipped, rect, cr, Vec2::splat(-o), blur, pal.light_shadow);
    // The clip is rectangular, so bands leak into the rounded corners; repaint everything
    // between the rounded outline and the bounding box with the base colour.
    let m = cr.nw.max(cr.ne).max(cr.sw).max(cr.se) as f32 + 2.0;
    let half = (m * 0.5).round() as u8;
    let mask_cr = CornerRadius {
        nw: cr.nw.saturating_add(half),
        ne: cr.ne.saturating_add(half),
        sw: cr.sw.saturating_add(half),
        se: cr.se.saturating_add(half),
    };
    clipped.rect_stroke(
        rect.expand(m * 0.5),
        mask_cr,
        Stroke::new(m, pal.base),
        egui::StrokeKind::Middle,
    );
}

fn inner_band(painter: &Painter, rect: Rect, cr: CornerRadius, shift: Vec2, blur: f32, c: Color32) {
    // x = signed distance from the shifted copy's edge (positive = toward the rect's edge).
    // Intensity follows smoothstep(-blur/2, blur/2, x), like a Gaussian-blurred CSS inset.
    let hole = rect.translate(shift);
    let half = blur * 0.5;
    let end = shift.x.abs() + blur;
    let mut x = -half + 0.5;
    while x < end {
        let u = ((x + half) / blur).clamp(0.0, 1.0);
        let intensity = u * u * (3.0 - 2.0 * u);
        let alpha = (c.a() as f32 * intensity).round() as u8;
        if alpha > 0 {
            let grow = x.max(0.0) as u8;
            let ring_cr = CornerRadius {
                nw: cr.nw.saturating_add(grow),
                ne: cr.ne.saturating_add(grow),
                sw: cr.sw.saturating_add(grow),
                se: cr.se.saturating_add(grow),
            };
            let color = Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), alpha);
            painter.rect_stroke(
                hole.expand(x),
                ring_cr,
                Stroke::new(1.0, color),
                egui::StrokeKind::Middle,
            );
        }
        x += 1.0;
    }
}

/// 2 px accent focus ring outset by 3 px (the only outline the design allows).
pub fn paint_focus_ring(painter: &Painter, rect: Rect, cr: CornerRadius, pal: &Palette) {
    let ring_cr = cr.at_least(cr.nw.saturating_add(3));
    painter.rect_stroke(
        rect.expand(3.0),
        ring_cr,
        Stroke::new(2.0, pal.accent),
        egui::StrokeKind::Outside,
    );
}

/// True when Windows "Show animations in Windows" is off.
pub fn is_reduced_motion() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SystemParametersInfoW, SPI_GETCLIENTAREAANIMATION,
    };
    let mut enabled: windows_sys::Win32::Foundation::BOOL = 1;
    // SAFETY: SPI_GETCLIENTAREAANIMATION writes one BOOL into the pointed-to local.
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            (&mut enabled as *mut windows_sys::Win32::Foundation::BOOL).cast(),
            0,
        )
    };
    ok != 0 && enabled == 0
}

/// Raised push button; `accent` makes it the filled primary action.
pub struct NeuButton<'a> {
    text: &'a str,
    pal: Palette,
    accent: bool,
    selected: bool,
    enabled: bool,
    min_size: Vec2,
    font_size: f32,
    cr: CornerRadius,
}

impl<'a> NeuButton<'a> {
    /// Regular raised button.
    pub fn new(text: &'a str, pal: Palette) -> Self {
        Self {
            text,
            pal,
            accent: false,
            selected: false,
            enabled: true,
            min_size: Vec2::new(36.0, 34.0),
            font_size: 14.0,
            cr: CornerRadius::same(14),
        }
    }

    /// Filled accent (primary action).
    pub fn accent(mut self, accent: bool) -> Self {
        self.accent = accent;
        self
    }

    /// Selected segment: drawn inset with accent text + underline dot (non-shadow cue).
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Disabled buttons use secondary text and ignore clicks.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Minimum size (hit target).
    pub fn min_size(mut self, size: Vec2) -> Self {
        self.min_size = size;
        self
    }

    /// Label font size.
    pub fn font_size(mut self, size: f32) -> Self {
        self.font_size = size;
        self
    }

    /// Corner radius.
    pub fn corner_radius(mut self, cr: CornerRadius) -> Self {
        self.cr = cr;
        self
    }
}

impl Widget for NeuButton<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let pal = self.pal;
        let text_color = if !self.enabled {
            pal.text_secondary
        } else if self.accent {
            pal.on_accent
        } else {
            pal.text
        };
        let galley = ui.painter().layout_no_wrap(
            self.text.to_owned(),
            egui::FontId::proportional(self.font_size),
            text_color,
        );
        let size = (galley.size() + Vec2::new(28.0, 16.0)).max(self.min_size);
        let sense = if self.enabled {
            Sense::click()
        } else {
            Sense::hover()
        };
        let (rect, response) = ui.allocate_exact_size(size, sense);
        if !ui.is_rect_visible(rect) {
            return response;
        }

        let painter = ui.painter();
        let down = self.enabled && response.is_pointer_button_down_on();
        if self.accent && self.enabled {
            if !down {
                painter.add(shadow(-3.0, 8.0, pal.light_shadow).as_shape(rect, self.cr));
                painter.add(shadow(3.0, 8.0, pal.dark_shadow).as_shape(rect, self.cr));
            }
            painter.rect_filled(rect, self.cr, pal.accent);
        } else if down || self.selected {
            paint_inset(painter, rect, self.cr, &pal, 0.6);
        } else {
            paint_raised(painter, rect, self.cr, &pal, 0.5);
        }

        let mut text_pos = rect.center() - galley.size() * 0.5;
        if self.selected {
            text_pos.y -= 2.0;
            painter.circle_filled(
                Pos2::new(rect.center().x, rect.bottom() - 6.0),
                2.5,
                pal.accent,
            );
        }
        painter.galley(text_pos, galley, text_color);

        if response.has_focus() {
            paint_focus_ring(painter, rect, self.cr, &pal);
        }
        response
    }
}

/// Pill toggle. ON = inset track + accent knob + "On"; OFF = raised + grey knob + "Off".
pub fn neu_toggle(ui: &mut Ui, value: &mut bool, pal: &Palette, label: &str) -> Response {
    let response = ui
        .horizontal(|ui| {
            let size = Vec2::new(52.0, 32.0);
            let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click());
            if resp.clicked() {
                *value = !*value;
                resp.mark_changed();
            }
            if ui.is_rect_visible(rect) {
                let cr = CornerRadius::same(16);
                let painter = ui.painter();
                let r = 11.0;
                if *value {
                    paint_inset(painter, rect, cr, pal, 0.5);
                    let c = Pos2::new(rect.right() - r - 5.0, rect.center().y);
                    painter.circle_filled(c, r, pal.accent);
                } else {
                    paint_raised(painter, rect, cr, pal, 0.4);
                    let c = Pos2::new(rect.left() + r + 5.0, rect.center().y);
                    painter.circle_filled(c, r, pal.text_secondary);
                }
                if resp.has_focus() {
                    paint_focus_ring(painter, rect, cr, pal);
                }
            }
            let state = if *value { "On" } else { "Off" };
            let text = if label.is_empty() {
                state.to_owned()
            } else {
                format!("{label} · {state}")
            };
            ui.label(egui::RichText::new(text).color(pal.text));
            resp
        })
        .inner;
    response
}

/// Slider: inset track, accent fill, raised knob. Arrow keys step 1/40 of the range
/// (Shift = 1/10) while focused.
pub fn neu_slider(
    ui: &mut Ui,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    pal: &Palette,
    width: f32,
) -> Response {
    let (min, max) = (*range.start(), *range.end());
    let span = (max - min).max(f32::EPSILON);
    let knob_r = 9.0;
    let (rect, mut resp) =
        ui.allocate_exact_size(Vec2::new(width.max(60.0), 32.0), Sense::click_and_drag());

    if resp.dragged() || resp.clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            let t = (p.x - rect.left() - knob_r) / (rect.width() - 2.0 * knob_r);
            *value = min + t.clamp(0.0, 1.0) * span;
            resp.mark_changed();
        }
    }
    if resp.has_focus() {
        let (left, right, shift) = ui.input(|i| {
            (
                i.key_pressed(Key::ArrowLeft) || i.key_pressed(Key::ArrowDown),
                i.key_pressed(Key::ArrowRight) || i.key_pressed(Key::ArrowUp),
                i.modifiers.shift,
            )
        });
        let step = span / if shift { 10.0 } else { 40.0 };
        if left || right {
            let delta = if right { step } else { -step };
            *value = (*value + delta).clamp(min, max);
            resp.mark_changed();
        }
    }

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let track = Rect::from_center_size(rect.center(), Vec2::new(rect.width(), 10.0));
        let track_cr = CornerRadius::same(5);
        paint_inset(painter, track, track_cr, pal, 0.6);

        let t = ((*value - min) / span).clamp(0.0, 1.0);
        let knob_x = rect.left() + knob_r + t * (rect.width() - 2.0 * knob_r);
        if t > 0.0 {
            let fill = Rect::from_min_max(track.min, Pos2::new(knob_x, track.max.y));
            painter.rect_filled(fill, track_cr, pal.accent);
        }
        let knob = Rect::from_center_size(Pos2::new(knob_x, rect.center().y), Vec2::splat(18.0));
        let knob_cr = CornerRadius::same(9);
        if resp.is_pointer_button_down_on() {
            paint_inset(painter, knob, knob_cr, pal, 0.3);
        } else {
            paint_raised(painter, knob, knob_cr, pal, 0.35);
        }
        painter.circle_filled(knob.center(), 3.0, pal.accent);
        if resp.has_focus() {
            paint_focus_ring(painter, rect, CornerRadius::same(10), pal);
        }
    }
    resp
}

/// Inset single-line text field without any frame stroke.
pub fn neu_text_field(
    ui: &mut Ui,
    text: &mut String,
    hint: &str,
    pal: &Palette,
    width: f32,
) -> Response {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 34.0), Sense::hover());
    let cr = CornerRadius::same(14);
    paint_inset(ui.painter(), rect, cr, pal, 0.5);
    let inner = rect.shrink2(Vec2::new(12.0, 7.0));
    let resp = ui
        .scope_builder(egui::UiBuilder::new().max_rect(inner), |ui| {
            ui.add(
                egui::TextEdit::singleline(text)
                    .frame(egui::Frame::NONE)
                    .desired_width(inner.width())
                    .text_color(pal.text)
                    .hint_text(egui::RichText::new(hint).color(pal.text_secondary)),
            )
        })
        .inner;
    if resp.has_focus() {
        paint_focus_ring(ui.painter(), rect, cr, pal);
    }
    resp
}

/// Inset multi-line text field without any frame stroke.
pub fn neu_text_area(ui: &mut Ui, text: &mut String, pal: &Palette, rows: usize) -> Response {
    let height = rows as f32 * 18.0 + 16.0;
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    let cr = CornerRadius::same(14);
    paint_inset(ui.painter(), rect, cr, pal, 0.5);
    let inner = rect.shrink(8.0);
    let resp = ui
        .scope_builder(egui::UiBuilder::new().max_rect(inner), |ui| {
            ui.add(
                egui::TextEdit::multiline(text)
                    .frame(egui::Frame::NONE)
                    .desired_rows(rows)
                    .desired_width(inner.width())
                    .text_color(pal.text),
            )
        })
        .inner;
    if resp.has_focus() {
        paint_focus_ring(ui.painter(), rect, cr, pal);
    }
    resp
}

/// Raised card with 20 px corners and 18 px padding; fills the available width.
pub fn neu_card<R>(ui: &mut Ui, pal: &Palette, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    let margin = 18.0;
    let bg = ui.painter().add(epaint::Shape::Noop);
    let outer = ui.available_rect_before_wrap();
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(outer.shrink(margin))
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    child.set_width(outer.width() - 2.0 * margin);
    let ret = add_contents(&mut child);
    // Fixed width keeps stacked cards aligned even when a row's content is narrower.
    let used = Rect::from_min_size(
        outer.min,
        Vec2::new(outer.width(), child.min_rect().height() + 2.0 * margin),
    );
    let cr = CornerRadius::same(20);
    ui.painter().set(
        bg,
        epaint::Shape::Vec(vec![
            shadow(-6.0, 12.0, pal.light_shadow)
                .as_shape(used, cr)
                .into(),
            shadow(6.0, 12.0, pal.dark_shadow).as_shape(used, cr).into(),
            epaint::Shape::rect_filled(used, cr, pal.base),
        ]),
    );
    ui.advance_cursor_after_rect(used);
    ret
}

/// Small inset tag (e.g. "drums") with secondary text.
pub fn neu_tag(ui: &mut Ui, text: &str, pal: &Palette) -> Response {
    let galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        egui::FontId::proportional(11.0),
        pal.text_secondary,
    );
    let size = galley.size() + Vec2::new(16.0, 8.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    paint_inset(ui.painter(), rect, CornerRadius::same(10), pal, 0.3);
    ui.painter().galley(
        rect.center() - galley.size() * 0.5,
        galley,
        pal.text_secondary,
    );
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contrast_black_white_is_21() {
        let ratio = contrast_ratio(Color32::BLACK, Color32::WHITE);
        assert!((ratio - 21.0).abs() <= 0.01, "got {ratio}");
        assert!((contrast_ratio(Color32::WHITE, Color32::WHITE) - 1.0).abs() <= 0.001);
    }

    #[test]
    fn palette_bases_match_spec() {
        assert_eq!(
            palette(Theme::Light).base,
            Color32::from_rgb(0xe0, 0xe5, 0xec)
        );
        assert_eq!(
            palette(Theme::Dark).base,
            Color32::from_rgb(0x2b, 0x2f, 0x36)
        );
    }

    #[test]
    fn coral_fails_on_light_base_so_accent_was_darkened() {
        let coral = Color32::from_rgb(0xff, 0x6b, 0x4a);
        let base = palette(Theme::Light).base;
        assert!(contrast_ratio(coral, base) < 3.0);
    }

    fn assert_theme_contrast(theme: Theme) {
        let p = palette(theme);
        let text = contrast_ratio(p.text, p.base);
        let sec = contrast_ratio(p.text_secondary, p.base);
        let accent = contrast_ratio(p.accent, p.base);
        let on_accent = contrast_ratio(p.on_accent, p.accent);
        assert!(text >= 7.0, "{theme:?} text {text:.2}");
        assert!(sec >= 4.5, "{theme:?} secondary {sec:.2}");
        assert!(accent >= 3.0, "{theme:?} accent {accent:.2}");
        assert!(on_accent >= 4.5, "{theme:?} on_accent {on_accent:.2}");
    }

    #[test]
    fn light_theme_contrast() {
        assert_theme_contrast(Theme::Light);
    }

    #[test]
    fn dark_theme_contrast() {
        assert_theme_contrast(Theme::Dark);
    }

    #[test]
    fn visuals_have_no_strokes_and_base_fill() {
        for theme in [Theme::Light, Theme::Dark] {
            let p = palette(theme);
            let v = visuals(theme, &p, false);
            assert_eq!(v.window_stroke, Stroke::NONE);
            assert_eq!(v.panel_fill, p.base);
            for w in [v.widgets.inactive, v.widgets.hovered, v.widgets.active] {
                assert_eq!(w.bg_stroke, Stroke::NONE);
                assert_eq!(w.bg_fill, p.base);
            }
        }
    }
}
