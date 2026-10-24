use core::arch::naked_asm;
use core::arch::x86_64::{__cpuid, _rdtsc};
use core::fmt::{self, Formatter};
use core::mem::offset_of;
use core::sync::atomic::{AtomicU64, Ordering};
use core::time::Duration;

use x86_64::instructions::interrupts;
use x86_64::instructions::port::Port;

use crate::interrupts::{InterruptSemaphore, IDT};

const NANOS_PER_SEC: u64 = 1_000_000_000;

static PIT_MEASUREMENT: PitMeasurement = PitMeasurement::new();

/// The global [`Tsc`] instance. During initialization, [`Tsc::calibrate`] must be
/// called.
pub static TSC: Tsc = Tsc::new();

/// The CPU's [time stamp counter](https://en.wikipedia.org/wiki/Time_Stamp_Counter).
///
/// This struct handles calibrating and keeping track of its frequency, then querying
/// its value at any instant in time.
pub struct Tsc {
    /// Frequency in Hz, zero when uncalibrated
    hz: AtomicU64,
}

impl Tsc {
    const fn new() -> Self {
        Self { hz: AtomicU64::new(0) }
    }

    /// The current tick count, which counts up from reset.
    #[inline(always)]
    pub fn now(&self) -> u64 {
        // SAFETY: rdtsc is available on every x86_64 CPU
        unsafe { _rdtsc() }
    }

    /// The frequency in Hz, or `None` if not calibrated.
    pub fn hz(&self) -> Option<u64> {
        match self.hz.load(Ordering::Relaxed) {
            0 => None,
            hz => Some(hz),
        }
    }

    /// Whether the TSC has a constant tick rate regardless of power states.
    pub fn is_invariant() -> bool {
        const INVARIANT_TSC_LEAF: u32 = 0x8000_0007;
        const INVARIANT_TSC_BIT: u32 = 1 << 8;

        __cpuid(0x8000_0000).eax >= INVARIANT_TSC_LEAF
            && __cpuid(INVARIANT_TSC_LEAF).edx & INVARIANT_TSC_BIT != 0
    }

    /// Converts a tick count to a [`Duration`], or `None` if uncalibrated.
    pub fn to_duration(&self, ticks: u64) -> Option<Duration> {
        let hz = self.hz()?;
        let nanos = ticks % hz * NANOS_PER_SEC / hz;
        Some(Duration::new(ticks / hz, nanos as u32))
    }

    /// Converts a [`Duration`] to a tick count, or `None` if uncalibrated.
    pub fn to_ticks(&self, duration: Duration) -> Option<u64> {
        let hz = self.hz()?;
        let subsec = u64::from(duration.subsec_nanos()) * hz / NANOS_PER_SEC;
        Some(duration.as_secs() * hz + subsec)
    }

    /// Measures the frequency using PIT channel 0 interrupt, and returns it.
    ///
    /// PIT channel 0 is connected to IRQ0 (vector 0x20 after PIC remap). It is programmed
    /// to fire an interrupt after 10ms and measure TSC ticks.
    #[rustfmt::skip]
    pub fn calibrate(&self) -> u64 {
        const PIT_FREQUENCY: u32 = 1_193_182;
        const CALIBRATION_MS: u32 = 10;
        const PIT_DIVISOR: u32 = PIT_FREQUENCY / (1000 / CALIBRATION_MS);

        // Reset existing calibration results
        PIT_MEASUREMENT.reset();

        let mut pic_command = Port::<u8>::new(0x20);
        let mut pic_data = Port::<u8>::new(0x21);
        let mut pit_channel0 = Port::<u8>::new(0x40);
        let mut pit_command = Port::<u8>::new(0x43);

        let hz = unsafe {
            // Disable interrupts during calibration and register our capture handler
            interrupts::disable();
            PitMeasurement::register_capture_handler();

            // Read current PIC mask and ensure IRQ0 is masked during PIT programming
            let master_mask = pic_data.read();

            // Program PIT on channel 0
            pic_data.write(master_mask | 0x01);            // mask IRQ0
            pit_command.write(0x30);                       // channel 0, LSB then MSB, mode 0, binary
            pic_command.write(0x20);                       // send EOI to existing interrupts

            // NOTE:
            // The firmware programs PIT in periodic mode. One of those ticks may have been left
            // off in a pending state while masked, which would fire immediately after the unmask,
            // triggering the PIT measure too early. We need to handle this before our own PIT
            // programming and measurement.
            
            // Ask the master controller whether IRQ0 is pending by OCW3 command (the IRR)
            if { pic_command.write(0x0A); pic_command.read() & 0x01 } != 0 {
                // If the timer is pending, let it fire, and clear it
                pic_data.write(master_mask & 0xFE);         // unmask
                PIT_MEASUREMENT.wait_for_capture();         // wait for interrupt
                pic_data.write(master_mask | 0x01);         // mask again
                PIT_MEASUREMENT.reset();                    // throw the result away
            }

            pit_channel0.write((PIT_DIVISOR & 0xFF) as u8);        // divisor LSB
            pit_channel0.write(((PIT_DIVISOR >> 8) & 0xFF) as u8); // divisor MSB

            // Capture TSC start
            let tsc_start = self.now();
            pic_data.write(master_mask & 0xFE); // unmask bit 0 to start timer

            // Enable interrupts and wait for PIT
            PIT_MEASUREMENT.wait_for_capture();
            pic_data.write(master_mask | 0x01); // mask timer again

            // Read captured TSC and calculate tick rate in Hz
            let tsc_end = PIT_MEASUREMENT.ticks();
            let tsc_ticks = tsc_end.wrapping_sub(tsc_start);
            (tsc_ticks * 1000) / CALIBRATION_MS as u64
        };

        self.hz.store(hz, Ordering::Relaxed);
        log::debug!("TSC measured at {hz} Hz over a {CALIBRATION_MS}ms PIT interval");
        hz
    }
}

/// A TSC tick count, displayed in milliseconds.
pub struct Ms(pub u64);

impl fmt::Display for Ms {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let micros = TSC
            .to_duration(self.0)
            .map_or(0, |time| time.as_secs() * 1_000_000 + u64::from(time.subsec_micros()));

        write!(f, "{}.{:02}ms", micros / 1000, micros % 1000 / 10)
    }
}

//
// Calibration Instruments
//

/// Captures TSC value and interrupt count during PIT interval measurement.
struct PitMeasurement {
    tsc: AtomicU64,
    interrupt: InterruptSemaphore,
}

impl PitMeasurement {
    const fn new() -> Self {
        Self { tsc: AtomicU64::new(0), interrupt: InterruptSemaphore::new() }
    }

    /// Read the TSC value at the time of measurement.
    fn ticks(&self) -> u64 {
        self.tsc.load(Ordering::SeqCst)
    }

    /// Reset the entire measurement.
    fn reset(&self) {
        self.tsc.store(0, Ordering::SeqCst);
        self.interrupt.reset();
    }

    /// Wait until the interrupt for capturing the TSC has been handled.
    fn wait_for_capture(&self) {
        self.interrupt.wait();
    }

    /// Registers an interrupt handler at IRQ0 which captures the TSC.
    fn register_capture_handler() {
        const TSC_OFFSET: usize = offset_of!(PitMeasurement, tsc);
        const INT_OFF: usize = offset_of!(PitMeasurement, interrupt);
        const COMPLETED_OFF: usize = PIT_MEASUREMENT.interrupt.completed_offset();
        const INT_COMPLETED_OFF: usize = INT_OFF + COMPLETED_OFF;

        #[unsafe(naked)]
        extern "C" fn irq_handler() {
            naked_asm!(
                // Snapshot registers
                "push rax",
                "push rbx",
                "push rdx",

                // Get PIT_MEASUREMENT singleton address
                "lea rbx, [rip + {pit}]",

                // Capture TSC
                "rdtsc",
                "shl rdx, 32",
                "or rax, rdx",
                "mov [rbx + {tsc_off}], rax",

                // Release semaphore
                "mov byte ptr [rbx + {int_off}], 1",
                "mfence",

                // EOI to PIC
                "mov al, 0x20",
                "out 0x20, al",

                // REstore registers
                "pop rdx",
                "pop rbx",
                "pop rax",
                "iretq",

                pit = sym PIT_MEASUREMENT,
                tsc_off = const TSC_OFFSET,
                int_off = const INT_COMPLETED_OFF,
            );
        }

        IDT.register_handler(0x20, irq_handler)
    }
}
