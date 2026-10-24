//! Unattended boot test (`boot-test` feature).
//!
//! Replaces the shell task: runs the memory self-tests, checks the
//! interrupt-safety of locks shared with interrupt handlers, runs the
//! scheduler runtime self-test, and ends the QEMU run with the verdict.
//! Panics and faults end the run as failures (see the panic handler and the
//! exception handlers).

use crate::arch::x86_64::{cpu, interrupt_controller, qemu};
use crate::mm::selftest::{self, Status};
use crate::{drivers, scheduler, serial_println, time};

/// Upper bound for the scheduler self-test; it normally finishes in ~1 s.
const SCHEDULER_TIMEOUT_MS: u64 = 30_000;

pub fn run() -> ! {
    serial_println!("boot-test: starting.");
    let mm_passed = memory_tests();
    let irq_passed = irq_lock_test();
    let scheduler_passed = scheduler_test();
    let passed = mm_passed && irq_passed && scheduler_passed;
    serial_println!(
        "boot-test: memory={} irq-locks={} scheduler={} -> {}",
        verdict(mm_passed),
        verdict(irq_passed),
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
