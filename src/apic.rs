use core::arch::asm;

use crate::asm::*;

const IA32_APIC_BASE_MSR: u32 = 0x1B;
static LAPIC_BASE: spin::Lazy<u32> = spin::Lazy::new(|| {
    let low: u32;
    let high: u32;

    unsafe {
        asm!(
            "rdmsr",
            in("ecx") IA32_APIC_BASE_MSR,
            out("eax") low,
            out("edx") high,
            options(nomem, nostack, preserves_flags),
        );
    }

    let value = ((high as u64) << 32) | (low as u64);
    let base = value & 0xFFFF_F000; // Bits 12-35

    // Bit 11 denotes whether LAPIC is globally enabled
    if (value & (1 << 11)) != 0 {
        base as u32
    } else {
        panic!("LAPIC disabled or x2APIC-only");
    }
});

/// [APIC](https://en.wikipedia.org/wiki/Advanced_Programmable_Interrupt_Controller) timer
/// abstraction for delay functionality.
#[derive(Debug, Clone, Copy)]
pub struct ApicTimer {
    /// Base frequency that the timer ticks at naturally
    frequency: u32,
    /// Divisor applied to the base frequency
    divisor: u32,
}

impl ApicTimer {
    /// Spurious interrupt vector number
    const SPURIOUS_VECTOR: u32 = 0xFF;
    /// APIC Software Enable bit
    const APIC_SW_ENABLE: u32 = 0x100;
    /// LVT Timer mask bit (disable interrupts)
    const LVT_MASKED: u32 = 0x10000;

    // IDT & IRQ constants
    const IDT_ENTRIES: usize = 256;
    const APIC_EOI: *mut u32 = 0xFEE000B0 as *mut u32;
    const APIC_SVR: *mut u32 = 0xFEE000F0 as *mut u32;
    const TIMER_VECTOR: u8 = 0x20;

    /// Spurious Interrupt Vector Register
    #[inline(always)]
    pub fn lapic_svr() -> *mut u32 {
        (*LAPIC_BASE + 0xF0) as *mut u32
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

    /// Initialize the APIC timer with the specified frequency and divisor.
    ///
    /// The divisor determines the timer frequency. The divisor must be a power
    /// of two from 1 to 128 (i.e., 1, 2, 4, 8, 16, 32, 64, or 128). For high
    /// precision, 16 is commonly used.
    pub fn init(frequency: u32, divisor: u32) -> Self {
        // Enable APIC with spurious interrupt vector
        unsafe {
            Self::lapic_svr().write_volatile(Self::APIC_SW_ENABLE | Self::SPURIOUS_VECTOR);
        }

        let timer = ApicTimer { frequency, divisor };
        timer.set_divisor(divisor);
        timer
    }

    /// Calibrate and initialize the APIC timer by measuring its actual frequency.
    ///
    /// This function uses the PIT (Programmable Interval Timer) to measure the
    /// APIC timer's base frequency. The calibration period is 10ms.
    ///
    /// The divisor determines the timer frequency. The divisor must be a power
    /// of two from 1 to 128 (i.e., 1, 2, 4, 8, 16, 32, 64, or 128). For high
    /// precision, 16 is commonly used.
    pub fn calibrate(divisor: u32) -> Self {
        // Emit the initial divisor into the register before measuring
        let _ = Self::init(0, divisor);

        let actual_frequency = unsafe {
            // Oneshot mode, masked interrupt, max initial count
            Self::lapic_lvt_timer().write_volatile(Self::LVT_MASKED);
            Self::lapic_icr().write_volatile(0xFFFFFFFF);

            // Wait for 10ms using PIT interrupts
            Self::pit_sleep_10ms_irq();

            // Read how much the timer counted down
            let current_count = Self::lapic_ccr().read_volatile();
            let ticks_in_10ms = 0xFFFFFFFF - current_count;

            (ticks_in_10ms as u64 * 100 * divisor as u64) as u32
        };

        Self::init(actual_frequency, divisor)
    }

    /// Sleep for 10ms using the PIT (Programmable Interval Timer) via interrupts.
    ///
    /// This is used during calibration. The PIT runs at a fixed 1.193182 MHz.
    pub(crate) fn pit_sleep_10ms_irq() {
        const PIT_FREQUENCY: u32 = 1193182;
        const PIT_CHANNEL_0: u16 = 0x40;
        const PIT_COMMAND: u16 = 0x43;
        const PIT_VECTOR: u8 = 0x21;

        register_irq_handler(PIT_VECTOR, pit_irq_handler);
        unsafe {
            // Enable PIC IRQ0 (PIT)
            let imr = inb(0xA1);
            outb(0xA1, imr & !1); // Unmask IRQ0

            // Program PIT for 10ms oneshot
            let count = (PIT_FREQUENCY / 100) as u16;
            outb(PIT_COMMAND, 0b00110000u8); // Channel 0, lobyte/hibyte, mode 0
            outb(PIT_CHANNEL_0, (count & 0xFF) as u8);
            outb(PIT_CHANNEL_0, ((count >> 8) & 0xFF) as u8);
        }

        unsafe {
            // Wait for interrupt
            Self::wait_for_irq();

            // Reset PIC IRQ0 mask
            let imr = inb(0xA1);
            outb(0xA1, imr | 1);
        }
    }

    /// Set the timer divisor.
    ///
    /// The APIC Timer Divide Configuration Register uses a specific encoding
    /// for divisor values, not the divisor value directly.
    pub fn set_divisor(&self, divisor: u32) {
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
        let effective_frequency = self.frequency / self.divisor;
        let ticks_per_ms = effective_frequency / 1_000;
        let ticks = delay_ms * ticks_per_ms;

        register_irq_handler(Self::TIMER_VECTOR, timer_irq_handler);
        unsafe {
            // Oneshot mode unmasked timer with our vector
            Self::lapic_lvt_timer().write_volatile(Self::TIMER_VECTOR as u32);
            Self::lapic_icr().write_volatile(ticks);

            // Wait for interrupt
            Self::wait_for_irq();
            Self::lapic_lvt_timer().write_volatile(Self::LVT_MASKED | Self::TIMER_VECTOR as u32);
        }
    }

    /// Enable the APIC spurious interrupt vector and maps the given vector
    /// to the IRQ trampoline handler.
    pub fn setup_apic_irq(vector: u8) {
        unsafe {
            let svr = Self::APIC_SVR.read_volatile();
            Self::APIC_SVR.write_volatile(svr | 0x100);
        }

        register_irq_handler(vector, irq_trampoline);
    }

    /// Send End of Interrupt (EOI) to the local APIC.
    pub fn send_eoi() {
        unsafe {
            Self::APIC_EOI.write_volatile(0);
            mfence();
        }
    }

    /// Waits for an interrupt associated with our timer, i.e., an interrupt has
    /// been fired, and the IRQ flag has been mutated.
    unsafe fn wait_for_irq() {
        // Reset state and initialize interrupt mask
        reset_irq_completed();
        sti();

        // Wait for interrupts, continue once our specific IRQ has been fired
        while !irq_completed() {
            hlt();
        }

        // Reset mask for the LVT
        cli();
    }
}

/// Reads the current value of the Time Stamp Counter.
#[inline(always)]
unsafe fn rdtsc() -> u64 {
    let lo: u32;
    let hi: u32;
    asm!("rdtsc", out("eax") lo, out("edx") hi, options(nomem, nostack, preserves_flags));
    ((hi as u64) << 32) | (lo as u64)
}

/// Calibrates the TSC frequency by measuring TSC ticks over a 10ms PIT interval.
/// Returns the frequency in Hz.
#[no_mangle]
pub extern "C" fn tsc_calibrate() -> u64 {
    unsafe {
        // Read starting TSC
        let start = rdtsc();

        // Wait 10ms using PIT interrupt
        ApicTimer::pit_sleep_10ms_irq();

        // Read ending TSC
        let end = rdtsc();

        // Ticks in 10ms, extrapolate to Hz (ticks per second)
        let ticks_10ms = end - start;
        ticks_10ms * 100
    }
}
}
