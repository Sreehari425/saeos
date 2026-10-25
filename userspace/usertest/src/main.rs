//! The QEMU `user-test` init: runs as PID 1 in ring 3 and checks what
//! userspace can do so far. Its exit code is the test verdict (0 = pass);
//! each failed check exits with its own code, so the log says which.
//!
//! `usertest crash` dereferences a null pointer instead: the kernel must
//! kill it with SIGSEGV and survive.

#![no_std]
#![no_main]

use saeos_rt::{Args, close, entry, exit, getpid, open, read, write};

entry!(main);

fn say(text: &[u8]) {
    write(1, text);
}

fn fail(code: i32, what: &[u8]) -> ! {
    say(b"usertest: FAIL: ");
    say(what);
    say(b"\n");
    exit(code)
}

fn rdtsc() -> u64 {
    let (low, high): (u32, u32);
    unsafe { core::arch::asm!("rdtsc", out("eax") low, out("edx") high, options(nomem, nostack)) };
    (u64::from(high) << 32) | u64::from(low)
}

fn main(args: Args) -> i32 {
    match args.get(1) {
        Some(b"crash") => {
            say(b"usertest: dereferencing null\n");
            let value = unsafe { core::ptr::read_volatile(core::ptr::null::<u8>()) };
            fail(
                10,
                if value == 0 {
                    b"null read returned 0"
                } else {
                    b"null read returned"
                },
            )
        }
        Some(b"ud") => {
            say(b"usertest: executing ud2\n");
            unsafe { core::arch::asm!("ud2", options(nomem, nostack)) };
            fail(11, b"ud2 did not fault")
        }
        _ => {}
    }

    say(b"usertest: hello from ring 3\n");
    if getpid() != 1 {
        fail(2, b"init is not pid 1");
    }
    if args.count() != 1 || args.get(0).is_none() {
        fail(3, b"argv");
    }
    // A bad fd and a kernel pointer are errors, not crashes.
    if write(99, b"x") != -9 {
        fail(4, b"write to a bad fd is not EBADF");
    }
    let kernel_pointer =
        unsafe { core::slice::from_raw_parts(0xffff_ffff_8000_0000usize as *const u8, 4) };
    if write(1, kernel_pointer) != -14 {
        fail(5, b"write from a kernel address is not EFAULT");
    }
    // Files through the syscall layer.
    let fd = open(b"/etc/hello\0", 0);
    if fd < 0 {
        fail(6, b"open /etc/hello");
    }
    let mut buffer = [0u8; 64];
    let count = read(fd as u32, &mut buffer);
    if count <= 0 || !buffer.starts_with(b"Hello from the SaeOS initramfs!") {
        fail(7, b"read /etc/hello");
    }
    if close(fd as u32) != 0 {
        fail(8, b"close");
    }
    floating_point();
    say(b"usertest: PASS\n");
    0
}

fn mxcsr() -> u32 {
    let mut value = 0u32;
    unsafe { core::arch::asm!("stmxcsr [{}]", in(reg) &raw mut value, options(nostack)) };
    value
}

fn set_mxcsr(value: u32) {
    unsafe { core::arch::asm!("ldmxcsr [{}]", in(reg) &value, options(nostack)) };
}

/// One step of a computation whose result depends on every bit of the
/// previous value and on the rounding mode.
fn step(x: f64) -> f64 {
    core::hint::black_box(x * 1.000_000_119 + 0.333_333_333_333_3) / 1.000_000_071
}

/// Hardware floating point (SSE, from the SaeOS target) must survive
/// preemption: the kernel saves this process's registers and MXCSR when it
/// switches away and restores them when it comes back. Kernel tasks run
/// with a clean FPU state, so a missing save or restore changes the result.
fn floating_point() {
    // Round toward zero: MXCSR is per-process state too.
    let rounding = 0x1f80 | (3 << 13);
    set_mxcsr(rounding);
    // The reference result, computed without waiting.
    const STEPS: u32 = 2_000;
    let mut expected = 1.0f64;
    for _ in 0..STEPS {
        expected = step(expected);
    }
    // The same computation, spread over long enough for many timer
    // preemptions to land in the middle of it.
    let mut value = 1.0f64;
    let start = rdtsc();
    for _ in 0..STEPS {
        value = step(value);
        let pause = rdtsc();
        while rdtsc().wrapping_sub(pause) < 200_000 {
            core::hint::spin_loop();
        }
    }
    if rdtsc().wrapping_sub(start) < 300_000_000 {
        fail(20, b"floating-point spin too short to be preempted");
    }
    if value.to_bits() != expected.to_bits() {
        fail(21, b"f64 result changed across preemption");
    }
    // The low six bits are sticky exception flags (inexact divisions set
    // the precision flag); the control bits must be exactly ours.
    if mxcsr() & !0x3f != rounding {
        fail(22, b"MXCSR rounding mode lost across preemption");
    }
    // And MXCSR really drives the arithmetic (it would not in a soft-float
    // build): rounding down and up give different results for any inexact
    // division.
    // Set the mode and divide in one asm block: LLVM assumes the default
    // FP environment, so ordinary Rust divisions may be moved (or merged)
    // across an MXCSR write.
    let third = |mode: u32| {
        let control = 0x1f80u32 | (mode << 13);
        let mut value = 1.0f64;
        unsafe {
            core::arch::asm!(
                "ldmxcsr [{control}]",
                "divsd {value}, {divisor}",
                control = in(reg) &control,
                value = inout(xmm_reg) value,
                divisor = in(xmm_reg) 3.0f64,
                options(nostack),
            );
        }
        value
    };
    let (down, up) = (third(1), third(2));
    if down.to_bits() == up.to_bits() {
        fail(23, b"MXCSR rounding mode has no effect (soft-float build?)");
    }
    set_mxcsr(0x1f80);
}
