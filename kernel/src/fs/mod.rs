//! Filesystems.
//!
//! ```text
//! fs::open / read / write / ...   per-process FileTable (fd -> OpenFile)
//! Vfs                              path walk, mounts, access checks, errno
//! Inode / FileSystem traits        what each filesystem implements
//!   ramfs (/), devfs (/dev), ...   BlockDevice for on-disk filesystems
//! ```
//!
//! Everything except `kernel` (the global instance and boot-time setup) is
//! also compiled into the host model (`tools/mm-model`) and tested there.

pub mod block;
pub mod devfs;
pub mod errno;
pub mod file;
pub mod initramfs;
pub mod inode;
pub mod lock;
pub mod ramfs;
pub mod vfs;

#[cfg(not(host_model))]
mod kernel;
#[cfg(not(host_model))]
pub use kernel::*;

pub use errno::{Errno, FsResult};
pub use file::{Fd, FileTable, OpenFlags, Whence};
pub use inode::{DirEntry, FileSystem, Inode, InodeKind, Stat};
