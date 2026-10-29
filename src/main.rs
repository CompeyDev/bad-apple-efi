#![no_main]
#![no_std]
#![feature(abi_x86_interrupt)]

#[cfg(not(target_arch = "x86_64"))]
compile_error!("only x86_64 targets are supported");

extern crate alloc;

use core::ffi::c_void;

use log::{info, warn};
use uefi::runtime::ResetType;
use uefi::{boot, runtime, table, Handle, Status};

use crate::apic::APIC_TIMER;
use crate::display::Display;
use crate::hda::Hda;
use crate::memory::UefiAllocatorManager;
use crate::pipeline::{AudioOutput, Pipeline};
use crate::serial::CONSOLE;
use crate::tsc::{Tsc, TSC};
use crate::video::{VideoPlayer, AUDIO_CHANNELS, AUDIO_SAMPLE_RATE};

mod apic;
mod cpu_features;
mod display;
mod hda;
mod interrupts;
mod libc;
mod memory;
mod paging;
mod pipeline;
mod pixel;
mod segments;
mod serial;
mod tsc;
mod video;

static VIDEO_DATA: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/bad_apple.webm"));

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

fn main_impl(internal_image_handle: Handle, internal_system_table: *const c_void) -> ! {
    unsafe {
        boot::set_image_handle(internal_image_handle);
        table::set_system_table(internal_system_table.cast());
    }

    serial::CONSOLE.init();
    uefi::helpers::init().unwrap();

    info!("UEFI helpers initialized");
    info!(
        "CPU features: AVX={avx}, XSAVE={xsave}",
        avx = cpu_features::is_avx_supported(),
        xsave = cpu_features::is_xsave_supported()
    );

    // display + memory
    let mut display = Display::open().expect("Failed to open display");
    let (region, mut tables) = unsafe { UefiAllocatorManager::init() };

    // timers
    APIC_TIMER.calibrate(16);

    let tsc_hz = TSC.hz().expect("TSC should be calibrated");
    info!("TSC frequency: {} MHz", tsc_hz / 1_000_000);

    if !Tsc::is_invariant() {
        warn!("TSC is not invariant, so frame pacing may not be accurate");
    }

    // audio + display setup
    let hda = Hda::init().inspect_err(|err| warn!("Audio is unavailable: {err}")).ok();
    display.set_write_combining(&mut tables, region.start as u64..region.end() as u64);
    display.clear();

    let mut player = VideoPlayer::new(VIDEO_DATA).expect("Failed to open video");
    let audio = hda.as_ref().filter(|_| player.has_audio()).and_then(|hda| {
        let stream = hda.open_stream(AUDIO_SAMPLE_RATE, AUDIO_CHANNELS.into());
        let stream = stream.inspect_err(|err| warn!("Audio is unavailable: {err}")).ok()?;
        Some(AudioOutput::new(hda, stream))
    });

    let mut pipeline = Pipeline::new(&mut display, audio, tsc_hz, player.framerate());

    let start = TSC.now();
    player.play(&mut pipeline).expect("Playback failed");
    pipeline.finish();

    if let Some(time) = TSC.to_duration(TSC.now() - start) {
        info!("Playback finished in {time:.2?}");
    }

    if cfg!(debug_assertions) {
        // Hang indefinitely in debug mode
        loop {
            x86_64::instructions::hlt()
        }
    } else {
        // Boot services are gone, so we can't return to the firmware, but runtime services are left
        runtime::reset(ResetType::SHUTDOWN, Status::SUCCESS, None)
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    CONSOLE.write_panic(info);

    // Exits QEMU through its `isa-debug-exit` device, with the same code as the uefi crate
    #[cfg(feature = "qemu")]
    {
        // While the `uefi` crate already provides the `qemu` feature, it relies on boot services
        // which we already exit early, since it relies on the simple text protocol. Since we init
        // our own serial console anyway, that's not necessary and we call `qemu-exit` directly
        use qemu_exit::QEMUExit;
        qemu_exit::X86::new(0xF4, 3).exit_failure();
    }

    #[cfg(not(feature = "qemu"))]
    loop {
        // Idle permanently
        x86_64::instructions::interrupts::disable();
        x86_64::instructions::hlt();
    }
}
