use core::fmt::{self, Display, Formatter, Write};

use log::{Level, LevelFilter, Log, Metadata, Record};
use owo_colors::{OwoColorize, Style};
use spin::Mutex;
use uart_16550::backend::PioBackend;
use uart_16550::spec::{calc_baud_rate, CLK_FREQUENCY_HZ};
use uart_16550::{BaudRate, Config, Uart16550, Uart16550Tty};
use uefi::boot::{self, Tpl};

use crate::tsc::TSC;

const COM1: u16 = 0x3F8;
const DEFAULT_BAUD_RATE: BaudRate = BaudRate::Baud115200;

#[rustfmt::skip]
const DEFAULT_LEVEL: LevelFilter = if cfg!(debug_assertions) {
    LevelFilter::Debug
} else {
    LevelFilter::Info
};

/// The global serial console instance.
pub static CONSOLE: SerialConsole = SerialConsole::new();

pub struct SerialConsole {
    uart: Mutex<Option<Uart16550Tty<PioBackend>>>,
}

impl SerialConsole {
    const fn new() -> Self {
        Self { uart: Mutex::new(None) }
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
        *self.uart.lock() = present.then(|| Uart16550Tty::from_inner(uart));

        if log::set_logger(self).is_ok() {
            log::set_max_level(
                // Allow setting a build-time max log level. In release builds, this is capped
                // to info.
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

    /// Handles panics by bypassing the logging facade, since the logger could also be an origin
    /// of the panic.
    #[cfg(not(feature = "qemu"))]
    pub(crate) fn write_panic(&self, info: &core::panic::PanicInfo) {
        // If the lock is held, or is Some, that means UART was initialized previously
        if !self.uart.try_lock().is_none_or(|uart| uart.is_some()) {
            return;
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

        let mut guard = self.uart.lock();
        let Some(uart) = guard.as_mut() else { return };

        let _ = Self::write_record(uart, record);
    }

    fn flush(&self) {}
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
