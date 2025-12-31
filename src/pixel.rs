#![allow(unused)]
#![allow(clippy::from_over_into)]

use alloc::vec;
use alloc::vec::Vec;
use core::arch::x86_64::*;

use resize::px::PixelFormat;
use resize::{formats, Resizer, Type};

/// Represents a color for drawing on the display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Color {
    #[default]
    Gray,
    Red,
    Green,
    Blue,
    Yellow,
    Cyan,
    Magenta,
    Rgb(u8, u8, u8),
}

impl Color {
    pub const BLACK: Color = Self::Rgb(0, 0, 0);
    pub const WHITE: Color = Self::Rgb(255, 255, 255);

    /// Makes the color grayscaled.
    pub fn to_grayscale(self) -> Color {
        let (r, g, b): (u8, u8, u8) = self.into();
        let gray = ((77u16 * r as u16 + 150u16 * g as u16 + 29u16 * b as u16) >> 8) as u8;
        Color::Rgb(gray, gray, gray)
    }

    /// Converts the color a black and white equivalent, around an optional threshold.
    pub fn to_bw(self, threshold: Option<u8>) -> Color {
        const DEFAULT_THRESHOLD: u8 = 128;

        self.to_two_tone(Color::WHITE, Color::BLACK, threshold.unwrap_or(DEFAULT_THRESHOLD))
    }

    /// Apply a two tone posterization, i.e., map values threshold into either an upper or
    /// lower value.
    pub fn to_two_tone(self, upper: Color, lower: Color, threshold: u8) -> Color {
        let (r, g, b): (u8, u8, u8) = self.into();
        let gray = ((77u16 * r as u16 + 150u16 * g as u16 + 29u16 * b as u16) >> 8) as u8;

        if gray >= threshold {
            upper
        } else {
            lower
        }
    }

    /// Invert the color into its complement.
    pub fn invert(self) -> Color {
        let (r, g, b): (u8, u8, u8) = self.into();
        Color::Rgb(255 - r, 255 - g, 255 - b)
    }
}

impl Into<u32> for Color {
    fn into(self) -> u32 {
        match self {
            Color::Gray => 0x222222,
            Color::Red => 0xFF0000,
            Color::Green => 0x00FF00,
            Color::Blue => 0x0000FF,
            Color::Yellow => 0xFFFF00,
            Color::Cyan => 0x00FFFF,
            Color::Magenta => 0xFF00FF,
            Color::Rgb(r, g, b) => ((r as u32) << 16) | ((g as u32) << 8) | (b as u32),
        }
    }
}

impl Into<(u8, u8, u8)> for Color {
    fn into(self) -> (u8, u8, u8) {
        match self {
            Color::Rgb(r, g, b) => (r, g, b),
            other => {
                let hex: u32 = other.into();
                let r = ((hex >> 16) & 0xFF) as u8;
                let g = ((hex >> 8) & 0xFF) as u8;
                let b = (hex & 0xFF) as u8;

                (r, g, b)
            }
        }
    }
}

// TODO: Improve `resize` create perf for `no_std` floating point arithmetic
// and swap to it, instead of custom nearest-neighbour implementation. The below
// code is unused.

#[rustfmt::skip]
#[allow(type_alias_bounds)]
type PixelFormatResizer<T: PixelFormat> = fn(usize, usize, usize, usize, T, Type) -> resize::Result<Resizer<T>>;

pub type Rgb = formats::Rgb<u8, u8>;
pub type RgbA = formats::Rgba<u8, u8>;
pub type Luma = formats::Gray<u8, u8>;

pub const RGB_RESIZER: PixelFormatResizer<Rgb> = resize::new::<Rgb>;
pub const RGBA_RESIZER: PixelFormatResizer<RgbA> = resize::new::<RgbA>;
pub const LUMA_RESIZER: PixelFormatResizer<Luma> = resize::new::<Luma>;

pub const RGB_CHANNELS_SIZE: usize = 3;
pub const RGBA_CHANNELS_SIZE: usize = 4;
pub const LUMA_CHANNELS_SIZE: usize = 1;

/// Syntatic sugar around converting between a `zune_core::colorspace::ColorSpace` and a constructed
/// `Resizer`.
///
/// ```no_run
/// let (scaled, bytes_per_pixel) = resize!(
///     colorspace,
///     pixels.as_slice(),
///     (src_width, src_height) => (dest_width, dest_height),
///     [
///         (ColorSpace::RGB,  as_rgb,  as_rgb_mut)  => (RGB_RESIZER,  RGB_CHANNELS_SIZE)  => Pixel::RGB8,
///         (ColorSpace::RGBA, as_rgba, as_rgba_mut) => (RGBA_RESIZER, RGBA_CHANNELS_SIZE) => Pixel::RGBA8,
///         (ColorSpace::Luma, as_gray, as_gray_mut) => (LUMA_RESIZER, LUMA_CHANNELS_SIZE) => Pixel::Gray8,
///     ]
/// )?;
/// ```
#[macro_export]
macro_rules! resize {
    (
        $var:expr,
        $src:expr,
        ($orig_width:expr, $orig_height:expr) => ($new_width:expr, $new_height:expr),
        [$(($colorspace:path,$method:ident,$method_mut:ident) => ($resizer:ident, $channel_size:ident) => $pixel:expr),+ $(,)?]
    ) => {{
        let result = match $var {
            $(
                $colorspace => {
                    let imp = || -> resize::Result<(alloc::vec::Vec<u8>, usize)> {
                        let mut dest = alloc::vec![0; $new_width * $new_height * $channel_size];
                        $resizer(
                            $orig_width,
                            $orig_height,
                            $new_width,
                            $new_height,
                            $pixel,
                            resize::Type::Point
                        )?
                        .resize(
                            rgb::FromSlice::<u8>::$method($src),
                            rgb::FromSlice::<u8>::$method_mut(dest.as_mut_slice())
                        )?;

                        Ok((dest, $channel_size))
                    };

                    imp()
                }
                ,
            )+
            _ => continue,
        };

        result
    }};
}

/// A fast, SSE-based nearest-neighbour scaling implementation for per-frame scaling.
/// Compromises slightly on quality over performance. Used as a fallback when AVX SIMD
/// is unavailable.
#[cfg(target_arch = "x86_64")]
#[inline(always)]
pub fn scale_nn_fast(
    src: &[u8],
    src_w: usize,
    src_h: usize,
    dst_w: usize,
    dst_h: usize,
    channels: usize,
    dst: &mut Vec<u8>,
) {
    // FIXME: Not the ideal SIMD implementation. Makes the most sense for RBGA data with AVX2,
    // to collect multiples of 4 bytes of data. Worth rewriting once I have more experience
    // with vectorized algorithms. At least, we do get a 5 FPS boost.

    let (x_map, y_map) = (
        (0..dst_w).map(|dx| ((dx * src_w) / dst_w) * channels).collect::<Vec<usize>>(),
        (0..dst_h).map(|dy| ((dy * src_h) / dst_h) * src_w * channels).collect::<Vec<usize>>(),
    );
    unsafe {
        let src_ptr = src.as_ptr();
        let dst_ptr = dst.as_mut_ptr();
        let mut dst_idx = 0;
        for &sy in &y_map {
            let mut x_iter = x_map.iter();
            // PERF: Process 16 pixels at a time with SSE (48 bytes)
            while let Some(chunks) = x_iter.as_slice().chunks_exact(16).next() {
                for chunk in chunks.chunks(16) {
                    for &sx in chunk {
                        let src_idx = sy + sx;
                        if dst_idx + 48 <= dst.len() && src_idx + 48 <= src.len() {
                            let data = _mm_loadu_si128(src_ptr.add(src_idx) as *const __m128i);
                            _mm_storeu_si128(dst_ptr.add(dst_idx) as *mut __m128i, data);
                            dst_idx += channels;
                        }
                    }
                }
                x_iter = x_iter.as_slice()[16..].iter();
            }
            for &sx in x_iter {
                let src_idx = sy + sx;
                *dst_ptr.add(dst_idx) = *src_ptr.add(src_idx);
                *dst_ptr.add(dst_idx + 1) = *src_ptr.add(src_idx + 1);
                *dst_ptr.add(dst_idx + 2) = *src_ptr.add(src_idx + 2);
                dst_idx += 3;
            }
        }
    }
}
