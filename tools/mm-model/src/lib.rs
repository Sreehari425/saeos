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
#[path = "../../../kernel/src/fs/mod.rs"]
pub mod fs;
#[cfg(test)]
mod fs_model;
#[cfg(test)]
mod fs_tests;
mod heap_model;
#[path = "../../../kernel/src/mm/holds.rs"]
pub mod holds;
#[cfg(test)]
mod holds_tests;
#[path = "../../../kernel/src/klog_ring.rs"]
pub mod klog_ring;
pub use heap_model::*;
mod lock_model;
#[path = "../../../kernel/src/task/mod.rs"]
mod task;
pub use task::*;
#[path = "../../../kernel/src/process.rs"]
mod process;
pub use process::*;
