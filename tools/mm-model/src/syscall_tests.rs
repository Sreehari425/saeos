//! The syscall table against the real VFS and a fake user address space.

use crate::fake_user::FakeUser;
use crate::fs::devfs::DevFs;
use crate::fs::ramfs::RamFs;
use crate::fs::vfs::Vfs;
use crate::fs::{Errno, FileTable};
use crate::syscall::abi::{self, STAT_SIZE};
use crate::syscall::memory::{
    ARCH_GET_FS, ARCH_SET_FS, MAP_ANONYMOUS, MAP_FIXED, MAP_PRIVATE, MAP_SHARED,
};
use crate::syscall::numbers::*;
use crate::syscall::{Context, Outcome, ProcessOps, dispatch};
use crate::vma::{MemoryMap, Protection};
use std::cell::{Cell, RefCell};

const BASE: u64 = 0x40_0000;

/// A process with one child (pid 9, exit code 3), and a memory map with
/// no pages behind it: the bookkeeping the kernel applies.
struct FakeProcess {
    waited: RefCell<Vec<Option<u64>>>,
    map: RefCell<MemoryMap>,
    fs_base: Cell<u64>,
}

impl ProcessOps for FakeProcess {
    fn getpid(&self) -> u64 {
        5
    }

    fn wait(&self, pid: Option<u64>) -> Result<(u64, i32), Errno> {
        self.waited.borrow_mut().push(pid);
        match pid {
            None | Some(9) => Ok((9, 3)),
            Some(_) => Err(Errno::ECHILD),
        }
    }

    fn brk(&self, address: u64) -> u64 {
        let mut map = self.map.borrow_mut();
        if address != 0 {
            let _ = map.set_brk(address);
        }
        map.brk()
    }

    fn mmap(
        &self,
        hint: u64,
        length: u64,
        protection: Protection,
        fixed: bool,
    ) -> Result<u64, Errno> {
        self.map
            .borrow_mut()
            .mmap(hint, length, protection, fixed)
            .map(|change| change.address)
    }

    fn munmap(&self, start: u64, length: u64) -> Result<(), Errno> {
        self.map.borrow_mut().munmap(start, length).map(|_| ())
    }

    fn mprotect(&self, start: u64, length: u64, protection: Protection) -> Result<(), Errno> {
        self.map
            .borrow_mut()
            .mprotect(start, length, protection)
            .map(|_| ())
    }

    fn set_fs_base(&self, base: u64) {
        self.fs_base.set(base);
    }

    fn fs_base(&self) -> u64 {
        self.fs_base.get()
    }
}

struct World {
    vfs: Vfs,
    files: FileTable,
    memory: FakeUser,
    process: FakeProcess,
}

impl World {
    fn new() -> Self {
        let vfs = Vfs::new();
        vfs.mount("/", RamFs::new()).unwrap();
        vfs.mkdir("/dev").unwrap();
        vfs.mount("/dev", DevFs::new()).unwrap();
        Self {
            vfs,
            files: FileTable::new(),
            memory: FakeUser::new(BASE, 64 * 1024),
            process: FakeProcess {
                waited: RefCell::new(Vec::new()),
                map: RefCell::new(MemoryMap::new(&[], 0x50_0000, 0x7000_0000).unwrap()),
                fs_base: Cell::new(0),
            },
        }
    }

    fn call(&self, number: u64, args: &[u64]) -> i64 {
        let mut all = [0u64; 6];
        all[..args.len()].copy_from_slice(args);
        let context = Context {
            memory: &self.memory,
            vfs: &self.vfs,
            files: &self.files,
            process: &self.process,
        };
        match dispatch(&context, number, all) {
            Outcome::Return(value) => value,
            Outcome::Exit(code) => panic!("unexpected exit({code})"),
        }
    }

    /// Put a NUL-terminated string in user memory at `offset`.
    fn string(&self, offset: u64, text: &str) -> u64 {
        let mut bytes = text.as_bytes().to_vec();
        bytes.push(0);
        self.memory.bytes.borrow_mut()[offset as usize..offset as usize + bytes.len()]
            .copy_from_slice(&bytes);
        BASE + offset
    }

    fn user(&self, offset: u64, length: usize) -> Vec<u8> {
        self.memory.bytes.borrow()[offset as usize..offset as usize + length].to_vec()
    }

    fn put(&self, offset: u64, data: &[u8]) -> u64 {
        self.memory.bytes.borrow_mut()[offset as usize..offset as usize + data.len()]
            .copy_from_slice(data);
        BASE + offset
    }
}

fn errno(error: Errno) -> i64 {
    -i64::from(error.code())
}

const O_RDONLY: u64 = abi::O_RDONLY;
const O_RDWR_CREAT: u64 = abi::O_RDWR | abi::O_CREAT;

#[test]
fn files_round_trip_through_user_memory() {
    let world = World::new();
    let path = world.string(0, "/hello.txt");
    let fd = world.call(OPEN, &[path, O_RDWR_CREAT | abi::O_TRUNC, 0o644]);
    assert_eq!(fd, 0, "lowest descriptor");
    let data = world.put(1000, b"hello from a user buffer");
    assert_eq!(world.call(WRITE, &[0, data, 24]), 24);
    assert_eq!(world.call(LSEEK, &[0, 6, 0]), 6);
    assert_eq!(
        world.call(READ, &[0, BASE + 2000, 100]),
        18,
        "short read at end"
    );
    assert_eq!(world.user(2000, 18), b"from a user buffer");
    assert_eq!(world.call(READ, &[0, BASE + 2000, 100]), 0, "end of file");
    assert_eq!(world.call(LSEEK, &[0, (-4i64) as u64, 2]), 20, "SEEK_END");
    assert_eq!(
        world.call(LSEEK, &[0, 0, 3]),
        errno(Errno::EINVAL),
        "bad whence"
    );
    assert_eq!(world.call(CLOSE, &[0]), 0);
    assert_eq!(world.call(CLOSE, &[0]), errno(Errno::EBADF));
    assert_eq!(world.call(READ, &[0, BASE, 1]), errno(Errno::EBADF));
    assert_eq!(world.call(READ, &[u64::MAX, BASE, 1]), errno(Errno::EBADF));
    // Devices.
    let null = world.string(100, "/dev/null");
    let fd = world.call(OPEN, &[null, abi::O_RDWR]) as u64;
    assert_eq!(world.call(WRITE, &[fd, data, 24]), 24);
    assert_eq!(world.call(READ, &[fd, BASE + 2000, 10]), 0);
}

#[test]
fn bad_pointers_are_efault_and_consume_nothing() {
    let world = World::new();
    let path = world.string(0, "/f");
    let fd = world.call(OPEN, &[path, O_RDWR_CREAT]) as u64;
    let data = world.put(100, b"abcdef");
    assert_eq!(world.call(WRITE, &[fd, data, 6]), 6);
    world.call(LSEEK, &[fd, 0, 0]);
    let outside = BASE + 64 * 1024;
    for (number, args) in [
        (READ, vec![fd, outside, 4]),
        (READ, vec![fd, 0xffff_8000_0000_0000, 4]),
        (READ, vec![fd, BASE + 64 * 1024 - 2, 4]),
        (WRITE, vec![fd, outside, 4]),
        (WRITE, vec![fd, u64::MAX - 1, 4]),
        (OPEN, vec![outside, O_RDONLY]),
        (OPEN, vec![0, O_RDONLY]),
        (STAT, vec![path, outside]),
        (FSTAT, vec![fd, outside]),
        (MKDIR, vec![outside]),
        (GETDENTS64, vec![fd, 0xffff_8000_0000_0000, 100]),
    ] {
        assert_eq!(
            world.call(number, &args),
            errno(Errno::EFAULT),
            "{number} {args:x?}"
        );
    }
    // The failed reads did not move the offset.
    assert_eq!(world.call(READ, &[fd, BASE + 200, 6]), 6);
    assert_eq!(world.user(200, 6), b"abcdef");
}

#[test]
fn paths_flags_and_errors_follow_linux() {
    let world = World::new();
    let empty = world.string(0, "");
    assert_eq!(world.call(OPEN, &[empty, O_RDONLY]), errno(Errno::ENOENT));
    let missing = world.string(10, "/missing");
    assert_eq!(world.call(OPEN, &[missing, O_RDONLY]), errno(Errno::ENOENT));
    assert_eq!(
        world.call(OPEN, &[missing, 3]),
        errno(Errno::EINVAL),
        "O_ACCMODE 3"
    );
    // O_CLOEXEC (0o2000000) and O_LARGEFILE (0o100000) are ignored.
    let fd = world.call(OPEN, &[missing, O_RDWR_CREAT | 0o2000000 | 0o100000]);
    assert!(fd >= 0);
    let exclusive = O_RDWR_CREAT | abi::O_EXCL;
    assert_eq!(
        world.call(OPEN, &[missing, exclusive]),
        errno(Errno::EEXIST)
    );
    assert_eq!(
        world.call(OPEN, &[missing, O_RDONLY | abi::O_DIRECTORY]),
        errno(Errno::ENOTDIR)
    );
    // A path without a NUL within PATH_MAX.
    world.put(8000, &[b'a'; 5000]);
    world.put(13000, &[0]);
    assert_eq!(
        world.call(OPEN, &[BASE + 8000, O_RDONLY]),
        errno(Errno::ENAMETOOLONG)
    );
    // Directories and renames.
    let directory = world.string(200, "/d");
    assert_eq!(world.call(MKDIR, &[directory, 0o755]), 0);
    assert_eq!(world.call(MKDIR, &[directory, 0o755]), errno(Errno::EEXIST));
    let moved = world.string(300, "/d/moved");
    assert_eq!(world.call(RENAME, &[missing, moved]), 0);
    assert_eq!(world.call(RMDIR, &[directory]), errno(Errno::ENOTEMPTY));
    assert_eq!(world.call(UNLINK, &[moved]), 0);
    assert_eq!(world.call(RMDIR, &[directory]), 0);
    assert_eq!(world.call(GETPID, &[]), 5);
    assert_eq!(world.call(1000, &[]), errno(Errno::ENOSYS));
    assert_eq!(world.call(u64::MAX, &[]), errno(Errno::ENOSYS));
}

#[test]
fn stat_layout_matches_x86_64() {
    let world = World::new();
    let path = world.string(0, "/sized");
    let fd = world.call(OPEN, &[path, O_RDWR_CREAT]) as u64;
    let data = world.put(100, &[7; 1000]);
    world.call(WRITE, &[fd, data, 1000]);
    assert_eq!(world.call(STAT, &[path, BASE + 2000]), 0);
    let stat = world.user(2000, STAT_SIZE);
    let field = |offset: usize, width: usize| {
        let mut bytes = [0u8; 8];
        bytes[..width].copy_from_slice(&stat[offset..offset + width]);
        u64::from_le_bytes(bytes)
    };
    assert_ne!(field(8, 8), 0, "st_ino");
    assert_eq!(field(16, 8), 1, "st_nlink");
    assert_eq!(field(24, 4), u64::from(abi::S_IFREG | 0o644), "st_mode");
    assert_eq!(field(48, 8), 1000, "st_size");
    assert_eq!(field(56, 8), 4096, "st_blksize");
    assert_eq!(field(64, 8), 2, "st_blocks (512-byte units)");
    assert_eq!(world.call(FSTAT, &[fd, BASE + 3000]), 0);
    assert_eq!(world.user(3000, STAT_SIZE), stat, "fstat agrees");
    let dev = world.string(400, "/dev/zero");
    assert_eq!(world.call(STAT, &[dev, BASE + 4000]), 0);
    let mode = u32::from_le_bytes(world.user(4000 + 24, 4).try_into().unwrap());
    assert_eq!(mode & 0o170000, abi::S_IFCHR);
}

/// Parse `linux_dirent64` records: (ino, off, type, name).
fn parse_dirents(bytes: &[u8]) -> Vec<(u64, u64, u8, String)> {
    let mut entries = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let record = &bytes[offset..];
        let ino = u64::from_le_bytes(record[0..8].try_into().unwrap());
        let off = u64::from_le_bytes(record[8..16].try_into().unwrap());
        let length = u16::from_le_bytes(record[16..18].try_into().unwrap()) as usize;
        assert_eq!(length % 8, 0, "records are 8-byte aligned");
        let name_end = record[19..length]
            .iter()
            .position(|&byte| byte == 0)
            .unwrap();
        let name = String::from_utf8(record[19..19 + name_end].to_vec()).unwrap();
        entries.push((ino, off, record[18], name));
        offset += length;
    }
    entries
}

#[test]
fn getdents64_pages_through_a_directory() {
    let world = World::new();
    let names: Vec<String> = (0..40).map(|index| format!("entry-{index:02}")).collect();
    for name in &names {
        let path = world.string(0, &format!("/{name}"));
        let fd = world.call(OPEN, &[path, O_RDWR_CREAT]) as u64;
        world.call(CLOSE, &[fd]);
    }
    let root = world.string(0, "/");
    let fd = world.call(OPEN, &[root, O_RDONLY | abi::O_DIRECTORY]) as u64;
    // A buffer too small for even one record.
    assert_eq!(
        world.call(GETDENTS64, &[fd, BASE + 1000, 10]),
        errno(Errno::EINVAL)
    );
    // Read with a small buffer: several calls, every entry exactly once.
    let mut seen = Vec::new();
    loop {
        let count = world.call(GETDENTS64, &[fd, BASE + 1000, 100]);
        assert!(count >= 0);
        if count == 0 {
            break;
        }
        seen.extend(parse_dirents(&world.user(1000, count as usize)));
    }
    let mut seen_names: Vec<_> = seen.iter().map(|entry| entry.3.clone()).collect();
    assert_eq!(seen_names.len(), 41, "40 files and /dev");
    seen_names.retain(|name| name != "dev");
    assert_eq!(seen_names, names);
    assert!(seen.iter().all(|entry| entry.0 != 0));
    let dev = seen.iter().find(|entry| entry.3 == "dev").unwrap();
    assert_eq!(dev.2, 4, "DT_DIR");
    assert!(
        seen.iter()
            .filter(|entry| entry.3 != "dev")
            .all(|entry| entry.2 == 8)
    );
    // d_off resumes after the entry: seek back to the 3rd entry's offset.
    let third = seen[2].1;
    assert_eq!(world.call(LSEEK, &[fd, third, 0]), third as i64);
    let count = world.call(GETDENTS64, &[fd, BASE + 1000, 4096]);
    let rest = parse_dirents(&world.user(1000, count as usize));
    assert_eq!(rest.first().map(|entry| &entry.3), Some(&seen[3].3));
    // Not a directory.
    let file = world.string(0, "/entry-00");
    let file_fd = world.call(OPEN, &[file, O_RDONLY]) as u64;
    assert_eq!(
        world.call(GETDENTS64, &[file_fd, BASE + 1000, 4096]),
        errno(Errno::ENOTDIR)
    );
}

#[test]
fn exit_and_wait4() {
    let world = World::new();
    let context = Context {
        memory: &world.memory,
        vfs: &world.vfs,
        files: &world.files,
        process: &world.process,
    };
    assert_eq!(
        dispatch(&context, EXIT, [0x1234, 0, 0, 0, 0, 0]),
        Outcome::Exit(0x34)
    );
    assert_eq!(
        dispatch(&context, EXIT_GROUP, [7, 0, 0, 0, 0, 0]),
        Outcome::Exit(7)
    );
    // wait4(9, &status, 0, NULL)
    assert_eq!(world.call(WAIT4, &[9, BASE + 500, 0, 0]), 9);
    assert_eq!(
        world.user(500, 4),
        (3i32 << 8).to_le_bytes(),
        "WEXITSTATUS = 3"
    );
    assert_eq!(
        world.call(WAIT4, &[u64::MAX, 0, 0, 0]),
        9,
        "pid -1: any child"
    );
    assert_eq!(world.call(WAIT4, &[4, 0, 0, 0]), errno(Errno::ECHILD));
    assert_eq!(
        world.call(WAIT4, &[9, 0, 1, 0]),
        errno(Errno::EINVAL),
        "WNOHANG"
    );
    assert_eq!(
        world.call(WAIT4, &[0, 0, 0, 0]),
        errno(Errno::EINVAL),
        "process group"
    );
    // A status pointer outside the user half is refused before waiting; one
    // that is merely unmapped fails after the child is collected, as on
    // Linux (wait4 reaps, then its put_user faults).
    assert_eq!(
        world.call(WAIT4, &[9, 0xffff_8000_0000_0000, 0, 0]),
        errno(Errno::EFAULT)
    );
    assert_eq!(
        world.call(WAIT4, &[9, BASE + 64 * 1024, 0, 0]),
        errno(Errno::EFAULT)
    );
    assert_eq!(
        *world.process.waited.borrow(),
        [Some(9), None, Some(4), Some(9)]
    );
}

const ANON: u64 = MAP_PRIVATE | MAP_ANONYMOUS;
const PROT_RW: u64 = 3;

#[test]
fn brk_and_mmap_decode_like_linux() {
    let world = World::new();
    // brk(0) reports the break; growing returns the new one; an impossible
    // request returns the old one (not an error).
    assert_eq!(world.call(BRK, &[0]), 0x50_0000);
    assert_eq!(world.call(BRK, &[0x50_2345]), 0x50_2345);
    assert_eq!(world.call(BRK, &[0x10]), 0x50_2345, "below the start");
    assert_eq!(world.call(BRK, &[u64::MAX]), 0x50_2345);
    // mmap(NULL, 8192, RW, PRIVATE|ANONYMOUS, -1, 0)
    let address = world.call(MMAP, &[0, 8192, PROT_RW, ANON, u64::MAX, 0]);
    assert!(address > 0 && address % 4096 == 0);
    let fixed = world.call(MMAP, &[0x2000_0000, 4096, 1, ANON | MAP_FIXED, u64::MAX, 0]);
    assert_eq!(fixed, 0x2000_0000);
    assert_eq!(
        world.call(MMAP, &[0, 4096, 7, ANON, u64::MAX, 0]),
        errno(Errno::EACCES),
        "W+X"
    );
    assert_eq!(
        world.call(MMAP, &[0, 4096, 0x10, ANON, u64::MAX, 0]),
        errno(Errno::EINVAL),
        "bad prot"
    );
    assert_eq!(
        world.call(MMAP, &[0, 4096, 1, MAP_PRIVATE, 3, 0]),
        errno(Errno::ENODEV),
        "file mapping"
    );
    assert_eq!(
        world.call(MMAP, &[0, 4096, 1, MAP_SHARED | MAP_ANONYMOUS, u64::MAX, 0]),
        errno(Errno::EINVAL)
    );
    assert_eq!(
        world.call(MMAP, &[0, 4096, 1, ANON, u64::MAX, 100]),
        errno(Errno::EINVAL),
        "offset"
    );
    assert_eq!(
        world.call(MMAP, &[0, 0, 1, ANON, u64::MAX, 0]),
        errno(Errno::EINVAL),
        "length 0"
    );
    assert_eq!(
        world.call(MMAP, &[0x2000_0001, 4096, 1, ANON | MAP_FIXED, u64::MAX, 0]),
        errno(Errno::EINVAL)
    );
    // MAP_NORESERVE | MAP_STACK are ignored.
    assert!(world.call(MMAP, &[0, 4096, 3, ANON | 0x4000 | 0x2_0000, u64::MAX, 0]) > 0);
    // mprotect and munmap.
    assert_eq!(world.call(MPROTECT, &[address as u64, 8192, 1]), 0);
    assert_eq!(
        world.call(MPROTECT, &[address as u64, 8192, 7]),
        errno(Errno::EACCES)
    );
    assert_eq!(world.call(MUNMAP, &[address as u64, 8192]), 0);
    assert_eq!(
        world.call(MPROTECT, &[address as u64, 8192, 1]),
        errno(Errno::ENOMEM),
        "gone"
    );
    assert_eq!(
        world.call(MUNMAP, &[address as u64 + 1, 8192]),
        errno(Errno::EINVAL)
    );
}

#[test]
fn arch_prctl_sets_and_reads_the_fs_base() {
    let world = World::new();
    assert_eq!(world.call(ARCH_PRCTL, &[ARCH_SET_FS, 0x7000_1000]), 0);
    assert_eq!(world.process.fs_base.get(), 0x7000_1000);
    assert_eq!(world.call(ARCH_PRCTL, &[ARCH_GET_FS, BASE + 64]), 0);
    assert_eq!(world.user(64, 8), 0x7000_1000u64.to_le_bytes());
    assert_eq!(
        world.call(ARCH_PRCTL, &[ARCH_SET_FS, 0xffff_8000_0000_0000]),
        errno(Errno::EPERM)
    );
    assert_eq!(world.process.fs_base.get(), 0x7000_1000, "unchanged");
    assert_eq!(
        world.call(ARCH_PRCTL, &[ARCH_GET_FS, 0]),
        errno(Errno::EFAULT)
    );
    assert_eq!(
        world.call(ARCH_PRCTL, &[0x1001, 0]),
        errno(Errno::EINVAL),
        "ARCH_SET_GS unsupported"
    );
}
