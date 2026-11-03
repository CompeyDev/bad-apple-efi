#![cfg(target_arch = "x86_64")]

use core::arch::x86_64::*;

use x86_64::registers::control::{Cr0, Cr0Flags, Cr4, Cr4Flags};
use x86_64::registers::mxcsr::{self, MxCsr};
use x86_64::registers::xcontrol::{XCr0, XCr0Flags};

pub const CPUID_XSAVE_BIT: u32 = 26;
pub const CPUID_AVX_BIT: u32 = 28;

/// Initializes the floating-point unit (FPU) and configures the required control registers.
///
/// ## Mode of Operation
/// 1. `EM` (Emulation) is cleared, and `MP` (Monitor Coprocessor) and `NE` (Numeric Error) are
///    set in `CR0`.
/// 2. `OSFXSR` and `OSXMMEXCPT` are enabled. If `XSAVE` is supported, `OSXSAVE` is also
///    enabled.
/// 3. `MXCSR` (responsible for handling and rounding modes for SIMD ops) is set to the default
///    value.
#[no_mangle]
pub unsafe extern "C" fn init_fpu() {
    Cr0::update(|flags| {
        flags.remove(Cr0Flags::EMULATE_COPROCESSOR);
        flags.insert(Cr0Flags::MONITOR_COPROCESSOR | Cr0Flags::NUMERIC_ERROR);
    });

    Cr4::update(|flags| {
        flags.insert(Cr4Flags::OSFXSR | Cr4Flags::OSXMMEXCPT_ENABLE);
        flags.set(Cr4Flags::OSXSAVE, is_xsave_supported());
    });

    mxcsr::write(MxCsr::default());
}

/// Initializes AVX (Advanced Vector Extensions) support by configuring the `XCR0` register to save
/// and restore the state of AVX registers.
///
/// ## Mode of Operation
/// x87, SSE and AVX bits are set for the`XCR0` register.
#[no_mangle]
pub unsafe extern "C" fn init_avx() {
    if is_avx_supported() {
        XCr0::write(XCr0Flags::X87 | XCr0Flags::SSE | XCr0Flags::AVX);
    }
}

/// Returns whether `XSAVE` (indicated by `CPUID.1:ECX[26]`) is supported by the processor.
#[inline(always)]
pub fn is_xsave_supported() -> bool {
    (__cpuid(1).ecx & (1 << CPUID_XSAVE_BIT)) != 0
}

/// Returns whether AVX (Advanced Vector Extensions) support (indicated by`CPUID.1:ECX[28]`) is
/// present in the processor.
#[inline(always)]
pub fn is_avx_supported() -> bool {
    (__cpuid(1).ecx & (1 << CPUID_AVX_BIT)) != 0
}
