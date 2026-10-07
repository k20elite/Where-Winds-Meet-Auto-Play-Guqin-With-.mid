//! Windows keyboard output and game-window discovery.

use crate::layout::KeyLayout;
use crate::mapping::{Modifier, Stroke};
use crate::settings::{InputBackend, Settings};
use std::collections::HashMap;
use std::mem::size_of;
use std::thread::sleep;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM, TRUE};
use windows_sys::Win32::System::Threading::GetCurrentProcessId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    MapVirtualKeyW, SendInput, VkKeyScanW, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
    KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MAPVK_VK_TO_VSC, VK_CONTROL, VK_SHIFT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, IsWindow, IsWindowVisible, PostMessageW, WM_KEYDOWN, WM_KEYUP,
};

/// Lowest-level keyboard event sink.
pub trait KeySink {
    /// Send virtual key down or up event.
    fn key(&mut self, vk: u16, down: bool);

    /// Send momentary modifier combo [mod down, key down, mod up].
    fn combo(&mut self, mod_vk: u16, vk: u16) {
        self.key(mod_vk, true);
        self.key(vk, true);
        self.key(mod_vk, false);
    }
}

/// Convert character to virtual-key code according to active keyboard layout.
/// Returns None if character requires shift state or has no virtual-key mapping.
pub fn char_to_vk(c: char) -> Option<u16> {
    let mut utf16 = [0u16; 2];
    let encoded = c.encode_utf16(&mut utf16);
    if encoded.len() != 1 {
        return None;
    }

    // SAFETY: VkKeyScanW inspects user active keyboard layout without side effects.
    let res = unsafe { VkKeyScanW(encoded[0]) };
    if res == -1 {
        return None;
    }

    let high = (res >> 8) & 0xFF;
    if high != 0 {
        return None;
    }

    Some((res & 0xFF) as u16)
}

/// Check if window title matches any keyword case-insensitively.
pub fn title_matches(title: &str, keywords: &[String]) -> bool {
    let title_lower = title.to_lowercase();
    keywords.iter().any(|k| {
        let kw_trimmed = k.trim();
        if kw_trimmed.is_empty() {
            false
        } else {
            title_lower.contains(&kw_trimmed.to_lowercase())
        }
    })
}

/// Top-level window finder matching title keywords.
pub struct GameWindow {
    keywords: Vec<String>,
    cached: Option<isize>,
    last_search: Option<Instant>,
}

impl GameWindow {
    /// Create new game window finder with given title keywords.
    pub fn new(keywords: Vec<String>) -> Self {
        Self {
            keywords,
            cached: None,
            last_search: None,
        }
    }

    /// Find valid game window handle, re-enumerating with rate limit when cache invalid.
    pub fn find(&mut self) -> Option<isize> {
        if let Some(hwnd) = self.cached {
            // SAFETY: IsWindow checks validity of window handle.
            let valid = unsafe { IsWindow(hwnd as HWND) != 0 };
            if valid {
                return Some(hwnd);
            }
            self.cached = None;
        }

        let now = Instant::now();
        if let Some(last) = self.last_search {
            if now.duration_since(last) < Duration::from_millis(500) {
                return None;
            }
        }
        self.last_search = Some(now);

        let mut found: Option<isize> = None;
        let mut ctx = SearchContext {
            keywords: &self.keywords,
            found: &mut found,
        };

        // SAFETY: EnumWindows passes valid pointer to stack context.
        unsafe {
            EnumWindows(
                Some(enum_windows_proc),
                &mut ctx as *mut SearchContext as LPARAM,
            );
        }

        self.cached = found;
        found
    }

    /// Check if game window handle is found and valid.
    pub fn is_found(&mut self) -> bool {
        self.find().is_some()
    }
}

struct SearchContext<'a> {
    keywords: &'a [String],
    found: &'a mut Option<isize>,
}

unsafe extern "system" fn enum_windows_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: lparam is valid pointer to SearchContext during EnumWindows call.
    let ctx = unsafe { &mut *(lparam as *mut SearchContext<'_>) };

    // SAFETY: IsWindowVisible checks whether window is top-level visible window.
    if unsafe { IsWindowVisible(hwnd) } == 0 {
        return TRUE;
    }

    // SAFETY: GetCurrentProcessId retrieves calling process ID.
    let our_pid = unsafe { GetCurrentProcessId() };
    let mut win_pid = 0u32;
    // SAFETY: GetWindowThreadProcessId writes thread/process ID to valid pointer.
    unsafe {
        GetWindowThreadProcessId(hwnd, &mut win_pid);
    }
    if win_pid == our_pid {
        return TRUE;
    }

    // SAFETY: GetWindowTextLengthW returns buffer length needed for title.
    let len = unsafe { GetWindowTextLengthW(hwnd) };
    if len <= 0 {
        return TRUE;
    }

    let mut buf = vec![0u16; (len + 1) as usize];
    // SAFETY: GetWindowTextW writes up to buf.len() WCHARs into valid slice.
    let copied = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    if copied <= 0 {
        return TRUE;
    }

    let title = String::from_utf16_lossy(&buf[..copied as usize]);
    if title_matches(&title, ctx.keywords) {
        *ctx.found = Some(hwnd as isize);
        return 0; // Stop enumeration
    }

    TRUE
}

/// Window sink posting WM_KEYDOWN and WM_KEYUP to target game window.
pub struct WindowSink {
    finder: GameWindow,
}

impl WindowSink {
    /// Create new WindowSink targeting windows matching keywords.
    pub fn new(keywords: Vec<String>) -> Self {
        Self {
            finder: GameWindow::new(keywords),
        }
    }

    /// Check if target game window currently exists.
    #[allow(dead_code)] // reason: convenient inspection helper for background window sink
    pub fn is_found(&mut self) -> bool {
        self.finder.is_found()
    }
}

impl KeySink for WindowSink {
    fn key(&mut self, vk: u16, down: bool) {
        let hwnd = match self.finder.find() {
            Some(h) => h as HWND,
            None => return,
        };

        // SAFETY: MapVirtualKeyW converts virtual-key to hardware scan code.
        let vsc = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) } as usize;

        let mut lparam: usize = 1 | ((vsc & 0xFF) << 16);
        let msg = if down {
            WM_KEYDOWN
        } else {
            lparam |= (1 << 30) | (1 << 31);
            WM_KEYUP
        };

        // SAFETY: PostMessageW posts asynchronous keyboard message to target HWND.
        unsafe {
            PostMessageW(hwnd, msg, vk as usize, lparam as isize);
        }
    }
}

/// Global sink using Windows SendInput with scan codes.
pub struct GlobalSink;

impl Default for GlobalSink {
    fn default() -> Self {
        Self::new()
    }
}

impl GlobalSink {
    /// Create new GlobalSink instance.
    pub fn new() -> Self {
        Self
    }
}

fn make_input(vk: u16, key_up: bool) -> INPUT {
    // SAFETY: MapVirtualKeyW converts virtual-key to hardware scan code.
    let sc = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) } as u16;
    let mut flags = KEYEVENTF_SCANCODE;
    if key_up {
        flags |= KEYEVENTF_KEYUP;
    }

    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: 0,
                wScan: sc,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// True when the foreground window belongs to this process (our own UI has focus).
fn own_window_focused() -> bool {
    // SAFETY: GetForegroundWindow has no preconditions; it may return null.
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_null() {
        return false;
    }
    let mut pid = 0u32;
    // SAFETY: hwnd came from the OS and pid is a valid out-pointer for this call.
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    // SAFETY: GetCurrentProcessId has no preconditions.
    pid == unsafe { GetCurrentProcessId() }
}

// SendInput goes to whatever window has focus. Key-downs are dropped while our own window
// is focused so playback never types into the app itself (search box, focused buttons).
// Key-ups always pass so nothing can stay stuck down.
impl KeySink for GlobalSink {
    fn key(&mut self, vk: u16, down: bool) {
        if down && own_window_focused() {
            return;
        }
        let input = make_input(vk, !down);
        // SAFETY: SendInput sends hardware input event to active foreground window.
        unsafe {
            SendInput(1, &input, size_of::<INPUT>() as i32);
        }
    }

    fn combo(&mut self, mod_vk: u16, vk: u16) {
        if own_window_focused() {
            return;
        }
        let mut inputs = [
            make_input(mod_vk, false),
            make_input(vk, false),
            make_input(mod_vk, true),
        ];
        // SAFETY: SendInput sends atomic array of hardware input events.
        unsafe {
            SendInput(
                inputs.len() as u32,
                inputs.as_mut_ptr(),
                size_of::<INPUT>() as i32,
            );
        }
    }
}

/// Stroke driver translating musical strokes to keyboard events.
pub struct Driver<S: KeySink> {
    sink: S,
    #[allow(dead_code)] // reason: retained for introspection and future layout updates
    layout: KeyLayout,
    modifier_delay: Duration,
    vks: [Option<u16>; 21],
    down_counts: HashMap<u16, u32>,
}

fn compute_vks(layout: &KeyLayout) -> [Option<u16>; 21] {
    let mut vks = [None; 21];
    for (i, item) in vks.iter_mut().enumerate() {
        if let Some(c) = layout.key(i as u8) {
            *item = char_to_vk(c);
        }
    }
    vks
}

impl<S: KeySink> Driver<S> {
    /// Create new Driver wrapping target sink.
    pub fn new(sink: S, layout: KeyLayout, modifier_delay: Duration) -> Self {
        let vks = compute_vks(&layout);
        Self {
            sink,
            layout,
            modifier_delay,
            vks,
            down_counts: HashMap::new(),
        }
    }

    /// Update active keyboard layout.
    #[allow(dead_code)] // reason: dynamic layout reconfiguration API
    pub fn set_layout(&mut self, layout: KeyLayout) {
        self.vks = compute_vks(&layout);
        self.layout = layout;
    }

    /// Update modifier delay before note key.
    #[allow(dead_code)] // reason: dynamic timing reconfiguration API
    pub fn set_modifier_delay(&mut self, d: Duration) {
        self.modifier_delay = d;
    }

    /// Immutable reference to underlying sink (for testing).
    #[allow(dead_code)] // reason: test inspection helper
    pub fn sink(&self) -> &S {
        &self.sink
    }

    fn slot_vk(&self, slot: u8) -> Option<u16> {
        let idx = slot as usize;
        if idx < self.vks.len() {
            self.vks[idx]
        } else {
            None
        }
    }

    /// Press a stroke with momentary modifiers if needed.
    pub fn press(&mut self, stroke: Stroke) {
        let vk = match self.slot_vk(stroke.slot) {
            Some(v) => v,
            None => return,
        };

        match stroke.modifier {
            Modifier::None => {
                let count = self.down_counts.entry(vk).or_insert(0);
                if *count > 0 {
                    self.sink.key(vk, false);
                }
                *count += 1;
                self.sink.key(vk, true);
            }
            Modifier::Shift | Modifier::Ctrl => {
                let mod_vk = if stroke.modifier == Modifier::Shift {
                    VK_SHIFT
                } else {
                    VK_CONTROL
                };

                let count = self.down_counts.entry(vk).or_insert(0);
                if *count > 0 {
                    self.sink.key(vk, false);
                }
                *count += 1;

                if self.modifier_delay.is_zero() {
                    self.sink.combo(mod_vk, vk);
                } else {
                    self.sink.key(mod_vk, true);
                    sleep(self.modifier_delay);
                    self.sink.key(vk, true);
                    self.sink.key(mod_vk, false);
                }
            }
        }
    }

    /// Release a stroke.
    pub fn release(&mut self, stroke: Stroke) {
        let vk = match self.slot_vk(stroke.slot) {
            Some(v) => v,
            None => return,
        };

        if let Some(count) = self.down_counts.get_mut(&vk) {
            if *count > 0 {
                *count -= 1;
                if *count == 0 {
                    self.down_counts.remove(&vk);
                    self.sink.key(vk, false);
                }
            }
        }
    }

    /// Release all currently held virtual keys.
    pub fn release_all(&mut self) {
        for &vk in self.down_counts.keys() {
            self.sink.key(vk, false);
        }
        self.down_counts.clear();
        self.sink.key(VK_SHIFT, false);
        self.sink.key(VK_CONTROL, false);
    }
}

/// High-level output facade configured from Settings.
pub enum Output {
    /// Window message backend driver.
    Window(Driver<WindowSink>),
    /// Global hardware input backend driver.
    Global(Driver<GlobalSink>),
}

impl Output {
    /// Create Output backend from application settings.
    pub fn from_settings(settings: &Settings) -> Self {
        let layout = settings.key_layout();
        let delay = Duration::from_millis(settings.modifier_delay_ms as u64);
        match settings.backend {
            InputBackend::Window => {
                let sink = WindowSink::new(settings.window_keywords.clone());
                Output::Window(Driver::new(sink, layout, delay))
            }
            InputBackend::Global => {
                let sink = GlobalSink::new();
                Output::Global(Driver::new(sink, layout, delay))
            }
        }
    }

    /// Press a stroke.
    pub fn press(&mut self, stroke: Stroke) {
        match self {
            Output::Window(d) => d.press(stroke),
            Output::Global(d) => d.press(stroke),
        }
    }

    /// Release a stroke.
    pub fn release(&mut self, stroke: Stroke) {
        match self {
            Output::Window(d) => d.release(stroke),
            Output::Global(d) => d.release(stroke),
        }
    }

    /// Release all held keys.
    pub fn release_all(&mut self) {
        match self {
            Output::Window(d) => d.release_all(),
            Output::Global(d) => d.release_all(),
        }
    }

    /// Check if target game window is found.
    #[allow(dead_code)] // reason: convenient inspection helper on Output enum
    pub fn game_found(&mut self) -> bool {
        match self {
            Output::Window(d) => d.sink.is_found(),
            Output::Global(_) => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct MockSink {
        events: Vec<(u16, bool)>,
    }

    impl KeySink for MockSink {
        fn key(&mut self, vk: u16, down: bool) {
            self.events.push((vk, down));
        }
    }

    #[test]
    fn test_title_matches_cases() {
        let kw = vec!["Where Winds Meet".into(), "燕云十六声".into(), "WWM".into()];
        assert!(title_matches("Where Winds Meet - Client", &kw));
        assert!(title_matches("where winds meet", &kw));
        assert!(title_matches("Game: 燕云十六声 v1.0", &kw));
        assert!(title_matches("[wwm] Play", &kw));

        // Empty keywords
        assert!(!title_matches("Where Winds Meet", &[]));
        assert!(!title_matches("Where Winds Meet", &["   ".into()]));
        assert!(!title_matches("Other Game", &kw));
    }

    #[test]
    fn test_char_to_vk_standard_and_invalid() {
        for c in 'a'..='z' {
            let expected = (c as u8).to_ascii_uppercase() as u16;
            assert_eq!(char_to_vk(c), Some(expected), "char '{c}'");
        }
        for c in '0'..='9' {
            let expected = c as u16;
            assert_eq!(char_to_vk(c), Some(expected), "char '{c}'");
        }

        // Accented or emoji / unshifted invalid chars
        assert_eq!(char_to_vk('€'), None);
        assert_eq!(char_to_vk('\u{1F600}'), None);
    }

    #[test]
    fn test_driver_natural_press_release() {
        let layout = KeyLayout::preset(crate::layout::LayoutPreset::Qwerty).unwrap();
        let mut driver = Driver::new(MockSink::default(), layout, Duration::ZERO);

        let vk_z = char_to_vk('z').unwrap();
        let s = Stroke {
            slot: 0,
            modifier: Modifier::None,
        };

        driver.press(s);
        driver.release(s);

        assert_eq!(driver.sink().events, vec![(vk_z, true), (vk_z, false)]);
    }

    #[test]
    fn test_driver_shift_momentary_sequence() {
        let layout = KeyLayout::preset(crate::layout::LayoutPreset::Qwerty).unwrap();
        let mut driver = Driver::new(MockSink::default(), layout, Duration::ZERO);

        let vk_z = char_to_vk('z').unwrap();
        let s = Stroke {
            slot: 0,
            modifier: Modifier::Shift,
        };

        driver.press(s);
        driver.release(s);

        // Expected: Shift down, Z down, Shift up, then later Z up
        assert_eq!(
            driver.sink().events,
            vec![
                (VK_SHIFT, true),
                (vk_z, true),
                (VK_SHIFT, false),
                (vk_z, false),
            ]
        );
    }

    #[test]
    fn test_driver_ctrl_momentary_sequence() {
        let layout = KeyLayout::preset(crate::layout::LayoutPreset::Qwerty).unwrap();
        let mut driver = Driver::new(MockSink::default(), layout, Duration::ZERO);

        let vk_z = char_to_vk('z').unwrap();
        let s = Stroke {
            slot: 0,
            modifier: Modifier::Ctrl,
        };

        driver.press(s);
        driver.release(s);

        assert_eq!(
            driver.sink().events,
            vec![
                (VK_CONTROL, true),
                (vk_z, true),
                (VK_CONTROL, false),
                (vk_z, false),
            ]
        );
    }

    #[test]
    fn test_driver_same_slot_retrigger_and_double_release() {
        let layout = KeyLayout::preset(crate::layout::LayoutPreset::Qwerty).unwrap();
        let mut driver = Driver::new(MockSink::default(), layout, Duration::ZERO);

        let vk_z = char_to_vk('z').unwrap();
        let s = Stroke {
            slot: 0,
            modifier: Modifier::None,
        };

        driver.press(s);
        driver.press(s);
        driver.release(s);
        driver.release(s);

        assert_eq!(
            driver.sink().events,
            vec![
                (vk_z, true),
                (vk_z, false), // re-trigger up
                (vk_z, true),  // re-trigger down
                (vk_z, false), // final release
            ]
        );
    }

    #[test]
    fn test_driver_release_not_down_noop() {
        let layout = KeyLayout::preset(crate::layout::LayoutPreset::Qwerty).unwrap();
        let mut driver = Driver::new(MockSink::default(), layout, Duration::ZERO);

        let s = Stroke {
            slot: 0,
            modifier: Modifier::None,
        };
        driver.release(s);
        assert!(driver.sink().events.is_empty());
    }

    #[test]
    fn test_driver_unknown_stroke_slot_ignored() {
        let layout = KeyLayout::preset(crate::layout::LayoutPreset::Qwerty).unwrap();
        let mut driver = Driver::new(MockSink::default(), layout, Duration::ZERO);

        let s = Stroke {
            slot: 50, // slot > 20 has no key in KeyLayout
            modifier: Modifier::None,
        };
        driver.press(s);
        driver.release(s);
        assert!(driver.sink().events.is_empty());
    }

    #[test]
    fn test_driver_release_all_clears_keys_and_modifiers() {
        let layout = KeyLayout::preset(crate::layout::LayoutPreset::Qwerty).unwrap();
        let mut driver = Driver::new(MockSink::default(), layout, Duration::ZERO);

        let vk_z = char_to_vk('z').unwrap();
        let vk_x = char_to_vk('x').unwrap();

        driver.press(Stroke {
            slot: 0,
            modifier: Modifier::None,
        });
        driver.press(Stroke {
            slot: 1,
            modifier: Modifier::None,
        });

        driver.release_all();

        // Must send up for z and x (in any order), followed by safety up for SHIFT and CONTROL
        let evs = &driver.sink().events;
        assert!(evs.contains(&(vk_z, false)));
        assert!(evs.contains(&(vk_x, false)));
        assert_eq!(evs[evs.len() - 2], (VK_SHIFT, false));
        assert_eq!(evs[evs.len() - 1], (VK_CONTROL, false));
    }
}
