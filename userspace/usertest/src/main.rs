//! The QEMU `user-test` init: runs as PID 1 in ring 3 and checks what
//! userspace can do so far. Its exit code is the test verdict (0 = pass);
//! each failed check exits with its own code, so the log says which.
//!
//! `usertest crash` dereferences a null pointer instead: the kernel must
//! kill it with SIGSEGV and survive.

#![no_std]
#![no_main]

extern crate alloc;

use libsaeos::{
    ARCH_GET_FS, ARCH_SET_FS, Args, MAP_FIXED, O_CREAT, O_RDONLY, O_TRUNC, O_WRONLY, PROT_EXEC,
    PROT_READ, PROT_WRITE, TIOCGWINSZ, WNOHANG, arch_prctl, brk, chdir, close, dup2, entry, execve,
    exit, exited_with, fork, getcwd, getpid, getppid, ioctl, killed_by, mmap_anonymous, mprotect,
    munmap, open, read, wait4, write,
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
        Some(b"tty") => tty(),
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
    processes();
    library();
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
    // libsaeos's own TLS (errno) is put back at the end.
    let mut runtime_tls = 0u64;
    arch_prctl(ARCH_GET_FS, &raw mut runtime_tls as u64);
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
    arch_prctl(ARCH_SET_FS, runtime_tls);
    if runtime_tls == 0 || fs_word(0) != runtime_tls {
        fail(54, b"libsaeos set no thread pointer (TLS)");
    }
}

/// Spin for about `cycles` TSC cycles (no sleep syscall yet).
fn spin(cycles: u64) {
    let start = rdtsc();
    while rdtsc().wrapping_sub(start) < cycles {
        core::hint::spin_loop();
    }
}

/// wait4 for `pid`; the status word, or fail with `code`.
fn wait_for(pid: i64, code: i32) -> i32 {
    let mut status = 0;
    if wait4(pid, &mut status, 0) != pid {
        fail(code, b"wait4");
    }
    status
}

/// Fork, run `child` in the child (its return value is the exit code),
/// and return the child's pid in the parent.
fn spawn(child: impl FnOnce() -> i32) -> i64 {
    match fork() {
        0 => exit(child()),
        pid if pid > 1 => pid,
        _ => fail(60, b"fork"),
    }
}

/// Written by both sides after fork: copy-on-write keeps them apart.
static mut SHARED: [u64; 512] = [0xa; 512];
/// Filled by a read() in the child: the kernel's copy_to_user must copy
/// this page too rather than write into the parent's.
static mut READ_BUFFER: [u8; 4096] = [0; 4096];

fn free_memory_kb() -> u64 {
    let fd = open(b"/proc/meminfo\0", O_RDONLY);
    let mut text = [0u8; 128];
    let count = read(fd as u32, &mut text).max(0) as usize;
    close(fd as u32);
    // "MemTotal: N kB\nMemFree:  N kB\n": the second number.
    let mut numbers = text[..count]
        .split(|byte| !byte.is_ascii_digit())
        .filter(|digits| !digits.is_empty());
    numbers.nth(1).map_or(0, |digits| {
        digits
            .iter()
            .fold(0, |value, digit| value * 10 + u64::from(digit - b'0'))
    })
}

/// Free memory once exited children's kernel stacks are reaped.
fn settled_free_memory() -> u64 {
    let mut previous = u64::MAX;
    for _ in 0..200 {
        let free = free_memory_kb();
        if free == previous {
            return free;
        }
        previous = free;
        spin(20_000_000);
    }
    previous
}

fn processes() {
    let shared = &raw mut SHARED;
    // fork, getppid, and copy-on-write from both user code and the kernel.
    let child = spawn(|| {
        if getppid() != 1 || getpid() <= 1 {
            return 1;
        }
        unsafe { (*shared)[7] = 0xb };
        if unsafe { (*shared)[7] } != 0xb {
            return 2;
        }
        let fd = open(b"/etc/hello\0", O_RDONLY);
        let buffer =
            unsafe { core::slice::from_raw_parts_mut((&raw mut READ_BUFFER).cast::<u8>(), 4096) };
        if read(fd as u32, buffer) <= 0 || !buffer.starts_with(b"Hello") {
            return 3;
        }
        0
    });
    let status = wait_for(child, 61);
    if exited_with(status) != Some(0) {
        fail(62, b"forked child failed its checks");
    }
    let read_buffer = (&raw const READ_BUFFER).cast::<u8>();
    if unsafe { (*shared)[7] } != 0xa || unsafe { read_buffer.read_volatile() } != 0 {
        fail(63, b"the child's writes reached the parent (copy-on-write)");
    }
    unsafe { (*shared)[7] = 0xc };
    if unsafe { (*shared)[7] } != 0xc {
        fail(64, b"parent cannot write after the child exited");
    }

    // fork + execve: the child becomes /bin/hello and exits 0.
    let child = spawn(|| {
        execve(b"/bin/hello\0", &[b"hello\0"], &[b"FROM=usertest\0"]);
        70
    });
    if exited_with(wait_for(child, 65)) != Some(0) {
        fail(66, b"exec of /bin/hello");
    }
    // A failed execve returns, leaving the caller as it was.
    if execve(b"/no/such/program\0", &[], &[]) != -2 {
        fail(67, b"execve of a missing file is not ENOENT");
    }

    // wait4(-1) gets whichever child exits first; WNOHANG does not block.
    let slow = spawn(|| {
        spin(300_000_000);
        1
    });
    let mut status = 0;
    if wait4(-1, &mut status, WNOHANG) != 0 {
        fail(68, b"WNOHANG returned a running child");
    }
    let fast = spawn(|| 2);
    if wait4(-1, &mut status, 0) != fast || exited_with(status) != Some(2) {
        fail(69, b"wait4(-1) did not return the first child to exit");
    }
    if wait4(-1, &mut status, 0) != slow || exited_with(status) != Some(1) {
        fail(70, b"wait4(-1) second child");
    }
    if wait4(-1, &mut status, 0) != -10 {
        fail(71, b"wait4 without children is not ECHILD");
    }

    // A child that faults is killed; the parent sees the signal.
    let crasher = spawn(|| unsafe { core::ptr::read_volatile(core::ptr::null::<i32>()) });
    if killed_by(wait_for(crasher, 72)) != Some(11) {
        fail(73, b"faulting child not killed by SIGSEGV");
    }

    // Orphans are reparented to init (this process) and collected here.
    let parent = spawn(|| {
        spawn(|| {
            spin(100_000_000);
            5
        });
        3
    });
    if exited_with(wait_for(parent, 74)) != Some(3) {
        fail(75, b"orphan's parent");
    }
    let orphan = wait4(-1, &mut status, 0);
    if orphan <= 1 || exited_with(status) != Some(5) {
        fail(76, b"the orphan was not reparented to init");
    }

    // Current directory and relative paths.
    let mut cwd = [0u8; 64];
    if chdir(b"/etc\0") != 0 || getcwd(&mut cwd) != 5 || &cwd[..5] != b"/etc\0" {
        fail(77, b"chdir /etc");
    }
    let fd = open(b"hello\0", O_RDONLY);
    if fd < 0 || close(fd as u32) != 0 {
        fail(78, b"relative open");
    }
    if chdir(b"..\0") != 0 || getcwd(&mut cwd) != 2 || &cwd[..2] != b"/\0" {
        fail(79, b"chdir ..");
    }

    // dup2 redirects a child's stdout into a file.
    let child = spawn(|| {
        let fd = open(b"/redirect.txt\0", O_WRONLY | O_CREAT | O_TRUNC);
        if fd < 0 || dup2(fd as u32, 1) != 1 {
            return 1;
        }
        write(1, b"redirected\n");
        0
    });
    if exited_with(wait_for(child, 80)) != Some(0) {
        fail(81, b"dup2 child");
    }
    let fd = open(b"/redirect.txt\0", O_RDONLY);
    let mut text = [0u8; 32];
    if read(fd as u32, &mut text) != 11 || &text[..11] != b"redirected\n" {
        fail(82, b"output was not redirected");
    }
    close(fd as u32);

    // fork/exit loops (with copy-on-write copies) give every frame back.
    let cycle = || {
        let child = spawn(|| {
            unsafe { (*shared)[100] = 1 };
            0
        });
        wait_for(child, 83);
    };
    (0..5).for_each(|_| cycle());
    let before = settled_free_memory();
    (0..30).for_each(|_| cycle());
    let after = settled_free_memory();
    if before != after {
        fail(84, b"fork/exit leaked memory");
    }
}

/// `usertest tty`: read typed lines from the console (fd 0), as edited by
/// the kernel's line discipline. The test types "hello tty", Enter,
/// "abx", Backspace, "c", Enter, then Ctrl-D, on the keyboard or serial.
fn tty() -> ! {
    say(b"usertest: tty ready\n");
    let mut line = [0u8; 128];
    let mut reads = |expected: &[u8]| {
        let count = read(0, &mut line);
        count >= 0 && &line[..count as usize] == expected
    };
    if !reads(b"hello tty\n") {
        fail(90, b"first line");
    }
    if !reads(b"abc\n") {
        fail(91, b"edited line (backspace)");
    }
    if !reads(b"") {
        fail(92, b"Ctrl-D is not end of file");
    }
    let mut size = [0u16; 4];
    if ioctl(0, TIOCGWINSZ, size.as_mut_ptr() as u64) != 0 || size[0] == 0 || size[1] == 0 {
        fail(93, b"TIOCGWINSZ on the console");
    }
    let fd = open(b"/etc/hello\0", O_RDONLY);
    if ioctl(fd as u32, TIOCGWINSZ, size.as_mut_ptr() as u64) != -25 {
        fail(94, b"a regular file is a terminal");
    }
    say(b"usertest: tty PASS\n");
    exit(0)
}

// ---------------------------------------------------------------------------
// libsaeos: the Rust-facing library and its C interface.

use libsaeos::prelude::*;
use libsaeos::{env, fs, process};

unsafe extern "C" {
    fn malloc(size: usize) -> *mut u8;
    fn calloc(count: usize, size: usize) -> *mut u8;
    fn realloc(block: *mut u8, size: usize) -> *mut u8;
    fn free(block: *mut u8);
    fn strlen(text: *const core::ffi::c_char) -> usize;
    fn strcmp(left: *const core::ffi::c_char, right: *const core::ffi::c_char) -> i32;
    fn getenv(name: *const core::ffi::c_char) -> *const core::ffi::c_char;
    fn memcmp(
        left: *const core::ffi::c_void,
        right: *const core::ffi::c_void,
        length: usize,
    ) -> i32;
    #[link_name = "open"]
    fn c_open(path: *const core::ffi::c_char, flags: i32, mode: u32) -> i32;
    fn __errno_location() -> *mut i32;
}

fn library() {
    // The allocator: many small blocks, growth, a large (mmap'd) block.
    let mut numbers: Vec<u64> = Vec::new();
    for value in 0..100_000u64 {
        numbers.push(value * 3);
    }
    if numbers.iter().sum::<u64>() != 3 * (99_999 * 100_000 / 2) {
        fail(100, b"Vec contents");
    }
    let words: Vec<String> = (0..2000).map(|index| format!("word-{index}")).collect();
    if words[1234] != "word-1234" || words.concat().len() < 2000 * 6 {
        fail(101, b"String formatting");
    }
    drop(words);
    let large = vec![0xa5u8; 1 << 20];
    if large.iter().any(|&byte| byte != 0xa5) {
        fail(102, b"large allocation");
    }
    drop(large);
    let boxed = Box::new([7u64; 512]);
    if boxed.iter().sum::<u64>() != 7 * 512 {
        fail(103, b"Box");
    }

    // Files and directories.
    if fs::write("/libsaeos.txt", b"line one\nline two\n").is_err() {
        fail(104, b"fs::write");
    }
    if fs::read_to_string("/libsaeos.txt").as_deref() != Ok("line one\nline two\n") {
        fail(105, b"fs::read_to_string");
    }
    let entries = fs::read_dir("/etc").unwrap_or_default();
    if !entries
        .iter()
        .any(|entry| entry.name == "hello" && entry.file_type == fs::FileType::File)
    {
        fail(106, b"read_dir /etc");
    }
    if fs::metadata("/libsaeos.txt").map(|metadata| metadata.size) != Ok(18)
        || fs::metadata("/etc").map(|metadata| metadata.file_type) != Ok(fs::FileType::Directory)
    {
        fail(107, b"metadata");
    }
    if fs::read_to_string("/no/such/file") != Err(Errno::ENOENT) {
        fail(108, b"missing file is not ENOENT");
    }
    if fs::remove_file("/libsaeos.txt").is_err() || fs::metadata("/libsaeos.txt").is_ok() {
        fail(109, b"remove_file");
    }

    // Arguments and environment (init gets HOME, PATH, TERM).
    if env::args().next().is_none()
        || env::var("PATH") != Some("/bin:/sbin")
        || env::var("NOPE").is_some()
    {
        fail(110, b"env");
    }

    // A child runs a closure-like task through fork + exec + wait.
    match process::fork() {
        Ok(process::Fork::Child) => {
            let error = process::exec_inherit("/bin/hello", &["hello"]);
            eprintln!("exec failed: {error}");
            process::exit(9);
        }
        Ok(process::Fork::Parent(pid)) => {
            if process::wait(Some(pid)).map(|(_, status)| status.success()) != Ok(true) {
                fail(111, b"process::wait");
            }
        }
        Err(_) => fail(112, b"process::fork"),
    }
    if process::current_dir().as_deref() != Ok("/") {
        fail(113, b"current_dir");
    }

    // The C interface: malloc family, strings, getenv, errno (in TLS).
    unsafe {
        let block = malloc(100);
        if block.is_null() || !(block as usize).is_multiple_of(16) {
            fail(114, b"malloc");
        }
        core::ptr::write_bytes(block, 0x42, 100);
        let grown = realloc(block, 100_000);
        if grown.is_null() || *grown.add(99) != 0x42 {
            fail(115, b"realloc kept the contents");
        }
        free(grown);
        let zeroed = calloc(1000, 8);
        if zeroed.is_null() || (0..8000).any(|index| *zeroed.add(index) != 0) {
            fail(116, b"calloc");
        }
        free(zeroed);
        free(core::ptr::null_mut());
        let hello = c"hello";
        if strlen(hello.as_ptr()) != 5 || strcmp(hello.as_ptr(), c"help".as_ptr()) >= 0 {
            fail(117, b"strlen/strcmp");
        }
        if memcmp(b"abc".as_ptr().cast(), b"abd".as_ptr().cast(), 3) >= 0 {
            fail(118, b"memcmp");
        }
        let path = getenv(c"PATH".as_ptr());
        if path.is_null()
            || strcmp(path, c"/bin:/sbin".as_ptr()) != 0
            || !getenv(c"NOPE".as_ptr()).is_null()
        {
            fail(119, b"getenv");
        }
        *__errno_location() = 0;
        if c_open(c"/no/such/file".as_ptr(), 0, 0) != -1 || *__errno_location() != 2 {
            fail(120, b"open/errno");
        }
    }
    println!(
        "usertest: libsaeos {} numbers, {} directory entries in /etc",
        numbers.len(),
        entries.len()
    );
}
