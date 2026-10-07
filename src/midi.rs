//! MIDI file parser and converter for Guqin note events.

use std::fmt;
use std::fs;
use std::path::Path;

/// Note event extracted from a MIDI track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoteEvent {
    /// Timestamp in microseconds from song start.
    pub time_us: u64,
    /// MIDI note pitch (0..=127).
    pub note: u8,
    /// True for Note-On, false for Note-Off.
    pub on: bool,
    /// Source track index (0-based).
    pub track: u16,
    /// MIDI channel number (0-based, 0..=15).
    pub channel: u8,
}

/// Metadata summary of a MIDI track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackInfo {
    /// 0-based track index.
    pub index: u16,
    /// Track name, default "Track {n}" (1-based index).
    pub name: String,
    /// Total note-on count in track.
    pub note_count: u32,
    /// True if note_count > 0 and all note-ons are on channel 9.
    pub is_drum: bool,
}

/// Fully parsed song with unified note events and track metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Song {
    /// Ordered note events (note-offs precede note-ons at equal timestamps).
    pub events: Vec<NoteEvent>,
    /// Metadata for each track in the SMF.
    pub tracks: Vec<TrackInfo>,
    /// Total duration in microseconds (timestamp of last event).
    pub duration_us: u64,
}

/// Error encountered while loading or parsing a MIDI file.
#[derive(Debug)]
pub enum MidiError {
    /// File system or I/O error.
    Io(std::io::Error),
    /// Invalid SMF format or corrupt data.
    Parse(String),
    /// Unsupported MIDI feature.
    Unsupported(&'static str),
    /// File contains no playable note events.
    Empty,
}

impl fmt::Display for MidiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MidiError::Io(e) => write!(f, "cannot read file: {e}"),
            MidiError::Parse(msg) => write!(f, "invalid MIDI: {msg}"),
            MidiError::Unsupported(what) => write!(f, "unsupported MIDI: {what}"),
            MidiError::Empty => write!(f, "MIDI has no notes"),
        }
    }
}

impl std::error::Error for MidiError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            MidiError::Io(e) => Some(e),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct TempoChange {
    tick: u64,
    tempo_us_per_quarter: u32,
}

#[derive(Debug, Clone, Copy)]
struct TempoSegment {
    start_tick: u64,
    start_us: u64,
    tempo: u32,
}

/// Load and parse a MIDI file from disk.
pub fn load_file(path: &Path) -> Result<Song, MidiError> {
    let bytes = fs::read(path).map_err(MidiError::Io)?;
    parse_bytes(&bytes)
}

/// Parse a MIDI file from raw byte slice.
pub fn parse_bytes(bytes: &[u8]) -> Result<Song, MidiError> {
    let smf = midly::Smf::parse(bytes).map_err(|e| MidiError::Parse(e.to_string()))?;

    let ticks_per_quarter = match smf.header.timing {
        midly::Timing::Metrical(t) => t.as_int() as u64,
        midly::Timing::Timecode(_, _) => {
            return Err(MidiError::Unsupported("SMPTE timecode timing"))
        }
    };

    if ticks_per_quarter == 0 {
        return Err(MidiError::Parse("zero ticks per quarter".to_string()));
    }

    // Pass 1: Extract tempo changes and raw note events per track
    let mut tempo_changes: Vec<TempoChange> = Vec::new();

    struct RawEvent {
        tick: u64,
        note: u8,
        on: bool,
        track: u16,
        channel: u8,
    }

    let mut raw_events: Vec<RawEvent> = Vec::new();
    let mut track_infos: Vec<TrackInfo> = Vec::with_capacity(smf.tracks.len());

    for (track_idx, track) in smf.tracks.iter().enumerate() {
        let t_idx = track_idx as u16;
        let mut curr_tick: u64 = 0;
        let mut track_name: Option<String> = None;
        let mut note_count: u32 = 0;
        let mut all_drum = true;

        for event in track.iter() {
            curr_tick = curr_tick.saturating_add(event.delta.as_int() as u64);

            match event.kind {
                midly::TrackEventKind::Meta(midly::MetaMessage::Tempo(tempo)) => {
                    tempo_changes.push(TempoChange {
                        tick: curr_tick,
                        tempo_us_per_quarter: tempo.as_int(),
                    });
                }
                midly::TrackEventKind::Meta(midly::MetaMessage::TrackName(data)) => {
                    if track_name.is_none() {
                        let name_str = String::from_utf8_lossy(data).trim().to_string();
                        if !name_str.is_empty() {
                            track_name = Some(name_str);
                        }
                    }
                }
                midly::TrackEventKind::Midi { channel, message } => {
                    let ch = channel.as_int();
                    match message {
                        midly::MidiMessage::NoteOn { key, vel } => {
                            let note = key.as_int();
                            let is_on = vel.as_int() > 0;
                            if is_on {
                                note_count = note_count.saturating_add(1);
                                if ch != 9 {
                                    all_drum = false;
                                }
                            }
                            raw_events.push(RawEvent {
                                tick: curr_tick,
                                note,
                                on: is_on,
                                track: t_idx,
                                channel: ch,
                            });
                        }
                        midly::MidiMessage::NoteOff { key, vel: _ } => {
                            let note = key.as_int();
                            raw_events.push(RawEvent {
                                tick: curr_tick,
                                note,
                                on: false,
                                track: t_idx,
                                channel: ch,
                            });
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }

        let name = track_name.unwrap_or_else(|| format!("Track {}", track_idx + 1));
        let is_drum = note_count > 0 && all_drum;

        track_infos.push(TrackInfo {
            index: t_idx,
            name,
            note_count,
            is_drum,
        });
    }

    // Sort tempo changes by tick stably
    tempo_changes.sort_by_key(|tc| tc.tick);

    // Build precomputed tempo segment table
    let mut segments: Vec<TempoSegment> = Vec::new();
    let mut current_tick: u64 = 0;
    let mut current_us: u64 = 0;
    let mut current_tempo: u32 = 500_000;

    for tc in tempo_changes {
        if tc.tick > current_tick {
            let delta_ticks = tc.tick - current_tick;
            let delta_us =
                (delta_ticks as u128 * current_tempo as u128) / ticks_per_quarter as u128;
            current_us = current_us.saturating_add(delta_us as u64);
            current_tick = tc.tick;
            segments.push(TempoSegment {
                start_tick: current_tick,
                start_us: current_us,
                tempo: tc.tempo_us_per_quarter,
            });
            current_tempo = tc.tempo_us_per_quarter;
        } else if tc.tick == current_tick {
            current_tempo = tc.tempo_us_per_quarter;
            if let Some(last) = segments.last_mut() {
                if last.start_tick == current_tick {
                    last.tempo = current_tempo;
                } else {
                    segments.push(TempoSegment {
                        start_tick: current_tick,
                        start_us: current_us,
                        tempo: current_tempo,
                    });
                }
            } else {
                // At tick 0
                segments.push(TempoSegment {
                    start_tick: 0,
                    start_us: 0,
                    tempo: current_tempo,
                });
            }
        }
    }

    if segments.is_empty() || segments[0].start_tick != 0 {
        segments.insert(
            0,
            TempoSegment {
                start_tick: 0,
                start_us: 0,
                tempo: 500_000,
            },
        );
    }

    let tick_to_us = |tick: u64| -> u64 {
        let idx = segments.partition_point(|seg| seg.start_tick <= tick);
        let seg_idx = if idx == 0 { 0 } else { idx - 1 };
        let seg = &segments[seg_idx];
        let delta_ticks = tick - seg.start_tick;
        let delta_us = (delta_ticks as u128 * seg.tempo as u128) / ticks_per_quarter as u128;
        seg.start_us.saturating_add(delta_us as u64)
    };

    let mut has_note_on = false;
    let mut events: Vec<NoteEvent> = Vec::with_capacity(raw_events.len());

    for rev in raw_events {
        if rev.on {
            has_note_on = true;
        }
        let time_us = tick_to_us(rev.tick);
        events.push(NoteEvent {
            time_us,
            note: rev.note,
            on: rev.on,
            track: rev.track,
            channel: rev.channel,
        });
    }

    if !has_note_on {
        return Err(MidiError::Empty);
    }

    // Sort events by (time_us, on): NoteOff (on=false) sorts before NoteOn (on=true)
    events.sort_by(|a, b| a.time_us.cmp(&b.time_us).then_with(|| a.on.cmp(&b.on)));

    let duration_us = events.last().map(|e| e.time_us).unwrap_or(0);

    Ok(Song {
        events,
        tracks: track_infos,
        duration_us,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_vlq(vec: &mut Vec<u8>, mut val: u32) {
        let mut buf = [0u8; 4];
        let mut i = 0;
        buf[i] = (val & 0x7F) as u8;
        val >>= 7;
        while val > 0 {
            i += 1;
            buf[i] = ((val & 0x7F) as u8) | 0x80;
            val >>= 7;
        }
        for b in buf[..=i].iter().rev() {
            vec.push(*b);
        }
    }

    fn make_smf(format: u16, division: u16, tracks: &[Vec<u8>]) -> Vec<u8> {
        let mut out = Vec::new();
        // MThd
        out.extend_from_slice(b"MThd");
        out.extend_from_slice(&6u32.to_be_bytes());
        out.extend_from_slice(&format.to_be_bytes());
        out.extend_from_slice(&(tracks.len() as u16).to_be_bytes());
        out.extend_from_slice(&division.to_be_bytes());

        for trk in tracks {
            out.extend_from_slice(b"MTrk");
            out.extend_from_slice(&(trk.len() as u32).to_be_bytes());
            out.extend_from_slice(trk);
        }
        out
    }

    #[test]
    fn test_empty_bytes_and_garbage() {
        let err = parse_bytes(&[]).unwrap_err();
        assert!(matches!(err, MidiError::Parse(_)));
        assert!(err.to_string().contains("invalid MIDI"));

        let err2 = parse_bytes(b"garbage-bytes-1234").unwrap_err();
        assert!(matches!(err2, MidiError::Parse(_)));
        assert!(err2.to_string().contains("invalid MIDI"));
    }

    #[test]
    fn test_smpte_timing_unsupported() {
        // High bit set on division -> SMPTE
        let bytes = make_smf(0, 0xE728, &[vec![0x00, 0xFF, 0x2F, 0x00]]);
        let err = parse_bytes(&bytes).unwrap_err();
        assert!(matches!(err, MidiError::Unsupported(_)));
        assert!(err.to_string().contains("SMPTE"));
    }

    #[test]
    fn test_valid_file_zero_note_ons() {
        let mut trk = Vec::new();
        // End of track meta event
        trk.extend_from_slice(&[0x00, 0xFF, 0x2F, 0x00]);
        let bytes = make_smf(0, 480, &[trk]);
        let err = parse_bytes(&bytes).unwrap_err();
        assert!(matches!(err, MidiError::Empty));
        assert_eq!(err.to_string(), "MIDI has no notes");
    }

    #[test]
    fn test_note_on_vel_zero_is_off_and_order() {
        let mut trk = Vec::new();
        // Delta 0, NoteOn ch 0, note 60, vel 64
        write_vlq(&mut trk, 0);
        trk.extend_from_slice(&[0x90, 60, 64]);
        // Delta 100, NoteOn ch 0, note 60, vel 0 (treated as off)
        write_vlq(&mut trk, 100);
        trk.extend_from_slice(&[0x90, 60, 0]);
        // Delta 0, NoteOn ch 0, note 60, vel 64 (at same tick 100)
        write_vlq(&mut trk, 0);
        trk.extend_from_slice(&[0x90, 60, 64]);
        // End of track
        write_vlq(&mut trk, 0);
        trk.extend_from_slice(&[0xFF, 0x2F, 0x00]);

        let bytes = make_smf(0, 500, &[trk]);
        let song = parse_bytes(&bytes).unwrap();
        assert_eq!(song.events.len(), 3);
        assert!(song.events[0].on);
        assert_eq!(song.events[0].time_us, 0);

        // At tick 100: note-off must sort BEFORE note-on
        assert_eq!(song.events[1].time_us, 100_000);
        assert!(!song.events[1].on);

        assert_eq!(song.events[2].time_us, 100_000);
        assert!(song.events[2].on);
    }

    #[test]
    fn test_two_tempo_changes_and_multitrack() {
        // Track 0: tempo changes at tick 0 (500_000 us/qn) and tick 480 (250_000 us/qn)
        let mut trk0 = Vec::new();
        // Delta 0: Tempo 500,000 (0x07A120)
        write_vlq(&mut trk0, 0);
        trk0.extend_from_slice(&[0xFF, 0x51, 0x03, 0x07, 0xA1, 0x20]);
        // Delta 480: Tempo 250,000 (0x03D090)
        write_vlq(&mut trk0, 480);
        trk0.extend_from_slice(&[0xFF, 0x51, 0x03, 0x03, 0xD0, 0x90]);
        // End track
        write_vlq(&mut trk0, 0);
        trk0.extend_from_slice(&[0xFF, 0x2F, 0x00]);

        // Track 1: notes at tick 240, 480, 720
        let mut trk1 = Vec::new();
        // Name track
        write_vlq(&mut trk1, 0);
        trk1.extend_from_slice(&[0xFF, 0x03, 0x04, b'P', b'i', b'a', b'n']);
        // Delta 240: Note 60 on
        write_vlq(&mut trk1, 240);
        trk1.extend_from_slice(&[0x90, 60, 80]);
        // Delta 240 (tick 480): Note 62 on
        write_vlq(&mut trk1, 240);
        trk1.extend_from_slice(&[0x90, 62, 80]);
        // Delta 240 (tick 720): Note 64 on
        write_vlq(&mut trk1, 240);
        trk1.extend_from_slice(&[0x90, 64, 80]);
        // End track
        write_vlq(&mut trk1, 0);
        trk1.extend_from_slice(&[0xFF, 0x2F, 0x00]);

        let bytes = make_smf(1, 480, &[trk0, trk1]);
        let song = parse_bytes(&bytes).unwrap();

        // 480 division:
        // Tick 240 = 240/480 * 500_000 = 250_000 us
        // Tick 480 = 480/480 * 500_000 = 500_000 us
        // Tick 720 = 500_000 + 240/480 * 250_000 = 500_000 + 125_000 = 625_000 us
        assert_eq!(song.events[0].time_us, 250_000);
        assert_eq!(song.events[1].time_us, 500_000);
        assert_eq!(song.events[2].time_us, 625_000);
        assert_eq!(song.duration_us, 625_000);
        assert_eq!(song.tracks[1].name, "Pian");
        assert_eq!(song.tracks[1].note_count, 3);
        assert!(!song.tracks[1].is_drum);
    }

    #[test]
    fn test_large_delta_no_overflow() {
        let mut trk = Vec::new();
        write_vlq(&mut trk, 0x0F_FF_FF_FF);
        trk.extend_from_slice(&[0x90, 60, 64]);
        write_vlq(&mut trk, 0);
        trk.extend_from_slice(&[0xFF, 0x2F, 0x00]);

        let bytes = make_smf(0, 480, &[trk]);
        let song = parse_bytes(&bytes).unwrap();
        assert_eq!(song.events.len(), 1);
        let expected = (0x0F_FF_FF_FF_u128 * 500_000) / 480;
        assert_eq!(song.events[0].time_us, expected as u64);
    }

    #[test]
    fn test_drum_detection() {
        let mut trk = Vec::new();
        write_vlq(&mut trk, 0);
        // NoteOn ch 9 (0x99)
        trk.extend_from_slice(&[0x99, 36, 100]);
        write_vlq(&mut trk, 10);
        trk.extend_from_slice(&[0x89, 36, 0]);
        write_vlq(&mut trk, 0);
        trk.extend_from_slice(&[0xFF, 0x2F, 0x00]);

        let bytes = make_smf(0, 480, &[trk]);
        let song = parse_bytes(&bytes).unwrap();
        assert!(song.tracks[0].is_drum);
        assert_eq!(song.tracks[0].note_count, 1);
    }
}
