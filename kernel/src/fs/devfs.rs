//! Device files: `/dev/null`, `/dev/zero`, `/dev/console`, `/dev/dsp`,
//! `/dev/fb0`.
//! Devices are ordinary inodes, so they use the same `open` / `write` path
//! as files; `/dev/dsp` and `/dev/fb0` hand each open its own inode
//! (`sound/mod.rs`, `drivers/fb.rs`).

use super::errno::{Errno, FsResult};
use super::file::OpenFlags;
use super::inode::{DirEntry, FileSystem, Inode, InodeKind, Stat, new_fs_id};
use alloc::string::ToString;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Device {
    /// Reads are end of file; writes are discarded.
    Null,
    /// Reads return zeros; writes are discarded.
    Zero,
    /// Reads are lines typed at the keyboard or serial port (through the
    /// line discipline, `drivers/tty.rs`); writes go to the screen and the
    /// serial console.
    Console,
    /// Sound output (OSS): each open gets the card through
    /// `sound::open`; `ENODEV` without a card.
    Dsp,
    /// The framebuffer (`drivers/fb.rs`): `ENODEV` without one.
    Fb,
}

const DEVICES: [(&str, Device); 5] = [
    ("console", Device::Console),
    ("dsp", Device::Dsp),
    ("fb0", Device::Fb),
    ("null", Device::Null),
    ("zero", Device::Zero),
];

struct DeviceNode {
    ino: u64,
    fs_id: u64,
    device: Device,
}

impl Inode for DeviceNode {
    fn stat(&self) -> Stat {
        Stat {
            ino: self.ino,
            fs_id: self.fs_id,
            kind: InodeKind::CharDevice,
            size: 0,
            nlink: 1,
        }
    }

    fn kind(&self) -> InodeKind {
        InodeKind::CharDevice
    }

    fn read_at(&self, _offset: u64, buf: &mut [u8]) -> FsResult<usize> {
        match self.device {
            Device::Null => Ok(0),
            Device::Zero => {
                buf.fill(0);
                Ok(buf.len())
            }
            Device::Console => Ok(console_read(buf)),
            Device::Dsp | Device::Fb => Err(Errno::EINVAL),
        }
    }

    fn write_at(&self, _offset: u64, buf: &[u8]) -> FsResult<usize> {
        if self.device == Device::Console {
            console_write(buf);
        }
        Ok(buf.len())
    }

    fn truncate(&self, _size: u64) -> FsResult<()> {
        Err(Errno::EINVAL)
    }

    fn is_terminal(&self) -> bool {
        self.device == Device::Console
    }

    fn open(&self, _flags: OpenFlags) -> FsResult<Option<Arc<dyn Inode>>> {
        match self.device {
            #[cfg(not(host_model))]
            Device::Dsp => crate::sound::open(self.ino, self.fs_id).map(Some),
            #[cfg(not(host_model))]
            Device::Fb => crate::drivers::fb::open(self.ino, self.fs_id).map(Some),
            #[cfg(host_model)]
            Device::Dsp | Device::Fb => Err(Errno::ENODEV),
            _ => Ok(None),
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(not(host_model))]
fn console_read(buffer: &mut [u8]) -> usize {
    crate::drivers::tty::read_console(buffer)
}

/// Host model: console input comes from this queue, through the same line
/// discipline the kernel uses; an empty queue reads as end of file rather
/// than blocking.
#[cfg(host_model)]
pub static CONSOLE_INPUT: std::sync::Mutex<Vec<u8>> = std::sync::Mutex::new(Vec::new());

#[cfg(host_model)]
static CONSOLE_DISCIPLINE: std::sync::Mutex<crate::tty::LineDiscipline> =
    std::sync::Mutex::new(crate::tty::LineDiscipline::new());

#[cfg(host_model)]
fn console_read(buffer: &mut [u8]) -> usize {
    let mut discipline = CONSOLE_DISCIPLINE.lock().unwrap();
    let mut echo = Vec::new();
    for byte in CONSOLE_INPUT.lock().unwrap().drain(..) {
        discipline.input(byte, &mut echo);
    }
    console_write(&echo);
    discipline.read(buffer).unwrap_or(0)
}

#[cfg(not(host_model))]
fn console_write(bytes: &[u8]) {
    crate::print!("{}", alloc::string::String::from_utf8_lossy(bytes));
}

/// Host model: console output is captured for tests.
#[cfg(host_model)]
pub static CONSOLE_CAPTURE: std::sync::Mutex<Vec<u8>> = std::sync::Mutex::new(Vec::new());

#[cfg(host_model)]
fn console_write(bytes: &[u8]) {
    CONSOLE_CAPTURE.lock().unwrap().extend_from_slice(bytes);
}

struct DeviceDirectory {
    fs_id: u64,
    nodes: Vec<(&'static str, Arc<DeviceNode>)>,
}

impl Inode for DeviceDirectory {
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
        self.nodes
            .iter()
            .find(|(node_name, _)| *node_name == name)
            .map(|(_, node)| node.clone() as Arc<dyn Inode>)
            .ok_or(Errno::ENOENT)
    }

    // The device set is fixed: nothing can be added or removed.
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
            .nodes
            .iter()
            .map(|(name, node)| DirEntry {
                name: name.to_string(),
                ino: node.ino,
                kind: InodeKind::CharDevice,
            })
            .collect())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub struct DevFs {
    root: Arc<DeviceDirectory>,
}

impl DevFs {
    pub fn new() -> Arc<Self> {
        let fs_id = new_fs_id();
        let nodes = DEVICES
            .iter()
            .zip(2u64..)
            .map(|(&(name, device), ino)| (name, Arc::new(DeviceNode { ino, fs_id, device })))
            .collect();
        Arc::new(Self {
            root: Arc::new(DeviceDirectory { fs_id, nodes }),
        })
    }
}

impl FileSystem for DevFs {
    fn root(&self) -> Arc<dyn Inode> {
        self.root.clone()
    }

    fn name(&self) -> &'static str {
        "devfs"
    }
}
