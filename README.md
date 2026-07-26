# Board verification for rtsched

This workspace keeps small board and example programs for exercising
[`rtsched`](rtsched/README.md), a no-std scheduler crate with CFS-style
background threads, EDF-style soft real-time threads, sleep/wake support, and an
idle-thread fallback for low-power behavior.

To test rtsched, follow these steps:

1. `git clone github.com/chungae9ri/rtsched-boards`
2. `cd rtsched-boards`
3. `git submodule update --init --recursive`
4. `cargo build --release -p lpc55s69`

## Example Patterns

The `rtsched/examples` directory contains small Cortex-M examples that focus on
one scheduler behavior at a time. Each example initializes the scheduler, creates
dedicated static thread and stack storage, registers a CFS idle thread, configures
SysTick, and starts the first thread.

### One CFS Worker

[`minimal_cfs.rs`](rtsched/examples/minimal_cfs.rs) starts an idle thread plus one
normal CFS worker. The worker does background work, then cooperatively yields so
other scheduler work can run:

```rust
extern "C" fn cfs_worker(_arg: *mut c_void) -> ! {
    loop {
        WORKER_RUNS.fetch_add(1, Ordering::Relaxed);
        common::spin(2_000);
        rtsched::yieldyi();
    }
}
```

### One RT Worker

[`minimal_rt.rs`](rtsched/examples/minimal_rt.rs) starts one `RtThread` backed by
an `RtKTimer`. The timer separates period, deadline, and runtime budget:

```rust
static mut CONTROL_TIMER: rtsched::RtKTimer = rtsched::RtKTimer::new_with_timing(
    rtsched::RtTiming::new(RT_PERIOD_TICKS, RT_DEADLINE_TICKS, RT_BUDGET_TICKS),
    core::ptr::null_mut(),
    "control",
);

extern "C" fn control_loop(_arg: *mut c_void) -> ! {
    loop {
        rtsched::set_rt_thread_start_time(0);
        CONTROL_JOBS.fetch_add(1, Ordering::Relaxed);
        common::spin(8_000);
        rtsched::yieldyi();
    }
}
```

### Sleep And Wake

[`sleep_wake.rs`](rtsched/examples/sleep_wake.rs) shows CFS threads entering the
wait queue with `msleepyi()`. The scheduler timer wakes each thread when its
sleep deadline expires:

```rust
extern "C" fn fast_sleeper(_arg: *mut c_void) -> ! {
    loop {
        FAST_WAKEUPS.fetch_add(1, Ordering::Relaxed);
        rtsched::msleepyi(50);
    }
}

extern "C" fn slow_sleeper(_arg: *mut c_void) -> ! {
    loop {
        SLOW_WAKEUPS.fetch_add(1, Ordering::Relaxed);
        rtsched::msleepyi(250);
    }
}
```

### Idle Power Behavior

All examples register `common::cpu_idle` as the idle thread. It is removed from
normal CFS fairness accounting and runs only when no normal CFS or RT work is
runnable. Board code can put low-power instructions there:

```rust
pub extern "C" fn cpu_idle(_arg: *mut c_void) -> ! {
    idle_forever();
}

pub fn idle_forever() -> ! {
    loop {
        cortex_m::asm::wfi();
    }
}
```
