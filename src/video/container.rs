//! [IVF](https://wiki.multimedia.cx/index.php/IVF) container parsing.

use zerocopy::little_endian::{U16, U32, U64};
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

use crate::video::VideoError;

#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct IvfHeader {
    signature: [u8; 4],
    version: U16,
    header_len: U16,
    fourcc: [u8; 4],
    width: U16,
    height: U16,
    timebase_den: U32,
    timebase_num: U32,
    frame_count: U32,
    _unused: U32,
}

#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct PacketHeader {
    size: U32,
    pts: U64,
}

/// A parsed IVF file. Packet data is borrowed from the embedded video.
pub struct IvfContainer {
    header: &'static IvfHeader,
    body: &'static [u8],
}

impl IvfContainer {
    pub fn parse(data: &'static [u8]) -> Result<Self, VideoError> {
        let (header, body) = IvfHeader::ref_from_prefix(data)
            .map_err(|_| VideoError::InvalidFormat("truncated header"))?;

        match header {
            h if h.signature != *b"DKIF" => Err(VideoError::InvalidFormat("bad signature")),
            h if h.version.get() != 0 => Err(VideoError::InvalidFormat("unsupported version")),
            h if h.header_len.get() != 32 => Err(VideoError::InvalidFormat("bad header length")),
            h if h.fourcc != *b"VP90" => Err(VideoError::InvalidFormat("not VP9")),
            h => {
                log::debug!(
                    "IVF: {}x{}, timebase {}/{}s, {} frames, {} KiB of packets",
                    h.width.get(),
                    h.height.get(),
                    h.timebase_num.get(),
                    h.timebase_den.get(),
                    h.frame_count.get(),
                    body.len() / 1024,
                );

                Ok(Self { header, body })
            }
        }
    }

    pub fn width(&self) -> u16 {
        self.header.width.get()
    }

    pub fn height(&self) -> u16 {
        self.header.height.get()
    }

    pub fn frame_count(&self) -> u32 {
        self.header.frame_count.get()
    }

    /// Calculated frames per second based on the packet's last timestamp. IVF only stores a base.
    pub fn framerate(&self) -> u32 {
        let last_pts = self.packets().last().map_or(0, |p| p.pts);
        let h = self.header;

        (u64::from(h.frame_count.get()) * u64::from(h.timebase_den.get()))
            .checked_div(u64::from(h.timebase_num.get()) * last_pts)
            .map_or_else(
                || {
                    log::warn!("Couldn't calculate framerate from timestamps, assuming 60 fps");
                    60
                },
                |fps| fps.clamp(1, 120) as u32,
            )
    }

    pub fn packets(&self) -> Packets {
        Packets(self.body)
    }
}

/// One compressed frame.
pub struct Packet {
    pub pts: u64,
    pub data: &'static [u8],
}

/// An iterator of packets, decoded from the [`IvfContainer`].
pub struct Packets(&'static [u8]);

impl Iterator for Packets {
    type Item = Packet;

    fn next(&mut self) -> Option<Packet> {
        let (header, rest) = PacketHeader::ref_from_prefix(self.0).ok()?;
        let (data, rest) = rest.split_at_checked(header.size.get() as usize)?;
        self.0 = rest;
        Some(Packet { pts: header.pts.get(), data })
    }
}
