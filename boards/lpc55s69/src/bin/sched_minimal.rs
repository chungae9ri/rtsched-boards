#![no_std]
#![no_main]

use core::ffi::c_void;
use core::mem::MaybeUninit;
use core::panic::PanicInfo;
#[cfg(feature = "board-rt")]
use core::sync::atomic::AtomicBool;
use core::sync::atomic::{AtomicU32, Ordering};

use cortex_m::peripheral::{SYST, syst::SystClkSource};
use cortex_m_rt::{entry, exception};
use rtt_target::rtt_init_print;
#[path = "../board_printf.rs"]
mod board_printf;
#[cfg(feature = "board-rt")]
#[allow(dead_code)]
#[path = "../drivers/mod.rs"]
mod drivers;

#[cfg(feature = "board-rt")]
use lpc55_pac as _;

use hal::{drivers::Serial, prelude::*};
use lpc55_hal as hal;

pub const STACK_WORDS: usize = 512;
pub const SYS_CLK_HZ: u32 = 12_000_000;
pub const TICKS_PER_MS: u32 = SYS_CLK_HZ / 1000;
const UART_BAUD: u32 = 115_200;

const FAST_PERIOD_TICKS: u32 = 1 * TICKS_PER_MS;
const FAST_DEADLINE_TICKS: u32 = 1 * TICKS_PER_MS;
const FAST_BUDGET_TICKS: u32 = 1 * TICKS_PER_MS;

pub const CFS_PERIOD_TICKS: u32 = 30 * TICKS_PER_MS;
pub const CFS_DEADLINE_TICKS: u32 = 10 * TICKS_PER_MS;

static mut IDLE_STACK: rtsched::AlignedStack<{ STACK_WORDS }> =
    rtsched::AlignedStack([0; STACK_WORDS]);
static mut IDLE_THREAD: MaybeUninit<rtsched::CfsThread> = MaybeUninit::uninit();

static mut BACKGROUND_STACK: rtsched::AlignedStack<{ STACK_WORDS }> =
    rtsched::AlignedStack([0; STACK_WORDS]);
static mut BACKGROUND_THREAD: MaybeUninit<rtsched::CfsThread> = MaybeUninit::uninit();

static mut FAST_RT_STACK: rtsched::AlignedStack<{ STACK_WORDS }> =
    rtsched::AlignedStack([0; STACK_WORDS]);
static mut FAST_RT_THREAD: MaybeUninit<rtsched::RtThread> = MaybeUninit::uninit();
static mut FAST_RT_TIMER: rtsched::RtKTimer = rtsched::RtKTimer::new_with_timing(
    rtsched::RtTiming::new(FAST_PERIOD_TICKS, FAST_DEADLINE_TICKS, FAST_BUDGET_TICKS),
    core::ptr::null_mut(),
    "fast_rt",
);

static FAST_JOBS: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "sched-minimal-timing")]
static LAST_TIMING_SAMPLES: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "board-rt")]
pub(crate) static UART_READY: AtomicBool = AtomicBool::new(false);

pub fn idle_forever() -> ! {
    loop {
        cortex_m::asm::wfi();
    }
}

pub extern "C" fn cpu_idle(_arg: *mut c_void) -> ! {
    idle_forever();
}

pub fn configure_systick(syst: &mut SYST) {
    let Some(reload) = rtsched::next_ktimer_reload() else {
        idle_forever();
    };

    syst.set_clock_source(SystClkSource::Core);
    syst.set_reload(reload);
    syst.clear_current();
    syst.enable_interrupt();
    syst.enable_counter();
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    idle_forever();
}

#[entry]
fn main() -> ! {
    unsafe {
        rtt_init_print!();
        rtsched::set_print_fn(board_printf::board_printf);
        board_printf::set_stdout_uart();

        rtsched::update_sys_clk_freq(SYS_CLK_HZ);
        rtsched::init_ktimer_queue();
        rtsched::init_cfs(CFS_PERIOD_TICKS, CFS_DEADLINE_TICKS);

        let idle = rtsched::CfsThreadBuilder::new("cpu_idle", cpu_idle, 16).spawn(
            core::ptr::addr_of_mut!(IDLE_THREAD),
            core::ptr::addr_of_mut!(IDLE_STACK),
        );
        rtsched::CfsThreadBuilder::new("background", background_work, 4).spawn(
            core::ptr::addr_of_mut!(BACKGROUND_THREAD),
            core::ptr::addr_of_mut!(BACKGROUND_STACK),
        );
        rtsched::RtThreadBuilder::new(
            "fast_rt",
            fast_rt_job,
            core::ptr::addr_of_mut!(FAST_RT_TIMER),
        )
        .spawn(
            core::ptr::addr_of_mut!(FAST_RT_THREAD),
            core::ptr::addr_of_mut!(FAST_RT_STACK),
        );

        let mut syst = init_board_hardware();
        #[cfg(feature = "sched-minimal-timing")]
        rtsched::reset_sched_tick_to_pendsv_timing();

        rtsched::register_idle_thread(idle);
        configure_systick(&mut syst);

        rtsched::spawn_main_thread(idle)
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

    let mut iocon = hal.iocon.enabled(&mut hal.syscon);
    let pins = match hal::Pins::take() {
        Some(pins) => pins,
        None => fatal("failed to take board pins\r\n"),
    };
    let flexcomm_token = match clocks.support_flexcomm_token() {
        Some(token) => token,
        None => fatal("missing flexcomm clock token\r\n"),
    };

    let usart = hal
        .flexcomm
        .0
        .enabled_as_usart(&mut hal.syscon, &flexcomm_token);
    let tx = pins.pio0_30.into_usart0_tx_pin(&mut iocon);
    let rx = pins.pio0_29.into_usart0_rx_pin(&mut iocon);
    let config = hal::drivers::serial::config::Config::default().speed(UART_BAUD.Hz());
    let _serial = Serial::new(usart, (tx, rx), config);

    UART_READY.store(true, Ordering::Release);
    board_printf::board_printf("sched_minimal uart ready on usart0 at ");
    board_print_u32(UART_BAUD);
    board_printf::board_printf(" baud\r\n");

    if !rtsched::init_dwt_cycle_counter(&mut hal.DCB, &mut hal.DWT) {
        board_printf::board_printf("failed to initialize DWT cycle counter\r\n");
    }

    hal.SYST
}

fn fatal(message: &str) -> ! {
    board_printf::board_printf(message);
    idle_forever();
}

extern "C" fn background_work(_arg: *mut c_void) -> ! {
    loop {
        #[cfg(feature = "sched-minimal-timing")]
        print_sched_isr_timing();
        rtsched::msleepyi(100);
    }
}

#[cfg(feature = "sched-minimal-timing")]
fn print_sched_isr_timing() {
    let timing = rtsched::sched_tick_to_pendsv_timing();
    if timing.samples == 0 {
        return;
    }

    let previous = LAST_TIMING_SAMPLES.swap(timing.samples, Ordering::Relaxed);
    if previous == timing.samples {
        return;
    }

    board_printf::board_printf("systick->pendsv ticks last=");
    board_print_u32(timing.last_ticks);
    board_printf::board_printf(" max=");
    board_print_u32(timing.max_ticks);
    board_printf::board_printf(" samples=");
    board_print_u32(timing.samples);
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

extern "C" fn fast_rt_job(_arg: *mut c_void) -> ! {
    loop {
        rtsched::set_rt_thread_start_time(0);
        FAST_JOBS.fetch_add(1, Ordering::Relaxed);
        rtsched::yieldyi();
    }
}

#[exception]
fn SysTick() {
    #[cfg(feature = "sched-minimal-timing")]
    rtsched::mark_sched_tick_to_pendsv_start();
    rtsched::handle_sched_tick();
}
