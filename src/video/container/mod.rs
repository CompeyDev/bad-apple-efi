use core::time::Duration;

mod webm;

pub use webm::{Packets, WebmContainer};

/// The video track, which uses the VP9 codec.
#[derive(Debug, Clone, Copy)]
pub struct VideoTrack {
    pub width: u32,
    pub height: u32,
    pub frame_duration: Option<Duration>,
}

/// The audio track, which uses the Opus codec.
#[derive(Debug, Clone, Copy)]
pub struct AudioTrack {
    pub sample_rate: u32,
    pub channels: u8,
}

/// Which track a packet belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Track {
    Video,
    Audio,
}

/// One compressed frame of a track.
pub struct Packet {
    pub track: Track,
    pub pts: Duration,
    pub data: &'static [u8],
}
