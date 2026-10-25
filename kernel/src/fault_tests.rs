//! Deliberate-failure test tasks, each enabled by its own feature and run by
//! `make test`. Each one ends the QEMU run through `crate::fatal`, which
//! reports PASS only for the fault that test expects.

#[cfg(any(
    feature = "mm-guard-test",
    feature = "fault-test",
    feature = "panic-lock-test",
    feature = "user-test"
))]
pub fn spawn() {
    #[cfg(feature = "user-test")]
    crate::scheduler::spawn_kernel_task(fpu_noise_task, "fpu-noise");
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

/// Competition for the user-test init: without another runnable task, a
/// timer interrupt just resumes the same task and per-task CPU state is
/// never switched. This task keeps loading its own FPU registers and
/// rounding mode (round down; usertest uses round toward zero) and has its
/// own FS base, and alternates running with sleeping, so the user program
/// is switched out and back in constantly. If the kernel failed to save or
/// restore user FPU state the program would see these values; if it failed
/// to switch the FS base, one of the two would read the other's thread
/// block (this task checks its own and fails the run).
#[cfg(feature = "user-test")]
fn fpu_noise_task() -> ! {
    #[repr(C, align(16))]
    struct Noise([u8; 256], u32);
    let noise = Noise([0xa5; 256], 0x1f80 | (1 << 13));
    // A thread block whose first word points at itself, like a user TLS
    // block; this task's FS base points at it.
    static mut NOISE_THREAD: [u64; 2] = [0; 2];
    let block = &raw mut NOISE_THREAD as u64;
    unsafe { (block as *mut u64).write(block) };
    crate::scheduler::with_scheduler(|scheduler| {
        if let Some(task) = scheduler
            .current()
            .and_then(|task| scheduler.task_mut(task))
        {
            task.fs_base = block;
        }
    });
    crate::arch::x86_64::cpu::set_fs_base(block);
    loop {
        let seen: u64;
        unsafe {
            core::arch::asm!("mov {}, fs:[0]", out(reg) seen, options(nostack, readonly, preserves_flags));
        }
        if seen != block {
            crate::fatal::report(
                format_args!("fpu-noise: FS base is not this task's ({seen:#x}): not switched"),
                false,
            );
        }
        unsafe {
            core::arch::asm!(
                "movdqu xmm0, [{p}]", "movdqu xmm1, [{p} + 16]",
                "movdqu xmm2, [{p} + 32]", "movdqu xmm3, [{p} + 48]",
                "movdqu xmm4, [{p} + 64]", "movdqu xmm5, [{p} + 80]",
                "movdqu xmm6, [{p} + 96]", "movdqu xmm7, [{p} + 112]",
                "movdqu xmm8, [{p} + 128]", "movdqu xmm9, [{p} + 144]",
                "movdqu xmm10, [{p} + 160]", "movdqu xmm11, [{p} + 176]",
                "movdqu xmm12, [{p} + 192]", "movdqu xmm13, [{p} + 208]",
                "movdqu xmm14, [{p} + 224]", "movdqu xmm15, [{p} + 240]",
                "ldmxcsr [{p} + 256]",
                p = in(reg) &noise,
                options(nostack, readonly, preserves_flags),
            );
        }
        let until = crate::time::ticks() + 2;
        while crate::time::ticks() < until {
            core::hint::spin_loop();
        }
        crate::scheduler::sleep_ms(1);
    }
}
