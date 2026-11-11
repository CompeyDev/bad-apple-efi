use core::time::Duration;

use crate::hda::{Hda, HdaError, Stream};
use crate::video::{AUDIO_CHANNELS, AUDIO_SAMPLE_RATE};

const NANOS_PER_SEC: u64 = 1_000_000_000;
const CURVE_POWER: f32 = 3.0; // power 3 = cubic

/// Plays audio through an HDA stream, whose progress is the clock frames are paced against.
pub struct AudioOutput<'a> {
    hda: &'a Hda,
    stream: Stream<'a>,
    playing: bool,
    /// Samples queued in total
    queued: u64,
    volume: u8,
    muted: bool,
}

impl<'a> AudioOutput<'a> {
    /// The maximum volume in percent that the output, also being the default volume.
    pub const MAX_VOLUME: u8 = 100;

    pub fn new(hda: &'a Hda, stream: Stream<'a>) -> Self {
        let volume = Self::MAX_VOLUME;
        Self { hda, stream, playing: false, queued: 0, volume, muted: false }
    }

    /// Pauses or resumes playing, once started.
    pub fn set_paused(&mut self, paused: bool) -> Result<(), HdaError> {
        self.stream.play(!paused)
    }

    /// Changes the volume by `step` percent, returning the new volume.
    pub fn adjust_volume(&mut self, step: i8) -> Result<u8, HdaError> {
        let volume = self.volume.saturating_add_signed(step).min(Self::MAX_VOLUME);
        self.apply(volume, self.muted)?;
        self.volume = volume;
        Ok(volume)
    }

    /// Mutes or unmutes the output, returning whether it is now muted.
    pub fn toggle_mute(&mut self) -> Result<bool, HdaError> {
        self.apply(self.volume, !self.muted)?;
        self.muted ^= true;
        Ok(self.muted)
    }

    /// Sets the output to `volume` percent, which is silent when `muted` is true or at 0%.
    ///
    /// The volume is of the shape set by [`CURVE_POWER`]. Currently, it is cubic, making the
    /// volume 60 dB quieter with each step down by a factor of 10. It is adjusted to the range
    /// of the output.
    fn apply(&self, volume: u8, muted: bool) -> Result<(), HdaError> {
        let range = -self.stream.volume_range()?.start() as f32;
        let depth = (2000.0 * CURVE_POWER).min(range); // each power is 20 dB quieter
        let fraction = f32::from(volume) / f32::from(Self::MAX_VOLUME);
        let decibels = depth * libm::log10f(fraction); // -inf at 0%, saturates due to cast

        self.stream.set_volume_db(decibels as i32)?;
        self.stream.set_muted(muted || volume == 0) // a volume of 0% should be a mute
    }

    /// Starts playing what has been queued so far, unless already playing.
    pub fn start(&mut self) -> Result<(), HdaError> {
        if !self.playing {
            log::debug!("Starting audio with {} samples queued", self.stream.queued());
            self.stream.play(true)?;
            self.playing = true;
        }

        Ok(())
    }

    /// Queues as many of the samples as there is space for, returning how many were queued.
    pub fn queue(&mut self, samples: &[i16]) -> usize {
        let queued = self.stream.queue(samples);
        self.queued += queued as u64;
        queued
    }

    /// Feeds queued samples to the hardware, see [`Hda::poll`] for how often it is needed.
    pub fn poll(&self) {
        self.hda.poll();
    }

    /// How much of the audio has been played.
    pub fn clock(&mut self) -> Duration {
        duration(self.stream.played())
    }

    /// When all of the queued audio will have been played.
    pub fn end(&self) -> Duration {
        duration(self.queued)
    }
}

/// Converts a count of interleaved samples to how long they play for.
fn duration(samples: u64) -> Duration {
    let frames = samples / u64::from(AUDIO_CHANNELS);
    Duration::from_nanos(frames * NANOS_PER_SEC / u64::from(AUDIO_SAMPLE_RATE))
}
