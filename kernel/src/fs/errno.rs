//! Error numbers, with Linux's values so syscalls return `-(errno)`
//! unchanged.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum Errno {
    EPERM = 1,
    ENOENT = 2,
    EIO = 5,
    E2BIG = 7,
    ENOEXEC = 8,
    EBADF = 9,
    ECHILD = 10,
    ENOMEM = 12,
    EACCES = 13,
    EFAULT = 14,
    EBUSY = 16,
    EEXIST = 17,
    EXDEV = 18,
    ENODEV = 19,
    ENOTDIR = 20,
    EISDIR = 21,
    EINVAL = 22,
    EMFILE = 24,
    ENOTTY = 25,
    ENOSPC = 28,
    ESPIPE = 29,
    EROFS = 30,
    ERANGE = 34,
    ENAMETOOLONG = 36,
    ENOSYS = 38,
    ENOTEMPTY = 39,
}

impl Errno {
    pub const fn code(self) -> i32 {
        self as i32
    }

    pub const fn name(self) -> &'static str {
        match self {
            Errno::EPERM => "EPERM",
            Errno::ENOENT => "ENOENT",
            Errno::EIO => "EIO",
            Errno::E2BIG => "E2BIG",
            Errno::ENOEXEC => "ENOEXEC",
            Errno::EBADF => "EBADF",
            Errno::ECHILD => "ECHILD",
            Errno::ENOMEM => "ENOMEM",
            Errno::EACCES => "EACCES",
            Errno::EFAULT => "EFAULT",
            Errno::EBUSY => "EBUSY",
            Errno::EEXIST => "EEXIST",
            Errno::EXDEV => "EXDEV",
            Errno::ENODEV => "ENODEV",
            Errno::ENOTDIR => "ENOTDIR",
            Errno::EISDIR => "EISDIR",
            Errno::EINVAL => "EINVAL",
            Errno::EMFILE => "EMFILE",
            Errno::ENOTTY => "ENOTTY",
            Errno::ENOSPC => "ENOSPC",
            Errno::ESPIPE => "ESPIPE",
            Errno::EROFS => "EROFS",
            Errno::ERANGE => "ERANGE",
            Errno::ENAMETOOLONG => "ENAMETOOLONG",
            Errno::ENOSYS => "ENOSYS",
            Errno::ENOTEMPTY => "ENOTEMPTY",
        }
    }
}

impl core::fmt::Display for Errno {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.name())
    }
}

pub type FsResult<T> = Result<T, Errno>;
