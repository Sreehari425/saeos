//! Error numbers, with Linux's values so syscalls can return `-(errno)`
//! unchanged once userspace exists.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum Errno {
    ENOENT = 2,
    EIO = 5,
    EBADF = 9,
    ENOMEM = 12,
    EBUSY = 16,
    EEXIST = 17,
    EXDEV = 18,
    ENOTDIR = 20,
    EISDIR = 21,
    EINVAL = 22,
    EMFILE = 24,
    ENOSPC = 28,
    ESPIPE = 29,
    EROFS = 30,
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
            Errno::ENOENT => "ENOENT",
            Errno::EIO => "EIO",
            Errno::EBADF => "EBADF",
            Errno::ENOMEM => "ENOMEM",
            Errno::EBUSY => "EBUSY",
            Errno::EEXIST => "EEXIST",
            Errno::EXDEV => "EXDEV",
            Errno::ENOTDIR => "ENOTDIR",
            Errno::EISDIR => "EISDIR",
            Errno::EINVAL => "EINVAL",
            Errno::EMFILE => "EMFILE",
            Errno::ENOSPC => "ENOSPC",
            Errno::ESPIPE => "ESPIPE",
            Errno::EROFS => "EROFS",
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
