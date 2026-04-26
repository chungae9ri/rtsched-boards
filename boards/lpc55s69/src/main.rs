#![no_std]
#![no_main]

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
use rtsc::{AlignedStack, CfsThread, RtThread, init_cfs, spawn_main_thread};

use crate::hal::drivers::Serial;
use rtt_target::{rprintln, rtt_init_print};

const STACK_LEN: usize = 1024;
const UART_BAUD: u32 = 115_200;
const CFS_PERIOD_MS: u32 = 10;
static mut BOOT_HAL: Option<hal::Peripherals> = None;
pub(crate) static UART_READY: AtomicBool = AtomicBool::new(false);

/// Main thread context and dedicated stack.
///
/// The reset handler enters `main` using MSP. We synthesize an initial thread
/// frame for the real application thread and start it through the same restore
/// path used by every other thread.
static mut MAIN_STACK: AlignedStack<STACK_LEN> = AlignedStack([0; STACK_LEN]);
static mut MAIN_THREAD: MaybeUninit<CfsThread> = MaybeUninit::uninit();

static mut SHELL_STACK: AlignedStack<STACK_LEN> = AlignedStack([0; STACK_LEN]);
static mut SHELL_THREAD: MaybeUninit<CfsThread> = MaybeUninit::uninit();
static mut DO_NOTHING_STACK: AlignedStack<STACK_LEN> = AlignedStack([0; STACK_LEN]);
static mut DO_NOTHING_THREAD: MaybeUninit<CfsThread> = MaybeUninit::uninit();

const SYS_CLK_FREQ: u32 = 12_000_000; // 12 MHz
const TICKS_PER_MS: u32 = SYS_CLK_FREQ / 1000;
const CFS_PERIOD_TICKS: u32 = CFS_PERIOD_MS * TICKS_PER_MS;

static mut RT_THREAD1_STACK: AlignedStack<STACK_LEN> = AlignedStack([0; STACK_LEN]);
static mut RT_THREAD1: MaybeUninit<RtThread> = MaybeUninit::uninit();
const RT_THREAD1_PERIOD_MS: u32 = 25;
const RT_THREAD1_PERIOD_TICKS: u32 = RT_THREAD1_PERIOD_MS * TICKS_PER_MS;
static mut RT_THREAD1_TIMER_ENTITY: rtsc::KTimerEntity = rtsc::KTimerEntity::new(
    RT_THREAD1_PERIOD_TICKS,
    RT_THREAD1_PERIOD_TICKS,
    rtsc::KTimerType::Rt,
    core::ptr::null_mut(),
);

static mut RT_THREAD2_STACK: AlignedStack<STACK_LEN> = AlignedStack([0; STACK_LEN]);
static mut RT_THREAD2: MaybeUninit<RtThread> = MaybeUninit::uninit();
const RT_THREAD2_PERIOD_MS: u32 = 51;
const RT_THREAD2_PERIOD_TICKS: u32 = RT_THREAD2_PERIOD_MS * TICKS_PER_MS;
static mut RT_THREAD2_TIMER_ENTITY: rtsc::KTimerEntity = rtsc::KTimerEntity::new(
    RT_THREAD2_PERIOD_TICKS,
    RT_THREAD2_PERIOD_TICKS,
    rtsc::KTimerType::Rt,
    core::ptr::null_mut(),
);

extern "C" fn rt_thread1_runner(_arg: *mut c_void) -> ! {
    loop {
        for i in 0..10 {
            rprintln!("rt_thread1 running at {}", i + 1);
        }
        rtsc::yieldyi();
    }
}

extern "C" fn rt_thread2_runner(_arg: *mut c_void) -> ! {
    loop {
        for i in 0..8 {
            rprintln!("rt_thread2 running at {}", i + 1);
        }
        rtsc::yieldyi();
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
        // Configure SysTick before handing the HAL instance to the first thread.
        // Seed the ktimer queue first so SysTick can be programmed from the
        // earliest queued timer rather than a hard-coded reload value.
        rtsc::init_ktimer_queue();
        init_cfs(CFS_PERIOD_TICKS);
        set_systick(&mut hal.SYST);
        BOOT_HAL = Some(hal);

        let main_thread = rtsc::forkyi(
            core::ptr::addr_of_mut!(MAIN_THREAD).cast::<CfsThread>(),
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
            core::ptr::addr_of_mut!(SHELL_THREAD).cast::<CfsThread>(),
            core::ptr::addr_of_mut!(SHELL_STACK)
                .cast::<AlignedStack<STACK_LEN>>()
                .cast::<u32>()
                .add(STACK_LEN),
            shell::shell_task,
            core::ptr::null_mut(),
            "shell",
            1,
        );
        rtsc::forkyi(
            core::ptr::addr_of_mut!(DO_NOTHING_THREAD).cast::<CfsThread>(),
            core::ptr::addr_of_mut!(DO_NOTHING_STACK)
                .cast::<AlignedStack<STACK_LEN>>()
                .cast::<u32>()
                .add(STACK_LEN),
            do_nothing_task,
            core::ptr::null_mut(),
            "do_nothing_1",
            8,
        );
        let rt_thread1 = rtsc::forkyi(
            core::ptr::addr_of_mut!(RT_THREAD1).cast::<RtThread>(),
            core::ptr::addr_of_mut!(RT_THREAD1_STACK)
                .cast::<AlignedStack<STACK_LEN>>()
                .cast::<u32>()
                .add(STACK_LEN),
            rt_thread1_runner,
            core::ptr::null_mut(),
            "rt_thread1",
            0, // Not used for RT thread
        );
        (*core::ptr::addr_of_mut!(RT_THREAD1_TIMER_ENTITY)).init_thread(rt_thread1);
        rtsc::enqueue_ktimer(core::ptr::addr_of_mut!(RT_THREAD1_TIMER_ENTITY));

        let rt_thread2 = rtsc::forkyi(
            core::ptr::addr_of_mut!(RT_THREAD2).cast::<RtThread>(),
            core::ptr::addr_of_mut!(RT_THREAD2_STACK)
                .cast::<AlignedStack<STACK_LEN>>()
                .cast::<u32>()
                .add(STACK_LEN),
            rt_thread2_runner,
            core::ptr::null_mut(),
            "rt_thread2",
            0, // Not used for RT thread
        );
        (*core::ptr::addr_of_mut!(RT_THREAD2_TIMER_ENTITY)).init_thread(rt_thread2);
        rtsc::enqueue_ktimer(core::ptr::addr_of_mut!(RT_THREAD2_TIMER_ENTITY));

        spawn_main_thread(main_thread)
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

pub fn set_systick(syst: &mut SYST) {
    let reload = rtsc::next_ktimer_reload().unwrap();

    syst.set_clock_source(SystClkSource::Core);
    syst.set_reload(reload);
    syst.clear_current();
    syst.enable_interrupt();
    syst.enable_counter();
}

#[exception]
fn SysTick() {
    rtsc::handle_systick();
}
