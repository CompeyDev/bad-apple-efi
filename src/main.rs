#![no_main]
#![no_std]

#[cfg(not(target_arch = "x86_64"))]
compile_error!("only x86_64 targets are supported");

extern crate alloc;

use core::ffi::c_void;
use core::fmt::Write;
use uefi::{boot, table, Handle, Status};

use crate::apic::ApicTimer;
use crate::display::Display;
use crate::memory::UefiAllocatorManager;
use crate::pipeline::Pipeline;
use crate::serial::Serial;
use crate::video::VideoPlayer;

mod apic;
mod asm;
mod cpu_features;
mod display;
mod interrupts;
mod libc;
mod memory;
mod pipeline;
mod pixel;
mod segments;
mod serial;
mod video;

static IVF_DATA: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/bad_apple.ivf"));

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
    crate::println!("UEFI helpers initialized");

    // Initialize display, memory, and APIC timer
    let mut display = Display::open().expect("Failed to open display");
    let _mem_region = unsafe { UefiAllocatorManager::init() };

    // NOTE: UEFI runtime services times are sometimes very inaccurate, so we track it
    // ourselves
    let timer = ApicTimer::calibrate(16);
    let tsc_hz = ApicTimer::calibrate_tsc_with_pit();

    display.clear();

    let mut player = VideoPlayer::new(IVF_DATA).expect("Failed to open video");
    let mut pipeline = Pipeline::new(&mut display, &timer, tsc_hz, player.framerate());

    player.play(&mut pipeline).expect("Playback failed");

    if cfg!(debug_assertions) {
        // Hang indefinitely in debug mode
        loop {
            core::hint::spin_loop()
        }
    } else {
        Status::SUCCESS
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
