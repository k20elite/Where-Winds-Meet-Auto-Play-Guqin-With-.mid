//! Music library scanning, Vietnamese diacritic-folding search, and playback queue.

use crate::settings::Repeat;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// Scanned MIDI library entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Full path to the file on disk.
    pub path: PathBuf,
    /// Relative path from library root with forward slashes '/'.
    pub rel: String,
    /// File stem title.
    pub title: String,
    /// File size in bytes.
    pub size: u64,
    /// Precomputed folded search key for fast query matching.
    search_key: String,
}

impl Entry {
    /// Construct an entry with precomputed diacritic-folded search key.
    pub fn new(path: PathBuf, rel: String, title: String, size: u64) -> Self {
        let folded_title = fold_diacritics(&title);
        let folded_rel = fold_diacritics(&rel);
        let search_key = format!("{folded_title} {folded_rel}");
        Self {
            path,
            rel,
            title,
            size,
            search_key,
        }
    }
}

/// Recursively scan root directory for valid MIDI files up to depth 8.
pub fn scan(root: &Path) -> io::Result<Vec<Entry>> {
    let meta = fs::metadata(root)?;
    if !meta.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "root is not a directory",
        ));
    }

    let mut entries = Vec::new();
    walk_dir(root, root, 0, &mut entries);

    entries.sort_by_cached_key(|e| (e.title.to_lowercase(), e.rel.clone()));

    Ok(entries)
}

fn walk_dir(root: &Path, current: &Path, depth: usize, out: &mut Vec<Entry>) {
    if depth > 8 {
        return;
    }

    let read_dir = match fs::read_dir(current) {
        Ok(rd) => rd,
        Err(_) => return,
    };

    for item in read_dir.flatten() {
        let path = item.path();
        let symlink_meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };

        if symlink_meta.file_type().is_symlink() {
            // Skip symlinked dirs to avoid cycles
            continue;
        }

        if symlink_meta.is_dir() {
            walk_dir(root, &path, depth + 1, out);
        } else if symlink_meta.is_file() {
            let is_midi_ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|ext| ext.eq_ignore_ascii_case("mid") || ext.eq_ignore_ascii_case("midi"))
                .unwrap_or(false);

            if !is_midi_ext {
                continue;
            }

            let size = symlink_meta.len();
            if !(14..=52_428_800).contains(&size) {
                continue;
            }

            // Quick MThd header verification (read 4 bytes only)
            let mut buf = [0u8; 4];
            let is_mthd = match fs::File::open(&path) {
                Ok(mut f) => f.read_exact(&mut buf).is_ok() && &buf == b"MThd",
                Err(_) => false,
            };

            if !is_mthd {
                continue;
            }

            let rel_path = match path.strip_prefix(root) {
                Ok(p) => p.to_string_lossy().replace('\\', "/"),
                Err(_) => path.to_string_lossy().replace('\\', "/"),
            };

            let title = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();

            out.push(Entry::new(path, rel_path, title, size));
        }
    }
}

/// Fold Vietnamese diacritics and uppercase letters into plain ASCII lowercase.
pub fn fold_diacritics(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if ('\u{0300}'..='\u{036F}').contains(&c) {
            // Drop combining diacritical marks (NFD normalization support)
            continue;
        }
        match c {
            'a' | 'A' | 'à' | 'À' | 'á' | 'Á' | 'ả' | 'Ả' | 'ã' | 'Ã' | 'ạ' | 'Ạ' | 'ă' | 'Ă'
            | 'ằ' | 'Ằ' | 'ắ' | 'Ắ' | 'ẳ' | 'Ẳ' | 'ẵ' | 'Ẵ' | 'ặ' | 'Ặ' | 'â' | 'Â' | 'ầ' | 'Ầ'
            | 'ấ' | 'Ấ' | 'ẩ' | 'Ẩ' | 'ẫ' | 'Ẫ' | 'ậ' | 'Ậ' => out.push('a'),
            'd' | 'D' | 'đ' | 'Đ' => out.push('d'),
            'e' | 'E' | 'è' | 'È' | 'é' | 'É' | 'ẻ' | 'Ẻ' | 'ẽ' | 'Ẽ' | 'ẹ' | 'Ẹ' | 'ê' | 'Ê'
            | 'ề' | 'Ề' | 'ế' | 'Ế' | 'ể' | 'Ể' | 'ễ' | 'Ễ' | 'ệ' | 'Ệ' => {
                out.push('e')
            }
            'i' | 'I' | 'ì' | 'Ì' | 'í' | 'Í' | 'ỉ' | 'Ỉ' | 'ĩ' | 'Ĩ' | 'ị' | 'Ị' => {
                out.push('i')
            }
            'o' | 'O' | 'ò' | 'Ò' | 'ó' | 'Ó' | 'ỏ' | 'Ỏ' | 'õ' | 'Õ' | 'ọ' | 'Ọ' | 'ô' | 'Ô'
            | 'ồ' | 'Ồ' | 'ố' | 'Ố' | 'ổ' | 'Ổ' | 'ỗ' | 'Ỗ' | 'ộ' | 'Ộ' | 'ơ' | 'Ơ' | 'ờ' | 'Ờ'
            | 'ớ' | 'Ớ' | 'ở' | 'Ở' | 'ỡ' | 'Ỡ' | 'ợ' | 'Ợ' => out.push('o'),
            'u' | 'U' | 'ù' | 'Ù' | 'ú' | 'Ú' | 'ủ' | 'Ủ' | 'ũ' | 'Ũ' | 'ụ' | 'Ụ' | 'ư' | 'Ư'
            | 'ừ' | 'Ừ' | 'ứ' | 'Ứ' | 'ử' | 'Ử' | 'ữ' | 'Ữ' | 'ự' | 'Ự' => {
                out.push('u')
            }
            'y' | 'Y' | 'ỳ' | 'Ỳ' | 'ý' | 'Ý' | 'ỷ' | 'Ỷ' | 'ỹ' | 'Ỹ' | 'ỵ' | 'Ỵ' => {
                out.push('y')
            }
            other => {
                for lc in other.to_lowercase() {
                    out.push(lc);
                }
            }
        }
    }
    out
}

/// Filter entries by space-separated query terms (logical AND).
pub fn filter(entries: &[Entry], query: &str) -> Vec<usize> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(fold_diacritics)
        .filter(|t| !t.is_empty())
        .collect();

    if terms.is_empty() {
        return (0..entries.len()).collect();
    }

    entries
        .iter()
        .enumerate()
        .filter_map(|(idx, entry)| {
            let matched = terms.iter().all(|term| entry.search_key.contains(term));
            if matched {
                Some(idx)
            } else {
                None
            }
        })
        .collect()
}

/// Fast pseudo-random number generator using xorshift64.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rng {
    state: u64,
}

impl Rng {
    /// Initialize generator with seed; falls back to default constant if seed is 0.
    pub fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 { 0x853c49e6748fea9b } else { seed },
        }
    }

    /// Generate next pseudo-random u64 value.
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    /// Generate random integer in `[0, bound)`.
    pub fn gen_range(&mut self, bound: usize) -> usize {
        if bound <= 1 {
            return 0;
        }
        (self.next_u64() % bound as u64) as usize
    }
}

/// Playback queue tracking indices into a song list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Queue {
    items: Vec<usize>,
    pos: Option<usize>,
    pending_shuffle: Vec<usize>,
}

impl Queue {
    /// Create an empty queue.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set queue item list and select initial position.
    pub fn set(&mut self, items: Vec<usize>, start: usize) {
        if items.is_empty() {
            self.items.clear();
            self.pos = None;
            self.pending_shuffle.clear();
            return;
        }
        let init_pos = if start < items.len() { start } else { 0 };
        self.pos = Some(init_pos);
        self.items = items;
        self.init_shuffle_bag(init_pos);
    }

    fn init_shuffle_bag(&mut self, current_pos: usize) {
        self.pending_shuffle.clear();
        for i in 0..self.items.len() {
            if i != current_pos {
                self.pending_shuffle.push(i);
            }
        }
    }

    fn refill_shuffle(&mut self, exclude: Option<usize>, rng: &mut Rng) -> usize {
        let mut candidates: Vec<usize> = (0..self.items.len()).collect();
        if let Some(ex) = exclude {
            if candidates.len() > 1 && ex < candidates.len() {
                candidates.swap_remove(ex);
                let pick = rng.gen_range(candidates.len());
                let first = candidates.swap_remove(pick);
                candidates.push(ex);
                self.pending_shuffle = candidates;
                return first;
            }
        }
        let pick = rng.gen_range(candidates.len());
        let first = candidates.swap_remove(pick);
        self.pending_shuffle = candidates;
        first
    }

    /// Current track item index, or None if queue is empty.
    pub fn current(&self) -> Option<usize> {
        self.pos.and_then(|p| self.items.get(p).copied())
    }

    /// Position of the current item within the queue, if any.
    pub fn position(&self) -> Option<usize> {
        self.pos
    }

    /// Advance to next song considering Repeat mode and optional shuffle RNG.
    pub fn next(&mut self, repeat: Repeat, shuffle_rng: Option<&mut Rng>) -> Option<usize> {
        if self.items.is_empty() {
            self.pos = None;
            self.pending_shuffle.clear();
            return None;
        }

        let curr_pos = self.pos.unwrap_or(0);

        if repeat == Repeat::One {
            self.pos = Some(curr_pos);
            return self.current();
        }

        if let Some(rng) = shuffle_rng {
            if self.items.len() == 1 {
                return match repeat {
                    Repeat::Off => {
                        self.pos = None;
                        None
                    }
                    Repeat::One | Repeat::All => {
                        self.pos = Some(0);
                        self.current()
                    }
                };
            }

            if self.pending_shuffle.is_empty() {
                match repeat {
                    Repeat::Off => {
                        self.pos = None;
                        return None;
                    }
                    Repeat::All => {
                        let next_pos = self.refill_shuffle(Some(curr_pos), rng);
                        self.pos = Some(next_pos);
                        return self.current();
                    }
                    Repeat::One => {
                        self.pos = Some(curr_pos);
                        return self.current();
                    }
                }
            }

            let pick_idx = rng.gen_range(self.pending_shuffle.len());
            let next_pos = self.pending_shuffle.swap_remove(pick_idx);
            self.pos = Some(next_pos);
            return self.current();
        }

        if curr_pos + 1 < self.items.len() {
            let next_pos = curr_pos + 1;
            self.pos = Some(next_pos);
            self.current()
        } else {
            match repeat {
                Repeat::All => {
                    self.pos = Some(0);
                    self.current()
                }
                Repeat::Off => {
                    self.pos = None;
                    None
                }
                Repeat::One => {
                    self.pos = Some(curr_pos);
                    self.current()
                }
            }
        }
    }

    /// Step back to previous track in queue.
    pub fn prev(&mut self) -> Option<usize> {
        if self.items.is_empty() {
            self.pos = None;
            return None;
        }
        let curr = self.pos.unwrap_or(0);
        let prev = curr.saturating_sub(1);
        self.pos = Some(prev);
        self.current()
    }

    /// Clear all queue items and reset position.
    pub fn clear(&mut self) {
        self.items.clear();
        self.pos = None;
        self.pending_shuffle.clear();
    }

    /// Total count of items currently in queue.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Returns true if queue has no tracks.
    #[allow(dead_code)] // reason: standard container predicate for Queue
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// Toggle favorite state for a track relative path; returns new state.
pub fn toggle_favorite(favs: &mut Vec<String>, rel: &str) -> bool {
    let normalized = rel.replace('\\', "/");
    if let Some(idx) = favs.iter().position(|f| f == &normalized) {
        favs.remove(idx);
        false
    } else {
        favs.push(normalized);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_midi(path: &Path, content: &[u8]) {
        if let Some(p) = path.parent() {
            let _ = fs::create_dir_all(p);
        }
        fs::write(path, content).unwrap();
    }

    #[test]
    fn test_scan_directory_rules() {
        let unique_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("wwm_scan_test_{unique_id}"));
        fs::create_dir_all(&root).unwrap();

        // Valid MIDI header + dummy data (14 bytes)
        let valid_midi_header = b"MThd\x00\x00\x00\x06\x00\x00\x00\x01\x01\xe0";
        assert_eq!(valid_midi_header.len(), 14);

        // 1. Nested folder valid .mid
        create_test_midi(&root.join("sub/song_b.mid"), valid_midi_header);
        // 2. Uppercase .MID
        create_test_midi(&root.join("SONG_A.MID"), valid_midi_header);
        // 3. .txt ignored
        create_test_midi(&root.join("song.txt"), valid_midi_header);
        // 4. Fake .mid without MThd header ignored
        create_test_midi(&root.join("fake.mid"), b"NOT_MThd_header_data_here");
        // 5. Empty file ignored
        create_test_midi(&root.join("empty.mid"), b"");

        let entries = scan(&root).expect("scan succeeds");
        assert_eq!(entries.len(), 2);
        // Sort order: case-insensitive title -> SONG_A before song_b
        assert_eq!(entries[0].title, "SONG_A");
        assert_eq!(entries[1].title, "song_b");
        assert_eq!(entries[1].rel, "sub/song_b.mid");

        // Missing root returns Err
        let missing = root.join("missing_dir_xyz");
        assert!(scan(&missing).is_err());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_diacritic_folding_and_search() {
        let e1 = Entry::new(
            PathBuf::from("a/Người Lạ Ơi.mid"),
            "a/Người Lạ Ơi.mid".to_string(),
            "Người Lạ Ơi".to_string(),
            100,
        );
        let e2 = Entry::new(
            PathBuf::from("b/Đường Về Quê.mid"),
            "b/Đường Về Quê.mid".to_string(),
            "Đường Về Quê".to_string(),
            200,
        );
        let list = vec![e1, e2];

        // "nguoi la" matches "Người Lạ"
        let res1 = filter(&list, "nguoi la");
        assert_eq!(res1, vec![0]);

        // "duong" matches "Đường"
        let res2 = filter(&list, "duong");
        assert_eq!(res2, vec![1]);

        // Two terms AND
        let res3 = filter(&list, "duong que");
        assert_eq!(res3, vec![1]);
        let res3_fail = filter(&list, "duong la");
        assert!(res3_fail.is_empty());

        // Empty query matches all
        let res_all = filter(&list, "   ");
        assert_eq!(res_all, vec![0, 1]);

        // NFD decomposed form test: "Người" with combining marks matches "nguoi"
        let nfd_song = "Ngu\u{031b}\u{0300}o\u{031b}i.mid";
        let e_nfd = Entry::new(
            PathBuf::from(nfd_song),
            nfd_song.to_string(),
            "Ngu\u{031b}\u{0300}o\u{031b}i".to_string(),
            123,
        );
        let list_nfd = vec![e_nfd];
        let res_nfd = filter(&list_nfd, "nguoi");
        assert_eq!(res_nfd, vec![0]);
    }

    #[test]
    fn test_queue_behavior_and_repeats() {
        let mut q = Queue::new();
        assert!(q.is_empty());
        assert_eq!(q.current(), None);
        assert_eq!(q.next(Repeat::Off, None), None);
        assert_eq!(q.prev(), None);

        q.set(vec![10, 20, 30], 0);
        assert_eq!(q.len(), 3);
        assert_eq!(q.current(), Some(10));

        // Prev at start stays at 0
        assert_eq!(q.prev(), Some(10));

        // Advance
        assert_eq!(q.next(Repeat::Off, None), Some(20));
        assert_eq!(q.next(Repeat::Off, None), Some(30));

        // Repeat::Off at end gives None
        assert_eq!(q.next(Repeat::Off, None), None);

        // Reset to end and test Repeat::All
        q.set(vec![10, 20, 30], 2);
        assert_eq!(q.next(Repeat::All, None), Some(10));

        // Repeat::One stays at same item
        assert_eq!(q.next(Repeat::One, None), Some(10));
    }

    #[test]
    fn test_queue_shuffle_off_and_all() {
        // Shuffle + Off: len 5 yields 4 more distinct items after start item, then None
        let mut q = Queue::new();
        q.set(vec![10, 20, 30, 40, 50], 0);
        let mut rng = Rng::new(42);

        let mut seen = std::collections::HashSet::new();
        seen.insert(q.current().unwrap()); // 10

        for _ in 0..4 {
            let next_item = q.next(Repeat::Off, Some(&mut rng)).expect("item expected");
            assert!(seen.insert(next_item), "items must be distinct in round");
        }
        assert_eq!(seen.len(), 5);
        assert_eq!(q.next(Repeat::Off, Some(&mut rng)), None);

        // Shuffle + All: 200 calls never repeats consecutively, each round of 5 covers all 5
        let mut q_all = Queue::new();
        q_all.set(vec![1, 2, 3, 4, 5], 0);
        let mut rng_all = Rng::new(9999);

        let mut prev = q_all.current().unwrap();
        // Next 4 calls complete round 1 (with start item = 5 distinct items)
        let mut first_round_seen = std::collections::HashSet::new();
        first_round_seen.insert(prev);
        for _ in 0..4 {
            let next = q_all
                .next(Repeat::All, Some(&mut rng_all))
                .expect("always some under Repeat::All");
            assert_ne!(prev, next, "never repeats consecutively when len > 1");
            prev = next;
            first_round_seen.insert(next);
        }
        assert_eq!(first_round_seen.len(), 5);

        // Subsequent full rounds of 5 calls each cover all 5 items
        for _ in 0..39 {
            let mut round_seen = std::collections::HashSet::new();
            for _ in 0..5 {
                let next = q_all
                    .next(Repeat::All, Some(&mut rng_all))
                    .expect("always some under Repeat::All");
                assert_ne!(prev, next, "never repeats consecutively when len > 1");
                prev = next;
                round_seen.insert(next);
            }
            assert_eq!(round_seen.len(), 5, "each round of 5 covers all 5 items");
        }
    }

    #[test]
    fn test_toggle_favorite() {
        let mut favs = vec!["songs/a.mid".to_string()];
        let state1 = toggle_favorite(&mut favs, "songs\\a.mid");
        assert!(!state1);
        assert!(favs.is_empty());

        let state2 = toggle_favorite(&mut favs, "songs/b.mid");
        assert!(state2);
        assert_eq!(favs, vec!["songs/b.mid"]);
    }
}
