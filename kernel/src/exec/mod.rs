//! Turning a program file into a ready-to-run user image.
//!
//! ```text
//! prepare("/bin/hello", argv, envp)
//!   read the file (VFS)          -> elf::plan: validated segments
//!   new AddressSpace             -> each segment page: zeroed frame, file
//!                                   bytes copied in, mapped R / RX / RW
//!   user stack below USER_STACK_TOP (guard page under it)
//!                                -> stack::build: argc, argv, envp, auxv
//!   => PreparedImage { space, entry, stack_pointer }
//! ```
//!
//! Entering the image (`iretq` to ring 3 at `entry` with `stack_pointer`)
//! comes with the ring-3 work; everything up to that point is here and is
//! checked by the boot test. Dropping a `PreparedImage` frees it entirely.

pub mod elf;
pub mod stack;

use crate::fs::{Errno, InodeKind};
use crate::mm::paging::{AddressSpace, PageFlags, phys_to_virt};
use crate::mm::uaccess::USER_TOP;
use crate::mm::{PhysAddr, PhysFrame, VirtAddr, frame};
use alloc::vec::Vec;

const PAGE: u64 = elf::PAGE_SIZE;

/// Top of the initial user stack. The page above it (just below
/// `USER_TOP`) stays unmapped.
pub const USER_STACK_TOP: u64 = USER_TOP - PAGE;
pub const USER_STACK_PAGES: u64 = 32;
/// Below the stack: one unmapped guard page. Programs load beneath it.
pub const USER_STACK_GUARD: u64 = USER_STACK_TOP - (USER_STACK_PAGES + 1) * PAGE;
/// Arguments and environment may use at most a quarter of the stack, as
/// on Linux.
const ARGUMENT_LIMIT: usize = (USER_STACK_PAGES * PAGE / 4) as usize;
/// Largest program file accepted.
const MAX_PROGRAM_SIZE: u64 = 64 * 1024 * 1024;

pub struct PreparedImage {
    pub space: AddressSpace,
    /// The image, heap start and stack, for `brk`/`mmap` later.
    pub map: crate::mm::vma::MemoryMap,
    pub entry: u64,
    pub stack_pointer: u64,
}

/// The memory map of a freshly loaded image: its segments and the stack;
/// the heap starts on the page after the highest segment, and `mmap` uses
/// the space below the stack guard.
fn memory_map(plan: &elf::LoadPlan) -> Result<crate::mm::vma::MemoryMap, Errno> {
    use crate::mm::vma::{MemoryMap, Protection, Region, RegionKind};
    let mut regions: Vec<Region> = plan
        .segments
        .iter()
        .map(|segment| Region {
            start: segment.start,
            end: segment.end,
            protection: Protection {
                read: true,
                write: segment.flags.write,
                execute: segment.flags.execute,
            },
            kind: RegionKind::Image,
        })
        .collect();
    regions.push(Region {
        start: USER_STACK_TOP - USER_STACK_PAGES * PAGE,
        end: USER_STACK_TOP,
        protection: Protection::READ_WRITE,
        kind: RegionKind::Stack,
    });
    let brk_start = plan
        .segments
        .iter()
        .map(|segment| segment.end)
        .max()
        .unwrap_or(crate::mm::vma::MIN_ADDRESS);
    MemoryMap::new(&regions, brk_start, USER_STACK_GUARD).map_err(|_| Errno::ENOEXEC)
}

fn read_program(path: &str) -> Result<Vec<u8>, Errno> {
    let inode = crate::fs::VFS.lookup(path)?;
    match inode.kind() {
        InodeKind::File => {}
        _ => return Err(Errno::EACCES),
    }
    let size = inode.stat().size;
    if size > MAX_PROGRAM_SIZE {
        return Err(Errno::ENOEXEC);
    }
    let mut file = alloc::vec![0u8; size as usize];
    let mut filled = 0;
    while filled < file.len() {
        match inode.read_at(filled as u64, &mut file[filled..])? {
            0 => break,
            count => filled += count,
        }
    }
    file.truncate(filled);
    Ok(file)
}

fn page_flags(flags: elf::SegmentFlags) -> PageFlags {
    if flags.execute {
        PageFlags::USER_READ_EXECUTE
    } else if flags.write {
        PageFlags::USER_READ_WRITE
    } else {
        PageFlags::USER_READ
    }
}

/// A zeroed frame and its kernel (direct-map) view.
fn new_page() -> Result<(PhysFrame, &'static mut [u8; 4096]), Errno> {
    let frame = frame::alloc_frame_zeroed().ok_or(Errno::ENOMEM)?;
    let page = unsafe { &mut *(phys_to_virt(PhysAddr(frame.0)).0 as *mut [u8; 4096]) };
    Ok((frame, page))
}

fn map_page(
    space: &AddressSpace,
    address: u64,
    frame: PhysFrame,
    flags: PageFlags,
) -> Result<(), Errno> {
    space
        .map_owned(VirtAddr(address), frame, flags)
        .map_err(|_| {
            frame::free_frame(frame);
            Errno::ENOMEM
        })
}

/// 16 bytes for `AT_RANDOM`. Not cryptographic: mixed from the TSC until
/// the kernel has an entropy source.
fn random_bytes() -> [u8; 16] {
    let mut state = crate::arch::x86_64::cpu::rdtsc() ^ 0x9e37_79b9_7f4a_7c15;
    let mut bytes = [0u8; 16];
    for chunk in bytes.chunks_mut(8) {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        chunk.copy_from_slice(&state.to_le_bytes());
    }
    bytes
}

/// Load `path` into a fresh address space with its initial stack.
pub fn prepare(path: &str, argv: &[&str], envp: &[&str]) -> Result<PreparedImage, Errno> {
    let file = read_program(path)?;
    let plan = elf::plan(&file, USER_STACK_GUARD)?;
    let map = memory_map(&plan)?;
    let space = AddressSpace::new().map_err(|_| Errno::ENOMEM)?;

    for segment in &plan.segments {
        for page_address in (segment.start..segment.end).step_by(PAGE as usize) {
            let (frame, page) = new_page()?;
            elf::LoadPlan::page_contents(segment, &file, page_address, page);
            map_page(&space, page_address, frame, page_flags(segment.flags))?;
        }
    }

    let stack = stack::build(
        USER_STACK_TOP,
        argv,
        envp,
        &[
            (stack::AT_PHDR, plan.phdr_vaddr),
            (stack::AT_PHENT, 56),
            (stack::AT_PHNUM, plan.phnum),
            (stack::AT_PAGESZ, PAGE),
            (stack::AT_ENTRY, plan.entry),
        ],
        random_bytes(),
        ARGUMENT_LIMIT,
    )?;
    // The stack bytes end at USER_STACK_TOP; write each page's share
    // through the direct map before mapping it.
    let stack_bottom = USER_STACK_TOP - USER_STACK_PAGES * PAGE;
    let bytes_start = USER_STACK_TOP - stack.bytes.len() as u64;
    for page_address in (stack_bottom..USER_STACK_TOP).step_by(PAGE as usize) {
        let (frame, page) = new_page()?;
        let from = page_address.max(bytes_start);
        let to = (page_address + PAGE).min(USER_STACK_TOP);
        if from < to {
            let source = (from - bytes_start) as usize;
            page[(from - page_address) as usize..(to - page_address) as usize]
                .copy_from_slice(&stack.bytes[source..source + (to - from) as usize]);
        }
        map_page(&space, page_address, frame, PageFlags::USER_READ_WRITE)?;
    }

    Ok(PreparedImage {
        space,
        map,
        entry: plan.entry,
        stack_pointer: stack.stack_pointer,
    })
}

/// Make `image` the current process's program: its address space replaces
/// the process's previous one (freed here), the FPU and FS base start
/// clean, and the caller enters user mode at the returned
/// `(entry, stack_pointer)`.
pub fn install_current(image: PreparedImage) -> (u64, u64) {
    let task = crate::scheduler::current().expect("exec without a current task");
    let previous = crate::scheduler::with_scheduler(|scheduler| {
        scheduler.task(task).and_then(|task| task.address_space)
    });
    let space = crate::mm::address_space::register_owned_with_map(image.space, image.map);
    crate::scheduler::set_address_space(task, Some(space))
        .expect("current task accepts an address space");
    crate::mm::address_space::activate_if_changed(Some(space))
        .expect("new address space is registered");
    if let Some(previous) = previous {
        // No longer active, and owned by nothing else.
        if let Err(error) = crate::mm::address_space::release(previous) {
            crate::kerror!("exec: cannot free the old address space: {:?}", error);
        }
    }
    unsafe { crate::arch::x86_64::fpu::restore(&crate::task::FpuState::INITIAL) };
    crate::scheduler::with_scheduler(|scheduler| {
        if let Some(task) = scheduler.task_mut(task) {
            task.fs_base = 0;
        }
    });
    crate::arch::x86_64::cpu::set_fs_base(0);
    (image.entry, image.stack_pointer)
}
