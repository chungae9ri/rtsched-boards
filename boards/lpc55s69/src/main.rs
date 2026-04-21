#![no_std]
#![no_main]

mod ctimer;
mod drivers;

use core::ffi::c_void;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicBool, Ordering};
use cortex_m::peripheral::{SYST, syst::SystClkSource};
use cortex_m_rt::entry;
use panic_halt as _;

use hal::{drivers::pins::Level, prelude::*};
use lpc55_hal as hal;
use rtsc::Task;
use rtsc::{AlignedStack, init_rq, spawn_main_task};

use crate::drivers::uart;
use crate::hal::drivers::Serial;
use rtt_target::{rprintln, rtt_init_print};

const STACK_LEN: usize = 1024;
const UART_BAUD: u32 = 115_200;
static mut BOOT_HAL: Option<hal::Peripherals> = None;
static UART_READY: AtomicBool = AtomicBool::new(false);

/// Main thread context and dedicated stack.
///
/// The reset handler enters `main` using MSP. We synthesize an initial task
/// frame for the real application thread and start it through the same restore
/// path used by every other task.
static mut MAIN_STACK: AlignedStack<STACK_LEN> = AlignedStack([0; STACK_LEN]);
static mut MAIN_THREAD: MaybeUninit<Task> = MaybeUninit::uninit();

static mut SHELL_STACK: AlignedStack<STACK_LEN> = AlignedStack([0; STACK_LEN]);
static mut SHELL_THREAD: MaybeUninit<Task> = MaybeUninit::uninit();
static mut FORKYI_STACK: AlignedStack<STACK_LEN> = AlignedStack([0; STACK_LEN]);
static mut FORKYI_THREAD: MaybeUninit<Task> = MaybeUninit::uninit();

const SYS_CLK_FREQ: u32 = 12_000_000; // 12 MHz

extern "C" fn shell_task(_arg: *mut c_void) -> ! {
    while !UART_READY.load(Ordering::Acquire) {
        cortex_m::asm::nop();
    }

    for &byte in b"shell> " {
        uart::write_byte(byte);
    }

    loop {
        match uart::try_read() {
            Ok(Some(byte)) => {
                uart::write_byte(byte);

                if byte == b'\r' {
                    uart::write_byte(b'\n');
                    for &byte in b"shell> " {
                        uart::write_byte(byte);
                    }
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

extern "C" fn do_nothing_task(_arg: *mut c_void) -> ! {
    loop {
        cortex_m::asm::nop();
    }
}

#[entry]
fn main() -> ! {
    let mut hal = hal::new();

    unsafe {
        // Configure SysTick before handing the HAL instance to the first task.
        // This should generate the first SysTick interrupt after all threads forked
        // and ready to be scheduled (context switch out).
        // Systick frequency is 100Hz
        set_systick(&mut hal.SYST, 10);
        BOOT_HAL = Some(hal);
        init_rq();

        rtsc::forkyi(
            core::ptr::addr_of_mut!(MAIN_THREAD).cast::<Task>(),
            core::ptr::addr_of_mut!(MAIN_STACK)
                .cast::<AlignedStack<STACK_LEN>>()
                .cast::<u32>()
                .add(STACK_LEN),
            runtime_main,
            core::ptr::null_mut(),
            0,
            "idle",
            4,
        );
        rtsc::forkyi(
            core::ptr::addr_of_mut!(SHELL_THREAD).cast::<Task>(),
            core::ptr::addr_of_mut!(SHELL_STACK)
                .cast::<AlignedStack<STACK_LEN>>()
                .cast::<u32>()
                .add(STACK_LEN),
            shell_task,
            core::ptr::null_mut(),
            1,
            "shell",
            1,
        );
        rtsc::forkyi(
            core::ptr::addr_of_mut!(FORKYI_THREAD).cast::<Task>(),
            core::ptr::addr_of_mut!(FORKYI_STACK)
                .cast::<AlignedStack<STACK_LEN>>()
                .cast::<u32>()
                .add(STACK_LEN),
            do_nothing_task,
            core::ptr::null_mut(),
            1,
            "do_nothing_1",
            8,
        );
        spawn_main_task(core::ptr::addr_of_mut!(MAIN_THREAD).cast::<Task>())
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

    let usart = hal
        .flexcomm
        .0
        .enabled_as_usart(&mut hal.syscon, &flexcomm_token);
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
