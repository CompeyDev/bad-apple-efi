#![cfg(target_arch = "x86_64")]

use core::arch::asm;
use core::arch::x86_64::*;

pub const XCR0_X87: u64 = 1 << 0;
pub const XCR0_SSE: u64 = 1 << 1;
pub const XCR0_AVX: u64 = 1 << 2;

pub const CR0_MASK: u64 = 0xFFFFFFFFFFFFFFFB;
pub const CR0_OR: u64 = 0x22;

pub const CR4_OSFXSR: u64 = 1 << 0;
pub const CR4_OSXMMEXCPT: u64 = 1 << 1;
pub const CR4_OSXSAVE: u64 = 1 << 18;

pub const MXCSR_DEFAULT: u32 = 0x1F80;

pub const CPUID_XSAVE_BIT: u32 = 26;
pub const CPUID_AVX_BIT: u32 = 28;

/// Initializes the floating-point unit (FPU) and configures the required control registers.
///
/// ## Mode of Operation
/// 1. `EM` (Emulation) and setting the `MP` (Monitor Coprocessor) flags are disabled for `CR0`.
/// 2. `OSFXSR` and `OSXMMEXCPT` are enabled. If `XSAVE` is supported, it `OSXSAVE` is also
///    enabled.
/// 3. `MXCSR` (responsible for handling and rounding modes for SIMD ops) is set to the default
///    value.
#[no_mangle]
pub unsafe extern "C" fn init_fpu() {
    asm!(
        "mov rax, cr0",
        "and rax, {mask}",
        "or rax, {or_bits}",
        "mov cr0, rax",
        mask = const CR0_MASK,
        or_bits = const CR0_OR,
        options(nostack)
    );

    if is_xsave_supported() {
        asm!(
            "mov rax, cr4",
            "or rax, {cr4_bits}",
            "mov cr4, rax",
            cr4_bits = const CR4_OSFXSR | CR4_OSXMMEXCPT | CR4_OSXSAVE,
            options(nostack)
        );
    } else {
        asm!(
            "mov rax, cr4",
            "or rax, {cr4_bits}",
            "mov cr4, rax",
            cr4_bits = const CR4_OSFXSR | CR4_OSXMMEXCPT,
            options(nostack)
        );
    }

    asm!(
        "ldmxcsr [{mxcsr}]",
        mxcsr = in(reg) &MXCSR_DEFAULT,
        options(nostack, readonly)
    );
}

/// Initializes AVX (Advanced Vector Extensions) support by configuring the `XCR0` register to save
/// and restore the state of AVX registers.
///
/// ## Mode of Operation
/// x87, SSE and AVX bits are set for the`XCR0` register.
#[no_mangle]
pub unsafe extern "C" fn init_avx() {
    if is_avx_supported() {
        _xsetbv(0, XCR0_X87 | XCR0_SSE | XCR0_AVX);
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
