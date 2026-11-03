/// libvpx, for the VP9 video.
pub mod vpx {
    use core::ffi::{c_char, c_int, c_uint, c_void};

    use ffi_opaque::opaque;

    use crate::video::VideoError;

    const VPX_DECODER_ABI_VERSION: c_int = 3 + 4 /* VPX_CODEC_ABI_VERSION */ + 5 /* VPX_CODEC_ABI_VERSION */;

    /// `vpx_codec_err_t`, returned by most libvpx calls.
    #[repr(transparent)]
    #[must_use]
    pub struct VpxStatus(c_int);

    impl VpxStatus {
        pub fn ok(self) -> Result<(), VideoError> {
            match self.0 {
                0 => Ok(()),
                code => Err(VideoError::Vpx(code)),
            }
        }
    }

    /// `vpx_color_space_t`
    #[repr(transparent)]
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub struct VpxColorSpace(c_int);

    #[cfg(feature = "high_res")]
    impl VpxColorSpace {
        pub const BT_709: Self = Self(2);
        pub const SMPTE_240: Self = Self(4);
        pub const BT_2020: Self = Self(5);
        pub const SRGB: Self = Self(7);
    }

    /// `vpx_color_range_t`
    #[repr(transparent)]
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub struct VpxColorRange(c_int);

    #[cfg(feature = "high_res")]
    impl VpxColorRange {
        pub const FULL: Self = Self(1);
    }

    /// `vpx_image_t`
    #[repr(C)]
    pub struct VpxImage {
        pub fmt: c_int,
        pub cs: VpxColorSpace,
        pub range: VpxColorRange,
        pub w: c_uint,
        pub h: c_uint,
        pub bit_depth: c_uint,
        pub d_w: c_uint,
        pub d_h: c_uint,
        pub r_w: c_uint,
        pub r_h: c_uint,
        pub x_chroma_shift: c_uint,
        pub y_chroma_shift: c_uint,
        pub planes: [*mut u8; 4],
        pub stride: [c_int; 4],
        pub bps: c_int,
        pub user_priv: *mut c_void,
        pub img_data: *mut u8,
        pub img_data_owner: c_int,
        pub self_allocd: c_int,
        pub fb_priv: *mut c_void,
    }

    /// `vpx_codec_ctx_t`. Fields are only touched by libvpx.
    #[repr(C)]
    pub struct VpxCodecCtx {
        name: *const c_char,
        iface: *const VpxCodecIface,
        err: c_int,
        err_detail: *const c_char,
        init_flags: c_int, // `long`, 4 bytes under the LLP64 ABI libvpx is built with
        config: *const c_void,
        priv_: *mut c_void,
    }

    opaque! {
        /// `vpx_codec_iface_t`, only ever handed by pointer.
        pub struct VpxCodecIface;
    }

    /// `vpx_codec_dec_cfg_t`
    #[repr(C)]
    pub struct VpxCodecDecCfg {
        pub threads: c_uint,
        pub w: c_uint,
        pub h: c_uint,
    }

    /// Simple reimplementation of the `vpx_codec_dec_init` macro in C.
    pub unsafe fn vpx_codec_dec_init(
        ctx: *mut VpxCodecCtx,
        iface: *const VpxCodecIface,
        cfg: *const VpxCodecDecCfg,
        flags: c_int,
    ) -> VpxStatus {
        unsafe { vpx_codec_dec_init_ver(ctx, iface, cfg, flags, VPX_DECODER_ABI_VERSION) }
    }

    extern "C" {
        pub fn vpx_codec_vp9_dx() -> *const VpxCodecIface;

        fn vpx_codec_dec_init_ver(
            ctx: *mut VpxCodecCtx,
            iface: *const VpxCodecIface,
            cfg: *const VpxCodecDecCfg,
            flags: c_int,
            ver: c_int,
        ) -> VpxStatus;

        pub fn vpx_codec_decode(
            ctx: *mut VpxCodecCtx,
            data: *const u8,
            size: c_uint,
            user_priv: *mut c_void,
            deadline: c_int, // `long`, see `VpxCodecCtx::init_flags`
        ) -> VpxStatus;

        pub fn vpx_codec_get_frame(
            ctx: *mut VpxCodecCtx,
            iter: *mut *const c_void,
        ) -> *const VpxImage;

        pub fn vpx_codec_destroy(ctx: *mut VpxCodecCtx) -> VpxStatus;

        pub fn vpx_codec_error_detail(ctx: *const VpxCodecCtx) -> *const c_char;
    }
}

/// libopus, for the Opus audio.
pub mod opus {
    use core::ffi::c_int;

    use ffi_opaque::opaque;

    opaque! {
        /// `OpusDecoder`, only ever handed by pointer.
        pub struct OpusDecoderState;
    }

    extern "C" {
        pub fn opus_decoder_create(
            sample_rate: i32,
            channels: c_int,
            error: *mut c_int,
        ) -> *mut OpusDecoderState;

        /// Returns the samples decoded per channel, or a negative error code.
        pub fn opus_decode(
            decoder: *mut OpusDecoderState,
            data: *const u8,
            len: i32,
            pcm: *mut i16,
            frame_size: c_int,
            decode_fec: c_int,
        ) -> c_int;

        pub fn opus_decoder_destroy(decoder: *mut OpusDecoderState);
    }
}
