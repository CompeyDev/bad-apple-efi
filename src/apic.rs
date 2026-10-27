use core::arch::naked_asm;
use core::arch::x86_64::{__cpuid, _mm_mfence};
use core::sync::atomic::{AtomicU32, Ordering};
use core::time::Duration;

use x86_64::instructions::interrupts;
use x86_64::registers::model_specific::{ApicBase, ApicBaseFlags, Msr};

use crate::interrupts::{InterruptSemaphore, IDT};
use crate::tsc::TSC;

static LAPIC_BASE: spin::Lazy<u32> = spin::Lazy::new(|| {
    let (frame, flags) = ApicBase::read();

    // Our driver needs MMIO, so we look for xAPIC mode
    let xapic = flags.intersection(ApicBaseFlags::LAPIC_ENABLE | ApicBaseFlags::X2APIC_ENABLE);
    assert_eq!(xapic, ApicBaseFlags::LAPIC_ENABLE, "LAPIC disabled or x2APIC-only");

    frame.start_address().as_u64() as u32
});

/// If a wait is issued for shorter than this threshold, it is too expensive to use the
/// interrupt round trip which may take longer than the requested time. In this case, we
/// use a simple spin loop to busy wait. Busy waiting keeps the CPU active for a very short
/// period of time too, so that is fine.
const SPIN_THRESHOLD: Duration = Duration::from_micros(20);

static TIMER_SEMAPHORE: InterruptSemaphore = InterruptSemaphore::new();

/// The global [`ApicTimer`] instance. [`ApicTimer::calibrate`] must be called once.
pub static APIC_TIMER: ApicTimer = ApicTimer::new();

//
// Local APIC Timer
//

/// [APIC](https://en.wikipedia.org/wiki/Advanced_Programmable_Interrupt_Controller) timer
/// abstraction for delay functionality.
pub struct ApicTimer {
    /// Base frequency that the timer ticks at naturally, zero when uncalibrated
    frequency: AtomicU32,
    /// Divisor applied to the base frequency
    divisor: AtomicU32,
}

impl ApicTimer {
    const fn new() -> Self {
        Self { frequency: AtomicU32::new(0), divisor: AtomicU32::new(1) }
    }

    /// Spurious interrupt vector number
    const SPURIOUS_VECTOR: u32 = 0xFF;
    /// APIC Software Enable bit
    const APIC_SW_ENABLE: u32 = 0x100;
    /// LVT Timer mask bit (disable interrupts)
    const LVT_MASKED: u32 = 0x10000;
    /// LVT Timer mode bits for TSC deadline mode
    const LVT_TSC_DEADLINE: u32 = 0b10 << 17;
    /// `IA32_TSC_DEADLINE` MSR, which fires the timer when the TSC reaches its value
    const TSC_DEADLINE_MSR: u32 = 0x6E0;
    /// Interrupt vector used for APIC timer interrupts
    pub const TIMER_VECTOR: u8 = 0x30;

    /// Spurious Interrupt Vector Register
    #[inline(always)]
    pub fn lapic_svr() -> *mut u32 {
        (*LAPIC_BASE + 0xF0) as *mut u32
    }

    #[inline(always)]
    pub fn lapic_eoi() -> *mut u32 {
        (*LAPIC_BASE + 0xB0) as *mut u32
    }

    /// Timer Divide Configuration Register
    #[inline(always)]
    pub fn lapic_tdcr() -> *mut u32 {
        (*LAPIC_BASE + 0x3E0) as *mut u32
    }

    /// Local Vector Table Timer Register
    #[inline(always)]
    pub fn lapic_lvt_timer() -> *mut u32 {
        (*LAPIC_BASE + 0x320) as *mut u32
    }

    /// Initial Count Register (Timer count)
    #[inline(always)]
    pub fn lapic_icr() -> *mut u32 {
        (*LAPIC_BASE + 0x380) as *mut u32
    }

    /// Current Count Register (Timer current value)
    #[inline(always)]
    pub fn lapic_ccr() -> *mut u32 {
        (*LAPIC_BASE + 0x390) as *mut u32
    }

    /// Calibrate and initialize the APIC timer by measuring its actual frequency.
    ///
    /// This function uses the PIT (Programmable Interval Timer) to measure the
    /// TSC frequency, then uses the TSC to calibrate the APIC timer.
    ///
    /// The divisor determines the timer frequency. The divisor must be a power
    /// of two from 1 to 128 (i.e., 1, 2, 4, 8, 16, 32, 64, or 128). For high
    /// precision, 16 is commonly used.
    pub fn calibrate(&self, divisor: u32) {
        // Enable APIC with spurious interrupt vector
        unsafe {
            Self::lapic_svr().write_volatile(Self::APIC_SW_ENABLE | Self::SPURIOUS_VECTOR);
            Self::lapic_lvt_timer().write_volatile(Self::LVT_MASKED);
            Self::lapic_eoi().write_volatile(0);
        }

        Self::set_divisor(divisor);

        TSC.calibrate();
        let frequency = Self::calibrate_apic_with_tsc(divisor);
        log::debug!("APIC timer base frequency: {} MHz (divisor {divisor})", frequency / 1_000_000);

        self.frequency.store(frequency, Ordering::Relaxed);
        self.divisor.store(divisor, Ordering::Relaxed);
    }

    /// Calibrate APIC timer base frequency using TSC as reference.
    ///
    /// Programs APIC timer to count for a known duration (measured via TSC),
    /// then derives the base bus frequency.
    fn calibrate_apic_with_tsc(divisor: u32) -> u32 {
        const CALIBRATION_MS: u64 = 100;

        unsafe {
            // Disable interrupts during calibration for accurate measurement
            interrupts::disable();

            // Mask timer during calibration (bit 16 set), one-shot mode (bits hi 18 and lo 17 unset)
            Self::lapic_lvt_timer().write_volatile(Self::LVT_MASKED | (Self::TIMER_VECTOR as u32));
            Self::lapic_icr().write_volatile(0xFFFFFFFF); // initial count
            _mm_mfence();

            let ccr_initial = Self::lapic_ccr().read_volatile();
            let tsc_start = TSC.now();
            let window = TSC.to_ticks(Duration::from_millis(CALIBRATION_MS));
            let tsc_target = tsc_start + window.expect("TSC is calibrated first");

            // Poll for required time period
            while TSC.now() < tsc_target {
                core::hint::spin_loop();
            }

            // Stop timer and get final count
            Self::lapic_lvt_timer().write_volatile(Self::LVT_MASKED);
            let ccr_final = Self::lapic_ccr().read_volatile();

            // Re-enable interrupts after calibration
            interrupts::enable();

            let apic_count = (ccr_initial - ccr_final) as u64; // timer counts downwards
            let per_second = 1000 / CALIBRATION_MS;
            let base_freq = apic_count.wrapping_mul(per_second).wrapping_mul(divisor as u64);

            base_freq as u32
        }
    }

    /// Set the timer divisor
    ///
    /// The APIC Timer Divide Configuration Register uses a specific encoding
    /// for divisor values, not the divisor value directly.
    pub fn set_divisor(divisor: u32) {
        let encoded = match divisor {
            1 => 0b1011,
            2 => 0b0000,
            4 => 0b0001,
            8 => 0b0010,
            16 => 0b0011,
            32 => 0b1000,
            64 => 0b1001,
            128 => 0b1010,
            _ => panic!(
                "Invalid APIC timer divisor: {}. Must be 1, 2, 4, 8, 16, 32, 64, or 128",
                divisor
            ),
        };

        unsafe {
            Self::lapic_tdcr().write_volatile(encoded);
        }
    }

    /// Set up the APIC timer for a specific delay in milliseconds. The number of
    /// ticks is calculated based on the desired delay, the timer frequency, and
    /// the configured divisor.
    pub fn delay(&self, delay_ms: u32) {
        let base_ticks_per_ms = self.frequency.load(Ordering::Relaxed) / 1_000;
        let base_ticks = delay_ms * base_ticks_per_ms;
        let effective_ticks = base_ticks / self.divisor.load(Ordering::Relaxed);

        if effective_ticks == 0 {
            return;
        }

        Self::wait_for_interrupt(|| unsafe {
            // Oneshot mode unmasked timer with our vector
            Self::lapic_lvt_timer().write_volatile(Self::TIMER_VECTOR as u32);
            Self::lapic_icr().write_volatile(effective_ticks);
        });
    }

    /// Runs `f`, expecting it to start a timer on [`Self::TIMER_VECTOR`], and halts until
    /// the interrupt fires, then remasks the timer.
    ///
    /// Used internally for both timer modes.
    fn wait_for_interrupt(f: impl FnOnce()) {
        #[unsafe(naked)]
        extern "C" fn timer_interrupt_handler() {
            naked_asm!(
                "push rax", // snapshot rax

                // Direct atomic store to semaphore state (AtomicBool is repr(transparent))
                "lea rax, [rip + {sem}]",
                "add rax, {off}",
                "mov byte ptr [rax], 1",
                "mfence",

                // EOI
                "mov rax, 0xFEE000B0",
                "mov dword ptr [rax], 0",
                "mfence",

                "pop rax", // restore rax
                "iretq",

                // Take in semaphore address and offset to inner field to calculate
                // the direct address to the field for the completion state
                sem = sym TIMER_SEMAPHORE,
                off = const TIMER_SEMAPHORE.completed_offset(),
            );
        }

        IDT.register_handler(Self::TIMER_VECTOR, timer_interrupt_handler);
        TIMER_SEMAPHORE.reset();
        f();

        // Wait for the timer to finish, and remask it
        TIMER_SEMAPHORE.wait();
        unsafe {
            Self::lapic_lvt_timer().write_volatile(Self::LVT_MASKED | Self::TIMER_VECTOR as u32);
        }
    }

    /// Whether the timer can wait until an absolute TSC value (`CPUID.01H:ECX[24]`).
    pub fn supports_tsc_deadline() -> bool {
        __cpuid(1).ecx & (1 << 24) != 0
    }

    /// Sleeps for `duration`. See [`ApicTimer::sleep_until`].
    pub fn sleep(&self, duration: Duration) {
        if let Some(ticks) = TSC.to_ticks(duration) {
            self.sleep_until(TSC.now() + ticks);
        }
    }

    /// Sleeps until the TSC reaches `deadline`, halting the CPU.
    ///
    /// If the wait time is under the [`SPIN_THRESHOLD`], busy waiting is used instead.
    ///
    /// On CPUs without support for TSC deadline mode on the APIC, oneshot mode is used to
    /// replicate its behavior, which only has whole millisecond granularity.
    pub fn sleep_until(&self, deadline: u64) {
        let Some(remaining) = deadline.checked_sub(TSC.now()) else { return };
        let remaining = TSC.to_duration(remaining).unwrap_or_default();

        if remaining >= SPIN_THRESHOLD {
            if Self::supports_tsc_deadline() {
                Self::sleep_tsc_deadline(deadline);
            } else {
                log::warn!("TSC deadline mode unsupported, using oneshot mode");
                let millis = remaining.as_secs() * 1000 + u64::from(remaining.subsec_millis());
                if millis > 0 {
                    self.delay(millis as u32);
                }
            }
        }

        // If the wait time is too small, fall back to a spin loop
        while TSC.now() < deadline {
            log::debug!("Wait time too granular, using spin loop");
            core::hint::spin_loop();
        }
    }

    fn sleep_tsc_deadline(deadline: u64) {
        Self::wait_for_interrupt(|| unsafe {
            let lvt = Self::LVT_TSC_DEADLINE | Self::TIMER_VECTOR as u32;
            Self::lapic_lvt_timer().write_volatile(lvt);
            _mm_mfence(); // mode switch should be ordered before MSR (source: SDM)
            Msr::new(Self::TSC_DEADLINE_MSR).write(deadline);
        });

        // The deadline disarms itself anyway, just to be safe
        unsafe { Msr::new(Self::TSC_DEADLINE_MSR).write(0) };
    }
}
