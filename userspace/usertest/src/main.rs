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
    // Spin in user mode long enough for timer preemptions to land here.
    let start = rdtsc();
    while rdtsc().wrapping_sub(start) < 400_000_000 {
        core::hint::spin_loop();
    }
    say(b"usertest: PASS\n");
    0
}
