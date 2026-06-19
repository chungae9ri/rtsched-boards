use core::ffi::c_void;
use core::sync::atomic::Ordering;

use cortex_m::interrupt;
use rtsched::{traverse_ktimer_queue_fn, traverse_run_queue};
use rtt_target::rprintln;

use crate::board_printf;
use crate::drivers::uart;

const SHELL_BUF_LEN: usize = 32;
const PS_SNAPSHOT_CAPACITY: usize = 8;

pub extern "C" fn shell_task(_arg: *mut c_void) -> ! {
    let mut line_buf = [0u8; SHELL_BUF_LEN];
    let mut line_len = 0usize;

    while !super::UART_READY.load(Ordering::Acquire) {
        cortex_m::asm::nop();
    }

    shell_write_str("shell> ");

    loop {
        match uart::try_read() {
            Ok(Some(byte)) => {
                match byte {
                    b'\r' | b'\n' => {
                        shell_write_byte(b'\r');
                        shell_write_byte(b'\n');

                        if line_len != 0 {
                            handle_shell_command(&line_buf[..line_len]);
                            line_len = 0;
                        }

                        shell_write_str("shell> ");
                    }
                    0x08 | 0x7f => {
                        if line_len != 0 {
                            line_len -= 1;
                            shell_write_str("\x08 \x08");
                        }
                    }
                    _ if byte.is_ascii_graphic() || byte == b' ' => {
                        if line_len < line_buf.len() {
                            line_buf[line_len] = byte;
                            line_len += 1;
                            shell_write_byte(byte);
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
        b"help" => dump_help(),
        b"ps" => dump_run_queue(),
        b"tmr" => dump_ktimer_queue(),
        b"uart" => {
            board_printf::set_stdout_uart();
            shell_write_str("stdout: uart\r\n");
        }
        b"rtt" => {
            board_printf::set_stdout_rtt();
            shell_write_str("stdout: rtt\r\n");
        }
        b"" => {}
        _ => {
            shell_write_str("unknown command: ");
            shell_write_bytes(line);
            shell_write_str("\r\n");
        }
    }
}

fn dump_help() {
    shell_write_str("commands:\r\n");
    shell_write_str("  help  show this command list\r\n");
    shell_write_str("  ps    show run queue threads\r\n");
    shell_write_str("  tmr   show kernel timer queue\r\n");
    shell_write_str("  uart  route board_printf output to UART\r\n");
    shell_write_str("  rtt   route board_printf output to RTT\r\n");
}

fn dump_run_queue() {
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

    shell_write_str("run queue:\r\n");
    for thread in &snapshot[..snapshot_len] {
        shell_write_str("  id=");
        shell_write_u32(thread.id);
        shell_write_str(" name=");
        shell_write_str(thread.name);
        shell_write_str(" prio=");
        shell_write_u32(thread.priority);
        shell_write_str(" state=");
        shell_write_str(thread_state_name(thread.state));
        shell_write_str(" ticks=");
        shell_write_u64(thread.sched_tick_cnt);
        shell_write_str(" vruntime=");
        shell_write_u64(thread.vruntime);
        shell_write_str("\r\n");
    }

    if truncated {
        shell_write_str("  ... truncated ...\r\n");
    }
}

fn dump_ktimer_queue() {
    shell_write_str("ktimer queue:\r\n");

    traverse_ktimer_queue_fn(|name, deadline| {
        shell_write_str("  name=");
        shell_write_str(name);
        shell_write_str(" deadline=");
        shell_write_u32(deadline);
        shell_write_str(" active=");
        shell_write_str(if rtsched::is_active_ktimer(name) {
            "yes"
        } else {
            "no"
        });
        shell_write_str("\r\n");
    });
}

fn shell_write_byte(byte: u8) {
    uart::write_byte(byte);
}

fn shell_write_str(s: &str) {
    shell_write_bytes(s.as_bytes());
}

fn shell_write_bytes(bytes: &[u8]) {
    for &byte in bytes {
        shell_write_byte(byte);
    }
}

fn shell_write_u32(mut value: u32) {
    let mut buf = [0u8; 10];
    let mut idx = buf.len();

    if value == 0 {
        shell_write_byte(b'0');
        return;
    }

    while value != 0 {
        idx -= 1;
        buf[idx] = b'0' + (value % 10) as u8;
        value /= 10;
    }

    shell_write_bytes(&buf[idx..]);
}

fn shell_write_u64(mut value: u64) {
    let mut buf = [0u8; 20];
    let mut idx = buf.len();

    if value == 0 {
        shell_write_byte(b'0');
        return;
    }

    while value != 0 {
        idx -= 1;
        buf[idx] = b'0' + (value % 10) as u8;
        value /= 10;
    }

    shell_write_bytes(&buf[idx..]);
}

fn ascii_debug(byte: u8) -> char {
    if byte.is_ascii_graphic() || byte == b' ' {
        byte as char
    } else {
        '.'
    }
}

fn thread_state_name(state: rtsched::ThreadState) -> &'static str {
    match state {
        rtsched::ThreadState::Ready => "Ready",
        rtsched::ThreadState::Running => "Running",
        rtsched::ThreadState::Waiting => "Waiting",
    }
}

#[derive(Clone, Copy)]
struct ThreadSnapshot {
    id: u32,
    name: &'static str,
    priority: u32,
    state: rtsched::ThreadState,
    sched_tick_cnt: u64,
    vruntime: u64,
}

impl Default for ThreadSnapshot {
    fn default() -> Self {
        Self {
            id: 0,
            name: "",
            priority: 0,
            state: rtsched::ThreadState::Waiting,
            sched_tick_cnt: 0,
            vruntime: 0,
        }
    }
}
