//! Unattended boot test (`boot-test` feature).
//!
//! Replaces the shell task: runs the memory self-tests, checks the
//! interrupt-safety of locks shared with interrupt handlers, runs the
//! scheduler runtime self-test, and ends the QEMU run with the verdict.
//! Panics and faults end the run as failures (see the panic handler and the
//! exception handlers).

use crate::arch::x86_64::{cpu, interrupt_controller, qemu};
use crate::mm::selftest::{self, Status};
use crate::sync::IrqSpinMutex;
use crate::task::{ExitStatus, WaitQueue};
use crate::{drivers, scheduler, serial_println, time};
use core::sync::atomic::{AtomicBool, Ordering};

/// Upper bound for the scheduler self-test; it normally finishes in ~1 s.
const SCHEDULER_TIMEOUT_MS: u64 = 30_000;

pub fn run() -> ! {
    serial_println!("boot-test: starting.");
    let mm_passed = memory_tests();
    let irq_passed = irq_lock_test();
    let wait_passed = wait_event_test();
    let idle_passed = idle_test();
    let scheduler_passed = scheduler_test();
    let passed = mm_passed && irq_passed && wait_passed && idle_passed && scheduler_passed;
    serial_println!(
        "boot-test: memory={} irq-locks={} wait-event={} idle={} scheduler={} -> {}",
        verdict(mm_passed),
        verdict(irq_passed),
        verdict(wait_passed),
        verdict(idle_passed),
        verdict(scheduler_passed),
        verdict(passed)
    );
    qemu::exit(passed)
}

fn verdict(passed: bool) -> &'static str {
    if passed { "pass" } else { "FAIL" }
}

fn memory_tests() -> bool {
    let report = selftest::run();
    for line in report
        .lines
        .iter()
        .filter(|line| line.status == Status::Fail)
    {
        serial_println!("boot-test: memory test failed: {}", line.name);
    }
    serial_println!(
        "boot-test: memory {} passed, {} failed, {} skipped.",
        report.passed,
        report.failed,
        report.skipped
    );
    report.failed == 0
}

/// Every lock an interrupt handler takes must mask interrupts while held;
/// otherwise an IRQ arriving inside the critical section spins on it forever
/// on this single CPU. Check that from a task running with interrupts on,
/// then check that timer interrupts still arrive.
fn irq_lock_test() -> bool {
    if !cpu::interrupts_enabled() {
        serial_println!("boot-test: irq-locks: task started with interrupts disabled.");
        return false;
    }
    let mut masked = true;
    for _ in 0..1000 {
        masked &= {
            let _queue = drivers::keyboard::KEY_QUEUE.lock();
            !cpu::interrupts_enabled()
        };
        masked &= {
            let _controller = interrupt_controller::CONTROLLER.lock();
            !cpu::interrupts_enabled()
        };
        masked &= {
            let _port = drivers::serial::SERIAL1.lock();
            !cpu::interrupts_enabled()
        };
    }
    let restored = cpu::interrupts_enabled();
    // Spin rather than sleep: this only finishes if timer IRQs (each taking
    // CONTROLLER for EOI) are still delivered. A dead timer hangs here and
    // the runner's timeout reports it.
    let deadline = time::ticks().saturating_add(20);
    while time::ticks() < deadline {
        cpu::pause();
    }
    if !(masked && restored) {
        serial_println!(
            "boot-test: irq-locks masked={} restored={}",
            masked,
            restored
        );
    }
    masked && restored
}

static TEST_WAIT: IrqSpinMutex<WaitQueue> =
    IrqSpinMutex::new(WaitQueue::new(scheduler::BOOT_TEST_WAIT_ID));
static TEST_FLAG: AtomicBool = AtomicBool::new(false);
const WAKER_DELAY_MS: u64 = 20;

fn waker_task() -> ! {
    scheduler::sleep_ms(WAKER_DELAY_MS);
    TEST_FLAG.store(true, Ordering::Release);
    scheduler::wake_one(&TEST_WAIT);
    scheduler::exit(ExitStatus(0))
}

/// `wait_event` returns at once when the condition already holds, and a
/// waiter sleeps until another task makes it true and wakes the queue.
fn wait_event_test() -> bool {
    let before = scheduler::stats().context_switches;
    scheduler::wait_event(&TEST_WAIT, || true);
    let no_block = scheduler::stats().context_switches == before;

    TEST_FLAG.store(false, Ordering::Release);
    scheduler::spawn_kernel_task(waker_task, "boot-test-waker");
    let start = time::ticks();
    scheduler::wait_event(&TEST_WAIT, || TEST_FLAG.load(Ordering::Acquire));
    let waited = time::ticks().saturating_sub(start);
    let woke = TEST_FLAG.load(Ordering::Acquire) && waited >= WAKER_DELAY_MS;
    if !(no_block && woke) {
        serial_println!(
            "boot-test: wait-event no_block={} woke={} waited={}ms",
            no_block,
            woke,
            waited
        );
    }
    no_block && woke
}

/// While this task sleeps, every other task (shell or reaper) is blocked on
/// a wait queue, so the idle task must own the CPU and the timer must not
/// keep switching. Busy-polling tasks would show up as missing idle ticks.
fn idle_test() -> bool {
    const IDLE_MS: u64 = 200;
    const MAX_SWITCHES: u64 = 20;
    // Let the waker exit and the reaper finish first.
    scheduler::sleep_ms(10);
    let before = scheduler::stats();
    scheduler::sleep_ms(IDLE_MS);
    let after = scheduler::stats();
    let switches = after.context_switches - before.context_switches;
    let idle_ticks = after.idle_ticks - before.idle_ticks;
    serial_println!(
        "boot-test: {} ms asleep: {} idle ticks, {} context switches.",
        IDLE_MS,
        idle_ticks,
        switches
    );
    switches <= MAX_SWITCHES && idle_ticks * 10 >= IDLE_MS * 9
}

fn scheduler_test() -> bool {
    if let Err(error) = scheduler::start_selftest() {
        serial_println!("boot-test: scheduler self-test unavailable: {}", error);
        return false;
    }
    let deadline = time::ticks().saturating_add(SCHEDULER_TIMEOUT_MS);
    while time::ticks() < deadline {
        if scheduler::selftest_passed() {
            return true;
        }
        scheduler::sleep_ms(50);
    }
    serial_println!("boot-test: scheduler self-test timed out.");
    false
}
