use lpc55_hal as hal;

pub type Error = hal::drivers::serial::Error;

pub fn write_byte(byte: u8) {
    let usart = regs();

    while !usart.fifostat.read().txnotfull().bit() {
        cortex_m::asm::nop();
    }

    usart.fifowr.write(|w| unsafe { w.bits(byte as u32) });
}

#[allow(dead_code)]
pub fn write_line(line: &[u8]) {
    for &byte in line {
        write_byte(byte);
    }
    write_byte(b'\r');
    write_byte(b'\n');
}

pub fn try_read() -> Result<Option<u8>, Error> {
    let usart = regs();
    let fifostat = usart.fifostat.read();

    if !fifostat.rxnotempty().bit() {
        return Ok(None);
    }

    let fiford = usart.fiford.read();

    if fiford.framerr().bit_is_set() {
        return Err(Error::Framing);
    }

    if fiford.parityerr().bit_is_set() {
        return Err(Error::Parity);
    }

    if fiford.rxnoise().bit_is_set() {
        return Err(Error::Noise);
    }

    if fifostat.rxerr().bit_is_set() {
        usart.fifocfg.modify(|_, w| w.emptyrx().set_bit());
        usart.fifostat.modify(|_, w| w.rxerr().set_bit());
        return Err(Error::Overrun);
    }

    Ok(Some(fiford.rxdata().bits() as u8))
}

fn regs() -> &'static hal::raw::usart0::RegisterBlock {
    unsafe { &*hal::raw::USART0::ptr() }
}
