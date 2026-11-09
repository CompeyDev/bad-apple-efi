use core::marker::PhantomData;
use core::ops::RangeInclusive;
use core::ptr::{self, NonNull};

use zerocopy::IntoBytes;

use crate::hda::bindings::*;
use crate::hda::{Hda, HdaError};

/// Size of the software queue that feeds the hardware buffer. uHDA rounds this value to whole pages.
const QUEUE_SIZE: u32 = 0x10000;
/// Size of the hardware buffer. uHDA splits this into 256 pages.
const BUFFER_SIZE: u32 = 256 * 0x1000;

/// Plays interleaved 16-bit PCM samples through one of the controller's outputs.
///
/// The stream only pulls from its queue while [`Hda::poll`] is called.
pub struct Stream<'a> {
    _hda: PhantomData<&'a Hda>,
    base: NonNull<UhdaStream>,
    path: NonNull<UhdaPath>,
    inner: NonNull<UhdaSimpleStream>,

    /// Number of bytes read by the hardware in total.
    played: u64,
    /// The position of the current byte in the buffer.
    position: u32,
}

impl<'a> Stream<'a> {
    /// The maximum volume streams start at.
    pub const MAX_VOLUME: u8 = 100;

    /// Sets up a stream to the most preferred output. See [`Output::preference`].
    pub(super) fn open(hda: &'a Hda, sample_rate: u32, channels: u32) -> Result<Self, HdaError> {
        let output = hda
            .outputs()
            .filter_map(|output| Some((output.preference()?, output)))
            .min_by_key(|&(preference, _)| preference)
            .map(|(_, output)| output)
            .ok_or(HdaError::NoOutput)?;

        log::debug!("Playing {channels} channels at {sample_rate} Hz to {output}");

        let mut path = ptr::null_mut();
        unsafe { uhda_find_path(output.0, ptr::null(), 0, true, &mut path) }.ok()?;

        let (mut streams, mut count) = (ptr::null_mut(), 0);
        unsafe { uhda_get_output_streams(hda.controller.as_ptr(), &mut streams, &mut count) };
        let base = match count {
            0 => return Err(HdaError::NoStream),
            _ => unsafe { *streams },
        };

        let mut inner = ptr::null_mut();
        unsafe { uhda_simple_stream_new(base, &mut inner) }.ok()?;

        // From here on, dropping the stream undoes whichever steps succeeded
        let stream = Self {
            _hda: PhantomData,
            base: NonNull::new(base).expect("uHDA output streams can't be null"),
            path: NonNull::new(path).expect("uHDA should return a path on success"),
            inner: NonNull::new(inner).expect("uHDA should return a stream on success"),
            played: 0,
            position: 0,
        };

        let params = UhdaSimpleStreamParams {
            sample_rate,
            channels,
            fmt: UhdaFormat::PCM16,
            ring_buffer_size: QUEUE_SIZE,
        };

        unsafe {
            uhda_simple_stream_setup(stream.inner.as_ptr(), &params).ok()?;
            uhda_simple_path_setup(stream.path.as_ptr(), stream.inner.as_ptr()).ok()?;
        }

        stream.set_volume(Self::MAX_VOLUME)?;
        stream.set_muted(false)?;
        Ok(stream)
    }

    /// Sets the volume, as a percentage up to [`Self::MAX_VOLUME`] of the output's amplifier.
    pub fn set_volume(&self, volume: u8) -> Result<(), HdaError> {
        unsafe { uhda_path_set_volume(self.path.as_ptr(), volume.into()) }.ok()
    }

    /// The range of volumes the output's amplifier can be set to, in hundredths of a decibel.
    pub fn volume_range(&self) -> Result<RangeInclusive<i32>, HdaError> {
        let mut range = UhdaVolumeRange::default();
        unsafe { uhda_path_get_volume_range(self.path.as_ptr(), &mut range) }.ok()?;
        Ok(range.min_cdb..=range.max_cdb)
    }

    /// Sets the volume in hundredths of a decibel, rounded and clamped to [`Self::volume_range`].
    pub fn set_volume_db(&self, volume: i32) -> Result<(), HdaError> {
        unsafe { uhda_path_set_volume_db(self.path.as_ptr(), volume) }.ok()
    }

    /// Mutes or unmutes the output.
    pub fn set_muted(&self, muted: bool) -> Result<(), HdaError> {
        unsafe { uhda_path_mute(self.path.as_ptr(), muted) }.ok()
    }

    /// Queues as many of the samples as there is space for, returning how many were queued.
    pub fn queue(&mut self, samples: &[i16]) -> usize {
        let bytes = samples.as_bytes();
        let mut size = u32::try_from(bytes.len()).unwrap_or(QUEUE_SIZE);

        let _ = unsafe {
            // always successful
            uhda_simple_stream_queue_data(self.inner.as_ptr(), bytes.as_ptr().cast(), &mut size)
        };

        size as usize / 2
    }

    /// The number of samples in the software queue not yet handed to the hardware.
    pub fn queued(&self) -> usize {
        let mut remaining = 0;
        let _ = unsafe { uhda_simple_stream_get_remaining(self.inner.as_ptr(), &mut remaining) };
        remaining as usize / 2
    }

    /// The number of samples the hardware has played so far.
    ///
    /// Must be called before the hardware loops around its buffer more than once, which is
    /// every 5.4s for 48KHz 16-bit stereo audio.
    pub fn played(&mut self) -> u64 {
        let position = unsafe { uhda_stream_get_position(self.base.as_ptr()) };
        self.played += u64::from((position + BUFFER_SIZE - self.position) % BUFFER_SIZE);
        self.position = position;

        self.played / 2
    }

    /// Starts or pauses playback.
    pub fn play(&mut self, play: bool) -> Result<(), HdaError> {
        unsafe { uhda_simple_stream_play(self.inner.as_ptr(), play) }.ok()?;

        // NOTE:
        // uHDA restarts paused streams at the start of their buffer, so we count what was played
        // before pausing, and track the position from zero after resuming. See patch #4 for uHDA.
        if play {
            self.position = 0;
        } else {
            self.played();
        }

        Ok(())
    }
}

impl Drop for Stream<'_> {
    fn drop(&mut self) {
        // Each step is a no-op if its setup step didn't run
        unsafe {
            let _ = uhda_simple_stream_play(self.inner.as_ptr(), false);
            let _ = uhda_path_shutdown(self.path.as_ptr());
            let _ = uhda_stream_shutdown(self.base.as_ptr());
            uhda_simple_stream_destroy(self.inner.as_ptr());
        }
    }
}
