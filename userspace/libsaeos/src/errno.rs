//! Error numbers (Linux's) and the per-thread `errno` the C interface sets.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Errno(pub i32);

pub type Result<T> = core::result::Result<T, Errno>;

impl Errno {
    pub const EPERM: Self = Self(1);
    pub const ENOENT: Self = Self(2);
    pub const EIO: Self = Self(5);
    pub const E2BIG: Self = Self(7);
    pub const ENOEXEC: Self = Self(8);
    pub const EBADF: Self = Self(9);
    pub const ECHILD: Self = Self(10);
    pub const ENOMEM: Self = Self(12);
    pub const EACCES: Self = Self(13);
    pub const EFAULT: Self = Self(14);
    pub const EBUSY: Self = Self(16);
    pub const EEXIST: Self = Self(17);
    pub const ENODEV: Self = Self(19);
    pub const ENOTDIR: Self = Self(20);
    pub const EISDIR: Self = Self(21);
    pub const EINVAL: Self = Self(22);
    pub const ENOTTY: Self = Self(25);
    pub const ENOSPC: Self = Self(28);
    pub const ERANGE: Self = Self(34);
    pub const ENAMETOOLONG: Self = Self(36);
    pub const ENOSYS: Self = Self(38);
    pub const ENOTEMPTY: Self = Self(39);

    pub const fn name(self) -> &'static str {
        match self.0 {
            1 => "EPERM",
            2 => "ENOENT",
            5 => "EIO",
            7 => "E2BIG",
            8 => "ENOEXEC",
            9 => "EBADF",
            10 => "ECHILD",
            12 => "ENOMEM",
            13 => "EACCES",
            14 => "EFAULT",
            16 => "EBUSY",
            17 => "EEXIST",
            19 => "ENODEV",
            20 => "ENOTDIR",
            21 => "EISDIR",
            22 => "EINVAL",
            25 => "ENOTTY",
            28 => "ENOSPC",
            34 => "ERANGE",
            36 => "ENAMETOOLONG",
            38 => "ENOSYS",
            39 => "ENOTEMPTY",
            _ => "E?",
        }
    }

    /// A human-readable description, like `strerror`.
    pub const fn description(self) -> &'static str {
        match self.0 {
            1 => "Operation not permitted",
            2 => "No such file or directory",
            5 => "Input/output error",
            7 => "Argument list too long",
            8 => "Exec format error",
            9 => "Bad file descriptor",
            10 => "No child processes",
            12 => "Cannot allocate memory",
            13 => "Permission denied",
            14 => "Bad address",
            16 => "Device or resource busy",
            17 => "File exists",
            19 => "No such device",
            20 => "Not a directory",
            21 => "Is a directory",
            22 => "Invalid argument",
            25 => "Inappropriate ioctl for device",
            28 => "No space left on device",
            34 => "Numerical result out of range",
            36 => "File name too long",
            38 => "Function not implemented",
            39 => "Directory not empty",
            _ => "Unknown error",
        }
    }
}

impl core::fmt::Display for Errno {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.description())
    }
}

/// A raw syscall result: `-4095..=-1` is an error, as in Linux's ABI.
pub fn check(result: i64) -> Result<u64> {
    if (-4095..0).contains(&result) {
        Err(Errno(-result as i32))
    } else {
        Ok(result as u64)
    }
}

/// The C `errno`: one per thread, in thread-local storage.
#[thread_local]
static mut ERRNO: i32 = 0;

/// Where `errno` lives (C's `errno` macro expands to `*__errno_location()`).
#[unsafe(no_mangle)]
pub extern "C" fn __errno_location() -> *mut i32 {
    &raw mut ERRNO
}

pub fn set_errno(error: Errno) {
    unsafe { *__errno_location() = error.0 };
}

pub fn errno() -> Errno {
    Errno(unsafe { *__errno_location() })
}
