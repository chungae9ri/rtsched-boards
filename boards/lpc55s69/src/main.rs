#![no_std]
#![no_main]

mod ctimer;

use core::ffi::c_void;
use cortex_m::peripheral::{SYST, syst::SystClkSource};
use cortex_m_rt::entry;
use panic_halt as _;

use hal::{drivers::pins::Level, prelude::*};
use lpc55_hal as hal;
use rtt_target::{rprintln, rtt_init_print};

const FORKYI_STACK_LEN: usize = 1024;
static mut FORKYI_STACK: [u32; FORKYI_STACK_LEN] = [0; FORKYI_STACK_LEN];
const SYS_CLK_FREQ: u32 = 12_000_000; // 12 MHz

extern "C" fn forkyi_task(_arg: *mut c_void) -> ! {
    loop {
        cortex_m::asm::nop();
    }
}

#[entry]
fn main() -> ! {
    rtt_init_print!();

    let mut hal = hal::new();

    // Set systick at 1Hz
    set_systick(&mut hal.SYST, 1000);

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
    let _forkyi_sp = unsafe {
        rtsc::forkyi(
            core::ptr::addr_of_mut!(FORKYI_STACK).cast::<u32>().add(FORKYI_STACK_LEN),
            forkyi_task,
            core::ptr::null_mut(),
        )
    };

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

pub fn set_systick(syst:&mut SYST, _dur_msec:u32) {
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
