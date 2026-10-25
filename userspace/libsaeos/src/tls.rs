//! Thread-local storage for the main thread (x86_64 "variant II"):
//!
//! ```text
//! [ .tdata copy | .tbss zeroed ][ TCB: self pointer ]
//!                               ^ thread pointer = FS base
//! ```
//!
//! The linker resolves thread-locals as negative offsets from the thread
//! pointer (local-exec), so the block sits just below it, sized and aligned
//! as the program's PT_TLS header says. `%fs:0` holds the thread pointer
//! itself, as the ABI requires.

use crate::raw;
use crate::startinfo::{AT_PHDR, AT_PHNUM, StartInfo};

const PT_TLS: u32 = 7;
const PROGRAM_HEADER_SIZE: u64 = 56;
const TCB_SIZE: u64 = 64;

pub(crate) fn init(info: &StartInfo) {
    // The TLS template (address, file bytes, memory size, alignment).
    let (mut template, mut file_size, mut memory_size, mut align) = (0u64, 0u64, 0u64, 8u64);
    if let (Some(headers), Some(count)) = (info.aux(AT_PHDR), info.aux(AT_PHNUM)) {
        for index in 0..count {
            let header = (headers + index * PROGRAM_HEADER_SIZE) as *const u8;
            let field =
                |offset: usize| unsafe { (header.add(offset) as *const u64).read_unaligned() };
            if unsafe { (header as *const u32).read_unaligned() } == PT_TLS {
                template = field(16);
                file_size = field(32);
                memory_size = field(40);
                align = field(48).max(8);
            }
        }
    }
    let block_size = memory_size.next_multiple_of(align);
    let total = (block_size + TCB_SIZE).next_multiple_of(4096);
    let block = raw::mmap_anonymous(0, total, raw::PROT_READ | raw::PROT_WRITE, 0);
    if block <= 0 {
        raw::write(2, b"libsaeos: cannot allocate thread-local storage\n");
        raw::exit_now(127);
    }
    let thread_pointer = block as u64 + block_size;
    unsafe {
        // The rest of the block is already zero (.tbss).
        core::ptr::copy_nonoverlapping(
            template as *const u8,
            (thread_pointer - block_size) as *mut u8,
            file_size as usize,
        );
        (thread_pointer as *mut u64).write(thread_pointer);
    }
    raw::arch_prctl(raw::ARCH_SET_FS, thread_pointer);
}
