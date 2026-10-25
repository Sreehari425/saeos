//! Linux x86_64 syscall numbers (`arch/x86/entry/syscalls/syscall_64.tbl`)
//! for the calls SaeOS implements. Everything else is `ENOSYS`.

pub const READ: u64 = 0;
pub const WRITE: u64 = 1;
pub const OPEN: u64 = 2;
pub const CLOSE: u64 = 3;
pub const STAT: u64 = 4;
pub const FSTAT: u64 = 5;
pub const LSEEK: u64 = 8;
pub const GETPID: u64 = 39;
pub const EXIT: u64 = 60;
pub const WAIT4: u64 = 61;
pub const RENAME: u64 = 82;
pub const MKDIR: u64 = 83;
pub const RMDIR: u64 = 84;
pub const UNLINK: u64 = 87;
pub const GETDENTS64: u64 = 217;
pub const EXIT_GROUP: u64 = 231;
