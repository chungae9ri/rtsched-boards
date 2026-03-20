#![no_std]
#![no_main]

use core::sync::atomic::{AtomicBool, Ordering};
use cortex_m::peripheral::{SYST, NVIC, syst::SystClkSource};
use cortex_m_rt::entry;
use rtsc as _;
use panic_halt as _;

use hal::{drivers::pins::Level, prelude::*};
use lpc55_hal as hal;
use lpc55_hal::raw::interrupt;
use rtt_target::{rprintln, rtt_init_print};

static TICK: AtomicBool = AtomicBool::new(false);
const SYS_CLK_FREQ: u32 = 12_000_000; // 12 MHz

#[entry]
fn main() -> ! {
    rtt_init_print!();

    //let mut cp = Peripherals::take().unwrap();
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

    // Enable CTIMER0 @ 1 MHz source (from HAL token)
    let ctimer0 = hal
        .ctimer
        .0
        .enabled(&mut hal.syscon, clocks.support_1mhz_fro_token().unwrap());

    // 0.2 Hz periodic interrupt from MR0
    ctimer0.mr[0].write(|w| unsafe { w.match_().bits(5_000_000) });
    ctimer0.mcr.modify(|_, w| w.mr0i().set_bit().mr0r().set_bit());
    ctimer0.tcr.write(|w| w.cen().enabled());

    unsafe { NVIC::unmask(hal::raw::Interrupt::CTIMER0) };

    loop {
        if TICK.swap(false, Ordering::AcqRel) {
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


#[interrupt]
fn CTIMER0() {
    let p = unsafe { hal::raw::Peripherals::steal() };

    // Clear match-0 interrupt flag (mandatory)
    p.CTIMER0.ir.write(|w| w.mr0int().set_bit());

    TICK.store(true, Ordering::Release);
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
