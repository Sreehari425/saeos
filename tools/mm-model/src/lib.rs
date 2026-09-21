#![allow(dead_code)]

extern crate alloc;

// Keep the host-tested model identical to the kernel's pure buddy/bitmap
// model until the production allocator stress suite is moved into a library
// target of its own.
#[path = "../../../kernel/src/mm/allocator_model.rs"]
mod model;
pub use model::*;
mod heap_model;
pub use heap_model::*;
mod lock_model;
