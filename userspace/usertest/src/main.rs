//! The QEMU `user-test` init: runs as PID 1 in ring 3 and checks what
//! userspace can do so far. Its exit code is the test verdict (0 = pass);
//! each failed check exits with its own code, so the log says which.
//!
//! `usertest crash` dereferences a null pointer instead: the kernel must
//! kill it with SIGSEGV and survive.

#![no_std]
#![no_main]

use saeos_rt::{
    ARCH_GET_FS, ARCH_SET_FS, Args, MAP_FIXED, PROT_EXEC, PROT_READ, PROT_WRITE, arch_prctl, brk,
    close, entry, exit, getpid, mmap_anonymous, mprotect, munmap, open, read, write,
};

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
    memory();
    thread_pointer();
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

const PAGE: u64 = 4096;
const ENOMEM: i64 = -12;
const EACCES: i64 = -13;

/// Write a recognisable byte to every page of `start..start + length`,
/// then check it reads back.
fn touch(start: u64, length: u64, seed: u8) -> bool {
    let pages = length.div_ceil(PAGE);
    for page in 0..pages {
        let byte = (start + page * PAGE) as *mut u8;
        let value = seed.wrapping_add(page as u8);
        unsafe {
            byte.write_volatile(value);
            byte.add((PAGE - 1) as usize).write_volatile(value ^ 0xff);
        }
    }
    (0..pages).all(|page| {
        let byte = (start + page * PAGE) as *const u8;
        let value = seed.wrapping_add(page as u8);
        unsafe {
            byte.read_volatile() == value
                && byte.add((PAGE - 1) as usize).read_volatile() == value ^ 0xff
        }
    })
}

fn memory() {
    // The heap: grow, use, shrink.
    let start = brk(0);
    if start == 0 || !start.is_multiple_of(PAGE) {
        fail(30, b"brk(0)");
    }
    let wanted = start + 3 * PAGE + 5;
    if brk(wanted) != wanted {
        fail(31, b"brk grow");
    }
    if !touch(start, 3 * PAGE + 6, 0x30) {
        fail(32, b"heap memory");
    }
    if brk(start) != start || brk(0) != start {
        fail(33, b"brk shrink");
    }
    // 1 MiB anonymous memory: every page zeroed, writable, then gone.
    let length = 1 << 20;
    let address = mmap_anonymous(0, length, PROT_READ | PROT_WRITE, 0);
    if address <= 0 || !(address as u64).is_multiple_of(PAGE) {
        fail(34, b"mmap 1 MiB");
    }
    let address = address as u64;
    let zeroed = (0..length / PAGE)
        .all(|page| unsafe { ((address + page * PAGE) as *const u8).read_volatile() == 0 });
    if !zeroed {
        fail(35, b"mmap memory not zeroed");
    }
    if !touch(address, length, 0x40) {
        fail(36, b"mmap memory");
    }
    if munmap(address, length) != 0 {
        fail(37, b"munmap");
    }
    // mprotect on the hole proves the mapping is gone.
    if mprotect(address, PAGE, PROT_READ) != ENOMEM {
        fail(38, b"munmapped range still mapped");
    }
    // MAP_FIXED lands exactly there; protections change; W+X is refused.
    let fixed = 0x2000_0000;
    if mmap_anonymous(fixed, 2 * PAGE, PROT_READ | PROT_WRITE, MAP_FIXED) != fixed as i64 {
        fail(39, b"MAP_FIXED");
    }
    if !touch(fixed, 2 * PAGE, 0x50) || mprotect(fixed, PAGE, PROT_READ) != 0 {
        fail(40, b"mprotect");
    }
    let readable = unsafe { (fixed as *const u8).read_volatile() } == 0x50;
    if !readable {
        fail(41, b"read-only page lost its contents");
    }
    if mmap_anonymous(0, PAGE, PROT_WRITE | PROT_EXEC, 0) != EACCES
        || mprotect(fixed, PAGE, PROT_WRITE | PROT_EXEC) != EACCES
    {
        fail(42, b"W+X was allowed");
    }
    if mmap_anonymous(0, PAGE, 0, 0) <= 0 {
        fail(43, b"PROT_NONE mapping");
    }
    if munmap(fixed, 2 * PAGE) != 0 {
        fail(44, b"munmap fixed");
    }
}

#[repr(C, align(64))]
struct ThreadBlock {
    this: u64,
    value: u64,
}

static mut THREAD: ThreadBlock = ThreadBlock {
    this: 0,
    value: 0x5ae0_7155,
};

fn fs_word(offset: u64) -> u64 {
    let value: u64;
    unsafe {
        core::arch::asm!("mov {}, fs:[{}]", out(reg) value, in(reg) offset, options(nostack, readonly));
    }
    value
}

/// The thread pointer (FS base) is per-task state: set it like a libc's
/// TLS setup does, then check `%fs`-relative loads stay right while the
/// kernel switches to tasks with a different (zero) FS base and back.
fn thread_pointer() {
    let block = &raw mut THREAD;
    unsafe { (*block).this = block as u64 };
    if arch_prctl(ARCH_SET_FS, block as u64) != 0 {
        fail(50, b"ARCH_SET_FS");
    }
    let mut reported = 0u64;
    if arch_prctl(ARCH_GET_FS, &raw mut reported as u64) != 0 || reported != block as u64 {
        fail(51, b"ARCH_GET_FS");
    }
    if arch_prctl(ARCH_SET_FS, 0xffff_8000_0000_0000) != -1 {
        fail(52, b"kernel FS base was not EPERM");
    }
    let start = rdtsc();
    while rdtsc().wrapping_sub(start) < 200_000_000 {
        if fs_word(0) != block as u64 || fs_word(8) != 0x5ae0_7155 {
            fail(53, b"FS base lost across a context switch");
        }
    }
}
