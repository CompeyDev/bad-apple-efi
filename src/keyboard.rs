use alloc::boxed::Box;

use heapless::Deque;
use pc_keyboard::{KeyCode, KeyState, ScancodeSet, ScancodeSet1, ScancodeSet2};
use spin::Mutex;
use x86_64::instructions::interrupts;
use x86_64::instructions::port::{Port, PortReadOnly, PortWriteOnly};
use x86_64::structures::idt::InterruptStackFrame;

use crate::interrupts::{pic, X86Handler, IDT};

/// The global PS/2 keyboard instance.
pub static KEYBOARD: Keyboard = Keyboard { state: Mutex::new(None) };

const KEYBOARD_IRQ: u8 = 1;
const QUEUE_SIZE: usize = 16; // 16 key presses per frame isn't humanly possible

#[derive(Debug, Clone, Copy, thiserror::Error)]
enum KeyboardError {
    #[error("no PS/2 controller found")]
    NoController,
    #[error("the PS/2 controller isn't responding")]
    Timeout,
    #[error("the keyboard rejected command {command:#04X} with {response:#04X}")]
    Rejected { command: u8, response: u8 },
}

type Result<T = ()> = core::result::Result<T, KeyboardError>;

/// A [PS/2] keyboard using the [Intel 8042] controller, which sends its key presses to IRQ1.
///
/// Most laptops and many external keyboards are PS/2, but external USB keyboards are not
/// supported by this yet.
///
/// [PS/2]: https://en.wikipedia.org/wiki/PS/2_port
/// [Intel 8042]: https://wiki.osdev.org/I8042_PS/2_Controller
pub struct Keyboard {
    // SAFETY: IRQ1 handler also holds this, so interrupts must be disabled before touching it
    state: Mutex<Option<State>>,
}

struct State {
    controller: Controller,
    scancodes: Box<dyn ScancodeSet + Send>,
    keys: Deque<KeyCode, QUEUE_SIZE>,
}

impl Keyboard {
    /// Command to restore the defaults (use Set 2) and and stop scanning.
    const RESTORE_DEFAULTS: u8 = 0xF5;
    /// Command to start scanning, i.e. sending key presses.
    const ENABLE_SCANNING: u8 = 0xF4;

    /// Sets up the keyboard controller to raise IRQ1, if the system has one.
    ///
    /// ## Safety
    /// Must be called after boot services has been exited and the IDT has been initialized. While
    /// boot services are active, the firmware also drives the controller.
    pub fn init(&self) {
        let mut controller = Controller::new();

        // NOTE:
        // We never write the translate bit. Exactly whatever the firmware has configured
        // for us as the default, we tolerate. This is the same model that the Linux kernel
        // follows.
        match Self::configure(&mut controller) {
            Ok(translated) => {
                let scancodes: Box<dyn ScancodeSet + Send> = if translated {
                    // The keyboard is configured to Set 2, but the controller translates to Set 2
                    Box::new(ScancodeSet1::new())
                } else {
                    // The keyboard sends Set 2, no translation
                    Box::new(ScancodeSet2::new())
                };

                IDT.register_handler(
                    pic::VECTOR_OFFSET + KEYBOARD_IRQ,
                    keyboard_interrupt as X86Handler,
                );

                interrupts::without_interrupts(|| {
                    *self.state.lock() = Some(State { controller, scancodes, keys: Deque::new() });
                    pic::unmask(KEYBOARD_IRQ);
                });

                let set = if translated { "set 1 (translated from set 2)" } else { "set 2" };
                log::debug!("PS/2 keyboard found, sending scancodes in {set}");
            }
            Err(err) => log::warn!("Keyboard input is unavailable: {err}"),
        }
    }

    /// Resets the keyboard to its defaults. In its default configuration, it always emits Set 2
    /// scancodes.
    ///
    /// Returns whether the scancodes are translated to Set 1 or returned as is.
    fn configure(controller: &mut Controller) -> Result<bool> {
        // Every bit should be set before the controller has started driving the bus
        if controller.status() == 0xFF {
            return Err(KeyboardError::NoController);
        }

        // We disable the AUX port, so that any mouse input does not block our keyboard inputs
        controller.disable_aux()?;
        controller.send_to_keyboard(Self::RESTORE_DEFAULTS)?;

        // Start scanning again
        let translated = controller.enable_keyboard_interrupt()?;
        controller.send_to_keyboard(Self::ENABLE_SCANNING)?;

        Ok(translated)
    }

    /// Takes the oldest key press which hasn't been handled yet.
    pub fn next_key(&self) -> Option<KeyCode> {
        interrupts::without_interrupts(|| self.state.lock().as_mut()?.keys.pop_front())
    }

    /// Halts until a key press is received. If one that is unhandled is already present, that is
    /// returned.
    ///
    /// If no keyboard is present, returns immediately.
    pub fn wait_for_key(&self) {
        // Check with interrupts disabled, so the key can't arrive between the check and halt
        interrupts::without_interrupts(|| {
            while self.state.lock().as_ref().is_some_and(|state| state.keys.is_empty()) {
                interrupts::enable_and_hlt();
                interrupts::disable();
            }
        });
    }
}

impl State {
    /// Receives a byte from the keyboard controller, then decodes and queues it as a key press.
    fn receive(&mut self) -> Option<()> {
        let byte = self.controller.read();
        let event = self.scancodes.advance_state(byte).ok().flatten();
        let code = event.filter(|event| event.state == KeyState::Down)?.code;

        if let Err(err) = self.keys.push_back(code) {
            log::warn!("Too many keys pressed at once, dropping {err:?}");
        }

        Some(())
    }
}

/// Queues the keyboard's key presses as their bytes arrive.
extern "x86-interrupt" fn keyboard_interrupt(_frame: InterruptStackFrame) {
    if let Some(state) = KEYBOARD.state.lock().as_mut() {
        state.receive();
    }

    pic::end_of_interrupt();
}

/// The Intel 8042 PS/2 controller, accessed through its data, status and command ports.
struct Controller {
    data: Port<u8>,
    status: PortReadOnly<u8>,
    command: PortWriteOnly<u8>,
}

impl Controller {
    /// Status: when there is a byte to read.
    const OUTPUT_FULL: u8 = 1 << 0;
    /// Status: the controller hasn't consumed the byte written to it yet.
    const INPUT_FULL: u8 = 1 << 1;

    /// Configuration: raise IRQ1 when a byte from the keyboard is ready.
    const KEYBOARD_INTERRUPT: u8 = 1 << 0;
    /// Configuration: raise IRQ12 when a byte from the mouse is ready.
    const AUX_INTERRUPT: u8 = 1 << 1;
    /// Configuration: translate the keyboard's scancodes into Set 1.
    const TRANSLATE: u8 = 1 << 6;

    /// Command: read the configuration byte.
    const READ_CONFIG: u8 = 0x20;
    /// Command: write the configuration byte with the next byte written.
    const WRITE_CONFIG: u8 = 0x60;
    /// Command: stop the mouse's port from sending bytes.
    const DISABLE_AUX: u8 = 0xA7;

    /// Keyboard response: the command was accepted.
    const ACK: u8 = 0xFA;

    /// Number of times to try getting the status, before failing. Each read takes a microsecond.
    const STATUS_TIMEOUT: usize = 100_000;

    const fn new() -> Self {
        Self {
            data: Port::new(0x60),
            status: PortReadOnly::new(0x64),
            command: PortWriteOnly::new(0x64),
        }
    }

    /// Disables the AUX port, discarding any existing bytes
    #[rustfmt::skip]
    fn disable_aux(&mut self) -> Result {
        self.send_command(Self::DISABLE_AUX)?; // mask the AUX port (mouse can't send any bytes)
        self.flush();                          // discard existing bytes from before
        Ok(())
    }

    /// Enables IRQ1 and disables IRQ12, returning whether scancodes are translated to Set 1.
    ///
    /// **NOTE**: This only disables the interrupt used by the AUX port, and does not stop the
    /// mouse from emitting its bytes. Call [`Controller::disable_aux`] first to mask the AUX
    /// port.
    fn enable_keyboard_interrupt(&mut self) -> Result<bool> {
        self.send_command(Self::READ_CONFIG)?;
        let old_config = self.read_response()?;

        let new_config = (old_config | Self::KEYBOARD_INTERRUPT) & !Self::AUX_INTERRUPT;
        self.send_command(Self::WRITE_CONFIG)?;
        self.write(new_config)?;

        Ok(new_config & Self::TRANSLATE != 0)
    }

    /// Sends a command to the keyboard itself, waiting for it to acknowledge it.
    fn send_to_keyboard(&mut self, command: u8) -> Result {
        self.write(command)?;
        match self.read_response()? {
            Self::ACK => Ok(()),
            response => Err(KeyboardError::Rejected { command, response }),
        }
    }

    fn status(&mut self) -> u8 {
        // SAFETY: the i8042 ports are fixed and exist, and this is simply a read
        unsafe { self.status.read() }
    }

    fn read(&mut self) -> u8 {
        // SAFETY: same as above
        unsafe { self.data.read() }
    }

    /// Sends a command (or an argument to the last command) to the keyboard.
    fn write(&mut self, byte: u8) -> Result {
        self.wait_for(Self::can_write)?;
        // SAFETY: same as above, the controller has taken the last byte written to it
        unsafe { self.data.write(byte) };
        Ok(())
    }

    /// Sends a command to the controller itself.
    fn send_command(&mut self, command: u8) -> Result {
        self.wait_for(Self::can_write)?;
        // SAFETY: same as above
        unsafe { self.command.write(command) };
        Ok(())
    }

    /// Reads the response to the last command, waiting if there are no responses. Times out after
    /// [`STATUS_TIMEOUT`] iterations.
    fn read_response(&mut self) -> Result<u8> {
        self.wait_for(Self::can_read)?;
        Ok(self.read())
    }

    /// Whether the controller has consumed the last byte written to it, so another can be written.
    fn can_write(status: u8) -> bool {
        status & Self::INPUT_FULL == 0
    }

    /// Whether there is a byte waiting to be read.
    fn can_read(status: u8) -> bool {
        status & Self::OUTPUT_FULL != 0
    }

    /// Discards every byte waiting to be read. Times out after [`STATUS_TIMEOUT`] iterations.
    fn flush(&mut self) {
        for _ in 0..Self::STATUS_TIMEOUT {
            if !Self::can_read(self.status()) {
                break;
            }

            self.read();
        }
    }

    /// Waits for the status to satisfy `ready` for [`STATUS_TIMEOUT`] iterations, otherwise timing out.
    fn wait_for(&mut self, ready: fn(u8) -> bool) -> Result {
        let is_ready = (0..Self::STATUS_TIMEOUT).any(|_| ready(self.status()));
        if is_ready {
            Ok(())
        } else {
            Err(KeyboardError::Timeout)
        }
    }
}
