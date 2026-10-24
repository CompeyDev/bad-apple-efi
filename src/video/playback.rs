use core::mem;

use crate::video::codec::Vp9Decoder;
use crate::video::container::{Packets, Track, WebmContainer};
use crate::video::{FrameSink, VideoError};

#[derive(Default)]
enum State {
    Ready {
        container: WebmContainer,
        decoder: Vp9Decoder,
    },

    Playing {
        packets: Packets,
        decoder: Vp9Decoder,
    },

    #[default]
    Done,
}

pub struct VideoPlayer {
    state: State,
    framerate: u32,
}

impl VideoPlayer {
    pub fn new(data: &'static [u8]) -> Result<Self, VideoError> {
        let container = WebmContainer::parse(data)?;
        let framerate = container.framerate();
        let (width, height) = (container.video().width, container.video().height);
        log::info!(
            "VideoPlayer: {width}x{height}, {:.2?}, {framerate} fps",
            container.duration().unwrap_or_default()
        );

        let state = State::Ready { container, decoder: Vp9Decoder::new()? };
        Ok(Self { state, framerate })
    }

    pub fn framerate(&self) -> u32 {
        self.framerate
    }

    /// Advances playback by a step, handing any decoded frame to `sink`. Returns
    /// `Ok(false)` once playback has ended.
    ///
    /// An error ends playback, with an `Err` return.
    pub fn tick<S: FrameSink>(&mut self, sink: &mut S) -> Result<bool, S::Error> {
        self.state = match mem::take(&mut self.state) {
            State::Ready { container, decoder } => {
                log::debug!("Playback started");
                State::Playing { packets: container.packets(), decoder }
            }

            State::Playing { mut packets, mut decoder } => match packets.next() {
                Some(packet) if packet.track == Track::Audio => {
                    // TODO: decode once there is an Opus decoder
                    log::trace!("Audio packet at {:?}: {} bytes", packet.pts, packet.data.len());
                    State::Playing { packets, decoder }
                }

                Some(packet) => {
                    log::trace!("Packet at {:?}: {} bytes", packet.pts, packet.data.len());
                    decoder.send_packet(packet.data)?;

                    if let Some(frame) = decoder.receive_frame() {
                        sink.present(&frame)?;
                    } else {
                        // Hidden VP9 reference frame
                        log::trace!("Packet at {:?} produced no frame", packet.pts);
                    }

                    State::Playing { packets, decoder }
                }

                None => State::Done,
            },

            State::Done => State::Done,
        };

        Ok(!matches!(self.state, State::Done))
    }

    pub fn play<S: FrameSink>(&mut self, sink: &mut S) -> Result<(), S::Error> {
        while self.tick(sink)? {}
        Ok(())
    }
}
