#![no_std]
#![no_main]

use cortex_m_rt::entry;
use kernel as _;
use panic_halt as _; // Stop execution on panic // Link the kernel crate

use hal::{drivers::pins::Level, prelude::*};
use lpc55_hal as hal;

#[entry]
fn main() -> ! {
    let hal = hal::new();

    let mut syscon = hal.syscon;
    let mut gpio = hal.gpio.enabled(&mut syscon);
    let mut iocon = hal.iocon.enabled(&mut syscon);

    let pins = hal::Pins::take().unwrap();

    // LPC55S69-EVK red LED is on PIO1_6 (active-low)
    let mut red = pins
        .pio1_6
        .into_gpio_pin(&mut iocon, &mut gpio)
        .into_output(Level::High);

    loop {
        for _ in 0..50_000 {
            red.set_low().unwrap();
        }
        for _ in 0..50_000 {
            red.set_high().unwrap();
        }
    }
}

