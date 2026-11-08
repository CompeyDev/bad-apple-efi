use core::ops::ControlFlow;
use core::time::Duration;

use fast_image_resize::{ImageBufferError, ResizeError};
use pc_keyboard::KeyCode;

use crate::display::Display;
use crate::keyboard::KEYBOARD;
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
/// Volume change per key press, in percent.
const VOLUME_STEP: i8 = 10;

/// Renders decoded frames to the display and plays the audio, with frames paced to the audio
/// if there is any.
pub struct Pipeline<'a, 'd> {
    display: &'a mut Display<'d>,
    audio: Option<AudioOutput<'a>>,
    renderer: Renderer,
    pacer: FramePacer,
    stats: FrameStats,
    paused: bool,
    stopped: bool,
}

impl<'a, 'd> Pipeline<'a, 'd> {
    pub fn new(
        display: &'a mut Display<'d>,
        audio: Option<AudioOutput<'a>>,
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
            pacer: FramePacer::new(fps),
            stats: FrameStats::new(fps),
            paused: false,
            stopped: false,
        }
    }

    /// Performs an action for the keys pressed since the last frame.
    fn handle_input(&mut self) {
        while let Some(key) = KEYBOARD.next_key() {
            match key {
                KeyCode::Escape | KeyCode::Stop => self.stopped = true,
                KeyCode::Spacebar | KeyCode::Play => self.set_paused(!self.paused),
                KeyCode::ArrowUp | KeyCode::VolumeUp => self.adjust_volume(VOLUME_STEP),
                KeyCode::ArrowDown | KeyCode::VolumeDown => self.adjust_volume(-VOLUME_STEP),
                KeyCode::M | KeyCode::Mute => self.toggle_mute(),
                _ => {}
            }
        }
    }

    fn set_paused(&mut self, paused: bool) {
        if let Some(Err(err)) = self.audio.as_mut().map(|audio| audio.set_paused(paused)) {
            log::warn!("Failed to pause the audio: {err}");
        }

        self.pacer.set_paused(paused);
        self.paused = paused;
        log::info!("{}", if paused { "Paused" } else { "Resumed" });
    }

    fn adjust_volume(&mut self, step: i8) {
        match self.audio.as_mut().map(|audio| audio.adjust_volume(step)) {
            Some(Ok(volume)) => log::info!("Volume: {volume}%"),
            Some(Err(err)) => log::warn!("Failed to change the volume: {err}"),
            None => {}
        }
    }

    fn toggle_mute(&mut self) {
        match self.audio.as_mut().map(AudioOutput::toggle_mute) {
            Some(Ok(muted)) => log::info!("{}", if muted { "Muted" } else { "Unmuted" }),
            Some(Err(err)) => log::warn!("Failed to toggle mute: {err}"),
            None => {}
        }
    }

    /// Waits until the queued audio has finished playing, then stops it. Returns whether
    /// playback was stopped early, in which case the audio is stopped right away.
    ///
    /// Otherwise, the hardware would stop being fed and it would keep looping the buffer it
    /// has remaining, playing what would sound like looped garbage.
    pub fn finish(mut self) -> bool {
        if let (false, Some(audio)) = (self.stopped, &mut self.audio) {
            // NOTE:
            // The clock tracks what the controller has fetched. Give it 100ms of silence to
            // finish playing out, rather than stopping immediately after the clock has finished,
            // which could cut off some of the audio.
            let end = audio.end() + AUDIO_TAIL;
            self.pacer.wait(end, Some(audio));
        }

        self.stopped
    }
}

impl FrameSink for Pipeline<'_, '_> {
    type Error = PipelineError;

    fn present(&mut self, frame: &VideoFrame) -> Result<ControlFlow<()>, PipelineError> {
        self.stats.begin_frame();

        // Audio starts along with the first frame, once the player has queued some ahead
        if let Some(Err(err)) = self.audio.as_mut().map(AudioOutput::start) {
            log::warn!("Failed to start audio, pacing by the TSC instead: {err}");
            self.audio = None;
        }

        if let Some(audio) = &self.audio {
            audio.poll();
        }

        // While paused, the same frame is displayed until playback is resumed or stopped
        self.handle_input();
        while self.paused && !self.stopped {
            KEYBOARD.wait_for_key();
            self.handle_input();
        }

        if self.stopped {
            return Ok(ControlFlow::Break(()));
        }

        // Rather than drifting behind the clock, skip frames if needed
        if self.pacer.is_overdue(frame.pts, self.audio.as_mut()) {
            log::trace!("Dropping frame at {:?}, more than a frame behind", frame.pts);
            self.stats.end_frame(false);
            return Ok(ControlFlow::Continue(()));
        }

        self.stats.time(Phase::Render, || self.renderer.render(frame))?;

        let on_time =
            self.stats.time(Phase::Wait, || self.pacer.wait(frame.pts, self.audio.as_mut()));
        self.stats.time(Phase::Blit, || self.display.blit(self.renderer.output()));

        self.stats.end_frame(on_time);
        Ok(ControlFlow::Continue(()))
    }
}

impl AudioSink for Pipeline<'_, '_> {
    fn queue(&mut self, samples: &[i16]) -> usize {
        self.audio.as_mut().map_or(0, |audio| audio.queue(samples))
    }
}
