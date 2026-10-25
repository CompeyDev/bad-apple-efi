use alloc::vec::Vec;
use core::mem;
use core::time::Duration;

use crate::video::codec::{OpusDecoder, Vp9Decoder};
use crate::video::container::{Packet, Packets, Track, WebmContainer};
use crate::video::{AudioSink, FrameSink, VideoError, AUDIO_CHANNELS, AUDIO_SAMPLE_RATE};

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
    audio: Option<AudioQueue>,
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

        let audio = AudioQueue::new(&container)?;
        let state = State::Ready { container, decoder: Vp9Decoder::new()? };
        Ok(Self { state, audio, framerate })
    }

    pub fn framerate(&self) -> u32 {
        self.framerate
    }

    /// Whether there is an audio track to play.
    pub fn has_audio(&self) -> bool {
        self.audio.is_some()
    }

    /// Advances playback by a step, handing any decoded frame to `sink`. Returns
    /// `Ok(false)` once playback has ended.
    ///
    /// An error ends playback, with an `Err` return.
    pub fn tick<S: FrameSink + AudioSink>(&mut self, sink: &mut S) -> Result<bool, S::Error> {
        self.state = match mem::take(&mut self.state) {
            State::Ready { container, decoder } => {
                log::debug!("Playback started");
                let packets = container.packets(Track::Video).expect("WebM always has video");
                State::Playing { packets, decoder }
            }

            State::Playing { mut packets, mut decoder } => {
                if let Some(audio) = &mut self.audio {
                    audio.fill(sink)?;
                }

                match packets.next() {
                    Some(packet) => {
                        log::trace!("Packet at {:?}: {} bytes", packet.pts, packet.data.len());
                        decoder.send_packet(&packet)?;

                        if let Some(frame) = decoder.receive_frame() {
                            sink.present(&frame)?;
                        } else {
                            // Hidden VP9 reference frame
                            log::trace!("Packet at {:?} produced no frame", packet.pts);
                        }

                        State::Playing { packets, decoder }
                    }

                    None => State::Done,
                }
            }

            State::Done => State::Done,
        };

        Ok(!matches!(self.state, State::Done))
    }

    pub fn play<S: FrameSink + AudioSink>(&mut self, sink: &mut S) -> Result<(), S::Error> {
        while self.tick(sink)? {}
        Ok(())
    }
}

/// Decodes the audio track, holding on to what the sink cannot add to its queue.
struct AudioQueue {
    packets: Packets,
    decoder: OpusDecoder,
    pcm: Vec<i16>,
    pos: usize,
    skip: usize, // opus pre-skip
}

impl AudioQueue {
    fn new(container: &WebmContainer) -> Result<Option<Self>, VideoError> {
        let (Some(track), Some(packets)) = (container.audio(), container.packets(Track::Audio))
        else {
            return Ok(None);
        };

        Ok(Some(Self {
            packets,
            decoder: OpusDecoder::new(AUDIO_CHANNELS)?,
            pcm: Vec::new(),
            pos: 0,
            skip: samples(track.codec_delay),
        }))
    }

    /// Feeds decoded samples to `sink` until it is full, or the track has ended.
    fn fill(&mut self, sink: &mut impl AudioSink) -> Result<(), VideoError> {
        loop {
            self.pos += sink.queue(&self.pcm[self.pos..]);
            if self.pos < self.pcm.len() {
                return Ok(());
            }

            match self.packets.next() {
                Some(packet) => self.decode(&packet)?,
                None => return Ok(()),
            }
        }
    }

    fn decode(&mut self, packet: &Packet) -> Result<(), VideoError> {
        let channels = usize::from(AUDIO_CHANNELS);
        self.pcm.resize(OpusDecoder::MAX_FRAME_SIZE * channels, 0);
        let decoded = self.decoder.decode(packet.data, &mut self.pcm)?;

        // Trim the padding from the end, and the pre-skip from the start
        let end = decoded.saturating_sub(samples(packet.discard));
        let start = self.skip.min(end);
        self.skip -= start;

        log::trace!("Audio packet at {:?}: {decoded} samples", packet.pts);
        self.pcm.truncate(end * channels);
        self.pos = start * channels;
        Ok(())
    }
}

/// Converts a duration to samples per channel and rounds it.
fn samples(duration: Duration) -> usize {
    let rate = u64::from(AUDIO_SAMPLE_RATE);
    let subsec = (u64::from(duration.subsec_nanos()) * rate + 500_000_000) / 1_000_000_000;
    (duration.as_secs() * rate + subsec) as usize
}
