#![no_std]
#![no_main]

mod board_printf;
mod ctimer;
mod drivers;
mod shell;

use core::ffi::c_void;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicBool, Ordering};
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
pub(crate) static UART_READY: AtomicBool = AtomicBool::new(false);

/// Main thread context and dedicated stack.
///
/// The reset handler enters `main` using MSP. We synthesize an initial thread
/// frame for the real application thread and start it through the same restore
/// path used by every other thread.
static mut MAIN_STACK: rtsched::AlignedStack<STACK_LEN> = rtsched::AlignedStack([0; STACK_LEN]);
static mut MAIN_THREAD: MaybeUninit<rtsched::CfsThread> = MaybeUninit::uninit();

static mut SHELL_STACK: rtsched::AlignedStack<STACK_LEN> = rtsched::AlignedStack([0; STACK_LEN]);
static mut SHELL_THREAD: MaybeUninit<rtsched::CfsThread> = MaybeUninit::uninit();
static mut DO_NOTHING_STACK: rtsched::AlignedStack<STACK_LEN> =
    rtsched::AlignedStack([0; STACK_LEN]);
static mut DO_NOTHING_THREAD: MaybeUninit<rtsched::CfsThread> = MaybeUninit::uninit();

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

extern "C" fn rt_thread1_runner(_arg: *mut c_void) -> ! {
    loop {
        rtsched::set_rt_thread_start_time(0);
        for i in 0..5 {
            board_print_thread_iteration("rt_thread1", i + 1);
            rtsched::msleepyi(10);
            for _ in 0..1000 {
                cortex_m::asm::nop();
            }
        }
        rtsched::yieldyi();
    }
}

extern "C" fn rt_thread2_runner(_arg: *mut c_void) -> ! {
    loop {
        rtsched::set_rt_thread_start_time(0);
        for i in 0..5 {
            board_print_thread_iteration("rt_thread2", i + 1);
            rtsched::msleepyi(6);
            for _ in 0..1000 {
                cortex_m::asm::nop();
            }
        }
        rtsched::yieldyi();
    }
}

extern "C" fn do_nothing_task(_arg: *mut c_void) -> ! {
    loop {
        for i in 0..10 {
            board_print_thread_iteration("do_nothing_task", i + 1);
            rtsched::msleepyi(1000);
        }
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

        let main_thread = rtsched::forkyi(
            core::ptr::addr_of_mut!(MAIN_THREAD).cast::<rtsched::CfsThread>(),
            core::ptr::addr_of_mut!(MAIN_STACK)
                .cast::<rtsched::AlignedStack<STACK_LEN>>()
                .cast::<u32>()
                .add(STACK_LEN),
            runtime_main,
            core::ptr::null_mut(),
            "idle",
            4,
        );
        rtsched::forkyi(
            core::ptr::addr_of_mut!(SHELL_THREAD).cast::<rtsched::CfsThread>(),
            core::ptr::addr_of_mut!(SHELL_STACK)
                .cast::<rtsched::AlignedStack<STACK_LEN>>()
                .cast::<u32>()
                .add(STACK_LEN),
            shell::shell_task,
            core::ptr::null_mut(),
            "shell",
            1,
        );
        rtsched::forkyi(
            core::ptr::addr_of_mut!(DO_NOTHING_THREAD).cast::<rtsched::CfsThread>(),
            core::ptr::addr_of_mut!(DO_NOTHING_STACK)
                .cast::<rtsched::AlignedStack<STACK_LEN>>()
                .cast::<u32>()
                .add(STACK_LEN),
            do_nothing_task,
            core::ptr::null_mut(),
            "do_nothing_1",
            8,
        );

        let rt_thread1 = rtsched::forkyi(
            core::ptr::addr_of_mut!(RT_THREAD1).cast::<rtsched::RtThread>(),
            core::ptr::addr_of_mut!(RT_THREAD1_STACK)
                .cast::<rtsched::AlignedStack<STACK_LEN>>()
                .cast::<u32>()
                .add(STACK_LEN),
            rt_thread1_runner,
            core::ptr::null_mut(),
            "rt_thread1",
            0, // Not used for RT thread
        );
        (*core::ptr::addr_of_mut!(RT_THREAD1_TIMER_ENTITY)).init_thread_ctx(rt_thread1);
        rtsched::enqueue_ktimer((*core::ptr::addr_of_mut!(RT_THREAD1_TIMER_ENTITY)).entity_mut());

        let rt_thread2 = rtsched::forkyi(
            core::ptr::addr_of_mut!(RT_THREAD2).cast::<rtsched::RtThread>(),
            core::ptr::addr_of_mut!(RT_THREAD2_STACK)
                .cast::<rtsched::AlignedStack<STACK_LEN>>()
                .cast::<u32>()
                .add(STACK_LEN),
            rt_thread2_runner,
            core::ptr::null_mut(),
            "rt_thread2",
            0, // Not used for RT thread
        );
        (*core::ptr::addr_of_mut!(RT_THREAD2_TIMER_ENTITY)).init_thread_ctx(rt_thread2);
        rtsched::enqueue_ktimer((*core::ptr::addr_of_mut!(RT_THREAD2_TIMER_ENTITY)).entity_mut());

        rtsched::spawn_main_thread(main_thread)
    }
}

extern "C" fn runtime_main(_arg: *mut c_void) -> ! {
    let mut hal = hal::new();

    let clocks = hal::ClockRequirements::default()
        .system_frequency(12.MHz())
        .configure(&mut hal.anactrl, &mut hal.pmc, &mut hal.syscon)
        .unwrap();
    if !rtsched::init_dwt_cycle_counter(&mut hal.DCB, &mut hal.DWT) {
        board_printf::board_printf("failed to initialize DWT cycle counter\r\n");
    }

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

    UART_READY.store(true, Ordering::Release);
    board_printf::board_printf("uart ready on usart0 at ");
    board_print_u32(UART_BAUD);
    board_printf::board_printf(" baud\r\n");

    ctimer::configure(
        hal.ctimer.0,
        &mut hal.syscon,
        clocks.support_1mhz_fro_token().unwrap(),
    );
    set_systick(&mut hal.SYST);

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

pub fn set_systick(syst: &mut SYST) {
    let reload = rtsched::next_ktimer_reload().unwrap();

    syst.set_clock_source(SystClkSource::Core);
    syst.set_reload(reload);
    syst.clear_current();
    syst.enable_interrupt();
    syst.enable_counter();
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
