use alloc::vec::Vec;

const MIDI_HEADER: &[u8; 4] = b"MThd";
const TRACK_HEADER: &[u8; 4] = b"MTrk";

/// A parser for Standard MIDI Files ([SMF]) that extracts note events and timing
/// information.
///
/// [SMF]: https://en.wikipedia.org/wiki/MIDI#Technical_specifications
#[derive(Debug, Clone)]
pub struct MidiReader {
    ticks_per_quarter: u16,
    tempo_us_per_quarter: u32,
    tracks: Vec<TrackState>,
}

/// Metadata about a MIDI file's content and characteristics.
#[allow(unused)]
#[derive(Debug, Clone)]
pub struct MidiInfo {
    /// The maximum number of notes playing at the same time at any point in the file
    pub max_simultaneous_notes: usize,
    /// The total number of note events in the file
    pub total_notes: usize,
    /// The total duration of the MIDI file in milliseconds
    pub duration_ms: u64,
}

/// A single note event with absolute timing and duration.
#[allow(unused)]
#[derive(Debug, Clone, Default)]
pub struct MidiEvent {
    /// Absolute timestamp in milliseconds from the start of the file
    pub timestamp_ms: u64,
    /// MIDI note number (0-127, where 60 = Middle C)
    pub note: u8,
    /// Note velocity (0-127), currently unused by PCS
    pub velocity: u8,
    /// Duration of the note in milliseconds
    pub duration_ms: u32,
}

#[derive(Debug, Clone)]
struct TrackState {
    cursor: Cursor,
    end: usize,
    absolute_tick: u64,
    running_status: u8,
    active_notes: Vec<(u8, u64)>,
}

pub type Result<T, E = MidiError> = core::result::Result<T, E>;
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MidiError {
    #[error("Invalid MIDI header")]
    HeaderMismatch,
    #[error("MIDI file must be at least 14 bytes")]
    TooSmall,
    #[error("Hit unexpected EOF while parsing data")]
    UnexpectedEof,
    #[error("Unsupported event type: {0}")]
    UnsupportedEvent(u8),
}

impl MidiReader {
    /// Create a new MIDI reader from raw file data.
    ///
    /// Parses the MIDI header and track headers to prepare for event extraction.
    /// Does not parse the actual events until [`parse`](Self::parse) is called.
    ///
    /// ## Errors
    /// - [`MidiError::TooSmall`] if the file is smaller than 14 bytes
    /// - [`MidiError::HeaderMismatch`] if the file header is invalid or corrupted
    /// - [`MidiError::UnexpectedEof`] if the file ends unexpectedly
    pub fn new(data: &[u8]) -> Result<Self> {
        if data.len() < 14 {
            return Err(MidiError::TooSmall);
        }

        let mut c = Cursor::new(data.to_vec(), 0);

        if c.read_u32()? != u32::from_be_bytes(*MIDI_HEADER) {
            return Err(MidiError::HeaderMismatch);
        }

        if c.read_u32()? != 6 {
            return Err(MidiError::HeaderMismatch);
        }

        let _format = c.read_u16()?;
        let num_tracks = c.read_u16()?;
        let division = c.read_u16()?;

        let ticks_per_quarter = division & 0x7FFF;

        let mut tracks = Vec::new();

        for _ in 0..num_tracks {
            if c.read_u32()? != u32::from_be_bytes(*TRACK_HEADER) {
                return Err(MidiError::HeaderMismatch);
            }

            let len = c.read_u32()? as usize;
            let start = c.pos;
            let end = start + len;

            tracks.push(TrackState {
                cursor: Cursor::new(c.data.clone(), start),
                end,
                absolute_tick: 0,
                running_status: 0,
                active_notes: Vec::new(),
            });

            c.pos = end;
        }

        Ok(Self { ticks_per_quarter, tempo_us_per_quarter: 500_000, tracks })
    }

    /// Parse all MIDI events from all tracks and return them sorted by timestamp.
    ///
    /// This consumes the reader and extracts all note events with absolute timing
    /// information. The events are sorted chronologically by their start time.
    ///
    /// ## Errors
    /// Returns [`MidiError`] if the MIDI data is malformed or contains unsupported
    /// event types.
    pub fn parse(mut self) -> Result<Vec<MidiEvent>> {
        let mut all_events = Vec::new();
        while let Some(event) = self.next_event() {
            all_events.push(event);
        }

        all_events.sort_by_key(|e| e.timestamp_ms);
        Ok(all_events)
    }

    /// Extract metadata information about the MIDI file without consuming the reader.
    ///
    /// Analyzes the MIDI file to determine polyphony characteristics and duration.
    /// Use [`MidiInfo::is_monophonic`] to check if the file contains only one note
    /// at a time.
    ///
    /// ## Errors
    /// Returns [`MidiError`] if the MIDI data cannot be parsed.
    pub fn info(&mut self) -> Result<MidiInfo> {
        let events = self.clone().parse()?;
        if events.is_empty() {
            return Ok(MidiInfo { max_simultaneous_notes: 0, total_notes: 0, duration_ms: 0 });
        }

        let mut max_simultaneous_notes = 0;
        let mut active_notes = Vec::new();

        let mut timeline = Vec::new();
        for event in &events {
            // Create note start and note end events timeline (time, on / off, note)
            timeline.push((event.timestamp_ms, true, event.note));
            timeline.push((event.timestamp_ms + event.duration_ms as u64, false, event.note));
        }

        timeline.sort_by_key(|(time, _, _)| *time);
        for (_time, is_on, note) in timeline {
            if is_on {
                active_notes.push(note);
                max_simultaneous_notes = max_simultaneous_notes.max(active_notes.len());
            } else {
                active_notes.retain(|n| *n != note);
            }
        }

        let duration = events
            .last()
            .map(|event| event.timestamp_ms + event.duration_ms as u64)
            .unwrap_or_default();

        Ok(MidiInfo { max_simultaneous_notes, total_notes: events.len(), duration_ms: duration })
    }

    /// Convert a polyphonic MIDI file to monophonic by keeping only the highest note
    /// at any given time.
    ///
    /// When multiple notes are playing simultaneously, this method selects the highest
    /// pitch and creates a continuous monophonic melody. Note transitions are handled
    /// smoothly by ending the current note and starting the new one at the exact moment
    /// of change.
    ///
    /// ## Errors
    /// Returns [`MidiError`] if the MIDI data cannot be parsed.
    pub fn as_monophonic(&mut self) -> Result<Vec<MidiEvent>> {
        let events = self.clone().parse()?;
        let mut timeline = Vec::new();

        for event in &events {
            timeline.push((event.timestamp_ms, true, event.clone()));
            timeline.push((event.timestamp_ms + event.duration_ms as u64, false, event.clone()));
        }

        timeline.sort_by_key(|event| {
            // Priority based on whether note is on or off
            (
                event.0,                     // time
                if event.1 { 0 } else { 1 }, // whether note is on or off
            )
        });

        let mut active_notes = Vec::new();
        let mut mono_events = Vec::new();
        let mut current_note: Option<(u8, u64)> = None; // (note, start_time)

        for (time, is_on, event) in timeline {
            if is_on {
                active_notes.push(event.note);
            } else {
                active_notes.retain(|&n| n != event.note);
            }

            let highest = active_notes.iter().max().copied();
            match (current_note, highest) {
                (Some((playing, start)), Some(should_play)) if playing != should_play => {
                    // Switch notes, end current and start new
                    mono_events.push(MidiEvent {
                        timestamp_ms: start,
                        note: playing,
                        velocity: 64,
                        duration_ms: (time - start) as u32,
                    });
                    current_note = Some((should_play, time));
                }
                (Some((playing, start)), None) => {
                    // Stop playing
                    mono_events.push(MidiEvent {
                        timestamp_ms: start,
                        note: playing,
                        velocity: 64,
                        duration_ms: (time - start) as u32,
                    });
                    current_note = None;
                }
                (None, Some(should_play)) => {
                    // Start playing
                    current_note = Some((should_play, time));
                }
                _ => {}
            }
        }

        Ok(mono_events)
    }

    /// Extract the next chronological event from any track.
    ///
    /// This method finds the track with the earliest next event and parses it.
    /// Returns `None` when parsing incomplete events (like note-on without note-off)
    /// or unsupported MIDI messages.
    ///
    /// **NOTE**: It is not suitable to use this method in a `while` pattern match
    /// loop, as it may return `None` in the case an event is unsupported. Instead,
    /// use [`MidiReader::next_event`] which discards unsupported events and only
    /// returns `None` when all events have been exhausted.
    ///
    /// ## Errors
    /// Returns [`MidiError`] if the event data is malformed.
    pub fn try_next_event(&mut self) -> Result<Option<MidiEvent>> {
        let mut best_track = None;
        let mut best_tick = u64::MAX;

        for (i, t) in self.tracks.iter().enumerate() {
            if t.cursor.pos < t.end {
                let delta = t.cursor.peek_vlq()? as u64;
                let tick = t.absolute_tick + delta;
                if tick < best_tick {
                    best_tick = tick;
                    best_track = Some(i);
                }
            }
        }

        match best_track {
            Some(i) => self.parse_event(i),
            None => Ok(None),
        }
    }

    /// Extract the next chronological event from any track, discarding it if unparseable.
    ///
    /// This method is typically used as a part of a `while` pattern matching loop, and returns
    /// `None` only once the all events have been exhausted.
    ///
    ///
    /// ## Example
    /// A simplified example where we play the notes from a MIDI file using the `PCSpeaker`
    /// driver.
    ///
    /// ```no_run
    /// let mut midi = MidiReader::new(include_bytes!("..."));
    /// let timer = ApicTimer::calibrate(16);
    ///
    /// while let Some(event) in midi.next_event() {
    ///     // Play the note
    ///     PCSpeaker::play_note(event.note);
    ///     
    ///     // Wait until the current note is finished
    ///     timer.delay(event.duration_ms);
    ///
    ///     // Stop playing
    ///     PCSpeaker::silence();
    /// }
    /// ```
    pub fn next_event(&mut self) -> Option<MidiEvent> {
        match self.try_next_event() {
            Ok(Some(event)) => Some(event),
            _ => {
                if !self.tracks.iter().all(|t| t.cursor.pos >= t.end) {
                    return self.next_event();
                }

                // Stop once all tracks are exhausted
                None
            }
        }
    }

    fn parse_event(&mut self, idx: usize) -> Result<Option<MidiEvent>> {
        let track = &mut self.tracks[idx];

        let delta = track.cursor.read_vlq()? as u64;
        track.absolute_tick += delta;

        let mut status = track.cursor.read_u8()?;
        if status < 0x80 {
            track.cursor.pos -= 1;
            status = track.running_status;
        } else {
            track.running_status = status;
        }

        let tpq = self.ticks_per_quarter;
        let tempo = self.tempo_us_per_quarter;

        match status & 0xF0 {
            0x80 => Self::note_off(track, tpq, tempo),
            0x90 => Self::note_on(track, tpq, tempo),
            0xA0 | 0xB0 | 0xE0 => {
                // Unsupported: 2 byte events
                track.cursor.read_u8()?;
                track.cursor.read_u8()?;
                Ok(None)
            }
            0xC0 | 0xD0 => {
                // Unsupported: 1 byte events
                track.cursor.read_u8()?;
                Ok(None)
            }
            0xF0 => Self::system_event(track, status, &mut self.tempo_us_per_quarter),
            e => Err(MidiError::UnsupportedEvent(e)),
        }
    }

    fn note_on(track: &mut TrackState, tpq: u16, tempo: u32) -> Result<Option<MidiEvent>> {
        let note = track.cursor.read_u8()?;
        let vel = track.cursor.read_u8()?;

        if vel == 0 {
            return Self::finish_note(track, note, tpq, tempo);
        }

        track.active_notes.push((note, track.absolute_tick));
        Ok(None)
    }

    fn note_off(track: &mut TrackState, tpq: u16, tempo: u32) -> Result<Option<MidiEvent>> {
        let note = track.cursor.read_u8()?;
        track.cursor.read_u8()?;
        Self::finish_note(track, note, tpq, tempo)
    }

    fn finish_note(
        track: &mut TrackState,
        note: u8,
        tpq: u16,
        tempo: u32,
    ) -> Result<Option<MidiEvent>> {
        if let Some(i) = track.active_notes.iter().position(|(n, _)| *n == note) {
            let (_, start_tick) = track.active_notes.remove(i);
            let duration = track.absolute_tick - start_tick;

            let timestamp_ms = (start_tick * tempo as u64 / tpq as u64) / 1_000;
            let duration_ms = (duration * tempo as u64 / tpq as u64) / 1_000;

            return Ok(Some(MidiEvent {
                timestamp_ms,
                note,
                velocity: 64,
                duration_ms: duration_ms as u32,
            }));
        }
        Ok(None)
    }

    fn system_event(
        track: &mut TrackState,
        status: u8,
        tempo_us_per_quarter: &mut u32,
    ) -> Result<Option<MidiEvent>> {
        if status == 0xFF {
            let meta = track.cursor.read_u8()?;
            let len = track.cursor.read_vlq()? as usize;

            if meta == 0x51 && len == 3 {
                *tempo_us_per_quarter = ((track.cursor.read_u8()? as u32) << 16)
                    | ((track.cursor.read_u8()? as u32) << 8)
                    | (track.cursor.read_u8()? as u32);
            } else {
                track.cursor.pos += len;
            }
        } else {
            let len = track.cursor.read_vlq()? as usize;
            track.cursor.pos += len;
        }
        Ok(None)
    }
}

/// A cursor for reading binary data with position tracking.
#[derive(Debug, Clone)]
struct Cursor {
    data: Vec<u8>,
    pos: usize,
}

impl Cursor {
    fn new(data: Vec<u8>, pos: usize) -> Self {
        Self { data, pos }
    }

    /// Read a single byte and advance the cursor.
    fn read_u8(&mut self) -> Result<u8> {
        if self.pos >= self.data.len() {
            return Err(MidiError::UnexpectedEof);
        }
        let b = self.data[self.pos];
        self.pos += 1;
        Ok(b)
    }

    /// Read a big-endian 16-bit unsigned integer.
    fn read_u16(&mut self) -> Result<u16> {
        Ok(((self.read_u8()? as u16) << 8) | self.read_u8()? as u16)
    }

    /// Read a big-endian 32-bit unsigned integer.
    fn read_u32(&mut self) -> Result<u32> {
        Ok(((self.read_u8()? as u32) << 24)
            | ((self.read_u8()? as u32) << 16)
            | ((self.read_u8()? as u32) << 8)
            | (self.read_u8()? as u32))
    }

    /// Read a MIDI variable-length quantity (VLQ).
    ///
    /// VLQs can be 1-4 bytes long, with the high bit indicating continuation.
    fn read_vlq(&mut self) -> Result<u32> {
        let mut val = 0u32;
        for _ in 0..4 {
            let b = self.read_u8()?;
            val = (val << 7) | (b & 0x7F) as u32;
            if b & 0x80 == 0 {
                break;
            }
        }
        Ok(val)
    }

    /// Peek at the next VLQ value without advancing the cursor.
    fn peek_vlq(&self) -> Result<u32> {
        let mut pos = self.pos;
        let mut val = 0u32;
        for _ in 0..4 {
            if pos >= self.data.len() {
                return Err(MidiError::UnexpectedEof);
            }
            let b = self.data[pos];
            pos += 1;
            val = (val << 7) | (b & 0x7F) as u32;
            if b & 0x80 == 0 {
                break;
            }
        }
        Ok(val)
    }
}

impl MidiInfo {
    /// Check if the MIDI file is monophonic (never has more than one note playing
    /// simultaneously).
    pub fn is_monophonic(&self) -> bool {
        self.max_simultaneous_notes <= 1
    }
}
