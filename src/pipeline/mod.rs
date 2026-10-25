use core::time::Duration;

use fast_image_resize::{ImageBufferError, ResizeError};

use crate::apic::ApicTimer;
use crate::display::Display;
use crate::tsc::Ms;
use crate::video::{AudioSink, FrameSink, VideoError, VideoFrame};

mod audio;
mod pacer;
mod render;
mod stats;

pub use audio::AudioOutput;
use pacer::FramePacer;
use render::Renderer;
use stats::{FrameStats, Phase};

#[derive(Debug, Clone, Copy, thiserror::Error)]
pub enum PipelineError {
    #[error("video error: {0}")]
    Video(#[from] VideoError),
    #[error("invalid source image: {0}")]
    Image(#[from] ImageBufferError),
    #[error("resize failed: {0}")]
    Resize(#[from] ResizeError),
}

/// Silence played after the audio before stopping it. See [`Pipeline::finish`].
const AUDIO_TAIL: Duration = Duration::from_millis(100);

/// Renders decoded frames to the display and plays the audio, with frames paced to the audio
/// if there is any.
pub struct Pipeline<'a, 'd> {
    display: &'a mut Display<'d>,
    audio: Option<AudioOutput<'a>>,
    renderer: Renderer,
    pacer: FramePacer<'a>,
    stats: FrameStats,
}

impl<'a, 'd> Pipeline<'a, 'd> {
    pub fn new(
        display: &'a mut Display<'d>,
        audio: Option<AudioOutput<'a>>,
        timer: &'a ApicTimer,
        tsc_hz: u64,
        fps: u32,
    ) -> Self {
        let &mut Display { width, height, .. } = display;
        let clock = if audio.is_some() { "audio" } else { "TSC" };
        log::debug!(
            "Pacing frames to {fps} fps, {} each, by the {clock}",
            Ms(tsc_hz / u64::from(fps))
        );

        Self {
            display,
            audio,
            renderer: Renderer::new(width, height),
            pacer: FramePacer::new(timer, fps),
            stats: FrameStats::new(fps),
        }
    }

    /// Waits until the queued audio has finished playing, then stops it.
    ///
    /// Otherwise, the hardware would stop being fed and it would keep looping the buffer it
    /// has remaining, playing what would sound like looped garbage.
    pub fn finish(mut self) {
        if let Some(audio) = &mut self.audio {
            // NOTE:
            // The clock tracks what the controller has fetched. Give it 100ms of silence to
            // finish playing out, rather than stopping immediately after the clock has finished,
            // which could cut off some of the audio.
            let end = audio.end() + AUDIO_TAIL;
            self.pacer.wait(end, Some(audio));
        }
    }
}

impl FrameSink for Pipeline<'_, '_> {
    type Error = PipelineError;

    fn present(&mut self, frame: &VideoFrame) -> Result<(), PipelineError> {
        // Audio starts along with the first frame, once the player has queued some ahead
        if let Some(Err(err)) = self.audio.as_mut().map(AudioOutput::start) {
            log::warn!("Failed to start audio, pacing by the TSC instead: {err}");
            self.audio = None;
        }

        if let Some(audio) = &self.audio {
            audio.poll();
        }

        // Rather than drifting behind the clock, skip frames if needed
        if self.pacer.is_overdue(frame.pts, self.audio.as_mut()) {
            log::trace!("Dropping frame at {:?}, more than a frame behind", frame.pts);
            self.stats.end_frame(false);
            return Ok(());
        }

        self.stats.time(Phase::Render, || self.renderer.render(frame))?;

        let on_time =
            self.stats.time(Phase::Wait, || self.pacer.wait(frame.pts, self.audio.as_mut()));
        self.stats.time(Phase::Blit, || self.display.blit(self.renderer.output()));

        self.stats.end_frame(on_time);
        Ok(())
    }
}

impl AudioSink for Pipeline<'_, '_> {
    fn queue(&mut self, samples: &[i16]) -> usize {
        self.audio.as_mut().map_or(0, |audio| audio.queue(samples))
    }
}
