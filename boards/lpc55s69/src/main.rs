#![no_std]
#![no_main]

mod board_printf;
mod ctimer;
mod drivers;
mod shell;

use core::ffi::c_void;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicU32, AtomicBool, Ordering};
use cortex_m::peripheral::{SYST, syst::SystClkSource};
use cortex_m_rt::{entry, exception};
use panic_halt as _;

use hal::{drivers::pins::Level, prelude::*};
use lpc55_hal as hal;

use crate::hal::drivers::Serial;
use rtt_target::rtt_init_print;

const STACK_LEN: usize = 1024;
const UART_BAUD: u32 = 115_200;
const CFS_EXEC_MS: u32 = 10;
const CFS_PERIOD_MS: u32 = 30;
type RedLed = hal::drivers::pins::Pin<
    hal::drivers::pins::Pio1_6,
    hal::typestates::pin::state::Gpio<hal::drivers::pins::direction::Output>,
>;
pub(crate) static UART_READY: AtomicBool = AtomicBool::new(false);

/// Idle thread context and dedicated stack.
///
/// The reset handler enters `main` using MSP. We synthesize an initial thread
/// frame for the CPU idle thread and start it through the same restore
/// path used by every other thread.
static mut MAIN_STACK: rtsched::AlignedStack<STACK_LEN> = rtsched::AlignedStack([0; STACK_LEN]);
static mut MAIN_THREAD: MaybeUninit<rtsched::CfsThread> = MaybeUninit::uninit();

static mut SHELL_STACK: rtsched::AlignedStack<STACK_LEN> = rtsched::AlignedStack([0; STACK_LEN]);
static mut SHELL_THREAD: MaybeUninit<rtsched::CfsThread> = MaybeUninit::uninit();
static mut LED_BLINK_STACK: rtsched::AlignedStack<STACK_LEN> =
    rtsched::AlignedStack([0; STACK_LEN]);
static mut LED_BLINK_THREAD: MaybeUninit<rtsched::CfsThread> = MaybeUninit::uninit();
static mut RED_LED: MaybeUninit<RedLed> = MaybeUninit::uninit();

const BOARD_SYS_CLK_FREQ: u32 = 12_000_000; // 12 MHz
const TICKS_PER_MS: u32 = BOARD_SYS_CLK_FREQ / 1000;
const CFS_EXEC_TICKS: u32 = CFS_EXEC_MS * TICKS_PER_MS;
const CFS_PERIOD_TICKS: u32 = CFS_PERIOD_MS * TICKS_PER_MS;

static mut RT_THREAD1_STACK: rtsched::AlignedStack<STACK_LEN> =
    rtsched::AlignedStack([0; STACK_LEN]);
static mut RT_THREAD1: MaybeUninit<rtsched::RtThread> = MaybeUninit::uninit();
const RT_THREAD1_PERIOD_MS: u32 = 100;
const RT_THREAD1_PERIOD_TICKS: u32 = RT_THREAD1_PERIOD_MS * TICKS_PER_MS;
static mut RT_THREAD1_TIMER_ENTITY: rtsched::RtKTimer =
    rtsched::RtKTimer::new(RT_THREAD1_PERIOD_TICKS, core::ptr::null_mut(), "rt_thread1");

static mut RT_THREAD2_STACK: rtsched::AlignedStack<STACK_LEN> =
    rtsched::AlignedStack([0; STACK_LEN]);
static mut RT_THREAD2: MaybeUninit<rtsched::RtThread> = MaybeUninit::uninit();
const RT_THREAD2_PERIOD_MS: u32 = 120;
const RT_THREAD2_PERIOD_TICKS: u32 = RT_THREAD2_PERIOD_MS * TICKS_PER_MS;
static mut RT_THREAD2_TIMER_ENTITY: rtsched::RtKTimer =
    rtsched::RtKTimer::new(RT_THREAD2_PERIOD_TICKS, core::ptr::null_mut(), "rt_thread2");

static mut RT_THREAD3_STACK: rtsched::AlignedStack<STACK_LEN> =
    rtsched::AlignedStack([0; STACK_LEN]);
static mut RT_THREAD3: MaybeUninit<rtsched::RtThread> = MaybeUninit::uninit();
const RT_THREAD3_PERIOD_MS: u32 = 120;
const RT_THREAD3_PERIOD_TICKS: u32 = RT_THREAD3_PERIOD_MS * TICKS_PER_MS;
static mut RT_THREAD3_TIMER_ENTITY: rtsched::RtKTimer =
    rtsched::RtKTimer::new(RT_THREAD3_PERIOD_TICKS, core::ptr::null_mut(), "rt_thread3");

static SEMA: rtsched::CountingSemaphore = rtsched::CountingSemaphore::empty(3);
static PRODUCED_TOKENS: AtomicU32 = AtomicU32::new(0);
static CONSUMED_TOKEN_1: AtomicU32 = AtomicU32::new(0);
static CONSUMED_TOKEN_2: AtomicU32 = AtomicU32::new(0);
static TOKEN_OVERFLOWS: AtomicU32 = AtomicU32::new(0);
static TAKE_ERRORS: AtomicU32 = AtomicU32::new(0);

extern "C" fn rt_thread1_runner(_arg: *mut c_void) -> ! {
    loop {
        rtsched::set_rt_thread_start_time(0);
        for i in 0..5 {
            board_print_thread_iteration("rt_thread1", i + 1);
            rtsched::msleepyi(10);
            for _ in 0..3 {
                if SEMA.give().is_ok() {
                    PRODUCED_TOKENS.fetch_add(1, Ordering::Relaxed);
                } else {
                    TOKEN_OVERFLOWS.fetch_add(1, Ordering::Relaxed);
                }
            }
            board_printf::board_printf("Produced ");
            board_print_u32(PRODUCED_TOKENS.load(Ordering::Relaxed));
            board_printf::board_printf(" tokens\r\n");
        }
        rtsched::yieldyi();
    }
}

extern "C" fn rt_thread2_runner(_arg: *mut c_void) -> ! {
    loop {
        rtsched::set_rt_thread_start_time(0);
        for i in 0..5 {
            board_print_thread_iteration("rt_thread2", i + 1);
            rtsched::msleepyi(10);
            if SEMA.take().is_ok() {
                CONSUMED_TOKEN_1.fetch_add(1, Ordering::Relaxed);
            } else {
                TAKE_ERRORS.fetch_add(1, Ordering::Relaxed);
            }

            board_printf::board_printf("Consumed by rt_thread2:");
            board_print_u32(CONSUMED_TOKEN_1.load(Ordering::Relaxed));
            board_printf::board_printf(" tokens\r\n");
        }
        rtsched::yieldyi();
    }
}

extern "C" fn rt_thread3_runner(_arg: *mut c_void) -> ! {
    loop {
        rtsched::set_rt_thread_start_time(0);
        for i in 0..5 {
            board_print_thread_iteration("rt_thread3", i + 1);
            rtsched::msleepyi(10);
            if SEMA.take().is_ok() {
                CONSUMED_TOKEN_2.fetch_add(1, Ordering::Relaxed);
            } else {
                TAKE_ERRORS.fetch_add(1, Ordering::Relaxed);
            }

            board_printf::board_printf("Consumed by rt_thread3:");
            board_print_u32(CONSUMED_TOKEN_2.load(Ordering::Relaxed));
            board_printf::board_printf(" tokens\r\n");
        }
        rtsched::yieldyi();
    }
}

extern "C" fn led_blink_task(_arg: *mut c_void) -> ! {
    let red = unsafe { &mut *core::ptr::addr_of_mut!(RED_LED).cast::<RedLed>() };
    let mut red_high = true;

    loop {
        if red_high {
            red.set_low().ok();
            board_printf::board_printf("set red low\r\n");
        } else {
            red.set_high().ok();
            board_printf::board_printf("set red high\r\n");
        }
        red_high = !red_high;
        rtsched::msleepyi(1000);
    }
}

#[entry]
fn main() -> ! {
    unsafe {
        rtt_init_print!();
        rtsched::set_print_fn(board_printf::board_printf);

        rtsched::update_sys_clk_freq(BOARD_SYS_CLK_FREQ);
        rtsched::init_ktimer_queue();
        rtsched::init_cfs(CFS_PERIOD_TICKS, CFS_EXEC_TICKS);

        let main_thread = rtsched::CfsThreadBuilder::new("cpu_idle", runtime_main, 16).spawn(
            core::ptr::addr_of_mut!(MAIN_THREAD),
            core::ptr::addr_of_mut!(MAIN_STACK),
        );
        rtsched::CfsThreadBuilder::new("shell", shell::shell_task, 1).spawn(
            core::ptr::addr_of_mut!(SHELL_THREAD),
            core::ptr::addr_of_mut!(SHELL_STACK),
        );
        rtsched::CfsThreadBuilder::new("led_blink", led_blink_task, 4).spawn(
            core::ptr::addr_of_mut!(LED_BLINK_THREAD),
            core::ptr::addr_of_mut!(LED_BLINK_STACK),
        );

        rtsched::RtThreadBuilder::new(
            "rt_thread1",
            rt_thread1_runner,
            core::ptr::addr_of_mut!(RT_THREAD1_TIMER_ENTITY),
        )
        .spawn(
            core::ptr::addr_of_mut!(RT_THREAD1),
            core::ptr::addr_of_mut!(RT_THREAD1_STACK),
        );

        rtsched::RtThreadBuilder::new(
            "rt_thread2",
            rt_thread2_runner,
            core::ptr::addr_of_mut!(RT_THREAD2_TIMER_ENTITY),
        )
        .spawn(
            core::ptr::addr_of_mut!(RT_THREAD2),
            core::ptr::addr_of_mut!(RT_THREAD2_STACK),
        );

        rtsched::RtThreadBuilder::new(
            "rt_thread3",
            rt_thread3_runner,
            core::ptr::addr_of_mut!(RT_THREAD3_TIMER_ENTITY),
        )
        .spawn(
            core::ptr::addr_of_mut!(RT_THREAD3),
            core::ptr::addr_of_mut!(RT_THREAD3_STACK),
        );

        let mut syst = init_board_hardware();

        rtsched::register_idle_thread(main_thread);
        set_systick(&mut syst);
        rtsched::spawn_main_thread(main_thread)
    }
}

fn init_board_hardware() -> SYST {
    let mut hal = hal::new();

    let clocks = match hal::ClockRequirements::default()
        .system_frequency(12.MHz())
        .configure(&mut hal.anactrl, &mut hal.pmc, &mut hal.syscon)
    {
        Ok(clocks) => clocks,
        Err(_) => fatal("failed to configure clocks\r\n"),
    };
    if !rtsched::init_dwt_cycle_counter(&mut hal.DCB, &mut hal.DWT) {
        board_printf::board_printf("failed to initialize DWT cycle counter\r\n");
    }

    let mut gpio = hal.gpio.enabled(&mut hal.syscon);
    let mut iocon = hal.iocon.enabled(&mut hal.syscon);
    let pins = match hal::Pins::take() {
        Some(pins) => pins,
        None => fatal("failed to take board pins\r\n"),
    };
    let flexcomm_token = match clocks.support_flexcomm_token() {
        Some(token) => token,
        None => fatal("missing flexcomm clock token\r\n"),
    };

    let red = pins
        .pio1_6
        .into_gpio_pin(&mut iocon, &mut gpio)
        .into_output(Level::High);
    unsafe {
        core::ptr::addr_of_mut!(RED_LED).write(MaybeUninit::new(red));
    }

    let usart = hal
        .flexcomm
        .0
        .enabled_as_usart(&mut hal.syscon, &flexcomm_token);
    let tx = pins.pio0_30.into_usart0_tx_pin(&mut iocon);
    let rx = pins.pio0_29.into_usart0_rx_pin(&mut iocon);
    let config = hal::drivers::serial::config::Config::default().speed(UART_BAUD.Hz());
    let _serial = Serial::new(usart, (tx, rx), config);

    UART_READY.store(true, Ordering::Release);
    board_printf::board_printf("uart ready on usart0 at ");
    board_print_u32(UART_BAUD);
    board_printf::board_printf(" baud\r\n");

    ctimer::configure(
        hal.ctimer.0,
        &mut hal.syscon,
        match clocks.support_1mhz_fro_token() {
            Some(token) => token,
            None => fatal("missing 1mhz fro clock token\r\n"),
        },
    );

    hal.SYST
}

extern "C" fn runtime_main(_arg: *mut c_void) -> ! {
    loop {
        // The CPU idle thread runs when no other thread is runnable.
        // This is the board hook for low-power idle behavior. It currently
        // uses the lightweight Cortex-M WFI instruction; platform-specific
        // power management can be added here to reduce power consumption further.
        board_printf::board_printf("entering cpu idle...\r\n");
        cortex_m::asm::wfi();
    }
}

pub fn set_systick(syst: &mut SYST) {
    let reload = match rtsched::next_ktimer_reload() {
        Some(reload) => reload,
        None => fatal("missing next ktimer reload\r\n"),
    };

    syst.set_clock_source(SystClkSource::Core);
    syst.set_reload(reload);
    syst.clear_current();
    syst.enable_interrupt();
    syst.enable_counter();
}

fn fatal(message: &str) -> ! {
    board_printf::board_printf(message);
    loop {
        cortex_m::asm::wfi();
    }
}

#[exception]
fn SysTick() {
    rtsched::handle_sched_tick();
}

fn board_print_thread_iteration(name: &str, iteration: u32) {
    board_printf::board_printf(name);
    board_printf::board_printf(" running at ");
    board_print_u32(iteration);
    board_printf::board_printf("\r\n");
}

fn board_print_u32(mut value: u32) {
    let mut buf = [0u8; 10];
    let mut idx = buf.len();

    if value == 0 {
        board_printf::board_printf("0");
        return;
    }

    while value != 0 {
        idx -= 1;
        buf[idx] = b'0' + (value % 10) as u8;
        value /= 10;
    }

    if let Ok(message) = core::str::from_utf8(&buf[idx..]) {
        board_printf::board_printf(message);
    }
}
