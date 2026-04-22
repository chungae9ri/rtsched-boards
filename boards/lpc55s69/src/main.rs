#![no_std]
#![no_main]

mod ctimer;
mod drivers;

use core::ffi::c_void;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicBool, Ordering};
use cortex_m::interrupt;
use cortex_m::peripheral::{SYST, syst::SystClkSource};
use cortex_m_rt::entry;
use panic_halt as _;

use hal::{drivers::pins::Level, prelude::*};
use lpc55_hal as hal;
use rtsc::Thread;
use rtsc::{AlignedStack, init_rq, spawn_main_thread, traverse_run_queue};

use rtt_target::{rprintln, rtt_init_print};
use crate::hal::drivers::Serial;
use crate::drivers::uart;

const STACK_LEN: usize = 1024;
const UART_BAUD: u32 = 115_200;
const SHELL_BUF_LEN: usize = 32;
const PS_SNAPSHOT_CAPACITY: usize = 8;
static mut BOOT_HAL: Option<hal::Peripherals> = None;
static UART_READY: AtomicBool = AtomicBool::new(false);

/// Main thread context and dedicated stack.
///
/// The reset handler enters `main` using MSP. We synthesize an initial thread
/// frame for the real application thread and start it through the same restore
/// path used by every other thread.
static mut MAIN_STACK: AlignedStack<STACK_LEN> = AlignedStack([0; STACK_LEN]);
static mut MAIN_THREAD: MaybeUninit<Thread> = MaybeUninit::uninit();

static mut SHELL_STACK: AlignedStack<STACK_LEN> = AlignedStack([0; STACK_LEN]);
static mut SHELL_THREAD: MaybeUninit<Thread> = MaybeUninit::uninit();
static mut FORKYI_STACK: AlignedStack<STACK_LEN> = AlignedStack([0; STACK_LEN]);
static mut FORKYI_THREAD: MaybeUninit<Thread> = MaybeUninit::uninit();

const SYS_CLK_FREQ: u32 = 12_000_000; // 12 MHz

extern "C" fn shell_task(_arg: *mut c_void) -> ! {
    let mut line_buf = [0u8; SHELL_BUF_LEN];
    let mut line_len = 0usize;

    while !UART_READY.load(Ordering::Acquire) {
        cortex_m::asm::nop();
    }

    uart_write_str("shell> ");

    loop {
        match uart::try_read() {
            Ok(Some(byte)) => {
                match byte {
                    b'\r' | b'\n' => {
                        uart::write_byte(b'\r');
                        uart::write_byte(b'\n');

                        if line_len != 0 {
                            handle_shell_command(&line_buf[..line_len]);
                            line_len = 0;
                        }

                        uart_write_str("shell> ");
                    }
                    0x08 | 0x7f => {
                        if line_len != 0 {
                            line_len -= 1;
                            uart_write_str("\x08 \x08");
                        }
                    }
                    _ if byte.is_ascii_graphic() || byte == b' ' => {
                        if line_len < line_buf.len() {
                            line_buf[line_len] = byte;
                            line_len += 1;
                            uart::write_byte(byte);
                        }
                    }
                    _ => {}
                }

                rprintln!("uart rx: 0x{:02x} '{}'", byte, ascii_debug(byte));
            }
            Ok(None) => {}
            Err(err) => {
                rprintln!("uart rx error: {:?}", err);
            }
        }
    }
}

fn handle_shell_command(line: &[u8]) {
    match line {
        b"ps" => dump_run_queue_uart(),
        b"" => {}
        _ => {
            uart_write_str("unknown command: ");
            uart_write_bytes(line);
            uart_write_str("\r\n");
        }
    }
}

fn uart_write_bytes(bytes: &[u8]) {
    for &byte in bytes {
        uart::write_byte(byte);
    }
}

fn uart_write_str(s: &str) {
    uart_write_bytes(s.as_bytes());
}

fn dump_run_queue_uart() {
    let mut snapshot = [ThreadSnapshot::default(); PS_SNAPSHOT_CAPACITY];
    let mut snapshot_len = 0usize;
    let mut truncated = false;

    interrupt::free(|_| unsafe {
        let mut cursor = None;

        while let Some(thread) = traverse_run_queue(cursor) {
            if snapshot_len == snapshot.len() {
                truncated = true;
                break;
            }

            let thread_ref = &*thread;
            snapshot[snapshot_len] = ThreadSnapshot {
                id: thread_ref.id,
                name: thread_ref.name,
                priority: thread_ref.sched_entity.priority,
                state: thread_ref.state,
                sched_tick_cnt: thread_ref.sched_entity.sched_tick_cnt(),
                vruntime: thread_ref.sched_entity.vruntime(),
            };
            snapshot_len += 1;
            cursor = Some(thread);
        }
    });

    uart_write_str("run queue:\r\n");
    for thread in &snapshot[..snapshot_len] {
        uart_write_str("  id=");
        uart_write_u32(thread.id);
        uart_write_str(" name=");
        uart_write_str(thread.name);
        uart_write_str(" prio=");
        uart_write_u32(thread.priority);
        uart_write_str(" state=");
        uart_write_str(thread_state_name(thread.state));
        uart_write_str(" ticks=");
        uart_write_u32(thread.sched_tick_cnt);
        uart_write_str(" vruntime=");
        uart_write_u64(thread.vruntime);
        uart_write_str("\r\n");
    }

    if truncated {
        uart_write_str("  ... truncated ...\r\n");
    }
}

fn uart_write_u32(mut value: u32) {
    let mut buf = [0u8; 10];
    let mut idx = buf.len();

    if value == 0 {
        uart::write_byte(b'0');
        return;
    }

    while value != 0 {
        idx -= 1;
        buf[idx] = b'0' + (value % 10) as u8;
        value /= 10;
    }

    uart_write_bytes(&buf[idx..]);
}

fn uart_write_u64(mut value: u64) {
    let mut buf = [0u8; 20];
    let mut idx = buf.len();

    if value == 0 {
        uart::write_byte(b'0');
        return;
    }

    while value != 0 {
        idx -= 1;
        buf[idx] = b'0' + (value % 10) as u8;
        value /= 10;
    }

    uart_write_bytes(&buf[idx..]);
}

extern "C" fn do_nothing_task(_arg: *mut c_void) -> ! {
    loop {
        cortex_m::asm::nop();
    }
}

#[entry]
fn main() -> ! {
    let mut hal = hal::new();

    unsafe {
        // Configure SysTick before handing the HAL instance to the first thread.
        // This should generate the first SysTick interrupt after all threads forked
        // and ready to be scheduled (context switch out).
        // Systick frequency is 100Hz
        set_systick(&mut hal.SYST, 10);
        BOOT_HAL = Some(hal);
        init_rq();

        rtsc::forkyi(
            core::ptr::addr_of_mut!(MAIN_THREAD).cast::<Thread>(),
            core::ptr::addr_of_mut!(MAIN_STACK)
                .cast::<AlignedStack<STACK_LEN>>()
                .cast::<u32>()
                .add(STACK_LEN),
            runtime_main,
            core::ptr::null_mut(),
            "idle",
            4,
        );
        rtsc::forkyi(
            core::ptr::addr_of_mut!(SHELL_THREAD).cast::<Thread>(),
            core::ptr::addr_of_mut!(SHELL_STACK)
                .cast::<AlignedStack<STACK_LEN>>()
                .cast::<u32>()
                .add(STACK_LEN),
            shell_task,
            core::ptr::null_mut(),
            "shell",
            1,
        );
        rtsc::forkyi(
            core::ptr::addr_of_mut!(FORKYI_THREAD).cast::<Thread>(),
            core::ptr::addr_of_mut!(FORKYI_STACK)
                .cast::<AlignedStack<STACK_LEN>>()
                .cast::<u32>()
                .add(STACK_LEN),
            do_nothing_task,
            core::ptr::null_mut(),
            "do_nothing_1",
            8,
        );
        spawn_main_thread(core::ptr::addr_of_mut!(MAIN_THREAD).cast::<Thread>())
    }
}

extern "C" fn runtime_main(_arg: *mut c_void) -> ! {
    rtt_init_print!();

    let mut hal = unsafe {
        let slot = core::ptr::addr_of_mut!(BOOT_HAL);
        let hal = slot.read().unwrap();
        slot.write(None);
        hal
    };

    let clocks = hal::ClockRequirements::default()
        .system_frequency(12.MHz())
        .configure(&mut hal.anactrl, &mut hal.pmc, &mut hal.syscon)
        .unwrap();

    let mut gpio = hal.gpio.enabled(&mut hal.syscon);
    let mut iocon = hal.iocon.enabled(&mut hal.syscon);
    let pins = hal::Pins::take().unwrap();
    let flexcomm_token = clocks.support_flexcomm_token().unwrap();

    let mut red = pins
        .pio1_6
        .into_gpio_pin(&mut iocon, &mut gpio)
        .into_output(Level::High);
    let mut red_high = true;

    let usart = hal.flexcomm.0.enabled_as_usart(&mut hal.syscon, &flexcomm_token);
    let tx = pins.pio0_30.into_usart0_tx_pin(&mut iocon);
    let rx = pins.pio0_29.into_usart0_rx_pin(&mut iocon);
    let config = hal::drivers::serial::config::Config::default().speed(UART_BAUD.Hz());
    let _serial = Serial::new(usart, (tx, rx), config);

    rprintln!("uart ready on usart0 at {} baud", UART_BAUD);
    UART_READY.store(true, Ordering::Release);

    ctimer::configure(
        hal.ctimer.0,
        &mut hal.syscon,
        clocks.support_1mhz_fro_token().unwrap(),
    );

    loop {
        if ctimer::take_tick() {
            if red_high {
                red.set_low().ok();
                rprintln!("set red low");
            } else {
                red.set_high().ok();
                rprintln!("set red high");
            }
            red_high = !red_high;
        }
    }
}

fn ascii_debug(byte: u8) -> char {
    if byte.is_ascii_graphic() || byte == b' ' {
        byte as char
    } else {
        '.'
    }
}

fn thread_state_name(state: rtsc::ThreadState) -> &'static str {
    match state {
        rtsc::ThreadState::Ready => "Ready",
        rtsc::ThreadState::Running => "Running",
        rtsc::ThreadState::Blocked => "Blocked",
        rtsc::ThreadState::Suspended => "Suspended",
    }
}

#[derive(Clone, Copy)]
struct ThreadSnapshot {
    id: u32,
    name: &'static str,
    priority: u32,
    state: rtsc::ThreadState,
    sched_tick_cnt: u32,
    vruntime: u64,
}

impl Default for ThreadSnapshot {
    fn default() -> Self {
        Self {
            id: 0,
            name: "",
            priority: 0,
            state: rtsc::ThreadState::Suspended,
            sched_tick_cnt: 0,
            vruntime: 0,
        }
    }
}

pub fn set_systick(syst: &mut SYST, _dur_msec: u32) {
    let ticks_per_ms = SYS_CLK_FREQ / 1000;
    let reload = _dur_msec
        .checked_mul(ticks_per_ms)
        .and_then(|v| v.checked_sub(1))
        .unwrap();

    assert!(reload <= 0x00FF_FFFF);

    syst.set_clock_source(SystClkSource::Core);
    syst.set_reload(reload);
    syst.clear_current();
    syst.enable_interrupt();
    syst.enable_counter();
}
