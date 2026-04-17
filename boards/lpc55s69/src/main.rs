#![no_std]
#![no_main]

mod ctimer;

use core::ffi::c_void;
use cortex_m::peripheral::{SYST, syst::SystClkSource};
use cortex_m_rt::entry;
use panic_halt as _;

use hal::{drivers::pins::Level, prelude::*};
use lpc55_hal as hal;
use rtsc::Task;
use rtsc::{AlignedStack, init_rq, start_first_task};

use rtt_target::{rprintln, rtt_init_print};

const STACK_LEN: usize = 1024;
static mut BOOT_HAL: Option<hal::Peripherals> = None;

/// Main thread context and dedicated stack.
///
/// The reset handler enters `main` using MSP. We synthesize an initial task
/// frame for the real application thread and start it through the same restore
/// path used by every other task.
static mut MAIN_STACK: AlignedStack<STACK_LEN> = AlignedStack([0; STACK_LEN]);
static mut MAIN_THREAD: Task = Task {
    sp: 0,
    exc_return: 0xFFFF_FFFD,
    id: 0,
    name: "main",
    priority: 0,
    state: rtsc::TaskState::Ready,
    sched_entity: rtsc::rbtree::sched_entity::new(0),
    callee_saved_regs: rtsc::CalleeSavedRegisters {
        r4: 0,
        r5: 0,
        r6: 0,
        r7: 0,
        r8: 0,
        r9: 0,
        r10: 0,
        r11: 0,
    },
};

static mut FORKYI_STACK: AlignedStack<STACK_LEN> = AlignedStack([0; STACK_LEN]);
static mut FORKYI_THREAD: Task = Task {
    sp: 0,
    exc_return: 0,
    id: 0,
    name: "",
    priority: 0,
    state: rtsc::TaskState::Suspended,
    sched_entity: rtsc::rbtree::sched_entity::new(0),
    callee_saved_regs: rtsc::CalleeSavedRegisters {
        r4: 0,
        r5: 0,
        r6: 0,
        r7: 0,
        r8: 0,
        r9: 0,
        r10: 0,
        r11: 0,
    },
};
const SYS_CLK_FREQ: u32 = 12_000_000; // 12 MHz

extern "C" fn forkyi_task(_arg: *mut c_void) -> ! {
    let mut cnt: u32 = 0;

    loop {
        cnt += 1;
        cortex_m::asm::nop();
        rprintln!("forkyi task: {}", cnt);
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
            &raw mut MAIN_THREAD,
            core::ptr::addr_of_mut!(MAIN_STACK)
                .cast::<AlignedStack<STACK_LEN>>()
                .cast::<u32>()
                .add(STACK_LEN),
            runtime_main,
            core::ptr::null_mut(),
            0,
            "main",
            0,
        );
        rtsc::forkyi(
            &raw mut FORKYI_THREAD,
            core::ptr::addr_of_mut!(FORKYI_STACK)
                .cast::<AlignedStack<STACK_LEN>>()
                .cast::<u32>()
                .add(STACK_LEN),
            forkyi_task,
            core::ptr::null_mut(),
            1,
            "forkyi",
            1,
        );
        start_first_task(&raw mut MAIN_THREAD)
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

    let mut red = pins
        .pio1_6
        .into_gpio_pin(&mut iocon, &mut gpio)
        .into_output(Level::High);
    let mut red_high = true;

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
