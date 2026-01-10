use core::arch::asm;

/// PC Speaker driver for playing notes
pub struct PCSpeaker;

impl PCSpeaker {
    const PIT_FREQUENCY: u32 = 1193182;
    const TIMER_CONTROL: u16 = 0x43;
    const TIMER_CHANNEL_2: u16 = 0x42;
    const PC_SPEAKER_PORT: u16 = 0x61;

    /// Convert MIDI note number to frequency in Hz.
    pub fn midi_to_freq(note: u8) -> u32 {
        // A4 (note 69) = 440 Hz
        // Frequency = 440 * 2^((note - 69) / 12)

        let exp = (note as f32 - 69.0) / 12.0;
        (440.0 * libm::powf(2.0, exp)) as u32
    }

    /// Play a note with the given frequency.
    pub fn play_freq(freq: u32) {
        if freq == 0 {
            Self::silence();
            return;
        }

        let divisor = (Self::PIT_FREQUENCY / freq) as u16;

        unsafe {
            // Set PIT channel 2 to square wave mode
            asm!(
                "out dx, al",
                in("dx") Self::TIMER_CONTROL,
                in("al") 0b10110110u8, // Channel 2, lobyte/hibyte, square wave
                options(nomem, nostack, preserves_flags)
            );

            // Low byte of divisor
            asm!(
                "out dx, al",
                in("dx") Self::TIMER_CHANNEL_2,
                in("al") (divisor & 0xFF) as u8,
                options(nomem, nostack, preserves_flags)
            );

            // High byte of divisor
            asm!(
                "out dx, al",
                in("dx") Self::TIMER_CHANNEL_2,
                in("al") ((divisor >> 8) & 0xFF) as u8,
                options(nomem, nostack, preserves_flags)
            );

            // Finally, enable the speaker
            let mut speaker_state: u8;
            asm!(
                "in al, dx",
                in("dx") Self::PC_SPEAKER_PORT,
                out("al") speaker_state,
                options(nomem, nostack, preserves_flags)
            );

            speaker_state |= 0x03;

            asm!(
                "out dx, al",
                in("dx") Self::PC_SPEAKER_PORT,
                in("al") speaker_state,
                options(nomem, nostack, preserves_flags)
            );
        }
    }

    /// Stop playing the current note (silence).
    pub fn silence() {
        unsafe {
            let mut speaker_state: u8;
            asm!(
                "in al, dx",
                in("dx") Self::PC_SPEAKER_PORT,
                out("al") speaker_state,
                options(nomem, nostack, preserves_flags)
            );

            speaker_state &= !0x03;

            asm!(
                "out dx, al",
                in("dx") Self::PC_SPEAKER_PORT,
                in("al") speaker_state,
                options(nomem, nostack, preserves_flags)
            );
        }
    }

    /// Play a MIDI note number.
    pub fn play_note(note: u8) {
        let freq = Self::midi_to_freq(note);
        Self::play_freq(freq);
    }
}
