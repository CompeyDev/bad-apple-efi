use core::arch::asm;

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

            // Wait for 10ms using PIT
            Self::pit_sleep_10ms();

            // Read how much the timer counted down
            let current_count = Self::lapic_ccr().read_volatile();
            let ticks_in_10ms = 0xFFFFFFFF - current_count;

            (ticks_in_10ms as u64 * 100 * divisor as u64) as u32
        };

        Self::init(actual_frequency, divisor)
    }

    /// Sleep for 10ms using the PIT (Programmable Interval Timer).
    ///
    /// This is used during calibration. The PIT runs at a fixed 1.193182 MHz.
    fn pit_sleep_10ms() {
        const PIT_FREQUENCY: u32 = 1193182;
        const PIT_CHANNEL_0: u16 = 0x40;
        const PIT_COMMAND: u16 = 0x43;

        // Interval of 10ms (1/100th of a second)
        let count = (PIT_FREQUENCY / 100) as u16;

        unsafe {
            // Set PIT to mode 0 (interrupt on terminal count), binary mode
            asm!(
                "out dx, al",
                in("dx") PIT_COMMAND,
                in("al") 0b00110000u8, // Channel 0, lobyte/hibyte, mode 0
                options(nomem, nostack, preserves_flags)
            );

            // Low byte of count
            asm!(
                "out dx, al",
                in("dx") PIT_CHANNEL_0,
                in("al") (count & 0xFF) as u8,
                options(nomem, nostack, preserves_flags)
            );

            // High byte of count
            asm!(
                "out dx, al",
                in("dx") PIT_CHANNEL_0,
                in("al") ((count >> 8) & 0xFF) as u8,
                options(nomem, nostack, preserves_flags)
            );

            // Wait for PIT to count down
            let mut prev_count = count;
            loop {
                // Latch count
                asm!(
                    "out dx, al",
                    in("dx") PIT_COMMAND,
                    in("al") 0b00000000u8,
                    options(nomem, nostack, preserves_flags)
                );

                // Read low byte
                let low: u8;
                asm!(
                    "in al, dx",
                    in("dx") PIT_CHANNEL_0,
                    out("al") low,
                    options(nomem, nostack, preserves_flags)
                );

                // Read high byte
                let high: u8;
                asm!(
                    "in al, dx",
                    in("dx") PIT_CHANNEL_0,
                    out("al") high,
                    options(nomem, nostack, preserves_flags)
                );

                // Check if count wrapped around (reached 0)
                let current_count = ((high as u16) << 8) | (low as u16);
                if current_count > prev_count {
                    break;
                }

                prev_count = current_count;
                core::hint::spin_loop();
            }
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

        unsafe {
            // Set mode to oneshot (0x0) and mask the interrupt
            Self::lapic_lvt_timer().write_volatile(Self::LVT_MASKED);
            Self::lapic_icr().write_volatile(ticks);
        }

        self.wait_for_timer();
    }

    /// Wait for the APIC timer to finish counting down to zero.
    fn wait_for_timer(&self) {
        unsafe {
            while Self::lapic_ccr().read_volatile() > 0 {
                core::hint::spin_loop();
            }
        }
    }
}
