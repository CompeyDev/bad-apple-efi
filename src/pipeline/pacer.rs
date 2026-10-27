use core::time::Duration;

use super::audio::AudioOutput;
use crate::apic::APIC_TIMER;
use crate::tsc::TSC;

/// Longest sleep while waiting on the audio clock, so the hardware buffer keeps getting data.
const AUDIO_POLL: Duration = Duration::from_millis(4);

/// Scheduler for frame rendering.
///
/// Frames are shown at their timestamps against a clock, which is the audio being played
/// when there is any, so the video stays in sync with it. Otherwise, the TSC is used.
pub struct FramePacer {
    period: Duration,
    start: Option<u64>,
}

impl FramePacer {
    pub fn new(fps: u32) -> Self {
        Self { period: Duration::from_secs(1) / fps, start: None }
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

            // With audio, we need to wake up a bit earlier to feed the buffer
            match audio.as_deref_mut() {
                Some(audio) => {
                    APIC_TIMER.sleep(remaining.min(AUDIO_POLL));
                    audio.poll();
                }
                None => APIC_TIMER.sleep(remaining),
            }
        }

        on_time
    }
}
