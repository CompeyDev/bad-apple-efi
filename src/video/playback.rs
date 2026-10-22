use core::mem;

use crate::video::codec::Vp9Decoder;
use crate::video::container::{IvfContainer, Packets};
use crate::video::{FrameSink, VideoError};

#[derive(Default)]
enum State {
    Ready {
        container: IvfContainer,
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
        let container = IvfContainer::parse(data)?;
        let framerate = container.framerate();

        log::info!(
            "VideoPlayer: {}x{}, {} frames, {framerate} fps",
            container.width(),
            container.height(),
            container.frame_count(),
        );

        Ok(Self { state: State::Ready { container, decoder: Vp9Decoder::new()? }, framerate })
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
                State::Playing { packets: container.packets(), decoder }
            }

            State::Playing { mut packets, mut decoder } => match packets.next() {
                Some(packet) => {
                    decoder.send_packet(packet.data)?;
                    if let Some(frame) = decoder.receive_frame() {
                        sink.present(&frame)?;
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
