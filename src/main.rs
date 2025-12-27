#![no_main]
#![no_std]

extern crate alloc;
use uefi::{boot, entry, runtime::Time, Status};

use crate::{
    apic::ApicTimer,
    display::{Color, Display},
    time::TimeExt,
};

mod apic;
mod display;
mod time;

include!(concat!(env!("OUT_DIR"), "/ascii.rs"));

const TARGET_FRAMERATE_MS: u32 = 33; // ~30 FPS

#[entry]
fn main() -> Status {
    uefi::helpers::init().unwrap();

    // Grab display, APIC timer, and exit boot services
    let mut display = Display::open().expect("Failed to open display");
    let timer = ApicTimer::calibrate(16);
    let _mmap = unsafe { boot::exit_boot_services(None) };

    display.clear();

    for frame in ASCII_FRAMES.iter().take(2180) {
        let start = Time::now().unwrap().as_timestamp();
        let content = frame.split('\n').enumerate().flat_map(|(y, line)| {
            line.as_bytes().iter().enumerate().map(move |(x, &value)| {
                (
                    x,
                    y,
                    if value == b'$' {
                        Color::White
                    } else {
                        Color::Gray
                    },
                )
            })
        });

        let _ = display.draw(content);

        let end = Time::now().unwrap().as_timestamp();
        let remaining_time = TARGET_FRAMERATE_MS.saturating_sub(end - start);

        timer.delay(remaining_time);
    }

    if cfg!(debug_assertions) {
        // Hang indefinitely in debug mode
        loop {
            core::hint::spin_loop()
        }
    } else {
        Status::SUCCESS
    }
}
