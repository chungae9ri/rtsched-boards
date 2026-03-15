#![no_std]
#![no_main]

use core::sync::atomic::{AtomicBool, Ordering};
use cortex_m::peripheral::{NVIC, syst::SystClkSource};
use cortex_m_rt::{entry, exception};
use kernel as _;
use panic_halt as _;

use hal::{drivers::pins::Level, prelude::*};
use lpc55_hal as hal;
use lpc55_hal::raw::interrupt;
use rtt_target::{rprintln, rtt_init_print};

static TICK: AtomicBool = AtomicBool::new(false);

#[entry]
fn main() -> ! {
    rtt_init_print!();

    let mut hal = hal::new();

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

    // 1 Hz periodic interrupt from MR0
    //ctimer0.mr[0].write(|w| unsafe { w.match_().bits(1_000_000) });
    ctimer0.mr[0].write(|w| unsafe { w.match_().bits(5_000_000) });
    ctimer0.mcr.modify(|_, w| w.mr0i().set_bit().mr0r().set_bit());
    ctimer0.tcr.write(|w| w.cen().enabled());

    unsafe { NVIC::unmask(hal::raw::Interrupt::CTIMER0) };

    // 1 Hz SysTick from the 12 MHz core clock.
    hal.SYST.set_clock_source(SystClkSource::Core);
    hal.SYST.set_reload(12_000_000 - 1);
    hal.SYST.clear_current();
    hal.SYST.enable_interrupt();
    hal.SYST.enable_counter();

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

#[exception]
fn SysTick() {
   rprintln!("SysTick");
}

#[interrupt]
fn CTIMER0() {
    let p = unsafe { hal::raw::Peripherals::steal() };

    // Clear match-0 interrupt flag (mandatory)
    p.CTIMER0.ir.write(|w| w.mr0int().set_bit());

    TICK.store(true, Ordering::Release);
}
