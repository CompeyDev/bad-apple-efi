#![no_main]
#![no_std]

extern crate alloc;
use uefi::runtime::Time;
use zune_png::zune_core::colorspace::ColorSpace;
use zune_png::zune_core::options::DecoderOptions;
use zune_png::PngDecoder;

use crate::apic::ApicTimer;
use crate::archive::ArchiveReader;
use crate::display::Display;
use crate::memory::UefiAllocatorManager;
use crate::pixel::*;
use crate::serial::Serial;
use crate::time::TimeExt;

mod apic;
mod archive;
mod display;
mod memory;
mod pixel;
mod serial;
mod time;

const FRAMES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/video_frames.arc"));
const TARGET_FRAMERATE_MS: u32 = 33; // ~30 FPS

// TODO: Proper error handling and reporting to display

#[uefi::entry]
fn main() -> uefi::Status {
    uefi::helpers::init().unwrap();

    // Initialize frame reader, display, memory, and APIC timer
    let mut reader = ArchiveReader::new(FRAMES);
    let mut display = Display::open().expect("Failed to open display");
    let viewmodel = display.as_frame();
    let _mem_region = unsafe { UefiAllocatorManager::init() };
    let timer = ApicTimer::calibrate(16);

    display.clear();

    while let Some((_, data)) = reader.next_file() {
        let start = Time::now().unwrap().as_timestamp();

        // TODO: Downscale if exceeding size
        let mut decoder = PngDecoder::new_with_options(
            data,
            DecoderOptions::default()
                .png_set_strip_to_8bit(true)
                .set_max_width(display.width)
                .set_max_height(display.height),
        );

        let pixels = decoder.decode().unwrap().u8().unwrap();
        let colorspace = decoder.get_colorspace().unwrap();
        let info = decoder.get_info().unwrap();
        let bytes_per_pixel = match colorspace {
            ColorSpace::RGB => RGB_CHANNELS_SIZE,
            ColorSpace::RGBA => RGBA_CHANNELS_SIZE,
            ColorSpace::Luma => LUMA_CHANNELS_SIZE,
            _ => continue,
        };

        let scaled_width = display.width;
        let scaled_height = display.height;
        let scaled = scale_nn_fast(
            &pixels,
            info.width,
            info.height,
            scaled_width,
            scaled_height,
            bytes_per_pixel,
        );

        let content = (0..scaled_height).flat_map(|y| {
            (0..scaled_width).map({
                let pixels_inner = &scaled;
                move |x| {
                    let idx = (y * scaled_width + x) * bytes_per_pixel;
                    let pixel = match colorspace {
                        ColorSpace::RGB | ColorSpace::RGBA => Color::Rgb(
                            pixels_inner[idx],
                            pixels_inner[idx + 1],
                            pixels_inner[idx + 2],
                        ),
                        ColorSpace::Luma | ColorSpace::LumaA => {
                            let gray = pixels_inner[idx];
                            Color::Rgb(gray, gray, gray)
                        }
                        _ => Color::default(),
                    };

                    // No need to two tone map for a "retro" feeling on high res mode
                    #[cfg(not(feature = "high_res"))]
                    let pixel = pixel.to_two_tone(Color::Gray, Color::WHITE, 160);
                    (x, y, pixel)
                }
            })
        });

        let _ = display.draw(content, viewmodel);

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
        uefi::Status::SUCCESS
    }
}

#[cfg(not(feature = "qemu"))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    writeln!(Serial, "panic: {}", info.message()).unwrap();

    if let Some(location) = info.location() {
        writeln!(Serial, "panic: file '{}' at line {}", location.file(), location.line()).unwrap();
    }

    loop {
        core::hint::spin_loop();
    }
}
