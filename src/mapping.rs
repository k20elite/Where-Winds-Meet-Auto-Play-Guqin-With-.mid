//! Note-to-keyboard-stroke mapping and auto-transposition engine for Guqin.

use crate::midi::NoteEvent;

/// Keystroke modifier keys supported by Guqin input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Modifier {
    /// No modifier key.
    None,
    /// Shift key pressed with slot.
    Shift,
    /// Ctrl key pressed with slot.
    Ctrl,
}

/// A target key slot (0..=20) and optional modifier key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Stroke {
    /// Slot index: row * 7 + degree (0..=20).
    pub slot: u8,
    /// Modifier key for chromatic accidentals.
    pub modifier: Modifier,
}

/// Instrument key layout mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum KeyMode {
    /// 21 natural keys (C-major heptatonic across 3 octaves).
    Natural21,
    /// 36 keys with Shift and Ctrl modifiers for chromatic accidentals.
    Chromatic36,
    /// Roblox 61-key piano (C2..=C7): 36 white keys `1..0 q..p a..l z..m`, black keys are
    /// Shift + the white key on their left.
    Roblox61,
}

/// Lowest note on the Roblox piano (C2, key `1`).
pub const ROBLOX_LOW: u8 = 36;
/// Highest note on the Roblox piano (C7, key `m`).
pub const ROBLOX_HIGH: u8 = 96;
/// Roblox piano white keys, lowest first (slot = index).
pub const ROBLOX_KEYS: &str = "1234567890qwertyuiopasdfghjklzxcvbnm";

/// Strategy for mapping out-of-scale or out-of-range notes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum NoteMode {
    /// Fold into 48..=83, snap to nearest natural (ties go down).
    Nearest,
    /// Fold into 48..=83, snap to nearest natural (ties go up).
    SnapUp,
    /// Fold into 48..=83, snap to nearest pentatonic degree (1, 2, 3, 5, 6; ties down).
    Pentatonic,
    /// Row selected by pitch thresholds (<54 low, <66 mid, else high); pitch class snapped nearest.
    Spread,
    /// Fold into mid and high rows (60..=83), nearest natural (ties down).
    Melody,
}

impl NoteMode {
    /// All available NoteMode variants.
    #[allow(dead_code)] // reason: convenient collection of all note mapping variants
    pub const ALL: [NoteMode; 5] = [
        NoteMode::Nearest,
        NoteMode::SnapUp,
        NoteMode::Pentatonic,
        NoteMode::Spread,
        NoteMode::Melody,
    ];

    /// Human-readable label.
    pub fn label(self) -> &'static str {
        match self {
            NoteMode::Nearest => "Nearest",
            NoteMode::SnapUp => "Snap Up",
            NoteMode::Pentatonic => "Pentatonic",
            NoteMode::Spread => "Spread",
            NoteMode::Melody => "Melody",
        }
    }

    /// Label as it applies to a key mode (the piano reuses the modes with its own meaning).
    pub fn label_for(self, key_mode: KeyMode) -> &'static str {
        match (key_mode, self) {
            (KeyMode::Roblox61, NoteMode::Nearest) => "Exact",
            (KeyMode::Roblox61, NoteMode::SnapUp) => "White keys",
            (KeyMode::Roblox61, NoteMode::Spread) => "Compact",
            _ => self.label(),
        }
    }

    /// Next mode in sequence (wrapping).
    pub fn next(self) -> Self {
        match self {
            NoteMode::Nearest => NoteMode::SnapUp,
            NoteMode::SnapUp => NoteMode::Pentatonic,
            NoteMode::Pentatonic => NoteMode::Spread,
            NoteMode::Spread => NoteMode::Melody,
            NoteMode::Melody => NoteMode::Nearest,
        }
    }

    /// Previous mode in sequence (wrapping).
    pub fn prev(self) -> Self {
        match self {
            NoteMode::Nearest => NoteMode::Melody,
            NoteMode::SnapUp => NoteMode::Nearest,
            NoteMode::Pentatonic => NoteMode::SnapUp,
            NoteMode::Spread => NoteMode::Pentatonic,
            NoteMode::Melody => NoteMode::Spread,
        }
    }
}

/// Configuration for mapping MIDI notes into playable strokes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mapper {
    /// Mode for mapping out-of-scale or out-of-range notes.
    pub note_mode: NoteMode,
    /// Instrument keyboard mode (21 natural vs 36 chromatic).
    pub key_mode: KeyMode,
    /// Semitone transposition (-6..=6 typical).
    pub transpose: i8,
    /// Octave shift (-2..=2 typical).
    pub octave: i8,
}

impl Mapper {
    /// Map a MIDI note number (0..=127) to an instrument stroke.
    /// Returns None if transposed pitch is outside 0..=127.
    pub fn map(&self, note: u8) -> Option<Stroke> {
        let effective = note as i32 + self.transpose as i32 + (self.octave as i32 * 12);
        if !(0..=127).contains(&effective) {
            return None;
        }
        let eff = effective as u8;

        match self.key_mode {
            KeyMode::Natural21 => {
                let slot = match self.note_mode {
                    NoteMode::Nearest => {
                        let folded = fold_octave_range(eff, 48, 83);
                        folded_to_natural_slot(folded, false)
                    }
                    NoteMode::SnapUp => {
                        let folded = fold_octave_range(eff, 48, 83);
                        folded_to_natural_slot(folded, true)
                    }
                    NoteMode::Pentatonic => {
                        let folded = fold_octave_range(eff, 48, 83);
                        folded_to_pentatonic_slot(folded)
                    }
                    NoteMode::Spread => {
                        let row = if eff < 54 {
                            0
                        } else if eff < 66 {
                            1
                        } else {
                            2
                        };
                        let pc = eff % 12;
                        let deg = snap_pc_natural(pc, false);
                        row * 7 + deg
                    }
                    NoteMode::Melody => {
                        let folded = fold_octave_range(eff, 60, 83);
                        // Folded note is in 60..=83 (row 1 or 2)
                        let row = (folded - 48) / 12;
                        let pc = folded % 12;
                        let deg = snap_pc_natural(pc, false);
                        row * 7 + deg
                    }
                };
                Some(Stroke {
                    slot,
                    modifier: Modifier::None,
                })
            }
            KeyMode::Chromatic36 => match self.note_mode {
                NoteMode::Pentatonic => {
                    let folded = fold_octave_range(eff, 48, 83);
                    let slot = folded_to_pentatonic_slot(folded);
                    Some(Stroke {
                        slot,
                        modifier: Modifier::None,
                    })
                }
                NoteMode::Melody => {
                    let folded = fold_octave_range(eff, 60, 83);
                    let row = (folded - 48) / 12;
                    let pc = folded % 12;
                    let (deg, modifier) = pc_to_chromatic(pc);
                    Some(Stroke {
                        slot: row * 7 + deg,
                        modifier,
                    })
                }
                NoteMode::Spread => {
                    let row = if eff < 54 {
                        0
                    } else if eff < 66 {
                        1
                    } else {
                        2
                    };
                    let pc = eff % 12;
                    let (deg, modifier) = pc_to_chromatic(pc);
                    Some(Stroke {
                        slot: row * 7 + deg,
                        modifier,
                    })
                }
                NoteMode::Nearest | NoteMode::SnapUp => {
                    let folded = fold_octave_range(eff, 48, 83);
                    let row = (folded - 48) / 12;
                    let pc = folded % 12;
                    let (deg, modifier) = pc_to_chromatic(pc);
                    Some(Stroke {
                        slot: row * 7 + deg,
                        modifier,
                    })
                }
            },
            KeyMode::Roblox61 => Some(self.map_roblox(eff)),
        }
    }

    /// Roblox piano: slot = white key index (0..=35), Shift = the black key above it.
    /// - Exact (Nearest): every note as written, out-of-range notes folded by octaves.
    /// - White keys (SnapUp): accidentals snap up to the next white key, no Shift needed.
    /// - Pentatonic: white keys snapped to do re mi so la.
    /// - Compact (Spread): exact, folded into the middle three octaves (C3..=C6).
    /// - Melody: exact, folded into the upper half (C4..=C7).
    fn map_roblox(&self, eff: u8) -> Stroke {
        let (lo, hi) = match self.note_mode {
            NoteMode::Spread => (48, 84),
            NoteMode::Melody => (60, ROBLOX_HIGH),
            _ => (ROBLOX_LOW, ROBLOX_HIGH),
        };
        let rel = fold_octave_range(eff, lo, hi) - ROBLOX_LOW;
        let (octave, pc) = (rel / 12, rel % 12);
        match self.note_mode {
            NoteMode::SnapUp => Stroke {
                slot: octave * 7 + snap_pc_natural(pc, true),
                modifier: Modifier::None,
            },
            NoteMode::Pentatonic => {
                // B snaps up to the next C; B6 lands on C7, the top key (slot 35).
                let (deg, row_offset) = snap_pc_pentatonic(pc);
                Stroke {
                    slot: (octave + row_offset as u8) * 7 + deg,
                    modifier: Modifier::None,
                }
            }
            _ => {
                let (deg, black) = piano_key(pc);
                Stroke {
                    slot: octave * 7 + deg,
                    modifier: if black {
                        Modifier::Shift
                    } else {
                        Modifier::None
                    },
                }
            }
        }
    }
}

/// Pitch class to (white key degree 0..=6, is black key). A black key is the white key
/// below it plus Shift, matching the Roblox piano layout.
fn piano_key(pc: u8) -> (u8, bool) {
    match pc {
        0 => (0, false),
        1 => (0, true),
        2 => (1, false),
        3 => (1, true),
        4 => (2, false),
        5 => (3, false),
        6 => (3, true),
        7 => (4, false),
        8 => (4, true),
        9 => (5, false),
        10 => (5, true),
        _ => (6, false),
    }
}

/// Fold a note into `[min, max]` by octave shifts (+/- 12).
fn fold_octave_range(note: u8, min: u8, max: u8) -> u8 {
    let mut n = note as i32;
    while n < min as i32 {
        n += 12;
    }
    while n > max as i32 {
        n -= 12;
    }
    n as u8
}

/// Snap pitch class (0..=11) to natural degree (0..=6).
/// Naturals: 0(C)->0, 2(D)->1, 4(E)->2, 5(F)->3, 7(G)->4, 9(A)->5, 11(B)->6.
/// Accidentals:
/// - 1 (C#): snap_up ? 1(D) : 0(C)
/// - 3 (Eb): snap_up ? 2(E) : 1(D)
/// - 6 (F#): snap_up ? 4(G) : 3(F)
/// - 8 (G#): snap_up ? 5(A) : 4(G)
/// - 10 (Bb): snap_up ? 6(B) : 5(A)
fn snap_pc_natural(pc: u8, snap_up: bool) -> u8 {
    match pc {
        0 => 0,
        1 => {
            if snap_up {
                1
            } else {
                0
            }
        }
        2 => 1,
        3 => {
            if snap_up {
                2
            } else {
                1
            }
        }
        4 => 2,
        5 => 3,
        6 => {
            if snap_up {
                4
            } else {
                3
            }
        }
        7 => 4,
        8 => {
            if snap_up {
                5
            } else {
                4
            }
        }
        9 => 5,
        10 => {
            if snap_up {
                6
            } else {
                5
            }
        }
        11 => 6,
        _ => 0,
    }
}

/// Convert folded note in 48..=83 to natural key slot (0..=20).
fn folded_to_natural_slot(folded: u8, snap_up: bool) -> u8 {
    let row = (folded - 48) / 12;
    let pc = folded % 12;
    let deg = snap_pc_natural(pc, snap_up);
    row * 7 + deg
}

/// Map pitch class to nearest pentatonic natural degree (0, 1, 2, 4, 5).
/// Degrees {do=0, re=1, mi=2, so=4, la=5}.
/// PC distances:
/// - 0 (C) -> deg 0 (dist 0)
/// - 1 (C#) -> dist to C(0) is 1, dist to D(2) is 1; tie goes down -> deg 0
/// - 2 (D) -> deg 1 (dist 0)
/// - 3 (Eb) -> dist to D(2) is 1, dist to E(4) is 1; tie goes down -> deg 1
/// - 4 (E) -> deg 2 (dist 0)
/// - 5 (F) -> dist to E(4) is 1, dist to G(7) is 2 -> deg 2
/// - 6 (F#) -> dist to E(4) is 2, dist to G(7) is 1 -> deg 4
/// - 7 (G) -> deg 4 (dist 0)
/// - 8 (G#) -> dist to G(7) is 1, dist to A(9) is 1; tie goes down -> deg 4
/// - 9 (A) -> deg 5 (dist 0)
/// - 10 (Bb) -> dist to A(9) is 1, dist to next C(12) is 2 -> deg 5
/// - 11 (B) -> dist to A(9) is 2, dist to next C(12) is 1 -> next C (deg 0 in next row)
fn snap_pc_pentatonic(pc: u8) -> (u8, i8) {
    match pc {
        0 => (0, 0),
        1 => (0, 0),
        2 => (1, 0),
        3 => (1, 0),
        4 => (2, 0),
        5 => (2, 0),
        6 => (4, 0),
        7 => (4, 0),
        8 => (4, 0),
        9 => (5, 0),
        10 => (5, 0),
        11 => (0, 1), // Nearest is C of next octave (dist 1 vs dist 2 to A)
        _ => (0, 0),
    }
}

fn folded_to_pentatonic_slot(folded: u8) -> u8 {
    let row = ((folded - 48) / 12) as i32;
    let pc = folded % 12;
    if row == 2 && pc == 11 {
        // B at top of range (folded 83, B5): upward snap would leave top row.
        // Snap down to la in row 2 instead (slot 19).
        return 2 * 7 + 5;
    }
    let (deg, row_offset) = snap_pc_pentatonic(pc);
    let target_row = row + row_offset as i32;
    (target_row as u8) * 7 + deg
}

/// Convert pitch class (0..=11) to degree (0..=6) and Modifier in Chromatic36 mode.
/// - C (0): deg 0, None
/// - C# (1): deg 0, Shift
/// - D (2): deg 1, None
/// - Eb (3): deg 2, Ctrl
/// - E (4): deg 2, None
/// - F (5): deg 3, None
/// - F# (6): deg 3, Shift
/// - G (7): deg 4, None
/// - G# (8): deg 4, Shift
/// - A (9): deg 5, None
/// - Bb (10): deg 6, Ctrl
/// - B (11): deg 6, None
fn pc_to_chromatic(pc: u8) -> (u8, Modifier) {
    match pc {
        0 => (0, Modifier::None),
        1 => (0, Modifier::Shift),
        2 => (1, Modifier::None),
        3 => (2, Modifier::Ctrl),
        4 => (2, Modifier::None),
        5 => (3, Modifier::None),
        6 => (3, Modifier::Shift),
        7 => (4, Modifier::None),
        8 => (4, Modifier::Shift),
        9 => (5, Modifier::None),
        10 => (6, Modifier::Ctrl),
        11 => (6, Modifier::None),
        _ => (0, Modifier::None),
    }
}

fn is_better_shift(score: i64, shift: i8, best_score: i64, best_shift: i8) -> bool {
    if score > best_score {
        true
    } else if score == best_score {
        if shift.abs() < best_shift.abs() {
            true
        } else {
            shift.abs() == best_shift.abs() && shift >= 0 && best_shift < 0
        }
    } else {
        false
    }
}

/// Calculate the recommended transposition offset (-6..=6) to optimize Guqin playability.
///
/// # Scoring Algorithm:
/// - Evaluates candidates `shift` in `-6..=6`.
/// - Natural21 mode:
///   - Count note-ons in `O(n)` to form a 12-bin pitch class histogram.
///   - For each shift, count how many shifted notes land on natural degrees (C, D, E, F, G, A, B).
///   - Penalize accidentals: Score = `natural_notes * 100 - accidental_notes * 200`.
///   - Minor distance penalty: `- |shift|` to favor original key.
/// - Chromatic36 mode:
///   - Every pitch class is playable.
///   - Compute pitch histogram (0..=127).
///   - Center target is instrument center: MIDI 66 (F#4).
///   - Score penalizes average distance of notes from 66:
///     `- sum(note_count * |note + shift - 66|)`.
///   - Minor distance penalty: `- |shift|`.
/// - Tie-breaking: smaller `|shift|`, then toward `0` (positive over negative on equal absolute).
pub fn auto_transpose(events: &[NoteEvent], key_mode: KeyMode) -> i8 {
    let mut best_shift: i8 = 0;
    let mut best_score: i64 = i64::MIN;

    match key_mode {
        KeyMode::Natural21 => {
            let mut pc_counts = [0u32; 12];
            let mut has_note_on = false;
            for e in events {
                if e.on {
                    has_note_on = true;
                    pc_counts[(e.note % 12) as usize] += 1;
                }
            }
            if !has_note_on {
                return 0;
            }

            // Natural pitch classes: 0, 2, 4, 5, 7, 9, 11
            let is_natural = |pc: usize| matches!(pc, 0 | 2 | 4 | 5 | 7 | 9 | 11);

            for shift in -6i8..=6i8 {
                let mut naturals = 0i64;
                let mut accidentals = 0i64;

                for (pc, &count) in pc_counts.iter().enumerate() {
                    let shifted_pc = (pc as i32 + shift as i32).rem_euclid(12) as usize;
                    let count = count as i64;
                    if is_natural(shifted_pc) {
                        naturals += count;
                    } else {
                        accidentals += count;
                    }
                }

                // Penalize accidentals heavily, slightly penalize |shift|
                let score = naturals * 100 - accidentals * 200 - (shift.abs() as i64);

                if is_better_shift(score, shift, best_score, best_shift) {
                    best_score = score;
                    best_shift = shift;
                }
            }
        }
        KeyMode::Chromatic36 => {
            let mut note_counts = [0u32; 128];
            let mut has_note_on = false;
            for e in events {
                if e.on {
                    has_note_on = true;
                    if (e.note as usize) < 128 {
                        note_counts[e.note as usize] += 1;
                    }
                }
            }
            if !has_note_on {
                return 0;
            }

            for shift in -6i8..=6i8 {
                let mut center_dist_sum = 0i64;

                for (pitch, &count) in note_counts.iter().enumerate() {
                    if count > 0 {
                        let shifted = pitch as i32 + shift as i32;
                        let dist = (shifted - 66).abs();
                        center_dist_sum += (count as i64) * (dist as i64);
                    }
                }

                // Reward being close to center 66; minor tie break for |shift|
                let score = -center_dist_sum * 10 - (shift.abs() as i64);

                if is_better_shift(score, shift, best_score, best_shift) {
                    best_score = score;
                    best_shift = shift;
                }
            }
        }
        KeyMode::Roblox61 => return roblox_transpose(events),
    }

    best_shift
}

/// Roblox piano: every pitch is playable, so pick the shift that (in order) keeps the most
/// notes inside C2..=C7, needs the fewest Shift (black key) presses, is smallest, and is
/// positive over negative.
fn roblox_transpose(events: &[NoteEvent]) -> i8 {
    let mut note_counts = [0u64; 128];
    for e in events.iter().filter(|e| e.on) {
        note_counts[(e.note & 0x7f) as usize] += 1;
    }
    let mut best: Option<((u64, u64, u8, bool), i8)> = None;
    for shift in -6i8..=6 {
        let (mut outside, mut black) = (0u64, 0u64);
        for (pitch, &count) in note_counts.iter().enumerate().filter(|(_, &c)| c > 0) {
            let shifted = pitch as i32 + shift as i32;
            if !(ROBLOX_LOW as i32..=ROBLOX_HIGH as i32).contains(&shifted) {
                outside += count;
            }
            if matches!(shifted.rem_euclid(12), 1 | 3 | 6 | 8 | 10) {
                black += count;
            }
        }
        let key = (outside, black, shift.unsigned_abs(), shift < 0);
        if best.is_none_or(|(b, _)| key < b) {
            best = Some((key, shift));
        }
    }
    // With no notes every shift ties on (0, 0), so the smallest one (0) wins.
    best.map_or(0, |(_, shift)| shift)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roblox(note_mode: NoteMode) -> Mapper {
        Mapper {
            note_mode,
            key_mode: KeyMode::Roblox61,
            transpose: 0,
            octave: 0,
        }
    }

    fn on(notes: &[u8]) -> Vec<NoteEvent> {
        notes
            .iter()
            .map(|&note| NoteEvent {
                time_us: 0,
                note,
                on: true,
                track: 0,
                channel: 0,
            })
            .collect()
    }

    #[test]
    fn test_roblox_exact_layout() {
        let m = roblox(NoteMode::Nearest);
        let key = |note| {
            let s = m.map(note).unwrap();
            let c = ROBLOX_KEYS.as_bytes()[s.slot as usize] as char;
            (c, s.modifier == Modifier::Shift)
        };
        assert_eq!(key(36), ('1', false)); // C2
        assert_eq!(key(37), ('1', true)); // C#2 = Shift+1 (!)
        assert_eq!(key(39), ('2', true)); // D#2 = Shift+2 (@)
        assert_eq!(key(42), ('4', true)); // F#2 = Shift+4 ($)
        assert_eq!(key(60), ('t', false)); // C4 (middle C)
        assert_eq!(key(61), ('t', true)); // C#4 = T
        assert_eq!(key(70), ('p', true)); // A#4 = P
        assert_eq!(key(90), ('c', true)); // F#6 = C
        assert_eq!(key(94), ('b', true)); // A#6 = B
        assert_eq!(key(95), ('n', false)); // B6
        assert_eq!(key(96), ('m', false)); // C7

        // Out of range folds by octaves.
        assert_eq!(key(24), ('1', false));
        assert_eq!(key(108), ('m', false)); // C8 -> C7
        assert_eq!(key(98), ('z', false)); // D7 -> D6
    }

    #[test]
    fn test_roblox_modes_stay_on_keyboard() {
        for &mode in NoteMode::ALL.iter() {
            let m = roblox(mode);
            for note in 0..=127 {
                let s = m.map(note).unwrap();
                assert!(s.slot <= 35, "mode {mode:?}, note {note}, slot {}", s.slot);
                // There is no black key above E or B (degrees 2 and 6) or above the top C.
                if s.modifier == Modifier::Shift {
                    assert!(!matches!(s.slot % 7, 2 | 6) && s.slot < 35);
                }
                assert_ne!(s.modifier, Modifier::Ctrl);
            }
        }
        for mode in [NoteMode::SnapUp, NoteMode::Pentatonic] {
            let m = roblox(mode);
            assert!((0..=127).all(|n| m.map(n).unwrap().modifier == Modifier::None));
        }
        assert_eq!(roblox(NoteMode::SnapUp).map(61).unwrap().slot, 15); // C#4 -> D4
        assert_eq!(roblox(NoteMode::Pentatonic).map(95).unwrap().slot, 35); // B6 -> C7
        assert!(roblox(NoteMode::Melody).map(40).unwrap().slot >= 14);
        let compact = roblox(NoteMode::Spread);
        assert!((0..=127).all(|n| (7..=28).contains(&compact.map(n).unwrap().slot)));
    }

    #[test]
    fn test_roblox_auto_transpose() {
        assert_eq!(auto_transpose(&[], KeyMode::Roblox61), 0);
        // C major: nothing to fix.
        assert_eq!(
            auto_transpose(&on(&[60, 62, 64, 65, 67, 69, 71]), KeyMode::Roblox61),
            0
        );
        // D major: -2 removes every black key.
        assert_eq!(
            auto_transpose(&on(&[62, 64, 66, 67, 69, 71, 73]), KeyMode::Roblox61),
            -2
        );
        // Keeping notes in range beats avoiding black keys: 97 (C#7) is out of range,
        // shifting down by 1 brings it to C7.
        assert_eq!(auto_transpose(&on(&[97, 97, 60]), KeyMode::Roblox61), -1);
    }

    #[test]
    fn test_roblox_labels() {
        assert_eq!(NoteMode::Nearest.label_for(KeyMode::Roblox61), "Exact");
        assert_eq!(NoteMode::Nearest.label_for(KeyMode::Natural21), "Nearest");
        assert_eq!(ROBLOX_KEYS.len(), 36);
    }

    #[test]
    fn test_note_mode_cycling() {
        assert_eq!(NoteMode::ALL.len(), 5);
        let mut mode = NoteMode::Nearest;
        for &expected in &NoteMode::ALL[1..] {
            mode = mode.next();
            assert_eq!(mode, expected);
        }
        assert_eq!(mode.next(), NoteMode::Nearest);
        assert_eq!(NoteMode::Nearest.prev(), NoteMode::Melody);
    }

    #[test]
    fn test_extreme_pitch_and_transposition_no_panic() {
        let modes = NoteMode::ALL;
        let key_modes = [KeyMode::Natural21, KeyMode::Chromatic36];

        for &km in &key_modes {
            for &nm in &modes {
                for oct in [-2, 0, 2] {
                    for tr in [-6, 0, 6] {
                        let mapper = Mapper {
                            note_mode: nm,
                            key_mode: km,
                            transpose: tr,
                            octave: oct,
                        };
                        let m0 = mapper.map(0);
                        let m127 = mapper.map(127);
                        if let Some(s) = m0 {
                            assert!(s.slot <= 20);
                        }
                        if let Some(s) = m127 {
                            assert!(s.slot <= 20);
                        }
                    }
                }
            }
        }

        // Out-of-range should return None
        let out_mapper = Mapper {
            note_mode: NoteMode::Nearest,
            key_mode: KeyMode::Natural21,
            transpose: -10,
            octave: -1,
        };
        assert_eq!(out_mapper.map(5), None); // 5 - 10 - 12 = -17
    }

    #[test]
    fn test_natural21_modifier_always_none_and_slot_bound() {
        for &mode in NoteMode::ALL.iter() {
            let mapper = Mapper {
                note_mode: mode,
                key_mode: KeyMode::Natural21,
                transpose: 0,
                octave: 0,
            };
            for note in 0..=127 {
                let stroke = mapper.map(note).unwrap();
                assert_eq!(
                    stroke.modifier,
                    Modifier::None,
                    "mode {:?}, note {}",
                    mode,
                    note
                );
                assert!(
                    stroke.slot <= 20,
                    "mode {:?}, note {}, slot {}",
                    mode,
                    note,
                    stroke.slot
                );
            }
        }
    }

    #[test]
    fn test_chromatic36_octave4_exact_mapping() {
        let mapper = Mapper {
            note_mode: NoteMode::Nearest,
            key_mode: KeyMode::Chromatic36,
            transpose: 0,
            octave: 0,
        };
        // Octave 4: 60..=71 (Row 1, mid)
        // C4 (60): slot 7, None
        assert_eq!(
            mapper.map(60),
            Some(Stroke {
                slot: 7,
                modifier: Modifier::None
            })
        );
        // C#4 (61): slot 7, Shift
        assert_eq!(
            mapper.map(61),
            Some(Stroke {
                slot: 7,
                modifier: Modifier::Shift
            })
        );
        // D4 (62): slot 8, None
        assert_eq!(
            mapper.map(62),
            Some(Stroke {
                slot: 8,
                modifier: Modifier::None
            })
        );
        // Eb4 (63): slot 9, Ctrl
        assert_eq!(
            mapper.map(63),
            Some(Stroke {
                slot: 9,
                modifier: Modifier::Ctrl
            })
        );
        // E4 (64): slot 9, None
        assert_eq!(
            mapper.map(64),
            Some(Stroke {
                slot: 9,
                modifier: Modifier::None
            })
        );
        // F4 (65): slot 10, None
        assert_eq!(
            mapper.map(65),
            Some(Stroke {
                slot: 10,
                modifier: Modifier::None
            })
        );
        // F#4 (66): slot 10, Shift
        assert_eq!(
            mapper.map(66),
            Some(Stroke {
                slot: 10,
                modifier: Modifier::Shift
            })
        );
        // G4 (67): slot 11, None
        assert_eq!(
            mapper.map(67),
            Some(Stroke {
                slot: 11,
                modifier: Modifier::None
            })
        );
        // G#4 (68): slot 11, Shift
        assert_eq!(
            mapper.map(68),
            Some(Stroke {
                slot: 11,
                modifier: Modifier::Shift
            })
        );
        // A4 (69): slot 12, None
        assert_eq!(
            mapper.map(69),
            Some(Stroke {
                slot: 12,
                modifier: Modifier::None
            })
        );
        // Bb4 (70): slot 13, Ctrl
        assert_eq!(
            mapper.map(70),
            Some(Stroke {
                slot: 13,
                modifier: Modifier::Ctrl
            })
        );
        // B4 (71): slot 13, None
        assert_eq!(
            mapper.map(71),
            Some(Stroke {
                slot: 13,
                modifier: Modifier::None
            })
        );
    }

    #[test]
    fn test_nearest_ties_vs_snap_up() {
        let nearest_map = Mapper {
            note_mode: NoteMode::Nearest,
            key_mode: KeyMode::Natural21,
            transpose: 0,
            octave: 0,
        };
        let snapup_map = Mapper {
            note_mode: NoteMode::SnapUp,
            key_mode: KeyMode::Natural21,
            transpose: 0,
            octave: 0,
        };

        // C#4 (61): mid row (row 1). Degree 0 is C, Degree 1 is D.
        // Nearest: tie down -> Degree 0 (slot 7)
        assert_eq!(
            nearest_map.map(61),
            Some(Stroke {
                slot: 7,
                modifier: Modifier::None
            })
        );
        // SnapUp: tie up -> Degree 1 (slot 8)
        assert_eq!(
            snapup_map.map(61),
            Some(Stroke {
                slot: 8,
                modifier: Modifier::None
            })
        );
    }

    #[test]
    fn test_melody_mode_never_returns_low_row() {
        let mapper = Mapper {
            note_mode: NoteMode::Melody,
            key_mode: KeyMode::Natural21,
            transpose: 0,
            octave: 0,
        };
        for note in 0..=127 {
            let stroke = mapper.map(note).unwrap();
            assert!(stroke.slot >= 7, "note {} gave slot {}", note, stroke.slot);
        }

        let mapper36 = Mapper {
            note_mode: NoteMode::Melody,
            key_mode: KeyMode::Chromatic36,
            transpose: 0,
            octave: 0,
        };
        for note in 0..=127 {
            let stroke = mapper36.map(note).unwrap();
            assert!(stroke.slot >= 7, "note {} gave slot {}", note, stroke.slot);
        }

        // Chromatic36 exact cases
        assert_eq!(
            mapper36.map(61),
            Some(Stroke {
                slot: 7,
                modifier: Modifier::Shift
            })
        );
        assert_eq!(
            mapper36.map(46),
            Some(Stroke {
                slot: 13,
                modifier: Modifier::Ctrl
            })
        );
    }

    #[test]
    fn test_pentatonic_mode() {
        for &km in &[KeyMode::Natural21, KeyMode::Chromatic36] {
            let mapper = Mapper {
                note_mode: NoteMode::Pentatonic,
                key_mode: km,
                transpose: 0,
                octave: 0,
            };
            // 60 (C4) -> slot 7 (mid row deg 0)
            assert_eq!(
                mapper.map(60),
                Some(Stroke {
                    slot: 7,
                    modifier: Modifier::None
                })
            );
            // 65 (F4) -> nearest pentatonic is E4 (64, deg 2), slot 9
            assert_eq!(
                mapper.map(65),
                Some(Stroke {
                    slot: 9,
                    modifier: Modifier::None
                })
            );

            // Exact cases: 61 -> 7, 66 -> 11, 71 -> 14, 83 -> 19, 82 -> 19, 48 -> 0
            assert_eq!(mapper.map(61).unwrap().slot, 7);
            assert_eq!(mapper.map(66).unwrap().slot, 11);
            assert_eq!(mapper.map(71).unwrap().slot, 14);
            assert_eq!(mapper.map(83).unwrap().slot, 19);
            assert_eq!(mapper.map(82).unwrap().slot, 19);
            assert_eq!(mapper.map(48).unwrap().slot, 0);
        }
    }

    #[test]
    fn test_spread_mode() {
        let mapper = Mapper {
            note_mode: NoteMode::Spread,
            key_mode: KeyMode::Natural21,
            transpose: 0,
            octave: 0,
        };
        // 50 -> slot 1 (D3, low row)
        assert_eq!(
            mapper.map(50),
            Some(Stroke {
                slot: 1,
                modifier: Modifier::None
            })
        );
        // 60 -> slot 7
        assert_eq!(
            mapper.map(60),
            Some(Stroke {
                slot: 7,
                modifier: Modifier::None
            })
        );
        // 72 -> slot 14
        assert_eq!(
            mapper.map(72),
            Some(Stroke {
                slot: 14,
                modifier: Modifier::None
            })
        );
        // 53 -> slot 3 (F3 low)
        assert_eq!(
            mapper.map(53),
            Some(Stroke {
                slot: 3,
                modifier: Modifier::None
            })
        );
        // 54 (F#3) -> mid row slot 10
        assert_eq!(
            mapper.map(54),
            Some(Stroke {
                slot: 10,
                modifier: Modifier::None
            })
        );
        // 66 (F#4) -> high row slot 17
        assert_eq!(
            mapper.map(66),
            Some(Stroke {
                slot: 17,
                modifier: Modifier::None
            })
        );
    }

    #[test]
    fn test_auto_transpose() {
        // Empty events
        assert_eq!(auto_transpose(&[], KeyMode::Natural21), 0);
        assert_eq!(auto_transpose(&[], KeyMode::Chromatic36), 0);

        // C-major scale: C4, D4, E4, F4, G4, A4, B4 (60, 62, 64, 65, 67, 69, 71)
        let c_scale: Vec<NoteEvent> = [60, 62, 64, 65, 67, 69, 71]
            .iter()
            .map(|&n| NoteEvent {
                time_us: 0,
                note: n,
                on: true,
                track: 0,
                channel: 0,
            })
            .collect();

        assert_eq!(auto_transpose(&c_scale, KeyMode::Natural21), 0);

        // Shifted up by 2 semitones: D major scale (62, 64, 66, 67, 69, 71, 73)
        // Should recommend -2 to shift back to C major
        let d_scale: Vec<NoteEvent> = [62, 64, 66, 67, 69, 71, 73]
            .iter()
            .map(|&n| NoteEvent {
                time_us: 0,
                note: n,
                on: true,
                track: 0,
                channel: 0,
            })
            .collect();

        assert_eq!(auto_transpose(&d_scale, KeyMode::Natural21), -2);

        // F-major scale: F4, G4, A4, Bb4, C5, D5, E5 (65, 67, 69, 70, 72, 74, 76)
        // Should recommend -5 with Natural21
        let f_scale: Vec<NoteEvent> = [65, 67, 69, 70, 72, 74, 76]
            .iter()
            .map(|&n| NoteEvent {
                time_us: 0,
                note: n,
                on: true,
                track: 0,
                channel: 0,
            })
            .collect();

        assert_eq!(auto_transpose(&f_scale, KeyMode::Natural21), -5);

        // Chromatic36: notes all at 60 -> +6
        let notes_60: Vec<NoteEvent> = vec![NoteEvent {
            time_us: 0,
            note: 60,
            on: true,
            track: 0,
            channel: 0,
        }];
        assert_eq!(auto_transpose(&notes_60, KeyMode::Chromatic36), 6);

        // Chromatic36: notes all at 66 -> 0
        let notes_66: Vec<NoteEvent> = vec![NoteEvent {
            time_us: 0,
            note: 66,
            on: true,
            track: 0,
            channel: 0,
        }];
        assert_eq!(auto_transpose(&notes_66, KeyMode::Chromatic36), 0);

        // Chromatic36: notes all at 75 -> -6
        let notes_75: Vec<NoteEvent> = vec![NoteEvent {
            time_us: 0,
            note: 75,
            on: true,
            track: 0,
            channel: 0,
        }];
        assert_eq!(auto_transpose(&notes_75, KeyMode::Chromatic36), -6);

        // Equal-score tie between +k and -k resolves to +k
        assert!(is_better_shift(100, 2, 100, -2));
        assert!(!is_better_shift(100, -2, 100, 2));

        // Construct Natural21 case where +1 and -1 tie in score and beat 0:
        // Notes C# and B: PC 1 and 11.
        // shift 0: PC 1 (accidental), PC 11 (natural).
        //          naturals=1, accidentals=1 -> score = 100 - 200 - 0 = -100.
        // shift -1: PC 0 (natural C), PC 10 (accidental Bb).
        //          naturals=1, accidentals=1 -> score = 100 - 200 - 1 = -101.
        // Let's craft: notes with pitch classes {1, 3, 6, 8, 10} (all 5 black keys).
        // shift +1: {2, 4, 7, 9, 11} -> all 5 are natural!
        //           naturals=5, accidentals=0, |shift|=1 -> score = 500 - 1 = 499.
        // shift -1: {0, 2, 5, 7, 9} -> all 5 are natural!
        //           naturals=5, accidentals=0, |shift|=1 -> score = 500 - 1 = 499.
        // shift 0: all 5 are accidental -> score = -1000.
        // Both -1 and +1 tie with score 499. Best shift must be +1 (+k over -k).
        let pentatonic_accidentals: Vec<NoteEvent> = [61, 63, 66, 68, 70]
            .iter()
            .map(|&n| NoteEvent {
                time_us: 0,
                note: n,
                on: true,
                track: 0,
                channel: 0,
            })
            .collect();
        assert_eq!(
            auto_transpose(&pentatonic_accidentals, KeyMode::Natural21),
            1
        );
    }
}
