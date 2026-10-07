//! Global hotkey parsing, registration, and background dispatcher thread.

use std::fmt;
use std::sync::mpsc::channel;
use std::thread::{self, JoinHandle};
use windows_sys::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, VK_ADD,
    VK_DECIMAL, VK_DELETE, VK_DIVIDE, VK_DOWN, VK_END, VK_F1, VK_F10, VK_F11, VK_F12, VK_F13,
    VK_F14, VK_F15, VK_F16, VK_F17, VK_F18, VK_F19, VK_F2, VK_F20, VK_F21, VK_F22, VK_F23, VK_F24,
    VK_F3, VK_F4, VK_F5, VK_F6, VK_F7, VK_F8, VK_F9, VK_HOME, VK_INSERT, VK_LEFT, VK_MULTIPLY,
    VK_NEXT, VK_NUMLOCK, VK_NUMPAD0, VK_NUMPAD1, VK_NUMPAD2, VK_NUMPAD3, VK_NUMPAD4, VK_NUMPAD5,
    VK_NUMPAD6, VK_NUMPAD7, VK_NUMPAD8, VK_NUMPAD9, VK_OEM_4, VK_OEM_6, VK_PAUSE, VK_PRIOR,
    VK_RIGHT, VK_SCROLL, VK_SUBTRACT, VK_UP,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetMessageW, PostThreadMessageW, MSG, WM_HOTKEY, WM_QUIT,
};

/// Playback and UI control actions triggered by global hotkeys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    /// Toggle playback pause state.
    PlayPause,
    /// Stop playback and reset playlist cursor.
    Stop,
    /// Jump to next track in queue.
    Next,
    /// Jump to previous track in queue.
    Prev,
    /// Switch to next NoteMode.
    ModeNext,
    /// Switch to previous NoteMode.
    ModePrev,
}

/// Hotkey specification combining Win32 modifier flags and virtual-key code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HotkeySpec {
    /// Win32 modifier flags (MOD_CONTROL, MOD_ALT, MOD_SHIFT, MOD_NOREPEAT).
    pub mods: u32,
    /// Target virtual-key code.
    pub vk: u16,
}

/// Errors occurring while parsing hotkey strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyError {
    /// The hotkey string is empty.
    Empty,
    /// Unrecognized key or modifier token.
    Unknown(String),
    /// Modifier specified more than once.
    Duplicate(String),
    /// Single alphanumeric key used without modifiers.
    BareKey(String),
}

impl fmt::Display for HotkeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HotkeyError::Empty => write!(f, "hotkey is empty"),
            HotkeyError::Unknown(name) => write!(f, "unknown key '{name}'"),
            HotkeyError::Duplicate(_) => write!(f, "duplicate modifier"),
            HotkeyError::BareKey(name) => write!(f, "key '{name}' needs Ctrl/Alt/Shift"),
        }
    }
}

impl std::error::Error for HotkeyError {}

fn parse_single_key(key: &str) -> Option<u16> {
    let lower = key.to_lowercase();
    match lower.as_str() {
        "f1" => Some(VK_F1),
        "f2" => Some(VK_F2),
        "f3" => Some(VK_F3),
        "f4" => Some(VK_F4),
        "f5" => Some(VK_F5),
        "f6" => Some(VK_F6),
        "f7" => Some(VK_F7),
        "f8" => Some(VK_F8),
        "f9" => Some(VK_F9),
        "f10" => Some(VK_F10),
        "f11" => Some(VK_F11),
        "f12" => Some(VK_F12),
        "f13" => Some(VK_F13),
        "f14" => Some(VK_F14),
        "f15" => Some(VK_F15),
        "f16" => Some(VK_F16),
        "f17" => Some(VK_F17),
        "f18" => Some(VK_F18),
        "f19" => Some(VK_F19),
        "f20" => Some(VK_F20),
        "f21" => Some(VK_F21),
        "f22" => Some(VK_F22),
        "f23" => Some(VK_F23),
        "f24" => Some(VK_F24),
        "scrolllock" | "scroll_lock" | "scroll" => Some(VK_SCROLL),
        "pause" => Some(VK_PAUSE),
        "insert" | "ins" => Some(VK_INSERT),
        "delete" | "del" => Some(VK_DELETE),
        "home" => Some(VK_HOME),
        "end" => Some(VK_END),
        "pageup" | "pgup" => Some(VK_PRIOR),
        "pagedown" | "pgdn" => Some(VK_NEXT),
        "numlock" => Some(VK_NUMLOCK),
        "up" => Some(VK_UP),
        "down" => Some(VK_DOWN),
        "left" => Some(VK_LEFT),
        "right" => Some(VK_RIGHT),
        "[" => Some(VK_OEM_4),
        "]" => Some(VK_OEM_6),
        "num0" | "numpad0" => Some(VK_NUMPAD0),
        "num1" | "numpad1" => Some(VK_NUMPAD1),
        "num2" | "numpad2" => Some(VK_NUMPAD2),
        "num3" | "numpad3" => Some(VK_NUMPAD3),
        "num4" | "numpad4" => Some(VK_NUMPAD4),
        "num5" | "numpad5" => Some(VK_NUMPAD5),
        "num6" | "numpad6" => Some(VK_NUMPAD6),
        "num7" | "numpad7" => Some(VK_NUMPAD7),
        "num8" | "numpad8" => Some(VK_NUMPAD8),
        "num9" | "numpad9" => Some(VK_NUMPAD9),
        "multiply" | "num*" => Some(VK_MULTIPLY),
        "add" | "num+" => Some(VK_ADD),
        "subtract" | "num-" => Some(VK_SUBTRACT),
        "decimal" | "num." => Some(VK_DECIMAL),
        "divide" | "num/" => Some(VK_DIVIDE),
        s if s.len() == 1 => {
            let c = s.chars().next()?;
            if c.is_ascii_alphanumeric() {
                Some((c as u8).to_ascii_uppercase() as u16)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Parse hotkey string into HotkeySpec.
pub fn parse(s: &str) -> Result<HotkeySpec, HotkeyError> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err(HotkeyError::Empty);
    }

    let parts: Vec<&str> = trimmed.split('+').map(|p| p.trim()).collect();
    if parts.is_empty() || parts.iter().any(|p| p.is_empty()) {
        return Err(HotkeyError::Empty);
    }

    let mut has_ctrl = false;
    let mut has_alt = false;
    let mut has_shift = false;
    let mut key_part: Option<&str> = None;

    for &part in &parts {
        let lower = part.to_lowercase();
        match lower.as_str() {
            "ctrl" | "control" => {
                if has_ctrl {
                    return Err(HotkeyError::Duplicate(part.to_string()));
                }
                has_ctrl = true;
            }
            "alt" => {
                if has_alt {
                    return Err(HotkeyError::Duplicate(part.to_string()));
                }
                has_alt = true;
            }
            "shift" => {
                if has_shift {
                    return Err(HotkeyError::Duplicate(part.to_string()));
                }
                has_shift = true;
            }
            _ => {
                if key_part.is_some() {
                    return Err(HotkeyError::Unknown(part.to_string()));
                }
                key_part = Some(part);
            }
        }
    }

    let raw_key = match key_part {
        Some(k) => k,
        None => return Err(HotkeyError::Empty),
    };

    let vk = match parse_single_key(raw_key) {
        Some(vk) => vk,
        None => return Err(HotkeyError::Unknown(raw_key.to_string())),
    };

    let has_any_mod = has_ctrl || has_alt || has_shift;
    if !has_any_mod && raw_key.len() == 1 {
        let c = raw_key.chars().next().expect("length checked to be 1");
        if c.is_ascii_alphanumeric() {
            return Err(HotkeyError::BareKey(raw_key.to_string()));
        }
    }

    let mut mods = MOD_NOREPEAT;
    if has_ctrl {
        mods |= MOD_CONTROL;
    }
    if has_alt {
        mods |= MOD_ALT;
    }
    if has_shift {
        mods |= MOD_SHIFT;
    }

    Ok(HotkeySpec { mods, vk })
}

/// Convert Settings hotkey mappings to parsed specs and errors.
#[allow(clippy::type_complexity)]
pub fn from_settings(
    h: &crate::settings::Hotkeys,
) -> (Vec<(Action, HotkeySpec)>, Vec<(Action, String)>) {
    let pairs = [
        (Action::PlayPause, &h.play_pause),
        (Action::Stop, &h.stop),
        (Action::Next, &h.next),
        (Action::Prev, &h.prev),
        (Action::ModeNext, &h.mode_next),
        (Action::ModePrev, &h.mode_prev),
    ];

    let mut specs = Vec::new();
    let mut errors = Vec::new();

    for (action, s) in pairs {
        match parse(s) {
            Ok(spec) => specs.push((action, spec)),
            Err(e) => errors.push((action, e.to_string())),
        }
    }

    (specs, errors)
}

/// Background thread handling global hotkey registration and dispatch.
pub struct HotkeyThread {
    thread_id: u32,
    handle: Option<JoinHandle<()>>,
}

impl HotkeyThread {
    /// Start hotkey thread, registering bindings and returning any failures.
    pub fn start(
        bindings: Vec<(Action, HotkeySpec)>,
        on_action: impl Fn(Action) + Send + 'static,
    ) -> (Self, Vec<(Action, String)>) {
        let (tx, rx) = channel();
        let fallback_bindings = bindings.clone();

        let handle = thread::spawn(move || {
            // SAFETY: std::mem::zeroed creates valid zero-initialized Win32 MSG struct.
            let mut msg: MSG = unsafe { std::mem::zeroed() };
            // SAFETY: PeekMessageW forces Win32 to create thread message queue if absent.
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::PeekMessageW(
                    &mut msg,
                    0 as HWND,
                    0,
                    0,
                    windows_sys::Win32::UI::WindowsAndMessaging::PM_NOREMOVE,
                );
            }

            // SAFETY: GetCurrentThreadId retrieves current Win32 thread ID.
            let tid = unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() };

            let mut failures = Vec::new();
            let mut registered = Vec::new();

            for (idx, &(action, spec)) in bindings.iter().enumerate() {
                let id = (idx + 1) as i32;
                // SAFETY: RegisterHotKey registers global hotkey on this thread's message queue.
                let ok = unsafe { RegisterHotKey(0 as HWND, id, spec.mods, spec.vk as u32) };
                if ok != 0 {
                    registered.push((id, action));
                } else {
                    failures.push((action, "already used by another app".to_string()));
                }
            }

            let _ = tx.send((tid, failures));

            // Message dispatch loop
            // SAFETY: GetMessageW waits for message on thread queue.
            while unsafe { GetMessageW(&mut msg, 0 as HWND, 0, 0) } > 0 {
                if msg.message == WM_HOTKEY {
                    let hotkey_id = msg.wParam as i32;
                    for &(reg_id, act) in &registered {
                        if reg_id == hotkey_id {
                            on_action(act);
                            break;
                        }
                    }
                }
            }

            // Cleanup registered hotkeys on exit
            for &(id, _) in &registered {
                // SAFETY: UnregisterHotKey releases registered hotkey ID.
                unsafe {
                    UnregisterHotKey(0 as HWND, id);
                }
            }
        });

        match rx.recv() {
            Ok((thread_id, failures)) => (
                Self {
                    thread_id,
                    handle: Some(handle),
                },
                failures,
            ),
            Err(_) => {
                let failures = fallback_bindings
                    .into_iter()
                    .map(|(a, _)| (a, "hotkey thread failed to start".to_string()))
                    .collect();
                (
                    Self {
                        thread_id: 0,
                        handle: None,
                    },
                    failures,
                )
            }
        }
    }

    /// Stop hotkey worker thread and wait for completion.
    pub fn stop(mut self) {
        self.stop_internal();
    }

    fn stop_internal(&mut self) {
        if let Some(handle) = self.handle.take() {
            // SAFETY: PostThreadMessageW posts WM_QUIT to hotkey worker thread.
            unsafe {
                PostThreadMessageW(self.thread_id, WM_QUIT, 0 as WPARAM, 0 as LPARAM);
            }
            let _ = handle.join();
        }
    }
}

impl Drop for HotkeyThread {
    fn drop(&mut self) {
        self.stop_internal();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn test_parse_valid_cases() {
        let s1 = parse("ScrollLock").unwrap();
        assert_eq!(s1.mods, MOD_NOREPEAT);
        assert_eq!(s1.vk, VK_SCROLL);

        let s2 = parse("ctrl+shift+F5").unwrap();
        assert_eq!(s2.mods, MOD_NOREPEAT | MOD_CONTROL | MOD_SHIFT);
        assert_eq!(s2.vk, VK_F5);

        let s3 = parse("Alt+[").unwrap();
        assert_eq!(s3.mods, MOD_NOREPEAT | MOD_ALT);
        assert_eq!(s3.vk, VK_OEM_4);

        let s4 = parse("F24").unwrap();
        assert_eq!(s4.mods, MOD_NOREPEAT);
        assert_eq!(s4.vk, VK_F24);

        let s5 = parse("Shift+A").unwrap();
        assert_eq!(s5.mods, MOD_NOREPEAT | MOD_SHIFT);
        assert_eq!(s5.vk, 0x41);
    }

    #[test]
    fn test_parse_invalid_cases() {
        let err_unknown = parse("F25").unwrap_err();
        assert!(matches!(err_unknown, HotkeyError::Unknown(_)));
        assert_eq!(err_unknown.to_string(), "unknown key 'F25'");

        let err_empty = parse("").unwrap_err();
        assert!(matches!(err_empty, HotkeyError::Empty));
        assert_eq!(err_empty.to_string(), "hotkey is empty");

        let err_dup = parse("Ctrl+Ctrl+A").unwrap_err();
        assert!(matches!(err_dup, HotkeyError::Duplicate(_)));
        assert_eq!(err_dup.to_string(), "duplicate modifier");

        let err_bare = parse("A").unwrap_err();
        assert!(matches!(err_bare, HotkeyError::BareKey(_)));
        assert_eq!(err_bare.to_string(), "key 'A' needs Ctrl/Alt/Shift");

        let err_space = parse("Space").unwrap_err();
        assert!(matches!(err_space, HotkeyError::Unknown(_)));
        assert_eq!(err_space.to_string(), "unknown key 'Space'");
    }

    #[test]
    fn test_hotkey_thread_start_and_stop() {
        let spec = parse("Ctrl+Alt+Shift+F24").expect("valid test spec");
        let bindings = vec![(Action::PlayPause, spec)];

        let start_time = Instant::now();
        let (thread, _failures) = HotkeyThread::start(bindings, |_act| {});
        thread.stop();
        let elapsed = start_time.elapsed();

        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "stop must complete in under 1 second, took {:?}",
            elapsed
        );
    }

    #[test]
    fn test_hotkey_thread_duplicate_spec() {
        let spec = parse("Ctrl+Alt+Shift+F23").expect("valid test spec");
        let bindings = vec![(Action::PlayPause, spec), (Action::Stop, spec)];

        let start_time = Instant::now();
        let (thread, failures) = HotkeyThread::start(bindings, |_act| {});
        thread.stop();
        let elapsed = start_time.elapsed();

        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "stop must complete in under 1 second, took {:?}",
            elapsed
        );
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].0, Action::Stop);
        assert!(
            failures[0].1.contains("already used"),
            "expected 'already used' in failure message: {}",
            failures[0].1
        );
    }
}
