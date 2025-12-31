#![no_main]
#![no_std]

extern crate alloc;

use alloc::vec;
use core::ffi::c_void;
use core::fmt::Write;

use fast_image_resize::images::Image;
use fast_image_resize::{PixelType, Resizer};
use uefi::runtime::Time;
use uefi::{boot, table, Handle, Status};
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
mod cpu_features;
mod display;
mod memory;
mod pixel;
mod serial;
mod time;

const FRAMES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/video_frames.arc"));
const TARGET_FRAMERATE_MS: u32 = 33; // ~30 FPS

#[unsafe(naked)]
#[unsafe(export_name = "efi_main")]
unsafe extern "efiapi" fn main() {
    // UEFI entrypoint which initializes required CPU features and calls the
    // actual main implementation. This is a naked function to prevent any
    // tampering or code injection by the compiler which may be depend on
    // uninitialized features (e.g. hardware floats), since the compiler is
    // configured to assume some features are guaranteed to exist.

    core::arch::naked_asm!(
        // Save UEFI parameters temporarily
        "push rcx", // image handle
        "push rdx", // system table

        "call {init_fpu}",
        "call {init_avx}",

        // Restore parameters and trigger real main
        "pop rdx",
        "pop rcx",
        "jmp {main_impl}",

        init_fpu = sym cpu_features::init_fpu,
        init_avx = sym cpu_features::init_avx,
        main_impl = sym main_impl,
    )
}

fn main_impl(internal_image_handle: Handle, internal_system_table: *const c_void) -> Status {
    unsafe {
        boot::set_image_handle(internal_image_handle);
        table::set_system_table(internal_system_table.cast());
    }

    uefi::helpers::init().unwrap();

    // Initialize frame reader, display, memory, and APIC timer
    let mut reader = ArchiveReader::new(FRAMES);
    let mut display = Display::open().expect("Failed to open display");
    let viewmodel = display.as_frame();
    let _mem_region = unsafe { UefiAllocatorManager::init() };
    let timer = ApicTimer::calibrate(16);

    display.clear();

    // PERF: We allocate this buffers once, and set their sizes on the initial frame,
    // then reuse them for the rest of the frames. Since we do not know the number of
    // channels, we assume the maximum possible channel count initially
    const LARGEST_PIXEL_TYPE: PixelType = PixelType::U8x4;
    let scaled_width = display.width;
    let scaled_height = display.height;

    let mut pixels = vec![0u8; display.width * display.height * LARGEST_PIXEL_TYPE.size()];
    let mut scaled = Image::new(scaled_width as u32, scaled_height as u32, LARGEST_PIXEL_TYPE);
    let mut resizer = Resizer::new();

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

        // Decode the image into the buffer
        decoder.decode_into(&mut pixels).unwrap();

        let colorspace = decoder.get_colorspace().unwrap();
        let pixel_type = match colorspace {
            ColorSpace::RGB => PixelType::U8x3,
            ColorSpace::RGBA => PixelType::U8x4,
            ColorSpace::Luma => PixelType::U8,
            _ => continue,
        };

        if scaled.pixel_type() != pixel_type {
            // Should only reallocate for the first frame, in case our assumption isn't true
            scaled = Image::new(scaled_width as u32, scaled_height as u32, pixel_type);
        }

        let (original_width, original_height) = {
            let info = decoder.get_info().unwrap();
            let dims = (info.width, info.height);

            // Actually resize the buffer if required
            pixels.resize(dims.0 * dims.1 * pixel_type.size(), 0u8);
            dims
        };

        // TODO: Detect SIMD support and fallback to basic implementation if unsupported
        // Scale the image up
        resizer
            .resize(
                &Image::from_slice_u8(
                    original_width as u32,
                    original_height as u32,
                    pixels.as_mut_slice(),
                    PixelType::U8x3,
                )
                .unwrap(),
                &mut scaled,
                None,
            )
            .unwrap();

        let content = (0..scaled_height).flat_map(|y| {
            (0..scaled_width).map({
                let pixels_inner = scaled.buffer();
                move |x| {
                    let idx = (y * scaled_width + x) * pixel_type.size();
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
