#![cfg(target_arch = "x86_64")]

use core::arch::naked_asm;
use core::arch::x86_64::*;
use core::mem::offset_of;

use uefi_raw::protocol::console::SimpleTextOutputProtocol;
use uefi_raw::table::boot::BootServices;
use uefi_raw::table::system::SystemTable;
use uefi_raw::Status;
use x86_64::registers::control::{Cr0, Cr0Flags, Cr4, Cr4Flags};
use x86_64::registers::mxcsr::{self, MxCsr};
use x86_64::registers::xcontrol::{XCr0, XCr0Flags};

pub const CPUID_FMA_BIT: u32 = 12;
pub const CPUID_SSE4_1_BIT: u32 = 19;
pub const CPUID_XSAVE_BIT: u32 = 26;
pub const CPUID_AVX_BIT: u32 = 28;
pub const CPUID_AVX2_BIT: u32 = 5; // NOTE: leaf 7, EBX instead of ECX

/// Every CPU feature the compiled code requires. See `.cargo/config.toml` and `build.rs`.
#[rustfmt::skip]
const REQUIRED_LEAF_1: u32 = {
      1 << CPUID_FMA_BIT    // libvpx and opus
    | 1 << CPUID_SSE4_1_BIT // SIMD for whole crate, specifically fast_image_resize
    | 1 << CPUID_AVX_BIT    // ^^ same ^^
    | 1 << CPUID_XSAVE_BIT  // `init_fpu` and `init_avx`
};

// Error message displayed on the UEFI text console. Constructed at compile time, doing
// this at runtime code depend on one of the CPU features which may not be supported.
static UNSUPPORTED_TITLE: [u16; TITLE.len() + 1] = ucs2(TITLE);
static UNSUPPORTED_BODY: [u16; BODY.len() + 1] = ucs2(BODY);

/// Encodes and null terminates ASCII into a UCS-2 string for UEFI.
const fn ucs2<const N: usize>(text: &str) -> [u16; N] {
    assert!(text.is_ascii() && text.len() < N);

    let mut out = [0; N];
    let mut i = 0;
    while i < text.len() {
        out[i] = text.as_bytes()[i] as u16;
        i += 1;
    }

    out
}

const TITLE: &str = concat!(" ", env!("CARGO_PKG_NAME"), " can't run on this CPU ");
const BODY: &str = concat!(
    "\r\n\r\n",
    " It needs an x86_64 CPU with AVX2 and FMA support\r\n",
    " (at least Intel Haswell / AMD Excavator CPUs or newer).\r\n\r\n",
    " This system will hand back to UEFI in 10 seconds...\r\n",
);

// Text attributes and styling
const BANNER_ATTRIBUTE: usize = 0x0F | 0x4 << 4; // white fg, red bg
const TEXT_ATTRIBUTE: usize = 0x07; // light grey fg, black bg (default)

/// How long the system stalls for after displaying the message, in microseconds.
const UNSUPPORTED_STALL: usize = 10_000_000; // 10s

/// Whether the CPU has every feature the code is compiled to assume.
///
/// This is naked, since any compiled code may already use those features.
#[unsafe(naked)]
#[rustfmt::skip]
pub unsafe extern "efiapi" fn is_supported() -> bool {
    naked_asm!(
        "push rbx",           // save it since CPUID writes it

        // Leaf 1 features
        "mov eax, 1",         // leaf 1, basic features
        "cpuid",              // ECX holds the features
        "and ecx, {leaf_1}",  // keep the ones we need
        "cmp ecx, {leaf_1}",  // which must all be set
        "jne 2f",             // otherwise unsupported

        // Leaf 7 for AVX2 check
        "xor eax, eax",       // leaf 0, highest supported leaf
        "cpuid",              // EAX holds the highest leaf
        "cmp eax, 7",         // leaf 7 must exist
        "jb 2f",              // otherwise there's no AVX2
        "mov eax, 7",         // leaf 7, extended features
        "xor ecx, ecx",       // subleaf 0
        "cpuid",              // EBX holds the features
        "test ebx, {avx2}",   // AVX2 must be set
        "jz 2f",              // otherwise unsupported

        // Passed above instructions, supported
        "mov eax, 1",         // produce true
        "pop rbx",            // restore it
        "ret",                // return true

        // Unsupported features handler
        "2:",
        "xor eax, eax",       // produce false
        "pop rbx",            // restore it
        "ret",                // return false

        leaf_1 = const REQUIRED_LEAF_1,
        avx2 = const 1 << CPUID_AVX2_BIT,
    )
}

/// Displays a message informing the user their CPU is unsupported, waits [`UNSUPPORTED_STALL`],
/// then exits with `EFI_UNSUPPORTED`.
///
/// This is naked for the same reason as [`is_supported`].
#[unsafe(naked)]
#[rustfmt::skip]
pub unsafe extern "efiapi" fn unsupported(system_table: *const SystemTable) -> Status {
    naked_asm!(
        "push rbx",                           // save the system table
        "push rsi",                           // save the console output protocol
        "sub rsp, 40",                        // calling convention: shadow space, align the stack
        "mov rbx, rcx",                       // system table
        "mov rsi, [rbx + {stdout}]",          // console output protocol

        // Clear the screen and disable the cursor
        "mov rcx, rsi",                       // this = SimpleTextOutputProtocol
        "call [rsi + {clear_screen}]",        // clear_screen(this)
        "mov rcx, rsi",                       // this = SimpleTextOutputProtocol
        "xor edx, edx",                       // visible = false
        "call [rsi + {enable_cursor}]",       // enable_cursor(this, visible)

        // Display the message (banner and explanation)
        "mov rcx, rsi",                       // this = SimpleTextOutputProtocol
        "mov edx, {banner}",                  // attribute = white on red
        "call [rsi + {set_attribute}]",       // set_attribute(this, attribute)
        "mov rcx, rsi",                       // this = SimpleTextOutputProtocol
        "lea rdx, [rip + {title}]",           // string = title
        "call [rsi + {output_string}]",       // output_string(this, string)
        "mov rcx, rsi",                       // this = SimpleTextOutputProtocol
        "mov edx, {text}",                    // attribute = default colours
        "call [rsi + {set_attribute}]",       // set_attribute(this, attribute)
        "mov rcx, rsi",                       // this = SimpleTextOutputProtocol
        "lea rdx, [rip + {body}]",            // string = body
        "call [rsi + {output_string}]",       // output_string(this, string)

        // TODO(#4): wait for a key press to shut down rather than fixed stall
        "mov rax, [rbx + {boot_services}]",   // boot services
        "mov rcx, {stall_us}",                // microseconds
        "call [rax + {stall}]",               // stall(microseconds)

        "mov rax, {unsupported}",             // return EFI_UNSUPPORTED
        "add rsp, 40",                        // free shadow space
        "pop rsi",                            // restore console output protocol
        "pop rbx",                            // restore system table
        "ret",                                // hand back to efi_main's caller (firmware)

        stdout = const offset_of!(SystemTable, stdout),
        clear_screen = const offset_of!(SimpleTextOutputProtocol, clear_screen),
        enable_cursor = const offset_of!(SimpleTextOutputProtocol, enable_cursor),
        set_attribute = const offset_of!(SimpleTextOutputProtocol, set_attribute),
        output_string = const offset_of!(SimpleTextOutputProtocol, output_string),
        boot_services = const offset_of!(SystemTable, boot_services),
        stall = const offset_of!(BootServices, stall),
        banner = const BANNER_ATTRIBUTE,
        text = const TEXT_ATTRIBUTE,
        title = sym UNSUPPORTED_TITLE,
        body = sym UNSUPPORTED_BODY,
        stall_us = const UNSUPPORTED_STALL,
        unsupported = const Status::UNSUPPORTED.0,
    )
}

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
