use core::ffi::{c_char, c_int, c_void};
use core::fmt;

use ffi_opaque::opaque;

use crate::hda::HdaError;

/// `UhdaStatus`, returned by most uHDA APIs.
#[repr(transparent)]
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UhdaStatus(c_int);

impl UhdaStatus {
    pub const SUCCESS: Self = Self(0);
    pub const UNSUPPORTED: Self = Self(1);
    pub const NO_MEMORY: Self = Self(2);

    pub fn ok(self) -> Result<(), HdaError> {
        match self {
            Self::SUCCESS => Ok(()),
            Self(code) => Err(HdaError::Uhda(code)),
        }
    }
}

/// `UhdaIrqHint`
pub type UhdaIrqHint = c_int;

/// `UhdaIrqState`, the saved `RFLAGS` of whoever holds a spinlock.
pub type UhdaIrqState = usize;

/// `UhdaIrqHandlerFn`
pub type UhdaIrqHandlerFn = unsafe extern "C" fn(arg: *mut c_void) -> bool;

/// `UhdaScatterChunk`
#[repr(C)]
pub struct UhdaScatterChunk {
    pub phys: usize,
    pub virt: *mut c_void,
}

/// `UhdaOutputType`
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct UhdaOutputType(c_int);

impl UhdaOutputType {
    const NAMES: [&str; 7] =
        ["line out", "speaker", "headphone", "CD", "S/PDIF out", "digital out", "unknown"];
}

impl fmt::Display for UhdaOutputType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(Self::NAMES.get(self.0 as usize).copied().unwrap_or("unknown"))
    }
}

/// `UhdaColor`, the colour of an output's jack.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct UhdaColor(pub c_int);

impl UhdaColor {
    pub const UNKNOWN: Self = Self(0);
    pub const BLACK: Self = Self(1);
    pub const GREY: Self = Self(2);
    pub const BLUE: Self = Self(3);
    pub const GREEN: Self = Self(4);
    pub const RED: Self = Self(5);
    pub const ORANGE: Self = Self(6);
    pub const YELLOW: Self = Self(7);
    pub const PURPLE: Self = Self(8);
    pub const PINK: Self = Self(9);
    pub const WHITE: Self = Self(14);
    pub const OTHER: Self = Self(15);
}

/// `UhdaOutputInfo`
#[repr(C)]
pub struct UhdaOutputInfo {
    pub kind: UhdaOutputType,
    pub color: UhdaColor,
    pub location: c_int,
}

opaque! {
    /// `UhdaController`, only ever handed by pointer.
    pub struct UhdaController;
    /// `UhdaCodec`, only ever handed by pointer.
    pub struct UhdaCodec;
    /// `UhdaOutputGroup`, only ever handed by pointer.
    pub struct UhdaOutputGroup;
    /// `UhdaOutput`, only ever handed by pointer.
    pub struct UhdaOutput;
    /// `UhdaPath`, only ever handed by pointer.
    pub struct UhdaPath;
    /// `UhdaStream`, only ever handed by pointer.
    pub struct UhdaStream;
    /// `UhdaSimpleStream`, only ever handed by pointer.
    pub struct UhdaSimpleStream;
}

/// `UhdaFormat`
#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct UhdaFormat(c_int);

impl UhdaFormat {
    pub const PCM16: Self = Self(1);
}

/// `UhdaSimpleStreamParams`
#[repr(C)]
pub struct UhdaSimpleStreamParams {
    pub sample_rate: u32,
    pub channels: u32,
    pub fmt: UhdaFormat,
    pub ring_buffer_size: u32,
}

extern "C" {
    pub static UHDA_OUTPUT_COLOR_STRINGS: [*const c_char; 16];
    pub static UHDA_LOCATION_STRINGS: [*const c_char; 16];

    pub fn uhda_device_matches(vendor: u16, device: u16) -> bool;
    pub fn uhda_class_matches(class: u16, subclass: u16) -> bool;

    pub fn uhda_init(handle: usize, res: *mut *mut UhdaController) -> UhdaStatus;
    pub fn uhda_destroy(controller: *mut UhdaController) -> UhdaStatus;

    pub fn uhda_get_codecs(
        controller: *mut UhdaController,
        codecs: *mut *const *const UhdaCodec,
        codec_count: *mut usize,
    );

    pub fn uhda_codec_get_output_groups(
        codec: *const UhdaCodec,
        output_groups: *mut *const *const UhdaOutputGroup,
        output_group_count: *mut usize,
    );

    pub fn uhda_output_group_get_outputs(
        output_group: *const UhdaOutputGroup,
        outputs: *mut *const *const UhdaOutput,
        output_count: *mut usize,
    );

    pub fn uhda_output_get_presence(output: *const UhdaOutput, presence: *mut bool) -> UhdaStatus;
    pub fn uhda_output_get_info(output: *const UhdaOutput) -> UhdaOutputInfo;

    pub fn uhda_get_output_streams(
        controller: *mut UhdaController,
        streams: *mut *mut *mut UhdaStream,
        stream_count: *mut usize,
    );

    pub fn uhda_find_path(
        dest: *const UhdaOutput,
        other_paths: *const *const UhdaPath,
        other_path_count: usize,
        same_stream: bool,
        res: *mut *mut UhdaPath,
    ) -> UhdaStatus;

    pub fn uhda_path_shutdown(path: *mut UhdaPath) -> UhdaStatus;
    pub fn uhda_path_set_volume(path: *mut UhdaPath, volume: c_int) -> UhdaStatus;
    pub fn uhda_path_mute(path: *mut UhdaPath, mute: bool) -> UhdaStatus;

    pub fn uhda_stream_shutdown(stream: *mut UhdaStream) -> UhdaStatus;

    pub fn uhda_simple_stream_new(
        base: *mut UhdaStream,
        res: *mut *mut UhdaSimpleStream,
    ) -> UhdaStatus;
    pub fn uhda_simple_stream_destroy(stream: *mut UhdaSimpleStream);
    pub fn uhda_simple_path_setup(path: *mut UhdaPath, stream: *mut UhdaSimpleStream)
        -> UhdaStatus;
    pub fn uhda_simple_stream_setup(
        stream: *mut UhdaSimpleStream,
        params: *const UhdaSimpleStreamParams,
    ) -> UhdaStatus;
    pub fn uhda_simple_stream_play(stream: *mut UhdaSimpleStream, play: bool) -> UhdaStatus;
    pub fn uhda_simple_stream_queue_data(
        stream: *mut UhdaSimpleStream,
        data: *const c_void,
        size: *mut u32,
    ) -> UhdaStatus;
    pub fn uhda_simple_stream_get_remaining(
        stream: *const UhdaSimpleStream,
        remaining: *mut u32,
    ) -> UhdaStatus;
}
