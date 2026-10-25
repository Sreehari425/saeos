//! `/proc`: read-only files whose contents are generated on every read,
//! like Linux's procfs. `cmdline` and `mounts` are registered by the kernel
//! (`fs::init`); the filesystem itself only knows names and generators.
//!
//! Each read regenerates the whole file and serves the requested offset,
//! so a reader that reads in small pieces sees one consistent snapshot only
//! if the contents do not change in between, as on Linux for small files.
//! `stat` reports size 0, also as on Linux.

use super::errno::{Errno, FsResult};
use super::inode::{DirEntry, FileSystem, Inode, InodeKind, Stat, new_fs_id};
use alloc::string::ToString;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;

/// Produces a file's current contents.
pub type Generator = Arc<dyn Fn() -> Vec<u8> + Send + Sync>;

struct ProcFile {
    ino: u64,
    fs_id: u64,
    generate: Generator,
}

impl Inode for ProcFile {
    fn stat(&self) -> Stat {
        Stat {
            ino: self.ino,
            fs_id: self.fs_id,
            kind: InodeKind::File,
            size: 0,
            nlink: 1,
        }
    }

    fn kind(&self) -> InodeKind {
        InodeKind::File
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> FsResult<usize> {
        let contents = (self.generate)();
        let Ok(start) = usize::try_from(offset) else {
            return Ok(0);
        };
        let Some(rest) = contents.get(start..) else {
            return Ok(0);
        };
        let count = rest.len().min(buf.len());
        buf[..count].copy_from_slice(&rest[..count]);
        Ok(count)
    }

    fn write_at(&self, _offset: u64, _buf: &[u8]) -> FsResult<usize> {
        Err(Errno::EROFS)
    }

    fn truncate(&self, _size: u64) -> FsResult<()> {
        Err(Errno::EROFS)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct ProcDirectory {
    fs_id: u64,
    files: Vec<(&'static str, Arc<ProcFile>)>,
}

impl Inode for ProcDirectory {
    fn stat(&self) -> Stat {
        Stat {
            ino: 1,
            fs_id: self.fs_id,
            kind: InodeKind::Directory,
            size: 0,
            nlink: 2,
        }
    }

    fn kind(&self) -> InodeKind {
        InodeKind::Directory
    }

    fn lookup(&self, name: &str) -> FsResult<Arc<dyn Inode>> {
        self.files
            .iter()
            .find(|(file_name, _)| *file_name == name)
            .map(|(_, file)| file.clone() as Arc<dyn Inode>)
            .ok_or(Errno::ENOENT)
    }

    fn create(&self, _name: &str, _kind: InodeKind) -> FsResult<Arc<dyn Inode>> {
        Err(Errno::EROFS)
    }

    fn unlink(&self, _name: &str) -> FsResult<()> {
        Err(Errno::EROFS)
    }

    fn rmdir(&self, _name: &str) -> FsResult<()> {
        Err(Errno::EROFS)
    }

    fn rename(&self, _old: &str, _new_dir: &Arc<dyn Inode>, _new: &str) -> FsResult<()> {
        Err(Errno::EROFS)
    }

    fn entries(&self) -> FsResult<Vec<DirEntry>> {
        Ok(self
            .files
            .iter()
            .map(|(name, file)| DirEntry {
                name: name.to_string(),
                ino: file.ino,
                kind: InodeKind::File,
            })
            .collect())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub struct ProcFs {
    root: Arc<ProcDirectory>,
}

impl ProcFs {
    /// A procfs with the given files, listed in this order.
    pub fn new(files: Vec<(&'static str, Generator)>) -> Arc<Self> {
        let fs_id = new_fs_id();
        let files = files
            .into_iter()
            .zip(2u64..)
            .map(|((name, generate), ino)| {
                (
                    name,
                    Arc::new(ProcFile {
                        ino,
                        fs_id,
                        generate,
                    }),
                )
            })
            .collect();
        Arc::new(Self {
            root: Arc::new(ProcDirectory { fs_id, files }),
        })
    }
}

impl FileSystem for ProcFs {
    fn root(&self) -> Arc<dyn Inode> {
        self.root.clone()
    }

    fn name(&self) -> &'static str {
        "proc"
    }
}
