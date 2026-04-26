use core::ffi::c_void;
use core::sync::atomic::Ordering;

use cortex_m::interrupt;
use rtsc::traverse_run_queue;
use rtt_target::rprintln;

use crate::drivers::uart;

const SHELL_BUF_LEN: usize = 32;
const PS_SNAPSHOT_CAPACITY: usize = 8;

pub extern "C" fn shell_task(_arg: *mut c_void) -> ! {
    let mut line_buf = [0u8; SHELL_BUF_LEN];
    let mut line_len = 0usize;

    while !super::UART_READY.load(Ordering::Acquire) {
        cortex_m::asm::nop();
    }

    uart_write_str("shell> ");

    loop {
        match uart::try_read() {
            Ok(Some(byte)) => {
                match byte {
                    b'\r' | b'\n' => {
                        uart::write_byte(b'\r');
                        uart::write_byte(b'\n');

                        if line_len != 0 {
                            handle_shell_command(&line_buf[..line_len]);
                            line_len = 0;
                        }

                        uart_write_str("shell> ");
                    }
                    0x08 | 0x7f => {
                        if line_len != 0 {
                            line_len -= 1;
                            uart_write_str("\x08 \x08");
                        }
                    }
                    _ if byte.is_ascii_graphic() || byte == b' ' => {
                        if line_len < line_buf.len() {
                            line_buf[line_len] = byte;
                            line_len += 1;
                            uart::write_byte(byte);
                        }
                    }
                    _ => {}
                }

                rprintln!("uart rx: 0x{:02x} '{}'", byte, ascii_debug(byte));
            }
            Ok(None) => {}
            Err(err) => {
                rprintln!("uart rx error: {:?}", err);
            }
        }
    }
}

fn handle_shell_command(line: &[u8]) {
    match line {
        b"ps" => dump_run_queue_uart(),
        b"" => {}
        _ => {
            uart_write_str("unknown command: ");
            uart_write_bytes(line);
            uart_write_str("\r\n");
        }
    }
}

fn dump_run_queue_uart() {
    let mut snapshot = [ThreadSnapshot::default(); PS_SNAPSHOT_CAPACITY];
    let mut snapshot_len = 0usize;
    let mut truncated = false;

    interrupt::free(|_| unsafe {
        let mut cursor = None;

        while let Some(thread) = traverse_run_queue(cursor) {
            if snapshot_len == snapshot.len() {
                truncated = true;
                break;
            }

            let thread_ref = &*thread;
            let sched_entity = thread_ref.sched_entity();
            snapshot[snapshot_len] = ThreadSnapshot {
                id: thread_ref.id,
                name: thread_ref.name,
                priority: sched_entity.map_or(0, |entity| entity.priority),
                state: thread_ref.state,
                sched_tick_cnt: sched_entity.map_or(0, |entity| entity.sched_tick_cnt()),
                vruntime: sched_entity.map_or(0, |entity| entity.vruntime()),
            };
            snapshot_len += 1;
            cursor = Some(thread);
        }
    });

    uart_write_str("run queue:\r\n");
    for thread in &snapshot[..snapshot_len] {
        uart_write_str("  id=");
        uart_write_u32(thread.id);
        uart_write_str(" name=");
        uart_write_str(thread.name);
        uart_write_str(" prio=");
        uart_write_u32(thread.priority);
        uart_write_str(" state=");
        uart_write_str(thread_state_name(thread.state));
        uart_write_str(" ticks=");
        uart_write_u64(thread.sched_tick_cnt);
        uart_write_str(" vruntime=");
        uart_write_u64(thread.vruntime);
        uart_write_str("\r\n");
    }

    if truncated {
        uart_write_str("  ... truncated ...\r\n");
    }
}

fn uart_write_bytes(bytes: &[u8]) {
    for &byte in bytes {
        uart::write_byte(byte);
    }
}

fn uart_write_str(s: &str) {
    uart_write_bytes(s.as_bytes());
}

fn uart_write_u32(mut value: u32) {
    let mut buf = [0u8; 10];
    let mut idx = buf.len();

    if value == 0 {
        uart::write_byte(b'0');
        return;
    }

    while value != 0 {
        idx -= 1;
        buf[idx] = b'0' + (value % 10) as u8;
        value /= 10;
    }

    uart_write_bytes(&buf[idx..]);
}

fn uart_write_u64(mut value: u64) {
    let mut buf = [0u8; 20];
    let mut idx = buf.len();

    if value == 0 {
        uart::write_byte(b'0');
        return;
    }

    while value != 0 {
        idx -= 1;
        buf[idx] = b'0' + (value % 10) as u8;
        value /= 10;
    }

    uart_write_bytes(&buf[idx..]);
}

fn ascii_debug(byte: u8) -> char {
    if byte.is_ascii_graphic() || byte == b' ' {
        byte as char
    } else {
        '.'
    }
}

fn thread_state_name(state: rtsc::ThreadState) -> &'static str {
    match state {
        rtsc::ThreadState::Ready => "Ready",
        rtsc::ThreadState::Running => "Running",
        rtsc::ThreadState::Blocked => "Blocked",
        rtsc::ThreadState::Suspended => "Suspended",
    }
}

#[derive(Clone, Copy)]
struct ThreadSnapshot {
    id: u32,
    name: &'static str,
    priority: u32,
    state: rtsc::ThreadState,
    sched_tick_cnt: u64,
    vruntime: u64,
}

impl Default for ThreadSnapshot {
    fn default() -> Self {
        Self {
            id: 0,
            name: "",
            priority: 0,
            state: rtsc::ThreadState::Suspended,
            sched_tick_cnt: 0,
            vruntime: 0,
        }
    }
}
