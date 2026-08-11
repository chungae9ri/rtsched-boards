use core::sync::atomic::{AtomicU8, Ordering};

use rtt_target::rprint;

use crate::drivers::uart;

use rtsched::Mutex;

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum Stdout {
    Uart = 0,
    Rtt = 1,
}

static STDIO_MTX: Mutex<u32> = Mutex::new(0);

static STDOUT: AtomicU8 = AtomicU8::new(Stdout::Uart as u8);

pub fn set_stdout(stdout: Stdout) {
    STDOUT.store(stdout as u8, Ordering::Release);
}

pub fn set_stdout_uart() {
    set_stdout(Stdout::Uart);
}

pub fn set_stdout_rtt() {
    set_stdout(Stdout::Rtt);
}

pub fn stdout() -> Stdout {
    match STDOUT.load(Ordering::Acquire) {
        value if value == Stdout::Rtt as u8 => Stdout::Rtt,
        _ => Stdout::Uart,
    }
}

pub fn board_printf(message: &str) {
    let Ok(_guard) = STDIO_MTX.lock() else {
        return;
    };

    match stdout() {
        Stdout::Uart if uart_ready() => uart_write_str(message),
        Stdout::Uart => rprint!("{}", message),
        Stdout::Rtt => rprint!("{}", message),
    }
}

fn uart_ready() -> bool {
    crate::UART_READY.load(Ordering::Acquire)
}

fn uart_write_str(message: &str) {
    for &byte in message.as_bytes() {
        uart::write_byte(byte);
    }
}
