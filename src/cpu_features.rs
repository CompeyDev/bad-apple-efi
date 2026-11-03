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

use self::util::{required_features, ucs2_static};

pub const CPUID_FMA_BIT: u32 = 12;
pub const CPUID_SSE4_1_BIT: u32 = 19;
pub const CPUID_XSAVE_BIT: u32 = 26;
pub const CPUID_AVX_BIT: u32 = 28;
pub const CPUID_AVX2_BIT: u32 = 5; // NOTE: leaf 7, EBX instead of ECX

/// Presence of this bit the absence of AVX2.
const AVX2_MISSING: u32 = 1 << 31;

// Every CPU feature the compiled code requires. See `.cargo/config.toml` and `build.rs`.
required_features! {
    "SSE4.1" => 1 << CPUID_SSE4_1_BIT, // SIMD for whole crate, specifically fast_image_resize
    "AVX"    => 1 << CPUID_AVX_BIT,    // ^^ same ^^
    "AVX2"   =>      AVX2_MISSING,     // ^^ same ^^, and libvpx and opus
    "FMA"    => 1 << CPUID_FMA_BIT,    // libvpx and opus
    "XSAVE"  => 1 << CPUID_XSAVE_BIT,  // `init_fpu` and `init_avx`
}

// Error message displayed on the UEFI text console. Constructed at compile time, since doing
// this at runtime could depend on one of the CPU features which may not be supported.
ucs2_static! {
    TITLE = concat!(" ", env!("CARGO_PKG_NAME"), " can't run on this CPU ");
    DETAILS = "\r\n\r\n This CPU does not support the ";
    SEPARATOR = ", ";
    TERMINATOR = ""; // prints nothing, only the null terminator
    FOOTER = concat!(
        " feature(s)\r\n (at least Intel Haswell / AMD Excavator CPUs or newer).\r\n\r\n",
        " This system will hand back to UEFI in 10 seconds...\r\n",
    );
}

// Text attributes and styling
const BANNER_ATTRIBUTE: usize = 0x0F | 0x4 << 4; // white fg, red bg
const TEXT_ATTRIBUTE: usize = 0x07; // light grey fg, black bg (default)

/// How long the system stalls for after displaying the message, in microseconds.
const UNSUPPORTED_STALL: usize = 10_000_000; // 10s

/// Returns which of the features the code is compiled to assume are missing, as a mask of
/// [`FEATURES`], which is zero when the CPU has all of them.
///
/// This is naked, since any compiled code may already use those features.
#[unsafe(naked)]
#[rustfmt::skip]
pub unsafe extern "efiapi" fn missing_features() -> u32 {
    naked_asm!(
        "push rbx",               // save it since CPUID writes it

        // Leaf 1 features
        "mov eax, 1",             // leaf 1, basic features
        "cpuid",                  // ECX holds the features
        "not ecx",                // set bits are now missing features
        "and ecx, {leaf_1}",      // keep the ones we need
        "mov r8d, ecx",           // missing so far, which CPUID doesn't write

        // Leaf 7 for AVX2 check
        "xor eax, eax",           // leaf 0, highest supported leaf
        "cpuid",                  // EAX holds the highest leaf
        "cmp eax, 7",             // leaf 7 must exist
        "jb 2f",                  // otherwise there's no AVX2
        "mov eax, 7",             // leaf 7, extended features
        "xor ecx, ecx",           // subleaf 0
        "cpuid",                  // EBX holds the features
        "test ebx, {avx2}",       // AVX2 must be set
        "jnz 3f",                 // which it is

        "2:",
        "or r8d, {avx2_missing}", // AVX2 is missing

        "3:",
        "mov eax, r8d",           // return the missing features
        "pop rbx",                // restore it
        "ret",

        leaf_1 = const REQUIRED_LEAF_1,
        avx2 = const 1 << CPUID_AVX2_BIT,
        avx2_missing = const AVX2_MISSING,
    )
}

/// Displays a message informing the user which `missing` features their CPU doesn't support,
/// waits [`UNSUPPORTED_STALL`], then exits with `EFI_UNSUPPORTED`.
///
/// This is naked for the same reason as [`missing_features`].
#[unsafe(naked)]
#[rustfmt::skip]
pub unsafe extern "efiapi" fn unsupported(
    system_table: *const SystemTable,
    missing: u32,
) -> Status {
    naked_asm!(
        "push rbx",                           // save the system table
        "push rsi",                           // save the console output protocol
        "push rdi",                           // save the missing features
        "push r12",                           // save the current feature
        "push r13",                           // save the end of the features
        "push r14",                           // save what to print before the next feature
        "sub rsp, 40",                        // calling convention: shadow space, align the stack
        "mov rbx, rcx",                       // system table
        "mov edi, edx",                       // missing features
        "mov rsi, [rbx + {stdout}]",          // console output protocol

        // Clear the screen and disable the cursor
        "mov rcx, rsi",                       // this = SimpleTextOutputProtocol
        "call [rsi + {clear_screen}]",        // clear_screen(this)
        "mov rcx, rsi",                       // this = SimpleTextOutputProtocol
        "xor edx, edx",                       // visible = false
        "call [rsi + {enable_cursor}]",       // enable_cursor(this, visible)

        // Display the banner
        "mov rcx, rsi",                       // this = SimpleTextOutputProtocol
        "mov edx, {banner}",                  // attribute = white on red
        "call [rsi + {set_attribute}]",       // set_attribute(this, attribute)
        "lea rdx, [rip + {title}]",           // string = title
        "call 3f",                            // print(string)
        "mov rcx, rsi",                       // this = SimpleTextOutputProtocol
        "mov edx, {text}",                    // attribute = default colours
        "call [rsi + {set_attribute}]",       // set_attribute(this, attribute)

        // List the missing features, separated by commas
        "lea rdx, [rip + {details}]",         // string = details
        "call 3f",                            // print(string)
        "lea r12, [rip + {features}]",        // current feature = first
        "lea r13, [r12 + {features_size}]",   // end of the features
        "lea r14, [rip + {terminator}]",      // nothing before the first feature
        "4:",
        "test [r12], edi",                    // is this feature missing?
        "jz 5f",                              // otherwise skip it
        "mov rdx, r14",                       // string = nothing or separator
        "call 3f",                            // print(string)
        "lea rdx, [r12 + {name}]",            // string = feature name
        "call 3f",                            // print(string)
        "lea r14, [rip + {separator}]",       // separators before the rest
        "5:",
        "add r12, {feature_size}",            // next feature
        "cmp r12, r13",                       // until the end
        "jb 4b",

        // The rest
        "lea rdx, [rip + {footer}]",          // string = footer
        "call 3f",                            // print(string)

        // TODO(#4): wait for a key press to shut down rather than fixed stall
        "mov rax, [rbx + {boot_services}]",   // boot services
        "mov rcx, {stall_us}",                // microseconds
        "call [rax + {stall}]",               // stall(microseconds)

        "mov rax, {unsupported}",             // return EFI_UNSUPPORTED
        "add rsp, 40",                        // free shadow space
        "pop r14",                            // restore what to print before the next feature
        "pop r13",                            // restore end of the features
        "pop r12",                            // restore current feature
        "pop rdi",                            // restore missing features
        "pop rsi",                            // restore console output protocol
        "pop rbx",                            // restore system table
        "ret",                                // hand back to efi_main's caller (firmware)

        // utility: print(string)
        "3:",
        "mov rcx, rsi",                       // this = SimpleTextOutputProtocol
        "jmp [rsi + {output_string}]",        // output_string(this, string), returns for us

        stdout = const offset_of!(SystemTable, stdout),
        clear_screen = const offset_of!(SimpleTextOutputProtocol, clear_screen),
        enable_cursor = const offset_of!(SimpleTextOutputProtocol, enable_cursor),
        set_attribute = const offset_of!(SimpleTextOutputProtocol, set_attribute),
        output_string = const offset_of!(SimpleTextOutputProtocol, output_string),
        boot_services = const offset_of!(SystemTable, boot_services),
        stall = const offset_of!(BootServices, stall),
        banner = const BANNER_ATTRIBUTE,
        text = const TEXT_ATTRIBUTE,
        title = sym TITLE,
        details = sym DETAILS,
        separator = sym SEPARATOR,
        terminator = sym TERMINATOR,
        footer = sym FOOTER,
        features = sym FEATURES,
        features_size = const size_of_val(&FEATURES),
        feature_size = const size_of::<Feature>(),
        name = const offset_of!(Feature, name),
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

mod util {
    macro_rules! required_features {
        ($($name:literal => $mask:expr,)*) => {
            #[repr(C)]
            struct Feature { mask: u32, name: [u16; 8] }

            /// All required CPU features.
            static FEATURES: [Feature; [$($name),*].len()] = [$(Feature {
                mask: $mask,
                name: $crate::cpu_features::util::ucs2($name),
            }),*];

            /// The required features in `CPUID.1:ECX` (exception being AVX2).
            const REQUIRED_LEAF_1: u32 = ($($mask)|*) & !AVX2_MISSING;
        };
    }

    /// Declares a block of static strings encoded as UCS-2 at compile time.
    macro_rules! ucs2_static {
        ($($name:ident = $text:expr;)*) => {
            $(static $name: [u16; $text.len() + 1] = $crate::cpu_features::util::ucs2($text);)*
        };
    }

    // So that we can place it in the bottom rather than having to define it before
    // the usage at the top of the file
    pub(super) use {required_features, ucs2_static};

    /// Encodes and null terminates ASCII into a UCS-2 string for UEFI.
    pub const fn ucs2<const N: usize>(text: &str) -> [u16; N] {
        assert!(text.is_ascii() && text.len() < N);

        let mut out = [0; N];
        let mut i = 0;
        while i < text.len() {
            out[i] = text.as_bytes()[i] as u16;
            i += 1;
        }

        out
    }
}
