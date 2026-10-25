//! Unattended boot test (`boot-test` feature).
//!
//! Replaces the shell task: runs the memory self-tests, checks the
//! interrupt-safety of locks shared with interrupt handlers, runs the
//! scheduler runtime self-test, and ends the QEMU run with the verdict.
//! Panics and faults end the run as failures (see the panic handler and the
//! exception handlers).

mod dma_msi;
mod pci;
mod user;

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
    let breakpoint_passed = breakpoint_test();
    let cache_passed = cache_attributes_test();
    // Everything printed so far must also be in the kernel log ring.
    let log_passed = crate::klog::contains("boot-test: starting.");
    let fs_passed = filesystem_test();
    let pci_passed = pci::pci_test();
    let dma_msi_passed = dma_msi::dma_msi_test();
    let gdt_passed = user::gdt_test();
    let fpu_passed = user::fpu_test();
    let space_passed = user::process_space_test();
    let uaccess_passed = user::uaccess_test();
    let syscall_passed = user::syscall_test();
    let exec_passed = user::exec_test();
    let user_memory_passed = user::user_memory_test();
    let wait_passed = wait_event_test();
    let idle_passed = idle_test();
    let scheduler_passed = scheduler_test();
    let passed = mm_passed
        && irq_passed
        && breakpoint_passed
        && cache_passed
        && log_passed
        && fs_passed
        && pci_passed
        && dma_msi_passed
        && gdt_passed
        && fpu_passed
        && space_passed
        && uaccess_passed
        && syscall_passed
        && exec_passed
        && user_memory_passed
        && wait_passed
        && idle_passed
        && scheduler_passed;
    serial_println!(
        "boot-test: memory={} irq-locks={} breakpoint={} cache={} klog={} fs={} pci={} dma-msi={} gdt-tss={} fpu={} process-space={} uaccess={} syscalls={} exec-prepare={} user-memory={} wait-event={} idle={} scheduler={} -> {}",
        verdict(mm_passed),
        verdict(irq_passed),
        verdict(breakpoint_passed),
        verdict(cache_passed),
        verdict(log_passed),
        verdict(fs_passed),
        verdict(pci_passed),
        verdict(dma_msi_passed),
        verdict(gdt_passed),
        verdict(fpu_passed),
        verdict(space_passed),
        verdict(uaccess_passed),
        verdict(syscall_passed),
        verdict(exec_passed),
        verdict(user_memory_passed),
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

/// The PAT holds the kernel's layout, device registers are uncached, and the
/// framebuffer is write-combining.
fn cache_attributes_test() -> bool {
    use crate::mm::PhysAddr;
    use crate::mm::paging::{self, CacheType};
    let pat = paging::pat_value();
    let pat_ok = pat.is_none_or(|value| value == paging::expected_pat_value());
    let lapic = {
        let controller = interrupt_controller::CONTROLLER.lock();
        (controller.mode == interrupt_controller::ControllerKind::Apic)
            .then(|| controller.lapic.base_address())
    };
    let lapic_ok = lapic.is_none_or(|base| {
        paging::cache_type_of(paging::phys_to_virt(PhysAddr(base))) == Some(CacheType::Uncached)
    });
    #[cfg(feature = "framebuffer")]
    let gop = {
        let writer = drivers::framebuffer::GOP_WRITER.lock();
        writer.is_active().then(|| writer.mapped_base())
    };
    #[cfg(not(feature = "framebuffer"))]
    let gop: Option<u64> = None;
    let display_ok = match gop {
        Some(base) => {
            paging::cache_type_of(crate::mm::VirtAddr(base & !0xfff))
                == Some(CacheType::WriteCombining)
        }
        None => paging::cache_type_of(paging::phys_to_virt(PhysAddr(0xb8000)))
            .is_none_or(|kind| kind == CacheType::Uncached),
    };
    if !(pat_ok && lapic_ok && display_ok) {
        serial_println!(
            "boot-test: cache pat={:x?} lapic_ok={} display_ok={}",
            pat,
            lapic_ok,
            display_ok
        );
    }
    pat_ok && lapic_ok && display_ok
}

fn fnv1a(hash: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(hash, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

/// Read a whole file through the fd API in awkward chunk sizes.
fn read_file(path: &str) -> Result<alloc::vec::Vec<u8>, crate::fs::Errno> {
    use crate::fs::{self, OpenFlags};
    let fd = fs::open(path, OpenFlags::READ)?;
    let mut contents = alloc::vec::Vec::new();
    let mut buffer = [0u8; 4096];
    let result = (|| {
        for chunk in [1usize, 7, 509, 4096].iter().cycle() {
            match fs::read(fd, &mut buffer[..*chunk])? {
                0 => return Ok(()),
                count => contents.extend_from_slice(&buffer[..count]),
            }
        }
        Ok(())
    })();
    fs::close(fd)?;
    result.map(|()| contents)
}

/// Every file listed in the test image's `/test/MANIFEST` (written by
/// `mkinitramfs --test-fixtures` as `path size fnv1a-hex` lines).
fn initramfs_manifest_test() -> bool {
    let manifest = match read_file("/test/MANIFEST") {
        Ok(manifest) => manifest,
        Err(error) => {
            serial_println!(
                "boot-test: fs: /test/MANIFEST: {} (booted without the test initramfs?)",
                error
            );
            return false;
        }
    };
    let Ok(manifest) = core::str::from_utf8(&manifest) else {
        return false;
    };
    let mut checked = 0;
    for line in manifest.lines() {
        let mut fields = line.split(' ');
        let (Some(path), Some(size), Some(hash)) = (fields.next(), fields.next(), fields.next())
        else {
            serial_println!("boot-test: fs: bad manifest line {:?}", line);
            return false;
        };
        let contents = match read_file(path) {
            Ok(contents) => contents,
            Err(error) => {
                serial_println!("boot-test: fs: cannot read {}: {}", path, error);
                return false;
            }
        };
        let digest = fnv1a(FNV_OFFSET, &contents);
        if size.parse() != Ok(contents.len()) || u64::from_str_radix(hash, 16) != Ok(digest) {
            serial_println!(
                "boot-test: fs: {} size {} hash {:016x} (want {} {})",
                path,
                contents.len(),
                digest,
                size,
                hash
            );
            return false;
        }
        checked += 1;
    }
    checked != 0
}

/// `/proc` reflects the boot command line and the mount table. The test
/// runner passes `saeos.test=boot` on the command line.
fn procfs_test() -> bool {
    let cmdline = read_file("/proc/cmdline").unwrap_or_default();
    let cmdline_ok = cmdline.ends_with(b"\n")
        && cmdline[..cmdline.len().saturating_sub(1)] == *crate::cmdline::raw().as_bytes()
        && crate::cmdline::params().get("saeos.test") == Some("boot");
    let mounts = read_file("/proc/mounts").unwrap_or_default();
    let mounts = core::str::from_utf8(&mounts).unwrap_or("");
    let mounts_ok = [
        "ramfs / ramfs rw 0 0",
        "devfs /dev devfs rw 0 0",
        "proc /proc proc rw 0 0",
    ]
    .iter()
    .all(|line| mounts.lines().any(|mounted| mounted == *line));
    let read_only = crate::fs::open("/proc/cmdline", crate::fs::OpenFlags::WRITE).and_then(|fd| {
        let result = crate::fs::write(fd, b"x");
        crate::fs::close(fd)?;
        result
    }) == Err(crate::fs::Errno::EROFS);
    if !(cmdline_ok && mounts_ok && read_only) {
        serial_println!(
            "boot-test: fs: /proc/cmdline {:?} /proc/mounts {:?} read-only={}",
            alloc::string::String::from_utf8_lossy(&cmdline),
            mounts,
            read_only
        );
    }
    cmdline_ok && mounts_ok && read_only
}

/// Filesystem checks through the same fd API userspace will use: the
/// initramfs the bootloader loaded, a write/read round trip, device files,
/// procfs, and concurrent tasks.
fn filesystem_test() -> bool {
    use crate::fs::{self, Errno, OpenFlags, Whence};
    let check = |name: &str, passed: bool| {
        if !passed {
            serial_println!("boot-test: fs: {} FAILED", name);
        }
        passed
    };

    // (a) Every initramfs file; the archive's memory was released after
    // unpacking (and so was every other boot-time hold).
    let manifest = initramfs_manifest_test();
    let held = crate::mm::frame::held_frames();
    if held != 0 {
        serial_println!("boot-test: fs: {} boot-time frames still held", held);
    }

    // (b) Round trip, including reading on after the name is gone.
    let round_trip = (|| -> Result<bool, Errno> {
        let fd = fs::open(
            "/boot-test.tmp",
            OpenFlags::READ_WRITE | OpenFlags::CREATE | OpenFlags::EXCL,
        )?;
        let pattern: alloc::vec::Vec<u8> = (0..10_000u32).map(|i| (i * 13) as u8).collect();
        let written = fs::write(fd, &pattern)? == pattern.len();
        fs::lseek(fd, 1234, Whence::Set)?;
        let mut buffer = [0u8; 100];
        let read_back = fs::read(fd, &mut buffer)? == 100 && buffer[..] == pattern[1234..1334];
        fs::unlink("/boot-test.tmp")?;
        let gone = fs::stat("/boot-test.tmp") == Err(Errno::ENOENT);
        let still_readable = fs::read(fd, &mut buffer)? == 100 && buffer[..] == pattern[1334..1434];
        fs::close(fd)?;
        Ok(written && read_back && gone && still_readable)
    })()
    .unwrap_or(false);

    // (d) Device files.
    let devices = (|| -> Result<bool, Errno> {
        let null = fs::open("/dev/null", OpenFlags::READ_WRITE)?;
        let mut buffer = [7u8; 16];
        let null_ok = fs::read(null, &mut buffer)? == 0 && fs::write(null, b"x")? == 1;
        fs::close(null)?;
        let zero = fs::open("/dev/zero", OpenFlags::READ)?;
        let zero_ok = fs::read(zero, &mut buffer)? == 16 && buffer == [0; 16];
        fs::close(zero)?;
        let console = fs::open("/dev/console", OpenFlags::WRITE)?;
        fs::write(console, b"boot-test: written through /dev/console\n")?;
        fs::close(console)?;
        let console_ok = crate::klog::contains("written through /dev/console");
        Ok(null_ok && zero_ok && console_ok)
    })()
    .unwrap_or(false);

    // (c) Concurrent tasks, each with its own process file table.
    let concurrent = concurrent_files_test();

    check("initramfs manifest", manifest)
        & check("initramfs memory released", held == 0)
        & check("procfs", procfs_test())
        & check("round trip", round_trip)
        & check("devices", devices)
        & check("concurrency", concurrent)
}

static FS_WORKERS_DONE: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
static FS_WORKERS_FAILED: AtomicBool = AtomicBool::new(false);
const FS_WORKERS: usize = 5;

fn fs_worker(index: usize) {
    use crate::fs::{self, OpenFlags};
    let path = alloc::format!("/worker-{index}");
    for round in 0..50usize {
        let data = alloc::vec![(index * 31 + round) as u8; 200 + round * 17];
        let result = (|| -> Result<bool, crate::fs::Errno> {
            let fd = fs::open(
                &path,
                OpenFlags::READ_WRITE | OpenFlags::CREATE | OpenFlags::TRUNC,
            )?;
            fs::write(fd, &data)?;
            fs::lseek(fd, 0, crate::fs::Whence::Set)?;
            let mut back = alloc::vec![0u8; data.len()];
            let mut filled = 0;
            while filled < back.len() {
                let count = fs::read(fd, &mut back[filled..])?;
                if count == 0 {
                    break;
                }
                filled += count;
            }
            fs::close(fd)?;
            Ok(back == data)
        })();
        if result != Ok(true) {
            serial_println!(
                "boot-test: fs worker {} round {}: {:?}",
                index,
                round,
                result
            );
            FS_WORKERS_FAILED.store(true, Ordering::Release);
        }
        scheduler::yield_now();
    }
    let _ = fs::unlink(&path);
}

fn fs_reader() {
    use crate::fs::{self, OpenFlags};
    for _ in 0..100 {
        let mut buffer = [0u8; 64];
        let ok = fs::open("/etc/hello", OpenFlags::READ)
            .and_then(|fd| {
                let count = fs::read(fd, &mut buffer);
                fs::close(fd)?;
                count
            })
            .is_ok_and(|count| buffer[..count].starts_with(b"Hello from the SaeOS initramfs!"));
        if !ok {
            FS_WORKERS_FAILED.store(true, Ordering::Release);
        }
        scheduler::yield_now();
    }
}

macro_rules! fs_worker_task {
    ($name:ident, $index:expr) => {
        fn $name() -> ! {
            fs_worker($index);
            FS_WORKERS_DONE.fetch_add(1, Ordering::AcqRel);
            scheduler::exit(ExitStatus(0))
        }
    };
}
fs_worker_task!(fs_worker_0, 0);
fs_worker_task!(fs_worker_1, 1);
fs_worker_task!(fs_worker_2, 2);
fs_worker_task!(fs_worker_3, 3);

fn fs_reader_task() -> ! {
    fs_reader();
    FS_WORKERS_DONE.fetch_add(1, Ordering::AcqRel);
    scheduler::exit(ExitStatus(0))
}

fn concurrent_files_test() -> bool {
    for (entry, name) in [
        (fs_worker_0 as crate::task::TaskEntry, "fs-worker-0"),
        (fs_worker_1, "fs-worker-1"),
        (fs_worker_2, "fs-worker-2"),
        (fs_worker_3, "fs-worker-3"),
        (fs_reader_task, "fs-reader"),
    ] {
        scheduler::spawn_kernel_task(entry, name);
    }
    let deadline = time::ticks().saturating_add(10_000);
    while FS_WORKERS_DONE.load(Ordering::Acquire) < FS_WORKERS && time::ticks() < deadline {
        scheduler::sleep_ms(10);
    }
    let done = FS_WORKERS_DONE.load(Ordering::Acquire);
    let failed = FS_WORKERS_FAILED.load(Ordering::Acquire);
    if done != FS_WORKERS || failed {
        serial_println!(
            "boot-test: fs concurrency done={}/{} failed={}",
            done,
            FS_WORKERS,
            failed
        );
    }
    done == FS_WORKERS && !failed
}

/// `int3` must be reported and resumed, not halt the kernel.
fn breakpoint_test() -> bool {
    let before = crate::arch::x86_64::idt::breakpoints_handled();
    unsafe { core::arch::asm!("int3", options(nomem, nostack)) };
    crate::arch::x86_64::idt::breakpoints_handled() == before + 1
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
