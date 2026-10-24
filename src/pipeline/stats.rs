use core::fmt::{self, Formatter};
use core::mem;

use crate::tsc::{Ms, TSC};

/// Records timings of each frame, under different phases. The results are logged to serial
/// at the trace level, and a summary is logged every second at the debug level.
pub struct FrameStats {
    /// Frames in a summary per second
    window: u32,
    /// Count of frames presented so far
    frame: u32,
    /// Timestamp of when the last frame was finished
    last_end: Option<u64>,
    /// Timestamp of when the first frame was started
    start: Option<u64>,
    /// Ticks each [`Phase`] of the current frame took
    phases: [u64; 3],
    /// Timings of frames since the last summary
    current: Window,
}

/// A step of presenting a frame, recorded by [`FrameStats::time`].
#[derive(Clone, Copy)]
pub enum Phase {
    Render,
    Wait,
    Blit,
}

/// Timings accumulated since the last summary.
#[derive(Default)]
struct Window {
    late: u32,
    decode: Span,
    render: Span,
    wait: Span,
    blit: Span,
}

impl FrameStats {
    pub fn new(fps: u32) -> Self {
        Self {
            window: fps,
            frame: 0,
            last_end: None,
            start: None,
            phases: [0; 3],
            current: Window::default(),
        }
    }

    /// Runs `f` as the given phase of the current frame, recording its time.
    pub fn time<T>(&mut self, phase: Phase, f: impl FnOnce() -> T) -> T {
        let start = TSC.now();
        self.start.get_or_insert(start);

        let result = f();
        self.phases[phase as usize] += TSC.now() - start;

        result
    }

    /// Records the current frame's timings, and starts the next one.
    pub fn end_frame(&mut self, on_time: bool) {
        let end = TSC.now();
        let start = self.start.take().unwrap_or(end);
        let [render, wait, blit] = mem::take(&mut self.phases);

        let decode = self.last_end.map_or(0, |last| start - last);
        self.last_end = Some(end);
        self.frame += 1;

        log::trace!(
            "Frame {}: decode {}, render {}, wait {}, blit {}",
            self.frame,
            Ms(decode),
            Ms(render),
            Ms(wait),
            Ms(blit)
        );

        let window = &mut self.current;
        window.late += u32::from(!on_time);
        window.decode.add(decode);
        window.render.add(render);
        window.wait.add(wait);
        window.blit.add(blit);

        if self.frame.is_multiple_of(self.window) {
            let Window { late, decode, render, wait, blit } = mem::take(&mut self.current);
            log::debug!(
                "Frames {}-{}, avg/max ms: decode={decode}, render={render}, wait={wait}, \
                 blit={blit}, {late} frame late",
                self.frame + 1 - self.window,
                self.frame,
            );
        }
    }
}

/// Total and worst case ticks for a [`Phase`].
#[derive(Default)]
struct Span {
    total: u64,
    max: u64,
    count: u64,
}

impl Span {
    fn add(&mut self, ticks: u64) {
        self.total += ticks;
        self.max = self.max.max(ticks);
        self.count += 1;
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", Ms(self.total / self.count.max(1)), Ms(self.max))
    }
}
