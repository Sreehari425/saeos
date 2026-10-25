//! Frame-pointer backtraces for the fatal path.
//!
//! The kernel is built with `-C force-frame-pointers=yes` (see
//! `.cargo/config.toml`), so every function starts with `push rbp; mov rbp,
//! rsp`: `[rbp]` holds the caller's frame pointer and `[rbp + 8]` the return
//! address. Walking that chain needs no unwinder and no allocation. Every
//! frame is validated before it is read, so a corrupted stack ends the trace
//! instead of faulting inside the fault report.

use crate::mm::frame::VirtAddr;
use crate::mm::{layout, paging};

pub const MAX_FRAMES: usize = 32;
const KERNEL_CODE_SELECTOR: u64 = 0x08;

fn read_word(address: u64) -> u64 {
    unsafe { core::ptr::read_unaligned(address as *const u64) }
}

/// An interrupt or exception frame pushed by the CPU, possibly after an
/// error code and alignment padding: `RIP CS RFLAGS RSP SS`, starting within
/// the first four words at `slot`. Returns `(rip, rsp)` of the interrupted
/// code. The pattern (kernel CS, RFLAGS with its always-one bit, a kernel
/// RSP, a null or kernel SS, and a RIP accepted by `is_code`) does not occur
/// in ordinary frames.
fn machine_frame(slot: u64, is_code: impl Fn(u64) -> bool) -> Option<(u64, u64)> {
    (0..4).map(|word| slot + word * 8).find_map(|rip_slot| {
        let (rip, cs, rflags) = (
            read_word(rip_slot),
            read_word(rip_slot + 8),
            read_word(rip_slot + 16),
        );
        let (rsp, ss) = (read_word(rip_slot + 24), read_word(rip_slot + 32));
        let plausible = cs == KERNEL_CODE_SELECTOR
            && rflags & 0x2 != 0
            && rflags < 1 << 22
            && rsp >= layout::DIRECT_MAP_BASE
            && (ss == 0 || ss == 0x10)
            && is_code(rip);
        plausible.then_some((rip, rsp))
    })
}

fn readable(address: u64, root: crate::mm::PhysFrame) -> bool {
    paging::is_mapped_in(root, VirtAddr(address & !0xfff))
}

#[cfg(not(target_os = "uefi"))]
fn frame_ok(rbp: u64, root: crate::mm::PhysFrame) -> bool {
    rbp.is_multiple_of(8)
        && rbp >= layout::DIRECT_MAP_BASE
        && rbp.checked_add(16).is_some()
        && readable(rbp, root)
        && readable(rbp + 8, root)
}

/// Call `visit(depth, return_address)` for each caller of the current
/// function, innermost first.
#[inline(always)]
pub fn walk_current(visit: impl FnMut(usize, u64)) {
    #[cfg(not(target_os = "uefi"))]
    walk(current_frame(), visit);
    #[cfg(target_os = "uefi")]
    win64::walk(current_ip(), current_stack(), current_frame(), visit);
}

/// SysV frame-pointer walk (BIOS build). Call `visit(depth, return_address)`
/// for each frame above `rbp`.
///
/// Callers normally live higher up the same stack. The one legitimate
/// exception is an interrupt frame: a handler on an IST stack (`#DF`) was
/// entered from a different stack. There `[rbp + 8]` holds the CPU-pushed
/// frame instead of a return address; its RIP is reported and the walk
/// continues into the interrupted stack.
#[cfg(not(target_os = "uefi"))]
pub fn walk(mut rbp: u64, mut visit: impl FnMut(usize, u64)) {
    let root = paging::current_root();
    let mut depth = 0;
    for _ in 0..MAX_FRAMES * 2 {
        if depth >= MAX_FRAMES || !frame_ok(rbp, root) {
            break;
        }
        let next = read_word(rbp);
        let slot = rbp + 8;
        let interrupted = readable(slot + 64, root)
            .then(|| machine_frame(slot, |rip| rip >= layout::KERNEL_IMAGE_BASE))
            .flatten();
        match interrupted {
            Some((rip, _)) => {
                visit(depth, rip);
                depth += 1;
            }
            None => {
                let return_address = read_word(slot);
                if return_address >= layout::KERNEL_IMAGE_BASE {
                    visit(depth, return_address);
                    depth += 1;
                }
                if next <= rbp {
                    break;
                }
            }
        }
        rbp = next;
    }
}

/// The current frame pointer.
#[inline(always)]
pub fn current_frame() -> u64 {
    let rbp: u64;
    unsafe {
        core::arch::asm!("mov {}, rbp", out(reg) rbp, options(nomem, nostack, preserves_flags));
    }
    rbp
}

/// The current stack pointer.
#[cfg(target_os = "uefi")]
#[inline(always)]
fn current_stack() -> u64 {
    let rsp: u64;
    unsafe {
        core::arch::asm!("mov {}, rsp", out(reg) rsp, options(nomem, nostack, preserves_flags));
    }
    rsp
}

/// The current instruction pointer.
#[cfg(target_os = "uefi")]
#[inline(always)]
fn current_ip() -> u64 {
    let rip: u64;
    unsafe {
        core::arch::asm!("lea {}, [rip]", out(reg) rip, options(nomem, nostack, preserves_flags));
    }
    rip
}

/// Windows x64 unwinding for the UEFI (PE) build.
///
/// The UEFI target follows Windows frame conventions: a prologue is
/// `push rbp; sub rsp, N; lea rbp, [rsp + K]` with `K` capped (so locals use
/// short displacements), which means `[rbp]` is generally *not* the saved
/// frame pointer. The PE image carries exact unwind data instead: `.pdata`
/// maps every function to an `UNWIND_INFO` that records the frame-register
/// offset and each prologue operation. Undoing the operations performed
/// before `rbp` was set yields the saved `rbp` and the return address.
#[cfg(target_os = "uefi")]
mod win64 {
    use super::{MAX_FRAMES, read_word as read_u64, readable};
    use crate::mm::{layout, paging};

    const RBP: u8 = 5;
    const UWOP_PUSH_NONVOL: u8 = 0;
    const UWOP_ALLOC_LARGE: u8 = 1;
    const UWOP_ALLOC_SMALL: u8 = 2;
    const UWOP_SET_FPREG: u8 = 3;
    const UWOP_SAVE_NONVOL: u8 = 4;
    const UWOP_SAVE_NONVOL_FAR: u8 = 5;
    const UWOP_SAVE_XMM128: u8 = 8;
    const UWOP_SAVE_XMM128_FAR: u8 = 9;

    /// The image is rebased to `KERNEL_IMAGE_BASE`, headers included.
    const IMAGE: u64 = layout::KERNEL_IMAGE_BASE;

    fn read_u32(address: u64) -> u32 {
        unsafe { core::ptr::read_unaligned(address as *const u32) }
    }

    /// `(start, count)` of the `.pdata` RUNTIME_FUNCTION table.
    fn exception_directory() -> Option<(u64, usize)> {
        let pe = IMAGE + read_u32(IMAGE + 0x3c) as u64;
        let optional = pe + 24;
        if read_u32(pe) != 0x0000_4550 || read_u32(optional + 108) <= 3 {
            return None;
        }
        let directory = optional + 112 + 3 * 8;
        let (rva, size) = (read_u32(directory), read_u32(directory + 4));
        (size != 0).then_some((IMAGE + rva as u64, size as usize / 12))
    }

    /// RVA of the UNWIND_INFO for the function containing `rip`.
    fn unwind_info(rip: u64) -> Option<u64> {
        let (table, count) = exception_directory()?;
        let rva = u32::try_from(rip.checked_sub(IMAGE)?).ok()?;
        let (mut low, mut high) = (0usize, count);
        while low < high {
            let middle = (low + high) / 2;
            let entry = table + middle as u64 * 12;
            let (begin, end) = (read_u32(entry), read_u32(entry + 4));
            if rva < begin {
                high = middle;
            } else if rva >= end {
                low = middle + 1;
            } else {
                return Some(IMAGE + read_u32(entry + 8) as u64);
            }
        }
        None
    }

    /// Unwind one frame of the function containing `rip`, whose registers
    /// are `rsp` and `rbp`. Returns `(slot, caller_rbp)`, where `slot` holds
    /// the return address (the caller's RSP is `slot + 8`).
    ///
    /// Like the Windows unwinder: a function with a frame register gets its
    /// RSP from `rbp - FrameOffset * 16` and only the operations before
    /// `SET_FPREG` are undone. When `rbp` is not trustworthy (after stepping
    /// over code without unwind data), the whole prologue is undone from the
    /// known RSP instead, which is exact for fixed-size frames.
    fn unwind(
        rip: u64,
        rsp: u64,
        rbp: u64,
        rbp_trusted: bool,
        root: crate::mm::PhysFrame,
    ) -> Option<(u64, Option<u64>)> {
        let info = unwind_info(rip)?;
        let header = read_u32(info);
        let flags = (header & 0xff) >> 3;
        let code_count = ((header >> 16) & 0xff) as usize;
        let frame_register = ((header >> 24) & 0x0f) as u8;
        let frame_offset = ((header >> 28) & 0x0f) as u64 * 16;
        // Chained unwind info (bit 2) is not produced for this code.
        if flags & 0x4 != 0 || (frame_register != 0 && frame_register != RBP) {
            return None;
        }
        let code = |index: usize| -> u16 {
            unsafe { core::ptr::read_unaligned((info + 4 + index as u64 * 2) as *const u16) }
        };
        let slots = |operation: u8, info_bits: u8| match (operation, info_bits) {
            (UWOP_ALLOC_LARGE, 0) | (UWOP_SAVE_NONVOL, _) | (UWOP_SAVE_XMM128, _) => 2,
            (UWOP_ALLOC_LARGE, _) | (UWOP_SAVE_NONVOL_FAR, _) | (UWOP_SAVE_XMM128_FAR, _) => 3,
            _ => 1,
        };
        // Codes are stored last-executed first.
        let mut index = 0;
        let mut rsp = rsp;
        if frame_register == RBP && rbp_trusted {
            while index < code_count {
                let operation = (code(index) >> 8) as u8 & 0x0f;
                let info_bits = (code(index) >> 12) as u8;
                index += slots(operation, info_bits);
                if operation == UWOP_SET_FPREG {
                    break;
                }
            }
            rsp = rbp.checked_sub(frame_offset)?;
        }
        let mut saved_rbp = None;
        while index < code_count {
            let operation = (code(index) >> 8) as u8 & 0x0f;
            let info_bits = (code(index) >> 12) as u8;
            match operation {
                UWOP_PUSH_NONVOL => {
                    if info_bits == RBP {
                        if !readable(rsp, root) {
                            return None;
                        }
                        saved_rbp = Some(read_u64(rsp));
                    }
                    rsp += 8;
                }
                UWOP_ALLOC_SMALL => rsp += info_bits as u64 * 8 + 8,
                UWOP_ALLOC_LARGE if info_bits == 0 => rsp += code(index + 1) as u64 * 8,
                UWOP_ALLOC_LARGE => rsp += code(index + 1) as u64 | (code(index + 2) as u64) << 16,
                UWOP_SET_FPREG | UWOP_SAVE_NONVOL | UWOP_SAVE_XMM128 | UWOP_SAVE_NONVOL_FAR
                | UWOP_SAVE_XMM128_FAR => {}
                // Machine frames and anything newer: give up on this frame.
                _ => return None,
            }
            index += slots(operation, info_bits);
        }
        Some((rsp, saved_rbp))
    }

    /// Whether `address` directly follows a `call` instruction.
    fn follows_call(address: u64) -> bool {
        let byte = |back: u64| unsafe { core::ptr::read_volatile((address - back) as *const u8) };
        // call rel32 (E8 xx xx xx xx), or FF /2 with a 2-, 3-, or 6-byte
        // encoding (call reg, call [reg+disp8], call [rip+disp32]).
        byte(5) == 0xe8
            || (byte(2) == 0xff && byte(1) & 0x38 == 0x10)
            || (byte(3) == 0xff && byte(2) & 0x38 == 0x10)
            || (byte(6) == 0xff && byte(5) & 0x38 == 0x10)
    }

    /// Step over a frame without unwind data (precompiled `core`): the
    /// nearest stack word above `rsp` that is a return address into code
    /// with unwind data.
    fn scan_for_return(rsp: u64, root: crate::mm::PhysFrame) -> Option<u64> {
        const SCAN_WORDS: u64 = 256;
        (0..SCAN_WORDS).map(|word| rsp + word * 8).find(|&slot| {
            readable(slot, root) && {
                let candidate = read_u64(slot);
                candidate >= IMAGE + 0x1000
                    && unwind_info(candidate).is_some()
                    && follows_call(candidate)
            }
        })
    }

    pub fn walk(mut rip: u64, mut rsp: u64, mut rbp: u64, mut visit: impl FnMut(usize, u64)) {
        let root = paging::current_root();
        let mut rbp_trusted = true;
        for depth in 0..MAX_FRAMES {
            // RSP may be unreadable (an overflow into the guard page) while a
            // frame-pointer unwind via RBP still works; every read is checked.
            let (slot, caller_rbp) = if unwind_info(rip).is_some() {
                let Some((slot, saved)) = unwind(rip, rsp, rbp, rbp_trusted, root) else {
                    return;
                };
                if saved.is_some() {
                    rbp_trusted = true;
                }
                (slot, saved.unwrap_or(rbp))
            } else {
                if rsp < layout::DIRECT_MAP_BASE || !readable(rsp, root) {
                    return;
                }
                let Some(slot) = scan_for_return(rsp, root) else {
                    return;
                };
                rbp_trusted = false;
                (slot, rbp)
            };
            if slot < layout::DIRECT_MAP_BASE || !readable(slot, root) || !readable(slot + 64, root)
            {
                return;
            }
            // An exception handler "returns" into the CPU-pushed frame: the
            // interrupted RSP comes from that frame, not from `slot + 8`.
            let (caller, caller_rsp) =
                match super::machine_frame(slot, |rip| unwind_info(rip).is_some()) {
                    Some(frame) => frame,
                    None => (read_u64(slot), slot + 8),
                };
            if caller < IMAGE {
                return;
            }
            visit(depth, caller);
            rip = caller;
            rsp = caller_rsp;
            rbp = caller_rbp;
        }
    }
}

/// Which binary the addresses belong to, for `scripts/symbolize.sh`.
pub const IMAGE: &str = if cfg!(target_os = "uefi") {
    "uefi"
} else {
    "bios"
};
