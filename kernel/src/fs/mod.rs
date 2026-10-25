//! Filesystems.
//!
//! ```text
//! fs::open / read / write / ...   per-process FileTable (fd -> OpenFile)
//! Vfs                              path walk, mounts, access checks, errno
//! Inode / FileSystem traits        what each filesystem implements
//!   ramfs (/), devfs (/dev),       BlockDevice for on-disk filesystems
//!   procfs (/proc), ...
//! ```
//!
//! The kernel API mirrors the Linux file system calls, errno values
//! included; the shell's `ls`, `cat`, `stat`, `write`, `mkdir` and `rm` use
//! it.
//!
//! Boot (`fs::init`):
//!
//! ```text
//! initramfs.toml + initramfs/ --tools/mkinitramfs--> initramfs.cpio (newc)
//!   BIOS  Multiboot module (qemu -initrd)      } held in memory by mm,
//!   UEFI  \saeos\initramfs.cpio on the ESP     } BootInfo.initramfs
//! ramfs mounted at /, every archive unpacked into it (initramfs.rs),
//! archive memory released, devfs at /dev, procfs at /proc
//! ```
//!
//! The archive is never part of the kernel image, so changing a file does
//! not relink the kernel, and the test fixtures (`mkinitramfs
//! --test-fixtures`, with a `/test/MANIFEST` the boot test checks) only
//! exist in the test image.
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
pub mod procfs;
pub mod ramfs;
pub mod vfs;

#[cfg(not(host_model))]
mod kernel;
#[cfg(not(host_model))]
pub use kernel::*;

pub use errno::{Errno, FsResult};
pub use file::{Fd, FileTable, OpenFlags, Whence};
pub use inode::{DirEntry, FileSystem, Inode, InodeKind, Stat};
