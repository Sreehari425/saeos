//! Deliberate-failure test tasks, each enabled by its own feature and run by
//! `make test`. Each one ends the QEMU run through `crate::fatal`, which
//! reports PASS only for the fault that test expects.

#[cfg(any(
    feature = "mm-guard-test",
    feature = "fault-test",
    feature = "panic-lock-test"
))]
pub fn spawn() {
    #[cfg(feature = "mm-guard-test")]
    crate::scheduler::spawn_kernel_task(guard_test_task, "guard-test");
    #[cfg(feature = "fault-test")]
    crate::scheduler::spawn_kernel_task(fault_test_task, "fault-test");
    #[cfg(feature = "panic-lock-test")]
    crate::scheduler::spawn_kernel_task(panic_lock_test_task, "panic-lock-test");
}

/// Recurse until the kernel stack runs into its guard. The kernel must
/// report the overflow by task name instead of corrupting memory.
#[cfg(feature = "mm-guard-test")]
fn guard_test_task() -> ! {
    // Unbounded on purpose: the guard page is the only way out.
    #[allow(unconditional_recursion)]
    #[inline(never)]
    fn recurse(depth: u64) -> u64 {
        let frame = core::hint::black_box([depth; 32]);
        recurse(depth + 1).wrapping_add(frame[depth as usize % 32])
    }
    crate::serial_println!("guard-test: recursing until the stack guard is hit.");
    let depth = recurse(0);
    panic!("guard-test: recursion returned {depth} without hitting the guard");
}

/// Execute an undefined instruction; the #UD report must name this task.
#[cfg(feature = "fault-test")]
fn fault_test_task() -> ! {
    crate::serial_println!("fault-test: executing ud2.");
    unsafe { core::arch::asm!("ud2", options(nomem, nostack)) };
    panic!("fault-test: ud2 did not fault");
}

/// Panic while holding the console and serial locks, as a fault in the
/// middle of `println!` would. The panic message must still come out.
#[cfg(feature = "panic-lock-test")]
fn panic_lock_test_task() -> ! {
    crate::serial_println!("panic-lock-test: panicking with console and serial locked.");
    let _console = crate::drivers::console::lock_for_panic_test();
    let _serial = crate::drivers::serial::SERIAL1.lock();
    panic!("panic-lock-test: deliberate panic");
}
