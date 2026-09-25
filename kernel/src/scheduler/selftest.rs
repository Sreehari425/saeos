use super::*;
use core::sync::atomic::{AtomicU64, Ordering};

#[cfg(feature = "scheduler-selftest")]
static STRESS_COUNTER: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "scheduler-selftest")]
static CPU_TASK_PROGRESS: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
#[cfg(feature = "scheduler-selftest")]
static CPU_LAST: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(usize::MAX);
#[cfg(feature = "scheduler-selftest")]
static CPU_ALTERNATIONS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "scheduler-selftest")]
static SLEEP_WAKES: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "scheduler-selftest")]
static SELFTEST_STARTED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
#[cfg(feature = "scheduler-selftest")]
static SELFTEST_FINISHED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Start the opt-in runtime workload when explicitly requested from the shell.
#[cfg(feature = "scheduler-selftest")]
pub fn start_selftest() -> Result<(), &'static str> {
    if SELFTEST_STARTED.swap(true, Ordering::AcqRel) {
        return Err("scheduler self-test has already been started");
    }
    let interrupts_were_enabled = cpu::interrupts_enabled();
    SELFTEST_FINISHED.store(false, Ordering::Release);
    scheduler_debug(format_args!("starting opt-in runtime self-test"));
    cpu::cli();
    let tasks = with_scheduler(|scheduler| {
        [
            scheduler.spawn_kernel_task(stress_cpu_task, "stress-cpu", false),
            scheduler.spawn_kernel_task(stress_cpu_task_2, "stress-cpu-2", false),
            scheduler.spawn_kernel_task(stress_yield_task, "stress-yield", false),
            scheduler.spawn_kernel_task(stress_sleep_task, "stress-sleep", false),
            scheduler.spawn_kernel_task(stress_exit_task, "stress-exit", false),
            scheduler.spawn_kernel_task(stress_verify_task, "stress-verify", false),
        ]
    });
    for task in tasks {
        attach_task_to_new_process(task);
    }
    if interrupts_were_enabled {
        cpu::sti();
    }
    Ok(())
}

#[cfg(feature = "scheduler-selftest")]
fn stress_cpu_task() -> ! {
    loop {
        if SELFTEST_FINISHED.load(Ordering::Acquire) {
            exit_current(ExitStatus(0));
        }
        STRESS_COUNTER.fetch_add(1, Ordering::Relaxed);
        CPU_TASK_PROGRESS[0].fetch_add(1, Ordering::Relaxed);
        if CPU_LAST.swap(0, Ordering::Relaxed) == 1 {
            CPU_ALTERNATIONS.fetch_add(1, Ordering::Relaxed);
        }
        core::hint::spin_loop();
    }
}

#[cfg(feature = "scheduler-selftest")]
fn stress_cpu_task_2() -> ! {
    loop {
        if SELFTEST_FINISHED.load(Ordering::Acquire) {
            exit_current(ExitStatus(0));
        }
        STRESS_COUNTER.fetch_add(1, Ordering::Relaxed);
        CPU_TASK_PROGRESS[1].fetch_add(1, Ordering::Relaxed);
        if CPU_LAST.swap(1, Ordering::Relaxed) == 0 {
            CPU_ALTERNATIONS.fetch_add(1, Ordering::Relaxed);
        }
        core::hint::spin_loop();
    }
}

#[cfg(feature = "scheduler-selftest")]
fn stress_yield_task() -> ! {
    loop {
        if SELFTEST_FINISHED.load(Ordering::Acquire) {
            exit_current(ExitStatus(0));
        }
        STRESS_COUNTER.fetch_add(1, Ordering::Relaxed);
        yield_now();
    }
}

#[cfg(feature = "scheduler-selftest")]
fn stress_sleep_task() -> ! {
    loop {
        if SELFTEST_FINISHED.load(Ordering::Acquire) {
            exit_current(ExitStatus(0));
        }
        STRESS_COUNTER.fetch_add(1, Ordering::Relaxed);
        let deadline = crate::time::ticks().saturating_add(5);
        sleep_until(deadline);
        assert!(
            crate::time::ticks() >= deadline,
            "sleep woke before deadline"
        );
        SLEEP_WAKES.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(feature = "scheduler-selftest")]
fn stress_exit_task() -> ! {
    for _ in 0..8 {
        STRESS_COUNTER.fetch_add(1, Ordering::Relaxed);
        yield_now();
    }
    exit_current(ExitStatus(0))
}

#[cfg(feature = "scheduler-selftest")]
fn stress_verify_task() -> ! {
    for _ in 0..20 {
        sleep_ms(50);
        let scheduler_stats = stats();
        if CPU_TASK_PROGRESS
            .iter()
            .all(|count| count.load(Ordering::Relaxed) > 0)
            && CPU_ALTERNATIONS.load(Ordering::Relaxed) > 0
            && SLEEP_WAKES.load(Ordering::Relaxed) > 0
            && scheduler_stats.tasks_reaped > 0
            && scheduler_stats.voluntary_yields > 0
            && scheduler_stats.timer_preemptions > 0
            && scheduler_stats.tasks_woken > 0
            && STRESS_COUNTER.load(Ordering::Relaxed) > 0
        {
            crate::serial_println!(
                "scheduler self-test: PASS (cpu alternation, yield, sleep deadlines, timer preemption, wake-up, exit, reaping)"
            );
            SELFTEST_FINISHED.store(true, Ordering::Release);
            exit_current(ExitStatus(0));
        }
    }
    let scheduler_stats = stats();
    let tasks = snapshots();
    crate::serial_println!(
        "scheduler self-test failed: timer={} yield={} wake={} reaped={} cpu={:?} alternations={} sleep_wakes={} counter={} tasks={:?}",
        scheduler_stats.timer_preemptions,
        scheduler_stats.voluntary_yields,
        scheduler_stats.tasks_woken,
        scheduler_stats.tasks_reaped,
        [
            CPU_TASK_PROGRESS[0].load(Ordering::Relaxed),
            CPU_TASK_PROGRESS[1].load(Ordering::Relaxed)
        ],
        CPU_ALTERNATIONS.load(Ordering::Relaxed),
        SLEEP_WAKES.load(Ordering::Relaxed),
        STRESS_COUNTER.load(Ordering::Relaxed),
        tasks
            .iter()
            .map(|task| (task.task_id, task.state, task.exec_runtime))
            .collect::<Vec<_>>()
    );
    panic!("scheduler self-test did not satisfy runtime assertions");
}
