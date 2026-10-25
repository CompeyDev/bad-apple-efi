use core::time::Duration;

use super::audio::AudioOutput;
use crate::apic::ApicTimer;
use crate::tsc::TSC;

/// Longest sleep while waiting on the audio clock, so the hardware buffer keeps getting data.
const AUDIO_POLL_MS: u64 = 4;

/// Scheduler for frame rendering.
///
/// Frames are shown at their timestamps against a clock, which is the audio being played
/// when there is any, so the video stays in sync with it. Otherwise, the TSC is used.
pub struct FramePacer<'a> {
    timer: &'a ApicTimer,
    period: Duration,
    start: Option<u64>,
}

impl<'a> FramePacer<'a> {
    pub fn new(timer: &'a ApicTimer, fps: u32) -> Self {
        Self { timer, period: Duration::from_secs(1) / fps, start: None }
    }

    /// The current playback time, derived from either the `audio` clock or the starting
    /// TSC timestamp.
    pub fn now(&mut self, audio: Option<&mut AudioOutput>) -> Duration {
        match audio {
            Some(audio) => audio.clock(),
            None => {
                // When no audio is present, syncing is done using the TSC clock
                let start = *self.start.get_or_insert_with(|| TSC.now());
                TSC.to_duration(TSC.now() - start).unwrap_or_default()
            }
        }
    }

    /// Whether a frame is more than a frame behind the clock. If so, it should be dropped
    /// to prevent desync.
    pub fn is_overdue(&mut self, pts: Duration, audio: Option<&mut AudioOutput>) -> bool {
        self.now(audio) > pts + self.period
    }

    /// Blocks until the frame's time has expired. Returns whether the timestamp was met.
    pub fn wait(&mut self, pts: Duration, mut audio: Option<&mut AudioOutput>) -> bool {
        let on_time = self.now(audio.as_deref_mut()) <= pts;

        loop {
            let now = self.now(audio.as_deref_mut());
            let Some(remaining) = pts.checked_sub(now).filter(|left| !left.is_zero()) else {
                break;
            };

            // TODO: use APIC TSC timer mode.
            let millis = remaining.as_secs() * 1000 + u64::from(remaining.subsec_millis());
            match audio.as_deref_mut() {
                _ if millis == 0 => core::hint::spin_loop(),
                Some(audio) => {
                    self.timer.delay(millis.min(AUDIO_POLL_MS) as u32);
                    audio.poll();
                }
                None => self.timer.delay(millis as u32),
            }
        }

        on_time
    }
}
