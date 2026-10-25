//! Userspace foundations, tested entirely in ring 0: segments and the TSS,
//! per-task FPU state, user memory access, process address spaces,
//! syscalls, and exec image preparation.

use crate::arch::x86_64::gdt;
use crate::task::ExitStatus;
use crate::{scheduler, serial_println, time};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// The GDT holds the user segments in `SYSRET` order and TR names the TSS.
pub fn gdt_test() -> bool {
    let (entries, task_register) = gdt::installed();
    let expected = [
        0,
        gdt::KERNEL_CODE_DESCRIPTOR,
        gdt::KERNEL_DATA_DESCRIPTOR,
        gdt::USER_DATA_DESCRIPTOR,
        gdt::USER_CODE_DESCRIPTOR,
    ];
    // The CPU sets the Accessed bit when it loads a segment.
    const ACCESSED: u64 = 1 << 40;
    let segments_ok = entries[..5]
        .iter()
        .zip(expected)
        .all(|(&entry, expected)| entry & !ACCESSED == expected);
    // TSS descriptor: present, 64-bit TSS, marked busy (0xb) once loaded.
    let tss_ok = (entries[5] >> 40) & 0xff == 0x8b && task_register == gdt::TSS_SELECTOR;
    if !(segments_ok && tss_ok) {
        serial_println!(
            "boot-test: gdt entries={:x?} tr={:#x}",
            entries,
            task_register
        );
    }
    segments_ok && tss_ok
}

/// The current task's kernel stack top, which TSS.rsp0 must name.
fn own_stack_top() -> u64 {
    let task = scheduler::current().expect("test runs in a task");
    scheduler::with_scheduler(|scheduler| {
        scheduler
            .task(task)
            .map(|task| task.kernel_stack.top)
            .unwrap_or(0)
    })
}

/// Register image the FPU tests load and compare: xmm0-15, MXCSR, and the
/// x87 control word.
#[repr(C, align(16))]
#[derive(Clone, Copy, PartialEq, Eq)]
struct FpuRegisters {
    xmm: [[u8; 16]; 16],
    mxcsr: u32,
    control_word: u16,
}

impl FpuRegisters {
    fn pattern(seed: u8) -> Self {
        let mut xmm = [[0u8; 16]; 16];
        for (register, bytes) in xmm.iter_mut().enumerate() {
            for (index, byte) in bytes.iter_mut().enumerate() {
                *byte = seed
                    .wrapping_mul(31)
                    .wrapping_add((register * 16 + index) as u8);
            }
        }
        Self {
            xmm,
            // Distinct rounding modes, all exceptions still masked.
            mxcsr: 0x1f80 | (u32::from(seed & 3) << 13),
            // Distinct precision control, all exceptions masked.
            control_word: 0x007f | (u16::from(seed & 3) << 8),
        }
    }

    /// Load into the CPU. The kernel is soft-float, so nothing else uses
    /// these registers; only a context switch can change them.
    fn load(&self) {
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
                "fldcw [{p} + 260]",
                p = in(reg) self,
                options(nostack, readonly, preserves_flags),
            );
        }
    }

    fn read() -> Self {
        let mut registers = Self {
            xmm: [[0; 16]; 16],
            mxcsr: 0,
            control_word: 0,
        };
        unsafe {
            core::arch::asm!(
                "movdqu [{p}], xmm0", "movdqu [{p} + 16], xmm1",
                "movdqu [{p} + 32], xmm2", "movdqu [{p} + 48], xmm3",
                "movdqu [{p} + 64], xmm4", "movdqu [{p} + 80], xmm5",
                "movdqu [{p} + 96], xmm6", "movdqu [{p} + 112], xmm7",
                "movdqu [{p} + 128], xmm8", "movdqu [{p} + 144], xmm9",
                "movdqu [{p} + 160], xmm10", "movdqu [{p} + 176], xmm11",
                "movdqu [{p} + 192], xmm12", "movdqu [{p} + 208], xmm13",
                "movdqu [{p} + 224], xmm14", "movdqu [{p} + 240], xmm15",
                "stmxcsr [{p} + 256]",
                "fnstcw [{p} + 260]",
                p = in(reg) &raw mut registers,
                options(nostack, preserves_flags),
            );
        }
        registers
    }
}

const FPU_TASKS: usize = 3;
const FPU_YIELDS: usize = 1000;
const FPU_PREEMPTIONS: u64 = 50;
static FPU_DONE: AtomicUsize = AtomicUsize::new(0);
static FPU_FAILED: AtomicBool = AtomicBool::new(false);

fn fail(reason: &str, seed: u8) {
    serial_println!("boot-test: fpu task {}: {}", seed, reason);
    FPU_FAILED.store(true, Ordering::Release);
}

/// Load a pattern, then keep checking it across voluntary switches and
/// timer preemptions while the other pattern task does the same. Also
/// checks TSS.rsp0 names this task's stack after every switch.
fn fpu_pattern_worker(seed: u8) {
    let expected = FpuRegisters::pattern(seed);
    expected.load();
    let stack_top = own_stack_top();
    for _ in 0..FPU_YIELDS {
        scheduler::yield_now();
        if FpuRegisters::read() != expected {
            return fail("registers changed across yield", seed);
        }
        if gdt::kernel_stack() != stack_top {
            return fail("TSS.rsp0 is not this task's stack", seed);
        }
    }
    let preemptions = || scheduler::stats().timer_preemptions;
    let start = preemptions();
    let deadline = time::ticks().saturating_add(10_000);
    while preemptions() < start + FPU_PREEMPTIONS && time::ticks() < deadline {
        if FpuRegisters::read() != expected {
            return fail("registers changed across preemption", seed);
        }
        if gdt::kernel_stack() != stack_top {
            return fail("TSS.rsp0 is not this task's stack (preempted)", seed);
        }
    }
    if preemptions() < start + FPU_PREEMPTIONS {
        fail("not enough timer preemptions", seed);
    }
}

fn fpu_task_a() -> ! {
    fpu_pattern_worker(1);
    FPU_DONE.fetch_add(1, Ordering::AcqRel);
    scheduler::exit(ExitStatus(0))
}

fn fpu_task_b() -> ! {
    fpu_pattern_worker(2);
    FPU_DONE.fetch_add(1, Ordering::AcqRel);
    scheduler::exit(ExitStatus(0))
}

/// A new task must start from the clean state, not from whatever the
/// previous task left in the registers.
fn fpu_fresh_task() -> ! {
    let initial = FpuRegisters::read();
    if initial.xmm != [[0; 16]; 16] || initial.mxcsr != 0x1f80 || initial.control_word != 0x037f {
        fail("new task did not start from the clean state", 0);
    }
    FPU_DONE.fetch_add(1, Ordering::AcqRel);
    scheduler::exit(ExitStatus(0))
}

pub fn fpu_test() -> bool {
    // Dirty this task's registers first, so a missing restore would leak
    // them into the fresh task.
    FpuRegisters::pattern(7).load();
    for (entry, name) in [
        (fpu_task_a as crate::task::TaskEntry, "fpu-a"),
        (fpu_task_b, "fpu-b"),
        (fpu_fresh_task, "fpu-fresh"),
    ] {
        scheduler::spawn_kernel_task(entry, name);
    }
    let deadline = time::ticks().saturating_add(20_000);
    while FPU_DONE.load(Ordering::Acquire) < FPU_TASKS && time::ticks() < deadline {
        scheduler::sleep_ms(10);
    }
    let own_ok = FpuRegisters::read() == FpuRegisters::pattern(7);
    if !own_ok {
        serial_println!("boot-test: fpu: the boot-test task's own registers changed");
    }
    let done = FPU_DONE.load(Ordering::Acquire);
    if done != FPU_TASKS {
        serial_println!("boot-test: fpu: {}/{} tasks finished", done, FPU_TASKS);
    }
    done == FPU_TASKS && !FPU_FAILED.load(Ordering::Acquire) && own_ok
}

// ---------------------------------------------------------------------------
// Process address spaces and user memory access.

use crate::mm::VirtAddr;
use crate::mm::paging::{self, AddressSpace, PageFlags};
use crate::mm::{frame, uaccess};

/// Where the tests map user pages.
const USER_BASE: u64 = 0x4000_0000;
const PAGE: u64 = 4096;

/// A fresh address space with `writable` RW pages at `USER_BASE`, then one
/// unmapped page (a hole), then `read_only` RO pages.
fn test_space(writable: u64, read_only: u64) -> AddressSpace {
    let space = AddressSpace::new().expect("address space");
    let map = |index: u64, flags: PageFlags| {
        let frame = frame::alloc_frame_zeroed().expect("user frame");
        space
            .map_owned(VirtAddr(USER_BASE + index * PAGE), frame, flags)
            .expect("map user page");
    };
    for index in 0..writable {
        map(index, PageFlags::USER_READ_WRITE);
    }
    for index in 0..read_only {
        map(writable + 1 + index, PageFlags::USER_READ);
    }
    space
}

static EXPECTED_ROOT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

fn space_child() -> ! {
    let ok = paging::current_root().0 == EXPECTED_ROOT.load(Ordering::Acquire);
    scheduler::exit(ExitStatus(if ok { 7 } else { 1 }))
}

/// Spawn a task in its own address space, wait for it, return its status.
fn run_in_space(entry: crate::task::TaskEntry, name: &'static str, space: AddressSpace) -> i64 {
    EXPECTED_ROOT.store(space.root_frame().0, Ordering::Release);
    let (_, process) = scheduler::spawn_in_new_process(entry, name, space);
    match scheduler::wait_child(process) {
        Ok(status) => status.0 as i64,
        Err(error) => {
            serial_println!("boot-test: wait for {} failed: {:?}", name, error);
            -1
        }
    }
}

/// A task runs with its own CR3, its parent collects its exit status, and
/// reaping frees every page-table and user frame of the space.
pub fn process_space_test() -> bool {
    let cycle = || run_in_space(space_child, "space-child", test_space(16, 0));
    // The first cycle may grow the heap; measure the second.
    let warm = cycle();
    let before = frame::stats().free_frames;
    let status = cycle();
    let after = frame::stats().free_frames;
    if warm != 7 || status != 7 || before != after {
        serial_println!(
            "boot-test: process space status={}/{} free frames {} -> {}",
            warm,
            status,
            before,
            after
        );
    }
    warm == 7 && status == 7 && before == after
}

/// Each failed check sets one bit of the child's exit status.
static UACCESS_FAILURES: AtomicUsize = AtomicUsize::new(0);

fn check(bit: usize, passed: bool) {
    if !passed {
        UACCESS_FAILURES.fetch_or(1 << bit, Ordering::AcqRel);
    }
}

/// Runs in a space with two RW pages, a hole, then one RO page.
fn uaccess_child() -> ! {
    use crate::fs::Errno;
    // (0) Round trip across the page boundary.
    let pattern: alloc::vec::Vec<u8> = (0..5000u32).map(|index| (index * 7) as u8).collect();
    check(0, uaccess::copy_to_user(USER_BASE + 100, &pattern).is_ok());
    let mut back = alloc::vec![0u8; pattern.len()];
    check(
        1,
        uaccess::copy_from_user(&mut back, USER_BASE + 100).is_ok() && back == pattern,
    );
    // (2) A copy running into the hole stops exactly at it.
    let mut buffer = [0u8; 150];
    check(
        2,
        uaccess::copy_from_user_partial(&mut buffer, USER_BASE + 2 * PAGE - 50) == 100,
    );
    // (3) Unmapped, kernel, and wrapping addresses are EFAULT.
    let kernel_address = &raw const UACCESS_FAILURES as u64;
    check(
        3,
        uaccess::copy_from_user(&mut buffer, USER_BASE + 2 * PAGE) == Err(Errno::EFAULT)
            && uaccess::copy_from_user(&mut buffer, kernel_address) == Err(Errno::EFAULT)
            && uaccess::copy_to_user(kernel_address, &buffer) == Err(Errno::EFAULT)
            && uaccess::copy_from_user(&mut buffer, u64::MAX - 10) == Err(Errno::EFAULT),
    );
    // (4) The read-only page is readable but not writable.
    let read_only = USER_BASE + 3 * PAGE;
    check(
        4,
        uaccess::copy_from_user(&mut buffer, read_only).is_ok()
            && uaccess::copy_to_user(read_only, b"x") == Err(Errno::EFAULT),
    );
    // (5) Strings across the page boundary; a missing NUL runs into the
    // hole (EFAULT) or past the limit (ENAMETOOLONG).
    let memory = uaccess::KernelUserMemory;
    use uaccess::UserMemory;
    let path = b"/etc/across/the/boundary\0";
    let start = USER_BASE + PAGE - 10;
    check(5, memory.write(start, path).is_ok());
    check(
        6,
        memory.read_cstr(start, 4096).as_deref() == Ok("/etc/across/the/boundary"),
    );
    check(
        7,
        memory.write(USER_BASE + 2 * PAGE - 20, &[b'x'; 20]).is_ok(),
    );
    check(
        8,
        memory.read_cstr(USER_BASE + 2 * PAGE - 20, 4096) == Err(Errno::EFAULT),
    );
    check(9, memory.read_cstr(start, 5) == Err(Errno::ENAMETOOLONG));
    // (10) SMAP: a kernel read without `stac` faults (and is fixed up);
    // without SMAP the same read succeeds.
    check(
        10,
        uaccess::probe_without_stac(USER_BASE) != uaccess::smap_enabled(),
    );
    let failures = UACCESS_FAILURES.load(Ordering::Acquire);
    scheduler::exit(ExitStatus(failures as i32))
}

pub fn uaccess_test() -> bool {
    let status = run_in_space(uaccess_child, "uaccess-child", test_space(2, 1));
    if status != 0 {
        serial_println!("boot-test: uaccess failed checks (bit mask) {:#b}", status);
    }
    serial_println!(
        "boot-test: uaccess smap={} smep={}",
        uaccess::smap_enabled(),
        uaccess::smep_enabled()
    );
    status == 0
}

// ---------------------------------------------------------------------------
// Syscalls, through the real table with real user pointers.

use crate::syscall::{abi, dispatch_current, numbers as nr};

fn syscall(number: u64, args: &[u64]) -> i64 {
    let mut all = [0u64; 6];
    all[..args.len()].copy_from_slice(args);
    dispatch_current(number, all)
}

fn errno(error: crate::fs::Errno) -> i64 {
    -i64::from(error.code())
}

/// Runs in a space with four RW pages at USER_BASE (then nothing mapped).
fn syscall_child() -> ! {
    use crate::fs::Errno;
    UACCESS_FAILURES.store(0, Ordering::Release);
    let path = USER_BASE;
    let buffer = USER_BASE + PAGE;
    check(0, uaccess::copy_to_user(path, b"/etc/hello\0").is_ok());
    let fd = syscall(nr::OPEN, &[path, abi::O_RDONLY]);
    check(1, fd >= 0);
    let count = syscall(nr::READ, &[fd as u64, buffer, 64]);
    let mut text = [0u8; 64];
    check(
        2,
        count > 0
            && uaccess::copy_from_user(&mut text[..count as usize], buffer).is_ok()
            && text.starts_with(b"Hello from the SaeOS initramfs!"),
    );
    // Bad buffers fail without consuming the file.
    check(3, syscall(nr::LSEEK, &[fd as u64, 0, 0]) == 0);
    check(
        4,
        syscall(nr::READ, &[fd as u64, 0xffff_8000_0000_0000, 4]) == errno(Errno::EFAULT),
    );
    check(
        5,
        syscall(nr::READ, &[fd as u64, USER_BASE + 8 * PAGE, 4]) == errno(Errno::EFAULT),
    );
    check(6, syscall(nr::READ, &[fd as u64, buffer, 5]) == 5);
    check(7, syscall(nr::CLOSE, &[fd as u64]) == 0);
    check(8, syscall(nr::CLOSE, &[fd as u64]) == errno(Errno::EBADF));
    // Console output from user memory.
    check(9, uaccess::copy_to_user(path, b"/dev/console\0").is_ok());
    let console = syscall(nr::OPEN, &[path, abi::O_WRONLY]);
    let message = b"boot-test: written by the write syscall\n";
    check(10, uaccess::copy_to_user(buffer, message).is_ok());
    check(
        11,
        syscall(nr::WRITE, &[console as u64, buffer, message.len() as u64]) == message.len() as i64,
    );
    check(12, crate::klog::contains("written by the write syscall"));
    check(13, syscall(999, &[]) == errno(Errno::ENOSYS));
    check(14, syscall(nr::GETPID, &[]) > 0);
    // wait4 on a grandchild, with the status written to user memory.
    let grandchild = test_space(1, 0);
    EXPECTED_ROOT.store(grandchild.root_frame().0, Ordering::Release);
    let (_, pid) = scheduler::spawn_in_new_process(space_child, "grandchild", grandchild);
    let status = USER_BASE + 2 * PAGE;
    check(
        15,
        syscall(nr::WAIT4, &[pid.0, status, 0, 0]) == pid.0 as i64,
    );
    let mut word = [0u8; 4];
    check(
        16,
        uaccess::copy_from_user(&mut word, status).is_ok()
            && i32::from_le_bytes(word) == abi::exit_status_word(7),
    );
    check(
        17,
        syscall(nr::WAIT4, &[u64::MAX, 0, 0, 0]) == errno(Errno::ECHILD),
    );
    // exit through the syscall; it does not return.
    let failures = UACCESS_FAILURES.load(Ordering::Acquire);
    syscall(nr::EXIT, &[failures as u64]);
    unreachable!("exit returned");
}

pub fn syscall_test() -> bool {
    let status = run_in_space(syscall_child, "syscall-child", test_space(4, 0));
    if status != 0 {
        serial_println!("boot-test: syscalls failed checks (bit mask) {:#b}", status);
    }
    status == 0
}

// ---------------------------------------------------------------------------
// exec::prepare: /bin/hello as a ready-to-run image.

use crate::exec::{self, elf};

struct ExecExpectations {
    file: alloc::vec::Vec<u8>,
    plan: elf::LoadPlan,
    stack_pointer: u64,
}

static EXEC_EXPECT: crate::sync::SpinMutex<Option<ExecExpectations>> =
    crate::sync::SpinMutex::new(None);

/// Runs inside the prepared image's address space and reads it back as the
/// program would see it.
fn exec_child() -> ! {
    UACCESS_FAILURES.store(0, Ordering::Release);
    let expected = EXEC_EXPECT.lock().take().expect("expectations set");
    // (0) Every page holds its file bytes, with .bss zeroed.
    let mut actual = [0u8; 4096];
    let mut wanted = [0u8; 4096];
    let pages_ok = expected.plan.segments.iter().all(|segment| {
        (segment.start..segment.end).step_by(4096).all(|page| {
            elf::LoadPlan::page_contents(segment, &expected.file, page, &mut wanted);
            uaccess::copy_from_user(&mut actual, page).is_ok() && actual == wanted
        })
    });
    check(0, pages_ok);
    // (1) The stack, as _start sees it.
    let read_word = |address: u64| {
        let mut bytes = [0u8; 8];
        uaccess::copy_from_user(&mut bytes, address).map(|()| u64::from_le_bytes(bytes))
    };
    use uaccess::UserMemory;
    let memory = uaccess::KernelUserMemory;
    let sp = expected.stack_pointer;
    let argc = read_word(sp);
    check(1, argc == Ok(2) && sp % 16 == 0);
    let argument = |index: u64| {
        read_word(sp + 8 + index * 8).and_then(|address| memory.read_cstr(address, 64))
    };
    check(
        2,
        argument(0).as_deref() == Ok("hello") && argument(1).as_deref() == Ok("x"),
    );
    // argv NULL.
    check(3, read_word(sp + 24) == Ok(0));
    let environment = read_word(sp + 32).and_then(|address| memory.read_cstr(address, 64));
    check(
        4,
        environment.as_deref() == Ok("A=B") && read_word(sp + 40) == Ok(0),
    );
    // (5) auxv: AT_ENTRY, and AT_PHDR pointing at the real program headers.
    let mut cursor = sp + 48;
    let (mut entry, mut phdr) = (None, None);
    for _ in 0..32 {
        let (Ok(kind), Ok(value)) = (read_word(cursor), read_word(cursor + 8)) else {
            break;
        };
        match kind {
            exec::stack::AT_ENTRY => entry = Some(value),
            exec::stack::AT_PHDR => phdr = Some(value),
            exec::stack::AT_NULL => break,
            _ => {}
        }
        cursor += 16;
    }
    check(5, entry == Some(expected.plan.entry));
    let mut headers = [0u8; 56];
    let phoff = u64::from_le_bytes(expected.file[32..40].try_into().unwrap()) as usize;
    check(
        6,
        phdr.is_some_and(|address| uaccess::copy_from_user(&mut headers, address).is_ok())
            && headers[..] == expected.file[phoff..phoff + 56],
    );
    let failures = UACCESS_FAILURES.load(Ordering::Acquire);
    scheduler::exit(ExitStatus(failures as i32))
}

/// Page-table checks on a prepared image, from outside it.
fn check_mappings(image: &exec::PreparedImage, plan: &elf::LoadPlan) -> bool {
    let root = image.space.root_frame();
    let leaf = |address: u64| {
        let (entries, levels) = paging::walk_entries(root, VirtAddr(address));
        (levels == 4).then(|| entries[3])
    };
    let segments_ok = plan.segments.iter().all(|segment| {
        (segment.start..segment.end).step_by(4096).all(|page| {
            leaf(page).is_some_and(|entry| {
                entry & paging::PRESENT != 0
                    && entry & paging::USER != 0
                    && (entry & paging::WRITABLE != 0) == segment.flags.write
                    && (entry & paging::NO_EXECUTE == 0) == segment.flags.execute
            })
        })
    });
    let stack_ok = (exec::USER_STACK_GUARD + 4096..exec::USER_STACK_TOP)
        .step_by(4096)
        .all(|page| {
            leaf(page).is_some_and(|entry| {
                entry & paging::USER != 0
                    && entry & paging::WRITABLE != 0
                    && entry & paging::NO_EXECUTE != 0
            })
        });
    let guards_ok = !paging::is_mapped_in(root, VirtAddr(exec::USER_STACK_GUARD))
        && !paging::is_mapped_in(root, VirtAddr(exec::USER_STACK_TOP))
        && !paging::is_mapped_in(root, VirtAddr(0));
    if !(segments_ok && stack_ok && guards_ok) {
        serial_println!(
            "boot-test: exec mappings segments={} stack={} guards={}",
            segments_ok,
            stack_ok,
            guards_ok
        );
    }
    segments_ok && stack_ok && guards_ok
}

pub fn exec_test() -> bool {
    use crate::fs::Errno;
    let prepare = || exec::prepare("/bin/hello", &["hello", "x"], &["A=B"]);
    // Warm up allocator caches, then measure a full prepare/run/reap cycle.
    drop(prepare());
    let file = {
        let inode = crate::fs::VFS.lookup("/bin/hello").ok();
        let mut file =
            alloc::vec![0u8; inode.as_ref().map_or(0, |inode| inode.stat().size as usize)];
        if let Some(inode) = inode {
            let _ = inode.read_at(0, &mut file);
        }
        file
    };
    let Ok(plan) = elf::plan(&file, exec::USER_STACK_GUARD) else {
        serial_println!("boot-test: exec: /bin/hello does not plan");
        return false;
    };
    let before = frame::stats().free_frames;
    let image = match prepare() {
        Ok(image) => image,
        Err(error) => {
            serial_println!("boot-test: exec: prepare /bin/hello: {}", error);
            return false;
        }
    };
    let mappings_ok = check_mappings(&image, &plan) && image.entry == plan.entry;
    *EXEC_EXPECT.lock() = Some(ExecExpectations {
        file,
        plan,
        stack_pointer: image.stack_pointer,
    });
    let status = run_in_space(exec_child, "exec-child", image.space);
    let after = frame::stats().free_frames;
    if status != 0 || before != after {
        serial_println!(
            "boot-test: exec child failures {:#b}, free frames {} -> {}",
            status,
            before,
            after
        );
    }
    let errors_ok = exec::prepare("/etc/hello", &[], &[]).err() == Some(Errno::ENOEXEC)
        && exec::prepare("/missing", &[], &[]).err() == Some(Errno::ENOENT)
        && exec::prepare("/bin", &[], &[]).err() == Some(Errno::EACCES);
    if !errors_ok {
        serial_println!("boot-test: exec error cases wrong");
    }
    mappings_ok && status == 0 && before == after && errors_ok
}
