use core::fmt::{self, Display, Formatter, Write};
use core::time::Duration;

use heapless::Deque;
use log::{Level, LevelFilter, Log, Metadata, Record};
use owo_colors::{OwoColorize, Style};
use spin::Mutex;
use uart_16550::backend::PioBackend;
use uart_16550::spec::registers::IER;
use uart_16550::spec::{calc_baud_rate, CLK_FREQUENCY_HZ, FIFO_SIZE};
use uart_16550::{BaudRate, Config, Uart16550, Uart16550Tty};
use uefi::boot::{self, Tpl};
use x86_64::instructions::interrupts;
use x86_64::structures::idt::InterruptStackFrame;

use crate::apic::APIC_TIMER;
use crate::interrupts::{pic, X86Handler, IDT};
use crate::tsc::TSC;

const COM1: u16 = 0x3F8;
const COM1_IRQ: u8 = 4;
const DEFAULT_BAUD_RATE: BaudRate = BaudRate::Baud115200;
const DEFAULT_LEVEL: LevelFilter = LevelFilter::Debug;

/// Size of the queue logs wait in while COM1 sends them (~150 lines).
const QUEUE_SIZE: usize = 16 * 1024;
const FIFO_SEND_TIME: Duration = Duration::from_micros(
    // UART: 10 bits per byte (start, 8 data and stop bits).
    (FIFO_SIZE * 10) as u64 * 1_000_000 / DEFAULT_BAUD_RATE.to_integer() as u64,
);

/// The global serial console instance.
pub static CONSOLE: SerialConsole = SerialConsole::new();

pub struct SerialConsole {
    // NOTE:
    // Once interrupts are enabled, the lock is also taken by COM1's interrupt handler, so
    // it must only be held with interrupts disabled, otherwise the handler would deadlock
    port: Mutex<Option<SerialPort>>,
}

/// The COM1 UART, and the logs queued for it.
struct SerialPort {
    uart: Uart16550Tty<PioBackend>,
    /// Bytes waiting to be sent, consumed by COM1's interrupt
    queue: Deque<u8, QUEUE_SIZE>,
    /// Whether logs are queued using interrupts
    interrupts: bool,
    /// Logs dropped since the queue was last full
    dropped: usize,
}

impl SerialConsole {
    const fn new() -> Self {
        Self { port: Mutex::new(None) }
    }

    /// Initializes a logger using the COM1 UART serial port. Must be called before exiting
    /// boot services.
    ///
    /// If an error is experienced during this process, logging is disabled.
    ///
    /// The log level can be set at build time by setting `EFI_LOG` or `RUST_LOG`.
    pub fn init(&'static self) {
        // While the boot services are present, the firmware has its own console driver which
        // also shares the same COM1 port. It is possible for one of its callbacks to consume
        // the loopback bytes sent during the self-test, which would cause a hang as there is
        // no timeout.
        //
        // SAFETY: we raise the priority level to NOTIFY for a few milliseconds of setup
        let tpl = unsafe { boot::raise_tpl(Tpl::NOTIFY) };

        // SAFETY: COM1 is a standard port address
        let mut uart = unsafe { Uart16550::new_port(COM1) }.expect("COM1 is a valid port address");
        let divisor = uart.dll_dlm();
        let baud_rate = match divisor {
            // If the firmware already programs UART, we read and establish that as our baud
            // rate. If not, we program it with our own baud rate
            (0, 0) => DEFAULT_BAUD_RATE,
            (dll, dlm) => {
                calc_baud_rate(CLK_FREQUENCY_HZ, u16::from_le_bytes([dll, dlm]).into(), None)
                    .map_or(DEFAULT_BAUD_RATE, BaudRate::from_integer)
            }
        };

        let present = uart.init(Config { baud_rate, ..Config::DEFAULT }).is_ok();
        let loopback = present.then(|| uart.test_loopback());
        drop(tpl); // drop priority back to APPLICATION

        // Register the UART instance and logger globally
        if present {
            *self.port.lock() = Some(SerialPort {
                uart: Uart16550Tty::from_inner(uart),
                queue: Deque::new(),
                interrupts: false,
                dropped: 0,
            });
        }

        if log::set_logger(self).is_ok() {
            log::set_max_level(
                // Allow setting a build-time max log level. In release builds, this is capped
                // to debug.
                [option_env!("EFI_LOG"), option_env!("RUST_LOG")]
                    .into_iter()
                    .find_map(|env| env?.parse().ok())
                    .unwrap_or(DEFAULT_LEVEL),
            );
        }

        let source = if divisor == (0, 0) { "default" } else { "set by firmware" };
        log::debug!("COM1 running at {} baud ({source})", baud_rate.to_integer());

        if let Some(Err(err)) = loopback {
            log::warn!("COM1 self-test failed ({err}), logs may not be reliable");
        }
    }

    /// Enables interrupts for the console, i.e., logs are queued in the backgroudn rather than
    /// blocking for writes.
    ///
    /// ## Safety
    /// Must be called after boot services has been exited, since the firmware also drives its
    /// own COM1 and PIC, after loading the IDT.
    pub fn enable_interrupts(&self) {
        IDT.register_handler(pic::VECTOR_OFFSET + COM1_IRQ, com1_interrupt as X86Handler);

        interrupts::without_interrupts(|| {
            let mut guard = self.port.lock();
            let Some(port) = guard.as_mut() else { return };

            port.uart.inner_mut().enable_interrupts(IER::THR_EMPTY);
            port.interrupts = true;
            pic::unmask(COM1_IRQ);
        });
    }

    /// Handles panics by bypassing the logging facade, since the logger could also be an origin
    /// of the panic.
    pub(crate) fn write_panic(&self, info: &core::panic::PanicInfo) {
        interrupts::disable();

        // If the lock is held, or the port is Some, that means it was initialized already
        if let Some(mut guard) = self.port.try_lock() {
            let Some(port) = guard.as_mut() else { return };
            port.drain();
        }

        // We construct a new TTY instance on the fly since the panic could have been triggered
        // from within a log call itself, so we cannot trust the stored field
        //
        // SAFETY: the port was configured before, only sending bytes
        let Ok(uart) = (unsafe { Uart16550::new_port(COM1) }) else { return };
        let mut uart = Uart16550Tty::from_inner(uart);

        let panic = Style::new().red().bold().style("panic");
        let _ = match info.location() {
            Some(location) => {
                let (file, line) = (location.file(), location.line());
                writeln!(uart, "\n{panic} at {file}:{line}: {}", info.message())
            }
            None => writeln!(uart, "\n{panic}: {}", info.message()),
        };
    }

    fn write_record(out: &mut impl Write, record: &Record) -> fmt::Result {
        let target = record
            .target()
            .strip_prefix(concat!(env!("CARGO_CRATE_NAME"), "::"))
            .unwrap_or(record.target());

        writeln!(
            out,
            "{} {} {:<16} {}",
            Timestamp(TSC.now()).dimmed(),
            LevelTag(record.level()),
            target.dimmed(),
            record.args()
        )
    }
}

impl Log for SerialConsole {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }

        interrupts::without_interrupts(|| {
            let mut guard = self.port.lock();
            let Some(port) = guard.as_mut() else { return };

            if port.interrupts {
                port.queue(record);
            } else {
                let _ = Self::write_record(&mut port.uart, record);
            }
        });
    }

    /// Waits until every queued log is sent.
    fn flush(&self) {
        let send = || self.port.lock().as_mut().is_none_or(SerialPort::send_rest);
        while !interrupts::without_interrupts(send) {
            // Sleep roughly how long a send takes in case the interrupt hasn't reached us yet
            APIC_TIMER.sleep(FIFO_SEND_TIME);
        }
    }
}

impl SerialPort {
    /// Queues a log write, and sends it to COM1 if it is idle.
    ///
    /// If the queue is full, the log is dropped.
    fn queue(&mut self, record: &Record) {
        let len = self.queue.len();
        match self.try_queue(record) {
            Ok(()) => self.dropped = 0,
            Err(_) => {
                self.queue.truncate(len);
                self.dropped += 1;
            }
        }

        self.send();
    }

    /// Tries to log `record` if the queue isn't full, otherwise logs a warning about the dropped log.
    fn try_queue(&mut self, record: &Record) -> fmt::Result {
        self.queue_dropped()?; // no log if nothing was dropped
        SerialConsole::write_record(&mut QueueWriter(&mut self.queue), record)
    }

    /// Queues a warning about logs dropped since the queue was last full, if any.
    fn queue_dropped(&mut self) -> fmt::Result {
        match self.dropped {
            0 => Ok(()),
            dropped => SerialConsole::write_record(
                &mut QueueWriter(&mut self.queue),
                &Record::builder()
                    .level(Level::Warn)
                    .target(module_path!())
                    .args(format_args!("{dropped} logs were dropped, since the queue was full"))
                    .build(),
            ),
        }
    }

    /// Writes the queued bytes into COM1's FIFO. Only writes as many as possible until it is full.
    fn send(&mut self) {
        let (front, _) = self.queue.as_slices();
        let sent = self.uart.inner_mut().send_bytes(front);
        for _ in 0..sent {
            self.queue.pop_front();
        }
    }

    /// Writes all queued bytes, blocking if COM1 is busy.
    fn drain(&mut self) {
        while !self.send_rest() {
            core::hint::spin_loop();
        }
    }

    /// Sends as much as fits into COM1's FIFO, returning whether everything has been sent.
    fn send_rest(&mut self) -> bool {
        // Only report the dropped logs warning once there is enough room for it
        if self.queue.is_empty() && self.dropped > 0 && self.queue_dropped().is_ok() {
            self.dropped = 0;
        }

        self.send();
        self.queue.is_empty()
    }
}

/// Feeds COM1's FIFO from the queue when it is empty.
extern "x86-interrupt" fn com1_interrupt(_frame: InterruptStackFrame) {
    if let Some(port) = CONSOLE.port.lock().as_mut() {
        port.uart.inner_mut().isr(); // indicate interrupt was received
        port.send();
    }

    pic::end_of_interrupt();
}

/// Writes bytes into a queue rather than sending to hardware directly.
///
/// Translates newlines for terminals, similar to [`Uart16550Tty`].
struct QueueWriter<'a>(&'a mut Deque<u8, QUEUE_SIZE>);

impl Write for QueueWriter<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            if byte == b'\n' {
                // Return carriage after newlines
                self.0.push_back(b'\r').map_err(|_| fmt::Error)?;
            }

            self.0.push_back(byte).map_err(|_| fmt::Error)?;
        }

        Ok(())
    }
}

/// A [`TSC`] reading, shown at the start of each log record.
struct Timestamp(u64);

impl Display for Timestamp {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let ticks = self.0;

        // Ticks from the start time when uncalibriated, seconds if TSC is calibrated
        match TSC.to_duration(ticks) {
            None => write!(f, "[{ticks:>12}t]"),
            Some(time) => {
                let (secs, micros) = (time.as_secs(), time.subsec_micros());
                write!(f, "[{secs:>5}.{micros:06}s]")
            }
        }
    }
}

/// A log level, colored and padded, shown in each log record.
struct LevelTag(Level);

impl Display for LevelTag {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let style = match self.0 {
            Level::Error => Style::new().red(),
            Level::Warn => Style::new().yellow(),
            Level::Info => Style::new().green(),
            Level::Debug => Style::new().blue(),
            Level::Trace => Style::new().bright_black(),
        };

        write!(f, "{:>5}", self.0.style(style))
    }
}
