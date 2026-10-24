//! Move the running UEFI image into the top-2-GiB kernel window.
//!
//! Firmware loads the PE image at an arbitrary physical address and runs it
//! identity mapped. Process address spaces only share the upper half, so the
//! kernel must not execute from that identity alias once user roots exist.
//!
//! The PE image is position independent: code is RIP-relative and every
//! absolute pointer lives in data described by `.reloc`. `mm::init_early`
//! has already mapped the image at `KERNEL_IMAGE_BASE + rva` with section
//! permissions while keeping the identity alias. With both aliases live it
//! does not matter whether a given pointer has been rebased yet, which is what
//! makes patching the running image safe. Interrupts stay disabled throughout.

use crate::arch::x86_64::cpu;
use crate::mm::layout;
use crate::{Handoff, kernel_main_high};
use core::mem::MaybeUninit;

const IMAGE_DIRECTORY_ENTRY_BASERELOC: usize = 5;
const IMAGE_REL_BASED_ABSOLUTE: u16 = 0;
const IMAGE_REL_BASED_DIR64: u16 = 10;
const PE32_PLUS_MAGIC: u16 = 0x20b;

/// Stack used from the jump until the scheduler starts its first task. The
/// firmware stack lives in identity-mapped boot-services memory.
const HIGH_BOOT_STACK_SIZE: usize = 512 * 1024;
#[repr(C, align(16))]
struct HighBootStack([u8; HIGH_BOOT_STACK_SIZE]);
static mut HIGH_BOOT_STACK: HighBootStack = HighBootStack([0; HIGH_BOOT_STACK_SIZE]);
static mut HANDOFF: MaybeUninit<Handoff> = MaybeUninit::uninit();

/// Rebase the image to the high window and continue in [`kernel_main_high`].
///
/// # Safety
/// Must be called exactly once, after `mm::init_early` mapped the image at
/// `KERNEL_IMAGE_BASE` and activated those tables, with interrupts disabled.
pub unsafe fn enter_higher_half(handoff: Handoff) -> ! {
    assert!(
        !cpu::interrupts_enabled(),
        "higher-half switch with interrupts enabled"
    );
    let image_base = handoff.boot_info.kernel_physical_start;
    let delta = layout::KERNEL_IMAGE_BASE.wrapping_sub(image_base);
    let relocations = unsafe { apply_pe_relocations(image_base, delta) };
    crate::serial_println!(
        "[UEFI] Rebased image {:#x} -> {:#x} ({} relocations).",
        image_base,
        layout::KERNEL_IMAGE_BASE,
        relocations
    );
    unsafe {
        (&raw mut HANDOFF).write(MaybeUninit::new(handoff));
    }
    // Addresses below the image window still translate as identity, so the
    // delta may be published while this code runs from the low alias.
    layout::set_image_delta(delta);

    // Take every address RIP-relative from the running (low) code so the
    // compiler cannot hand back a pointer that was already rebased.
    let entry: u64;
    let stack: u64;
    let handoff_address: u64;
    unsafe {
        core::arch::asm!(
            "lea {entry}, [rip + {entry_sym}]",
            "lea {stack}, [rip + {stack_sym}]",
            "lea {handoff}, [rip + {handoff_sym}]",
            entry = out(reg) entry,
            stack = out(reg) stack,
            handoff = out(reg) handoff_address,
            entry_sym = sym higher_half_entry,
            stack_sym = sym HIGH_BOOT_STACK,
            handoff_sym = sym HANDOFF,
            options(nomem, nostack, preserves_flags)
        );
    }
    let high = |low: u64| low.wrapping_add(delta);
    // SysV expects rsp + 8 to be 16-byte aligned at function entry.
    let stack_top = high(stack) + HIGH_BOOT_STACK_SIZE as u64 - 8;
    unsafe {
        core::arch::asm!(
            "mov rsp, {stack}",
            "xor ebp, ebp",
            "jmp {entry}",
            stack = in(reg) stack_top,
            entry = in(reg) high(entry),
            in("rdi") high(handoff_address),
            options(noreturn)
        );
    }
}

extern "sysv64" fn higher_half_entry(handoff: *const Handoff) -> ! {
    let handoff = unsafe { (*handoff.cast::<MaybeUninit<Handoff>>()).assume_init() };
    kernel_main_high(handoff)
}

/// Add `delta` to every `DIR64` base relocation. Returns the number applied.
///
/// # Safety
/// `image_base` must be the identity-mapped base of the running PE image and
/// interrupts must be disabled.
unsafe fn apply_pe_relocations(image_base: u64, delta: u64) -> usize {
    let read_u16 = |address: u64| unsafe { core::ptr::read_unaligned(address as *const u16) };
    let read_u32 = |address: u64| unsafe { core::ptr::read_unaligned(address as *const u32) };
    assert_eq!(read_u16(image_base), 0x5a4d, "image is not a PE file");
    let pe = image_base + read_u32(image_base + 0x3c) as u64;
    assert_eq!(read_u32(pe), 0x0000_4550, "missing PE signature");
    let optional = pe + 24;
    assert_eq!(read_u16(optional), PE32_PLUS_MAGIC, "image is not PE32+");
    let directory_count = read_u32(optional + 108) as usize;
    assert!(
        directory_count > IMAGE_DIRECTORY_ENTRY_BASERELOC,
        "image has no base-relocation directory"
    );
    let directory = optional + 112 + IMAGE_DIRECTORY_ENTRY_BASERELOC as u64 * 8;
    let table = image_base + read_u32(directory) as u64;
    let table_end = table + read_u32(directory + 4) as u64;

    // Section permissions already apply to every alias. Interrupts are off,
    // so lift supervisor write protection just for the patch loop.
    let cr0 = cpu_cr0();
    unsafe { set_cr0(cr0 & !CR0_WP) };
    let mut applied = 0;
    let mut block = table;
    while block + 8 <= table_end {
        let page = image_base + read_u32(block) as u64;
        let size = read_u32(block + 4) as u64;
        assert!(
            size >= 8 && block + size <= table_end,
            "malformed .reloc block"
        );
        let mut entry = block + 8;
        while entry < block + size {
            let value = read_u16(entry);
            match value >> 12 {
                IMAGE_REL_BASED_ABSOLUTE => {}
                IMAGE_REL_BASED_DIR64 => {
                    let slot = (page + (value & 0xfff) as u64) as *mut u64;
                    unsafe { slot.write_unaligned(slot.read_unaligned().wrapping_add(delta)) };
                    applied += 1;
                }
                kind => panic!("unsupported PE relocation type {kind}"),
            }
            entry += 2;
        }
        block += size;
    }
    unsafe { set_cr0(cr0) };
    applied
}

const CR0_WP: u64 = 1 << 16;

fn cpu_cr0() -> u64 {
    let value: u64;
    unsafe {
        core::arch::asm!("mov {}, cr0", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}

unsafe fn set_cr0(value: u64) {
    unsafe {
        core::arch::asm!("mov cr0, {}", in(reg) value, options(nostack, preserves_flags));
    }
}
