#![allow(dead_code)]

extern crate alloc;

#[cfg(test)]
mod alloc_counter;

// Keep the host-tested model identical to the kernel's pure buddy/bitmap
// model until the production allocator stress suite is moved into a library
// target of its own.
#[path = "../../../kernel/src/mm/allocator_model.rs"]
mod model;
pub use model::*;
#[path = "../../../kernel/src/boot/boot_info.rs"]
pub mod boot_info;
#[path = "../../../kernel/src/mm/buddy.rs"]
pub mod buddy;
#[cfg(test)]
mod buddy_tests;
#[path = "../../../kernel/src/cmdline.rs"]
pub mod cmdline;
#[cfg(test)]
mod cmdline_tests;
#[path = "../../../kernel/src/mm/dma.rs"]
pub mod dma;
#[cfg(test)]
mod dma_tests;
#[path = "../../../kernel/src/exec/elf.rs"]
pub mod elf;
#[path = "../../../kernel/src/exec/stack.rs"]
pub mod exec_stack;
#[cfg(test)]
mod exec_tests;
#[cfg(test)]
mod fake_user;
#[path = "../../../kernel/src/fs/mod.rs"]
pub mod fs;
#[cfg(test)]
mod fs_model;
#[cfg(test)]
mod fs_tests;
#[path = "../../../kernel/src/drivers/hda/codec.rs"]
pub mod hda_codec;
#[cfg(test)]
mod hda_tests;
mod heap_model;
#[path = "../../../kernel/src/mm/holds.rs"]
pub mod holds;
#[cfg(test)]
mod holds_tests;
#[path = "../../../kernel/src/klog_ring.rs"]
pub mod klog_ring;
#[cfg(test)]
mod libsaeos_tests;
#[cfg(test)]
mod msi_tests;
#[path = "../../../kernel/src/drivers/pci/mod.rs"]
pub mod pci;
#[cfg(test)]
mod pci_tests;
#[cfg(test)]
mod resample_tests;
#[path = "../../../userspace/sh/src/parse.rs"]
pub mod sh_parse;
#[cfg(test)]
mod sh_tests;
#[path = "../../../kernel/src/sound/mod.rs"]
pub mod sound;
#[cfg(test)]
mod sound_tests;
#[path = "../../../kernel/src/syscall/mod.rs"]
pub mod syscall;
#[cfg(test)]
mod syscall_tests;
#[cfg(test)]
mod time_tests;
#[path = "../../../kernel/src/drivers/tty.rs"]
pub mod tty;
#[cfg(test)]
mod tty_tests;
#[path = "../../../kernel/src/mm/uaccess.rs"]
pub mod uaccess;
#[cfg(test)]
mod uaccess_tests;
#[path = "../../../userspace/libsaeos/src/heap.rs"]
pub mod user_heap;
#[path = "../../../userspace/libsaeos/src/lines.rs"]
pub mod user_lines;
#[path = "../../../userspace/libsaeos/src/startinfo.rs"]
pub mod user_startinfo;
#[path = "../../../kernel/src/mm/vma.rs"]
pub mod vma;
#[cfg(test)]
mod vma_tests;
#[path = "../../../userspace/coreutils/src/wav.rs"]
pub mod wav;
#[cfg(test)]
mod wav_tests;
pub use heap_model::*;
mod lock_model;
#[path = "../../../kernel/src/task/mod.rs"]
mod task;
pub use task::*;
#[path = "../../../kernel/src/process.rs"]
mod process;
pub use process::*;
