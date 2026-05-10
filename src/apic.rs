use core::arch::{asm, naked_asm};
use core::mem::offset_of;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::asm::*;
use crate::interrupts::{InterruptSemaphore, IDT};

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

static TIMER_SEMAPHORE: InterruptSemaphore = InterruptSemaphore::new();
static PIT_MEASUREMENT: PitMeasurement = PitMeasurement::new();

//
// Local APIC Timer
//

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

    /// Initialize the APIC timer with the specified frequency and divisor.
    ///
    /// The divisor determines the timer frequency. The divisor must be a power
    /// of two from 1 to 128 (i.e., 1, 2, 4, 8, 16, 32, 64, or 128). For high
    /// precision, 16 is commonly used.
    pub fn init(frequency: u32, divisor: u32) -> Self {
        // Enable APIC with spurious interrupt vector
        unsafe {
            Self::lapic_svr().write_volatile(Self::APIC_SW_ENABLE | Self::SPURIOUS_VECTOR);
            Self::lapic_lvt_timer().write_volatile(Self::LVT_MASKED);
            Self::lapic_eoi().write_volatile(0);
        }

        Self::set_divisor(divisor);
        ApicTimer { frequency, divisor }
    }

    /// Calibrate and initialize the APIC timer by measuring its actual frequency.
    ///
    /// This function uses the PIT (Programmable Interval Timer) to measure the
    /// TSC frequency, then uses the TSC to calibrate the APIC timer.
    ///
    /// The divisor determines the timer frequency. The divisor must be a power
    /// of two from 1 to 128 (i.e., 1, 2, 4, 8, 16, 32, 64, or 128). For high
    /// precision, 16 is commonly used.
    pub fn calibrate(divisor: u32) -> Self {
        unsafe {
            Self::lapic_svr().write_volatile(Self::APIC_SW_ENABLE | Self::SPURIOUS_VECTOR);
            Self::lapic_lvt_timer().write_volatile(Self::LVT_MASKED);
            Self::lapic_eoi().write_volatile(0);
        }
        
        Self::set_divisor(divisor);

        let tsc_hz = Self::calibrate_tsc_with_pit();
        let frequency = Self::calibrate_apic_with_tsc(tsc_hz, divisor);
        
        Self { frequency, divisor }
    }

    /// Calibrate TSC frequency using PIT channel 0 interrupt.
    ///
    /// PIT channel 0 is connected to IRQ0 (vector 0x20 after PIC remap).
    /// We program it to fire an interrupt after 10ms and measure TSC ticks.
    #[allow(clippy::let_and_return)]
    #[rustfmt::skip]
    fn calibrate_tsc_with_pit() -> u64 {
        const PIT_FREQUENCY: u32 = 1_193_182;
        const CALIBRATION_MS: u32 = 10;
        const PIT_DIVISOR: u32 = PIT_FREQUENCY / (1000 / CALIBRATION_MS);
        
        // Reset existing calibration results
        PIT_MEASUREMENT.reset();
        
        unsafe {
            // Disable interrupts during calibration and register our capture handler
            cli();
            PitMeasurement::register_capture_handler();

            // Read current PIC mask and ensure IRQ0 is masked during PIT programming
            let master_mask = inb(0x21);

            // Program PIT on channel 0
            outb(0x21, master_mask | 0x01);                // mask IRQ0
            outb(0x43, 0x30);                              // channel 0, LSB then MSB, mode 0, binary
            outb(0x40, (PIT_DIVISOR & 0xFF) as u8);        // divisor LSB
            outb(0x40, ((PIT_DIVISOR >> 8) & 0xFF) as u8); // divisor MSB

            // Capture TSC start
            let tsc_start = rdtsc();
            outb(0x20, 0x20);               // send EOI to existing interrupts
            outb(0x21, master_mask & 0xFE); // unmask bit 0 to start timer
            
            // Enable interrupts and wait for PIT
            PIT_MEASUREMENT.wait_for_capture();
            outb(0x21, master_mask | 0x01); // mask timer again
            
            // Read captured TSC and calculate tick rate in Hz
            let tsc_end = PIT_MEASUREMENT.ticks();
            let tsc_ticks = tsc_end.wrapping_sub(tsc_start);
            let tsc_hz = (tsc_ticks * 1000) / CALIBRATION_MS as u64;

            tsc_hz
        }
    }

    /// Calibrate APIC timer base frequency using TSC as reference.
    ///
    /// Programs APIC timer to count for a known duration (measured via TSC),
    /// then derives the base bus frequency.
    fn calibrate_apic_with_tsc(tsc_hz: u64, divisor: u32) -> u32 {
        unsafe {
            // Disable interrupts during calibration for accurate measurement
            cli();

            // Mask timer during calibration (bit 16 set), one-shot mode (bits hi 18 and lo 17 unset)
            Self::lapic_lvt_timer().write_volatile(Self::LVT_MASKED | (Self::TIMER_VECTOR as u32));
            Self::lapic_icr().write_volatile(0xFFFFFFFF); // initial count
            mfence();

            let ccr_initial = Self::lapic_ccr().read_volatile();
            let tsc_start = rdtsc();
            let tsc_target = tsc_start + (tsc_hz / 10); // 100ms

            // Poll for required time period
            while rdtsc() < tsc_target {
                core::hint::spin_loop();
            }

            // Stop timer and get final count
            Self::lapic_lvt_timer().write_volatile(Self::LVT_MASKED);
            let ccr_final = Self::lapic_ccr().read_volatile();

            // Re-enable interrupts after calibration
            sti();

            let apic_count = (ccr_initial - ccr_final) as u64; // timer counts downwards
            let base_freq = apic_count.wrapping_mul(10).wrapping_mul(divisor as u64);

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
        let base_ticks_per_ms = self.frequency / 1_000;
        let base_ticks = delay_ms * base_ticks_per_ms;
        let effective_ticks = base_ticks / self.divisor;

        if effective_ticks == 0 {
            return;
        }

        #[unsafe(naked)]
        extern "C" fn delay_interrupt_handler() {
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

        IDT.register_handler(Self::TIMER_VECTOR, delay_interrupt_handler);
        unsafe {
            // Oneshot mode unmasked timer with our vector
            Self::lapic_lvt_timer().write_volatile(Self::TIMER_VECTOR as u32);
            Self::lapic_icr().write_volatile(effective_ticks);

            // Wait for the timer to finish, and remask it
            TIMER_SEMAPHORE.wait();
            Self::lapic_lvt_timer().write_volatile(Self::LVT_MASKED | Self::TIMER_VECTOR as u32);
        }
    }
}

//
// Calibration Instruments
//

/// Captures TSC value and interrupt count during PIT interval measurement.
pub struct PitMeasurement {
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
    pub fn register_capture_handler() {
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

