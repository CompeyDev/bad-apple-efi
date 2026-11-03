use core::time::Duration;

use crate::hda::{Hda, HdaError, Stream};
use crate::video::{AUDIO_CHANNELS, AUDIO_SAMPLE_RATE};

const NANOS_PER_SEC: u64 = 1_000_000_000;

/// Plays audio through an HDA stream, whose progress is the clock frames are paced against.
pub struct AudioOutput<'a> {
    hda: &'a Hda,
    stream: Stream<'a>,
    playing: bool,
    /// Samples queued in total
    queued: u64,
}

impl<'a> AudioOutput<'a> {
    pub fn new(hda: &'a Hda, stream: Stream<'a>) -> Self {
        Self { hda, stream, playing: false, queued: 0 }
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
