use uefi::{runtime, Result};

pub trait TimeExt {
    /// Get the current time using UEFI runtime services.
    fn now() -> Result<Self>
    where
        Self: core::marker::Sized;

    /// Convert the [Time] to a timestamp in milliseconds.
    fn as_timestamp(&self) -> u32;
}

impl TimeExt for runtime::Time {
    fn now() -> Result<runtime::Time> {
        runtime::get_time()
    }

    fn as_timestamp(&self) -> u32 {
        let mut total_time_ms: u32 = 0;

        total_time_ms += (self.year() as u32) * 365 * 24 * 60 * 60 * 1000;
        total_time_ms += (self.month() as u32) * 30 * 24 * 60 * 60 * 1000;
        total_time_ms += (self.day() as u32) * 24 * 60 * 60 * 1000;
        total_time_ms += (self.hour() as u32) * 60 * 60 * 1000;
        total_time_ms += (self.minute() as u32) * 60 * 1000;
        total_time_ms += (self.second() as u32) * 1000;
        total_time_ms += self.nanosecond() / 1_000_000;
        total_time_ms
    }
}
