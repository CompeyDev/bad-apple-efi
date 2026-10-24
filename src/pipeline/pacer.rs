use crate::apic::ApicTimer;
use crate::tsc::{Ms, TSC};

/// Scheduler for frame rendering.
///
/// Keeps track of time consumed for decoding and rendering against the frametime
/// budget and wait times adjusts accordingly in order to not accumulate a time
/// error across multiple frames.
pub struct FramePacer<'a> {
    timer: &'a ApicTimer,
    period: u64,
    deadline: Option<u64>,
}

impl<'a> FramePacer<'a> {
    pub fn new(timer: &'a ApicTimer, tsc_hz: u64, fps: u32) -> Self {
        Self { timer, period: tsc_hz / u64::from(fps), deadline: None }
    }

    /// Blocks until the current frame's deadline, then schedules the next one. Returns whether
    /// the deadline was met.
    pub fn wait(&mut self) -> bool {
        let now = TSC.now();
        let deadline = *self.deadline.get_or_insert(now);

        if let Some(remaining) = deadline.checked_sub(now).and_then(|ticks| TSC.to_duration(ticks))
        {
            let millis = remaining.as_secs() * 1000 + u64::from(remaining.subsec_millis());
            self.timer.delay(millis as u32);
            // TODO: use APIC TSC timer mode.
        }

        // When more than a frame behind, resync instead of trying to catch up.
        if now > deadline + self.period {
            log::trace!("{} behind schedule, resyncing", Ms(now - deadline));
        }
        self.deadline = Some(deadline.max(now.saturating_sub(self.period)) + self.period);

        now <= deadline
    }
}
