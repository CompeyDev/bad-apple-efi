use core::fmt::Write;

pub struct Serial;

impl Serial {
    pub const COM1: u16 = 0x3F8;

    #[allow(clippy::identity_op)]
    pub fn init() {
        unsafe {
            x86::outb(Self::COM1 + 1, 0x00); // Disable all interrupts
            x86::outb(Self::COM1 + 3, 0x80); // Enable DLAB (set baud rate divisor)
            x86::outb(Self::COM1 + 0, 0x03); // Set divisor to 3 (lo byte) 38400 baud
            x86::outb(Self::COM1 + 1, 0x00); //                  (hi byte)
            x86::outb(Self::COM1 + 3, 0x03); // 8 bits, no parity, one stop bit
            x86::outb(Self::COM1 + 2, 0xC7); // Enable FIFO, clear them, with 14-byte threshold
            x86::outb(Self::COM1 + 4, 0x0B); // IRQs enabled, RTS/DSR set
            x86::outb(Self::COM1 + 4, 0x0B); // Normal operation mode

            // TODO: Echo test?
        }
    }

    pub fn put(c: u8) {
        unsafe {
            while (x86::inb(Self::COM1 + 5) & 0x20) == 0 {}
            x86::outb(Self::COM1, c);
        }
    }
}

impl Write for Serial {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for byte in s.bytes() {
            Self::put(byte);
        }

        Ok(())
    }
}

#[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
mod x86 {
    use core::arch::asm;

    pub unsafe fn outb(port: u16, value: u8) {
        asm!(
            "out dx, al",
            in("dx") port,
            in("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }

    pub unsafe fn inb(port: u16) -> u8 {
        let value: u8;
        asm!(
            "in al, dx",
            in("dx") port,
            out("al") value,
            options(nomem, nostack, preserves_flags)
        );

        value
    }
}
