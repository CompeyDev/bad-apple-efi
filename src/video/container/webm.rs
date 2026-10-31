//! [WebM] demuxing, which is a subset of [Matroska].
//!
//! The scope of this module is only limited to what is produced by ffmpeg, and what is the
//! bare minimum required for playback, and is not intended to be general purpose.
//!
//! [WebM]: https://www.webmproject.org/docs/container/
//! [Matroska]: https://www.rfc-editor.org/rfc/rfc9559/

use core::time::Duration;

use zerocopy::big_endian::I16;
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

use self::ebml::{children, element, element_with_id, float, int, uint, vint, vint_data};
use crate::video::container::{AudioTrack, Packet, Track, VideoTrack};
use crate::video::VideoError;

/// Element IDs as raw bytes, including their `VINT_MARKER`, as they would appear in the file.
mod id {
    pub const EBML: u64 = 0x1A45_DFA3;
    pub const DOC_TYPE: u64 = 0x4282;
    pub const SEGMENT: u64 = 0x1853_8067;
    pub const INFO: u64 = 0x1549_A966;
    pub const TIMESTAMP_SCALE: u64 = 0x2A_D7B1;
    pub const DURATION: u64 = 0x4489;
    pub const TRACKS: u64 = 0x1654_AE6B;
    pub const TRACK_ENTRY: u64 = 0xAE;
    pub const TRACK_NUMBER: u64 = 0xD7;
    pub const CODEC_ID: u64 = 0x86;
    pub const CODEC_DELAY: u64 = 0x56AA;
    pub const DEFAULT_DURATION: u64 = 0x23_E383;
    pub const VIDEO: u64 = 0xE0;
    pub const PIXEL_WIDTH: u64 = 0xB0;
    pub const PIXEL_HEIGHT: u64 = 0xBA;
    pub const AUDIO: u64 = 0xE1;
    pub const SAMPLING_FREQUENCY: u64 = 0xB5;
    pub const CHANNELS: u64 = 0x9F;
    pub const CLUSTER: u64 = 0x1F43_B675;
    pub const TIMESTAMP: u64 = 0xE7;
    pub const SIMPLE_BLOCK: u64 = 0xA3;
    pub const BLOCK_GROUP: u64 = 0xA0;
    pub const BLOCK: u64 = 0xA1;
    pub const DISCARD_PADDING: u64 = 0x75A2;
}

/// Default for [`id::TIMESTAMP_SCALE`] - milliseconds.
const DEFAULT_TIMESTAMP_SCALE: u64 = 1_000_000;

/// Lacing bits of a `SimpleBlock`.
const LACING: u8 = 0b0000_0110;

const NANOS_PER_SEC: u64 = 1_000_000_000;

// NOTE: `u128` division is broken. This is a workaround for that, since [`Duration::as_nanos`]
// returns it.
fn nanos(duration: Duration) -> u64 {
    duration.as_secs() * NANOS_PER_SEC + u64::from(duration.subsec_nanos())
}

/// [EBML](https://www.rfc-editor.org/rfc/rfc8794) primitives.
mod ebml {
    use core::iter;

    /// Reads a Variable-Size Integer (VINT), returning its raw value including the `VINT_MARKER`,
    /// along with its length.
    #[inline(always)]
    pub fn vint(data: &[u8]) -> Option<(u64, usize)> {
        let len = data.first()?.leading_zeros() as usize + 1;
        let bytes = data.get(..len).filter(|_| len <= 8)?;
        Some((uint(bytes), len))
    }

    /// Reads the `VINT_DATA` bits, removing the `VINT_MARKER` from a [`vint`].
    #[inline(always)]
    pub fn vint_data((raw, len): (u64, usize)) -> u64 {
        raw & ((1 << (7 * len)) - 1)
    }

    /// Reads a big-endian unsigned integer of up to 8 bytes.
    #[inline(always)]
    pub fn uint(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0, |value, &byte| value << 8 | u64::from(byte))
    }

    /// Reads a big-endian signed integer of up to 8 bytes.
    #[inline(always)]
    pub fn int(bytes: &[u8]) -> i64 {
        // HACK: shifting the sign bit to the top and back extends it
        let shift = 64 - 8 * bytes.len().clamp(1, 8) as u32;
        (uint(bytes) << shift) as i64 >> shift
    }

    /// Reads a big-endian float, 4 or 8 bytes long.
    pub fn float(bytes: &[u8]) -> Option<f64> {
        match *bytes {
            [] => Some(0.0),
            [a, b, c, d] => Some(f32::from_be_bytes([a, b, c, d]).into()),
            _ => Some(f64::from_be_bytes(bytes.try_into().ok()?)),
        }
    }

    pub struct Element {
        pub id: u64,
        pub header_len: usize,
        pub body: &'static [u8],
    }

    /// Reads the element at the start of `data`, returning it along with the bytes after it.
    ///
    /// An element of unknown size is assumed to be all of `data`.
    #[inline(always)]
    pub fn element(data: &'static [u8]) -> Option<(Element, &'static [u8])> {
        let (id, id_len) = vint(data)?;
        let size = vint(&data[id_len..])?;
        let header_len = id_len + size.1;
        let rest = &data[header_len..];

        let (body, rest) = match vint_data(size) {
            // all bits set = unknown size
            value if value == vint_data((u64::MAX, size.1)) => (rest, &[][..]),
            value => rest.split_at_checked(usize::try_from(value).ok()?)?,
        };

        Some((Element { id, header_len, body }, rest))
    }

    /// Reads the element at the start of `data` like [`element`], if it has the given ID.
    #[inline(always)]
    pub fn element_with_id(data: &'static [u8], id: u64) -> Option<(Element, &'static [u8])> {
        element(data).filter(|(element, _)| element.id == id)
    }

    /// Provides an iterator of child elements under an element.
    pub fn children(mut data: &'static [u8]) -> impl Iterator<Item = Element> {
        iter::from_fn(move || {
            let (element, rest) = element(data)?;
            data = rest;
            Some(element)
        })
    }
}

/// The header of a track out of all the tracks.
///
/// By itself, it is not possible to know whether the track is an audio or video one, so this
/// struct holds all the possible fields. Only the codec can know which kind of track it is.
#[derive(Default)]
struct TrackEntry {
    number: u64,
    codec: &'static [u8],
    default_duration: Option<u64>,
    codec_delay: u64,
    width: u32,
    height: u32,
    sample_rate: Option<f64>,
    channels: Option<u64>,
}

impl TrackEntry {
    fn parse(body: &'static [u8]) -> Self {
        let mut entry = Self::default();
        for element in children(body) {
            match element.id {
                id::TRACK_NUMBER => entry.number = uint(element.body),
                id::CODEC_ID => entry.codec = element.body,
                id::DEFAULT_DURATION => entry.default_duration = Some(uint(element.body)),
                id::CODEC_DELAY => entry.codec_delay = uint(element.body),
                id::VIDEO | id::AUDIO => {
                    for setting in children(element.body) {
                        let value = uint(setting.body);
                        match setting.id {
                            id::PIXEL_WIDTH => entry.width = value as u32,
                            id::PIXEL_HEIGHT => entry.height = value as u32,
                            id::SAMPLING_FREQUENCY => entry.sample_rate = float(setting.body),
                            id::CHANNELS => entry.channels = Some(value),
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }

        entry
    }
}

/// A parsed WebM file.
pub struct WebmContainer {
    video: VideoTrack,
    audio: Option<AudioTrack>,
    duration: Option<Duration>,
    packets: Packets,
    audio_number: Option<u64>,
}

impl WebmContainer {
    /// Creates an instance of [`WebmContainer`], by parsing the given `data`.
    pub fn parse(data: &'static [u8]) -> Result<Self, VideoError> {
        use VideoError::InvalidFormat;

        // Header checks
        let (header, rest) = element_with_id(data, id::EBML).ok_or(InvalidFormat("not EBML"))?;
        if !children(header.body).any(|e| e.id == id::DOC_TYPE && e.body == b"webm") {
            return Err(InvalidFormat("not WebM"));
        }

        let (segment, _) = element_with_id(rest, id::SEGMENT).ok_or(InvalidFormat("no segment"))?;

        // Everything we need comes before the first cluster
        let (mut info, mut tracks) = (None, None);
        for element in children(segment.body).take_while(|element| element.id != id::CLUSTER) {
            match element.id {
                id::INFO => info = Some(element.body),
                id::TRACKS => tracks = Some(element.body),
                _ => {}
            }
        }

        let (mut scale, mut duration) = (DEFAULT_TIMESTAMP_SCALE, None);
        for element in children(info.ok_or(InvalidFormat("no segment info"))?) {
            match element.id {
                id::TIMESTAMP_SCALE => scale = uint(element.body),
                id::DURATION => duration = float(element.body),
                _ => {}
            }
        }

        // Only the first VP9 and Opus tracks are kept, others skipped
        let (mut video, mut audio) = (None, None);
        for element in children(tracks.ok_or(InvalidFormat("no tracks"))?) {
            let entry = match element.id {
                id::TRACK_ENTRY => TrackEntry::parse(element.body),
                _ => continue,
            };

            match entry.codec {
                b"V_VP9" if video.is_none() => video = Some(entry),
                b"A_OPUS" if audio.is_none() => audio = Some(entry),
                _ => {}
            }
        }

        let video = video.ok_or(InvalidFormat("no VP9 track"))?;
        let container = Self {
            duration: duration.map(|ticks| Duration::from_nanos((ticks * scale as f64) as u64)),

            video: VideoTrack {
                width: video.width,
                height: video.height,
                frame_duration: video.default_duration.map(Duration::from_nanos),
            },

            audio: audio.as_ref().map(|audio| AudioTrack {
                // Matroska's defaults: 8KHz sample rate and mono channel
                sample_rate: audio.sample_rate.unwrap_or(8000.0) as u32,
                channels: audio.channels.unwrap_or(1) as u8,
                codec_delay: Duration::from_nanos(audio.codec_delay),
            }),

            packets: Packets { data: segment.body, cluster_time: 0, scale, track: video.number },
            audio_number: audio.map(|audio| audio.number),
        };

        let VideoTrack { width, height, .. } = container.video;
        let size = data.len() / 1024;
        log::debug!(
            "WebM: {width}x{height} VP9, {:.2?}, {size} KiB",
            container.duration.unwrap_or_default()
        );

        if let Some(AudioTrack { sample_rate, channels, codec_delay }) = container.audio {
            log::debug!(
                "Opus audio, {sample_rate} Hz, {channels} channels, {codec_delay:?} pre-skip"
            );
        } else {
            log::debug!("no audio track");
        }

        Ok(container)
    }

    pub fn video(&self) -> VideoTrack {
        self.video
    }

    /// Calculates the video track's frames per second.
    pub fn framerate(&self) -> u32 {
        let fps = (self.video.frame_duration.map(nanos))
            .filter(|&nanos| nanos != 0)
            .map(|nanos| (NANOS_PER_SEC + nanos / 2) / nanos);

        fps.map_or_else(
            || {
                log::warn!("Couldn't calculate framerate from the frame duration, assuming 60 fps");
                60
            },
            |fps| fps.clamp(1, 120) as u32,
        )
    }

    /// Total runtime of the file, which covers every track.
    pub fn duration(&self) -> Option<Duration> {
        self.duration
    }

    pub fn audio(&self) -> Option<AudioTrack> {
        self.audio
    }

    /// Provides the packets of one track, if the file has it.
    ///
    /// Every track walks the file on its own, so they can be read at different paces.
    pub fn packets(&self, track: Track) -> Option<Packets> {
        let track = match track {
            Track::Video => self.packets.track,
            Track::Audio => self.audio_number?,
        };

        Some(Packets { track, ..self.packets.clone() })
    }
}

/// An iterator of a track's packets, demuxed from the [`WebmContainer`].
#[derive(Clone)]
pub struct Packets {
    data: &'static [u8],
    cluster_time: u64,
    scale: u64,
    track: u64,
}

/// The fixed-size part of a block's header, which follows its track number.
#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct BlockHeader {
    /// Timestamp relative to the cluster's, in [`id::TIMESTAMP_SCALE`] ticks.
    timestamp: I16,
    flags: u8,
}

impl Packets {
    /// Parses a `SimpleBlock` or `Block` which contains the track number, a timestamp relavtive
    /// to the cluster and the packet's data.
    ///
    /// Returns `None` when a block cannot produce a packet (another track, unsupported or invalid).
    #[inline(always)]
    fn block(&self, body: &'static [u8]) -> Option<Packet> {
        let number = vint(body)?;
        if vint_data(number) != self.track {
            return None;
        }

        let (header, data) = BlockHeader::ref_from_prefix(&body[number.1..]).ok()?;
        if header.flags & LACING != 0 {
            // TODO: add support for laced blocks. so far, ffmpeg hasnt produced any
            log::warn!("Skipping a laced block, which is unsupported");
            return None;
        }

        // NOTE:
        // Offsets are signed, so a block may sit slightly before its cluster. Only a block before the
        // start of the file would go negative, which ffmpeg never writes since it stores the Opus
        // pre-skip in `CodecDelay` instead, so this just saturates at zero
        let ticks = self.cluster_time.saturating_add_signed(header.timestamp.get().into());
        let pts = Duration::from_nanos(ticks.saturating_mul(self.scale));
        Some(Packet { pts, data, discard: Duration::ZERO })
    }
}

impl Iterator for Packets {
    type Item = Packet;

    fn next(&mut self) -> Option<Packet> {
        loop {
            let data = self.data;
            let (element, rest) = element(data)?;
            self.data = rest;

            match element.id {
                id::CLUSTER => self.data = &data[element.header_len..],
                id::TIMESTAMP => self.cluster_time = uint(element.body),
                id::SIMPLE_BLOCK => {
                    if let Some(packet) = self.block(element.body) {
                        return Some(packet);
                    }
                }
                id::BLOCK_GROUP => {
                    let (mut block, mut discard) = (None, 0);
                    for child in children(element.body) {
                        match child.id {
                            id::BLOCK => block = Some(child.body),
                            id::DISCARD_PADDING => discard = int(child.body),
                            _ => {}
                        }
                    }

                    if let Some(packet) = block.and_then(|block| self.block(block)) {
                        // Negative padding would trim the start instead, which ffmpeg never writes
                        let discard = Duration::from_nanos(discard.max(0) as u64);
                        return Some(Packet { discard, ..packet });
                    }
                }
                _ => {}
            }
        }
    }
}
