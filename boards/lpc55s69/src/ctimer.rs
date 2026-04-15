use core::sync::atomic::{AtomicBool, Ordering};
use cortex_m::peripheral::NVIC;
use lpc55_hal as hal;
use lpc55_hal::raw::interrupt;

static TICK: AtomicBool = AtomicBool::new(false);

pub fn configure(
    ctimer0: hal::peripherals::ctimer::Ctimer0,
    syscon: &mut hal::Syscon,
    token: hal::typestates::ClocksSupport1MhzFroToken,
) {
    let ctimer0 = ctimer0.enabled(syscon, token);

    // 0.2 Hz periodic interrupt from MR0.
    ctimer0.mr[0].write(|w| unsafe { w.match_().bits(1_000_000) });
    ctimer0
        .mcr
        .modify(|_, w| w.mr0i().set_bit().mr0r().set_bit());
    ctimer0.tcr.write(|w| w.cen().enabled());

    unsafe { NVIC::unmask(hal::raw::Interrupt::CTIMER0) };
}

pub fn take_tick() -> bool {
    TICK.swap(false, Ordering::AcqRel)
}

#[interrupt]
fn CTIMER0() {
    let p = unsafe { hal::raw::Peripherals::steal() };

    // Clear match-0 interrupt flag before signaling the main loop.
    p.CTIMER0.ir.write(|w| w.mr0int().set_bit());
    TICK.store(true, Ordering::Release);
}
