use fast_image_resize::{ImageBufferError, ResizeError};

use crate::apic::ApicTimer;
use crate::display::Display;
use crate::tsc::Ms;
use crate::video::{FrameSink, VideoError, VideoFrame};

mod pacer;
mod render;
mod stats;

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

/// Renders decoded frames to the display, paced to the video's framerate.
pub struct Pipeline<'a, 'd> {
    display: &'a mut Display<'d>,
    renderer: Renderer,
    pacer: FramePacer<'a>,
    stats: FrameStats,
}

impl<'a, 'd> Pipeline<'a, 'd> {
    pub fn new(display: &'a mut Display<'d>, timer: &'a ApicTimer, tsc_hz: u64, fps: u32) -> Self {
        let &mut Display { width, height, .. } = display;
        log::debug!("Pacing frames to {fps} fps, {} each", Ms(tsc_hz / u64::from(fps)));

        Self {
            display,
            renderer: Renderer::new(width, height),
            pacer: FramePacer::new(timer, tsc_hz, fps),
            stats: FrameStats::new(fps),
        }
    }
}

impl FrameSink for Pipeline<'_, '_> {
    type Error = PipelineError;

    fn present(&mut self, frame: &VideoFrame) -> Result<(), PipelineError> {
        self.stats.time(Phase::Render, || self.renderer.render(frame))?;

        let on_time = self.stats.time(Phase::Wait, || self.pacer.wait());
        self.stats.time(Phase::Blit, || self.display.blit(self.renderer.output()));

        self.stats.end_frame(on_time);
        Ok(())
    }
}
