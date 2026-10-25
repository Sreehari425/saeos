//! Unit tests for the VFS, ramfs, devfs, the tar parser, and RamDisk.

use crate::fs::block::{BlockDevice, RamDisk};
use crate::fs::devfs::{CONSOLE_CAPTURE, DevFs};
use crate::fs::initramfs::{self, EntryKind, TarError};
use crate::fs::ramfs::RamFs;
use crate::fs::vfs::Vfs;
use crate::fs::{Errno, FileSystem, FileTable, InodeKind, OpenFlags, Whence};
use std::sync::Arc;

const RW: OpenFlags = OpenFlags::READ_WRITE;
const CREATE: OpenFlags = OpenFlags::CREATE;

/// ramfs at `/`, devfs at `/dev`.
fn setup() -> (Vfs, FileTable) {
    let vfs = Vfs::new();
    vfs.mount("/", RamFs::new()).unwrap();
    vfs.mkdir("/dev").unwrap();
    vfs.mount("/dev", DevFs::new()).unwrap();
    (vfs, FileTable::new())
}

fn write_file(vfs: &Vfs, files: &FileTable, path: &str, data: &[u8]) {
    let fd = vfs
        .open(files, path, OpenFlags::WRITE | CREATE | OpenFlags::TRUNC)
        .unwrap();
    assert_eq!(vfs.write(files, fd, data).unwrap(), data.len());
    vfs.close(files, fd).unwrap();
}

fn read_file(vfs: &Vfs, files: &FileTable, path: &str) -> Result<Vec<u8>, Errno> {
    let fd = vfs.open(files, path, OpenFlags::READ)?;
    let mut contents = Vec::new();
    let mut chunk = [0u8; 7];
    loop {
        let count = vfs.read(files, fd, &mut chunk)?;
        if count == 0 {
            break;
        }
        contents.extend_from_slice(&chunk[..count]);
    }
    vfs.close(files, fd)?;
    Ok(contents)
}

#[test]
fn paths_normalize_dots_and_stop_at_root() {
    let (vfs, files) = setup();
    vfs.mkdir("/a").unwrap();
    write_file(&vfs, &files, "/a/f", b"x");
    for path in [
        "/a/f",
        "a/f",
        "//a///f",
        "/a/./f",
        "/a/../a/f",
        "/../../a/f",
        "/dev/../a/f",
    ] {
        assert_eq!(read_file(&vfs, &files, path), Ok(b"x".to_vec()), "{path}");
    }
    assert_eq!(vfs.stat(""), Err(Errno::ENOENT));
    assert_eq!(vfs.stat("/a/f/"), Err(Errno::ENOTDIR));
    assert_eq!(vfs.stat("/a/f/x"), Err(Errno::ENOTDIR));
    assert_eq!(vfs.stat("/a/missing/x"), Err(Errno::ENOENT));
    assert_eq!(
        vfs.stat(&format!("/{}", "n".repeat(256))),
        Err(Errno::ENAMETOOLONG)
    );
    assert_eq!(vfs.stat("/").unwrap().kind, InodeKind::Directory);
}

#[test]
fn open_flags_follow_linux() {
    let (vfs, files) = setup();
    vfs.mkdir("/d").unwrap();
    assert_eq!(
        vfs.open(&files, "/f", OpenFlags::CREATE),
        Err(Errno::EINVAL)
    );
    assert_eq!(vfs.open(&files, "/f", OpenFlags::READ), Err(Errno::ENOENT));
    let fd = vfs.open(&files, "/f", OpenFlags::WRITE | CREATE).unwrap();
    vfs.close(&files, fd).unwrap();
    assert_eq!(
        vfs.open(&files, "/f", OpenFlags::WRITE | CREATE | OpenFlags::EXCL),
        Err(Errno::EEXIST)
    );
    assert_eq!(vfs.open(&files, "/d", OpenFlags::WRITE), Err(Errno::EISDIR));
    assert_eq!(
        vfs.open(&files, "/d", OpenFlags::READ | OpenFlags::TRUNC),
        Err(Errno::EISDIR)
    );
    assert_eq!(
        vfs.open(&files, "/f", OpenFlags::READ | OpenFlags::DIRECTORY),
        Err(Errno::ENOTDIR)
    );
    assert_eq!(
        vfs.open(&files, "/f/x", OpenFlags::WRITE | CREATE),
        Err(Errno::ENOTDIR)
    );

    let read_only = vfs.open(&files, "/f", OpenFlags::READ).unwrap();
    assert_eq!(vfs.write(&files, read_only, b"no"), Err(Errno::EBADF));
    assert_eq!(vfs.ftruncate(&files, read_only, 0), Err(Errno::EBADF));
    let write_only = vfs.open(&files, "/f", OpenFlags::WRITE).unwrap();
    assert_eq!(vfs.read(&files, write_only, &mut [0; 4]), Err(Errno::EBADF));
    let directory = vfs.open(&files, "/d", OpenFlags::READ).unwrap();
    assert_eq!(vfs.read(&files, directory, &mut [0; 4]), Err(Errno::EISDIR));
    assert_eq!(vfs.read(&files, 99, &mut [0; 4]), Err(Errno::EBADF));
    assert_eq!(vfs.close(&files, 99), Err(Errno::EBADF));
}

#[test]
fn reads_writes_seeks_and_truncation() {
    let (vfs, files) = setup();
    let fd = vfs.open(&files, "/f", RW | CREATE).unwrap();
    assert_eq!(vfs.write(&files, fd, b"hello world").unwrap(), 11);
    assert_eq!(vfs.lseek(&files, fd, -5, Whence::End).unwrap(), 6);
    let mut buffer = [0u8; 16];
    assert_eq!(vfs.read(&files, fd, &mut buffer).unwrap(), 5);
    assert_eq!(&buffer[..5], b"world");
    assert_eq!(vfs.read(&files, fd, &mut buffer).unwrap(), 0, "end of file");
    assert_eq!(vfs.lseek(&files, fd, -1, Whence::Set), Err(Errno::EINVAL));
    // A write past the end leaves a zero-filled gap.
    vfs.lseek(&files, fd, 20, Whence::Set).unwrap();
    vfs.write(&files, fd, b"!").unwrap();
    let contents = read_file(&vfs, &files, "/f").unwrap();
    assert_eq!(contents.len(), 21);
    assert!(contents[11..20].iter().all(|&byte| byte == 0));
    // Truncate down and up.
    vfs.ftruncate(&files, fd, 5).unwrap();
    assert_eq!(read_file(&vfs, &files, "/f").unwrap(), b"hello");
    vfs.ftruncate(&files, fd, 8).unwrap();
    assert_eq!(read_file(&vfs, &files, "/f").unwrap(), b"hello\0\0\0");
    vfs.close(&files, fd).unwrap();
    // APPEND always writes at the end; TRUNC empties.
    let append = vfs
        .open(&files, "/f", OpenFlags::WRITE | OpenFlags::APPEND)
        .unwrap();
    vfs.lseek(&files, append, 0, Whence::Set).unwrap();
    vfs.write(&files, append, b"+").unwrap();
    assert_eq!(read_file(&vfs, &files, "/f").unwrap(), b"hello\0\0\0+");
    let truncating = vfs
        .open(&files, "/f", OpenFlags::WRITE | OpenFlags::TRUNC)
        .unwrap();
    assert_eq!(vfs.fstat(&files, truncating).unwrap().size, 0);
}

#[test]
fn directories_and_their_errors() {
    let (vfs, files) = setup();
    vfs.mkdir("/a").unwrap();
    assert_eq!(vfs.mkdir("/a"), Err(Errno::EEXIST));
    assert_eq!(vfs.mkdir("/"), Err(Errno::EEXIST));
    assert_eq!(vfs.mkdir("/missing/b"), Err(Errno::ENOENT));
    vfs.mkdir("/a/b").unwrap();
    write_file(&vfs, &files, "/a/f", b"");
    assert_eq!(vfs.rmdir("/a"), Err(Errno::ENOTEMPTY));
    assert_eq!(vfs.rmdir("/a/f"), Err(Errno::ENOTDIR));
    assert_eq!(vfs.unlink("/a/b"), Err(Errno::EISDIR));
    assert_eq!(vfs.unlink("/a/nope"), Err(Errno::ENOENT));
    assert_eq!(vfs.rmdir("/"), Err(Errno::EBUSY));
    assert_eq!(vfs.unlink("/"), Err(Errno::EISDIR));
    assert_eq!(vfs.rmdir("/a/."), Err(Errno::EINVAL));
    assert_eq!(vfs.rmdir("/dev"), Err(Errno::EBUSY), "mount point");

    let fd = vfs
        .open(&files, "/a", OpenFlags::READ | OpenFlags::DIRECTORY)
        .unwrap();
    let names: Vec<_> = vfs
        .readdir(&files, fd)
        .unwrap()
        .into_iter()
        .map(|entry| (entry.name, entry.kind))
        .collect();
    assert_eq!(
        names,
        [
            ("b".to_string(), InodeKind::Directory),
            ("f".to_string(), InodeKind::File)
        ]
    );
    assert!(
        vfs.readdir(&files, fd).unwrap().is_empty(),
        "position advanced"
    );
    vfs.lseek(&files, fd, 0, Whence::Set).unwrap();
    assert_eq!(vfs.readdir(&files, fd).unwrap().len(), 2, "rewound");
    vfs.close(&files, fd).unwrap();
    vfs.unlink("/a/f").unwrap();
    vfs.rmdir("/a/b").unwrap();
    vfs.rmdir("/a").unwrap();
    assert_eq!(vfs.stat("/a"), Err(Errno::ENOENT));
}

#[test]
fn rename_rules() {
    let (vfs, files) = setup();
    for directory in ["/a", "/a/sub", "/b", "/full", "/full/x"] {
        vfs.mkdir(directory).unwrap();
    }
    write_file(&vfs, &files, "/f", b"one");
    write_file(&vfs, &files, "/g", b"two");
    vfs.rename("/f", "/a/f").unwrap();
    assert_eq!(read_file(&vfs, &files, "/a/f").unwrap(), b"one");
    vfs.rename("/g", "/a/f").unwrap();
    assert_eq!(
        read_file(&vfs, &files, "/a/f").unwrap(),
        b"two",
        "file replaced"
    );
    vfs.rename("/a/f", "/a/f").unwrap();
    assert_eq!(vfs.rename("/a", "/a/sub/inside"), Err(Errno::EINVAL));
    assert_eq!(vfs.rename("/a", "/a/x"), Err(Errno::EINVAL));
    assert_eq!(vfs.rename("/a", "/full"), Err(Errno::ENOTEMPTY));
    assert_eq!(vfs.rename("/a", "/a/f"), Err(Errno::EINVAL));
    // Onto the source's own (non-empty) parent: found by the model test,
    // where it used to self-deadlock.
    assert_eq!(vfs.rename("/a/sub", "/a"), Err(Errno::ENOTEMPTY));
    assert_eq!(vfs.rename("/a/f", "/b"), Err(Errno::EISDIR));
    write_file(&vfs, &files, "/plain", b"");
    assert_eq!(vfs.rename("/b", "/plain"), Err(Errno::ENOTDIR));
    assert_eq!(vfs.rename("/a/f", "/dev/f"), Err(Errno::EXDEV));
    assert_eq!(vfs.rename("/dev", "/devices"), Err(Errno::EBUSY));
    assert_eq!(vfs.rename("/missing", "/x"), Err(Errno::ENOENT));
    // Moving a directory carries its subtree and fixes its parent link.
    vfs.rename("/a", "/b/a").unwrap();
    assert_eq!(read_file(&vfs, &files, "/b/a/f").unwrap(), b"two");
    assert_eq!(read_file(&vfs, &files, "/b/a/sub/../f").unwrap(), b"two");
    assert_eq!(vfs.rename("/b", "/b/a/sub/b"), Err(Errno::EINVAL));
    // An empty directory may be replaced by a directory.
    vfs.mkdir("/empty").unwrap();
    vfs.rename("/b", "/empty").unwrap();
    assert_eq!(read_file(&vfs, &files, "/empty/a/f").unwrap(), b"two");
}

#[test]
fn unlinked_files_live_until_closed() {
    let filesystem = RamFs::new();
    let vfs = Vfs::new();
    vfs.mount("/", filesystem.clone()).unwrap();
    let files = FileTable::new();
    let fd = vfs.open(&files, "/tmp", RW | CREATE).unwrap();
    vfs.write(&files, fd, &[7; 1000]).unwrap();
    vfs.unlink("/tmp").unwrap();
    assert_eq!(vfs.stat("/tmp"), Err(Errno::ENOENT));
    vfs.write(&files, fd, &[8; 24]).unwrap();
    vfs.lseek(&files, fd, 990, Whence::Set).unwrap();
    let mut buffer = [0u8; 64];
    assert_eq!(vfs.read(&files, fd, &mut buffer).unwrap(), 34);
    assert_eq!(filesystem.used_bytes(), 1024, "still accounted while open");
    vfs.close(&files, fd).unwrap();
    assert_eq!(filesystem.used_bytes(), 0, "released on last close");
}

#[test]
fn space_limit_is_enospc_without_partial_damage() {
    let filesystem = RamFs::with_limit(1000);
    let vfs = Vfs::new();
    vfs.mount("/", filesystem.clone()).unwrap();
    let files = FileTable::new();
    let fd = vfs.open(&files, "/f", RW | CREATE).unwrap();
    vfs.write(&files, fd, &[1; 600]).unwrap();
    vfs.lseek(&files, fd, 0, Whence::Set).unwrap();
    assert_eq!(vfs.write(&files, fd, &[2; 1001]), Err(Errno::ENOSPC));
    assert_eq!(vfs.ftruncate(&files, fd, 1001), Err(Errno::ENOSPC));
    assert_eq!(
        read_file(&vfs, &files, "/f").unwrap(),
        vec![1; 600],
        "unchanged"
    );
    vfs.ftruncate(&files, fd, 1000).unwrap();
    assert_eq!(filesystem.used_bytes(), 1000);
    vfs.ftruncate(&files, fd, 0).unwrap();
    assert_eq!(filesystem.used_bytes(), 0);
}

#[test]
fn descriptors_reuse_the_lowest_and_cap_at_the_limit() {
    let (vfs, files) = setup();
    write_file(&vfs, &files, "/f", b"");
    let opened: Vec<_> = (0..crate::fs::file::MAX_FDS)
        .map(|_| vfs.open(&files, "/f", OpenFlags::READ).unwrap())
        .collect();
    assert_eq!(
        opened,
        (0..crate::fs::file::MAX_FDS as u32).collect::<Vec<_>>()
    );
    assert_eq!(vfs.open(&files, "/f", OpenFlags::READ), Err(Errno::EMFILE));
    vfs.close(&files, 7).unwrap();
    vfs.close(&files, 3).unwrap();
    assert_eq!(vfs.open(&files, "/f", OpenFlags::READ).unwrap(), 3);
    assert_eq!(vfs.open(&files, "/f", OpenFlags::READ).unwrap(), 7);
}

#[test]
fn mounts_cross_and_reject_duplicates() {
    let (vfs, files) = setup();
    assert_eq!(vfs.mount("/dev", DevFs::new()), Err(Errno::EBUSY));
    assert_eq!(vfs.mount("/nowhere", DevFs::new()), Err(Errno::ENOENT));
    write_file(&vfs, &files, "/file", b"");
    assert_eq!(vfs.mount("/file", DevFs::new()), Err(Errno::ENOTDIR));
    assert_eq!(vfs.stat("/dev/null").unwrap().kind, InodeKind::CharDevice);
    assert_eq!(vfs.stat("/dev/../file").unwrap().kind, InodeKind::File);
    assert_ne!(
        vfs.stat("/dev").unwrap().fs_id,
        vfs.stat("/").unwrap().fs_id
    );
    let mounts = vfs.mounts();
    assert_eq!(mounts.len(), 2);
}

#[test]
fn device_files() {
    let (vfs, files) = setup();
    let null = vfs.open(&files, "/dev/null", RW).unwrap();
    assert_eq!(vfs.read(&files, null, &mut [9; 8]).unwrap(), 0);
    assert_eq!(vfs.write(&files, null, b"gone").unwrap(), 4);
    assert_eq!(vfs.lseek(&files, null, 0, Whence::Set), Err(Errno::ESPIPE));
    let zero = vfs.open(&files, "/dev/zero", OpenFlags::READ).unwrap();
    let mut buffer = [9u8; 8];
    assert_eq!(vfs.read(&files, zero, &mut buffer).unwrap(), 8);
    assert_eq!(buffer, [0; 8]);
    let console = vfs.open(&files, "/dev/console", OpenFlags::WRITE).unwrap();
    CONSOLE_CAPTURE.lock().unwrap().clear();
    vfs.write(&files, console, b"hi console").unwrap();
    assert_eq!(CONSOLE_CAPTURE.lock().unwrap().as_slice(), b"hi console");
    assert_eq!(
        vfs.open(&files, "/dev/new", OpenFlags::WRITE | CREATE),
        Err(Errno::EROFS)
    );
    assert_eq!(vfs.unlink("/dev/null"), Err(Errno::EROFS));
    assert_eq!(vfs.mkdir("/dev/dir"), Err(Errno::EROFS));
}

/// Minimal ustar writer for the parser tests (mirrors kernel/build.rs).
fn tar_entry(archive: &mut Vec<u8>, path: &str, kind: u8, data: &[u8]) {
    let mut header = [0u8; 512];
    header[..path.len()].copy_from_slice(path.as_bytes());
    let mut octal = |range: std::ops::Range<usize>, value: u64| {
        let width = range.len() - 1;
        header[range].copy_from_slice(format!("{value:0width$o}\0").as_bytes());
    };
    octal(100..108, 0o644);
    octal(108..116, 0);
    octal(116..124, 0);
    octal(124..136, data.len() as u64);
    octal(136..148, 0);
    header[156] = kind;
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    header[148..156].fill(b' ');
    let checksum: u64 = header.iter().map(|&byte| u64::from(byte)).sum();
    header[148..156].copy_from_slice(format!("{checksum:06o}\0 ").as_bytes());
    archive.extend_from_slice(&header);
    archive.extend_from_slice(data);
    archive.resize(archive.len().next_multiple_of(512), 0);
}

fn sample_archive() -> Vec<u8> {
    let mut archive = Vec::new();
    tar_entry(&mut archive, "etc", b'5', &[]);
    tar_entry(&mut archive, "etc/hello", b'0', b"hello\n");
    tar_entry(&mut archive, "./deep/a/b/file", b'0', &[5; 1300]);
    archive.extend_from_slice(&[0; 1024]);
    archive
}

#[test]
fn tar_parses_and_unpacks() {
    let archive = sample_archive();
    let entries = initramfs::parse(&archive).unwrap();
    let summary: Vec<_> = entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry.kind, entry.data.len()))
        .collect();
    assert_eq!(
        summary,
        [
            ("etc", EntryKind::Directory, 0),
            ("etc/hello", EntryKind::File, 6),
            ("deep/a/b/file", EntryKind::File, 1300),
        ]
    );
    let filesystem = RamFs::new();
    let vfs = Vfs::new();
    vfs.mount("/", filesystem.clone()).unwrap();
    assert_eq!(
        initramfs::unpack(&filesystem.root(), &archive),
        Ok((2, 1306))
    );
    let files = FileTable::new();
    assert_eq!(read_file(&vfs, &files, "/etc/hello").unwrap(), b"hello\n");
    assert_eq!(
        read_file(&vfs, &files, "/deep/a/b/file").unwrap(),
        vec![5; 1300]
    );
}

#[test]
fn tar_rejects_every_kind_of_damage() {
    let archive = sample_archive();
    // Any truncation (losing the end marker or part of an entry) fails.
    let step = if cfg!(miri) { 97 } else { 1 };
    for length in (0..archive.len() - 1024).step_by(step) {
        assert!(
            initramfs::parse(&archive[..length]).is_err(),
            "truncated to {length}"
        );
    }
    let corrupt = |offset: usize, byte: u8| {
        let mut copy = archive.clone();
        copy[offset] = byte;
        initramfs::parse(&copy).err()
    };
    assert_eq!(corrupt(0, b'X'), Some(TarError::BadChecksum));
    assert_eq!(corrupt(257, b'X'), Some(TarError::BadMagic));
    let mut bad_size = Vec::new();
    tar_entry(&mut bad_size, "f", b'0', b"abc");
    bad_size[124] = b'9';
    assert!(matches!(
        initramfs::parse(&bad_size),
        Err(TarError::BadNumber | TarError::BadChecksum)
    ));
    let mut huge = Vec::new();
    tar_entry(&mut huge, "f", b'0', &[]);
    huge.extend_from_slice(&[0; 1024]);
    // Claim a 1 GiB body (with a correct checksum).
    let mut patched = huge.clone();
    patched[124..136].copy_from_slice(b"10000000000\0");
    patched[148..156].fill(b' ');
    let checksum: u64 = patched[..512].iter().map(|&byte| u64::from(byte)).sum();
    patched[148..156].copy_from_slice(format!("{checksum:06o}\0 ").as_bytes());
    assert_eq!(initramfs::parse(&patched).unwrap_err(), TarError::Truncated);
    let mut escape = Vec::new();
    tar_entry(&mut escape, "../etc/passwd", b'0', b"x");
    escape.extend_from_slice(&[0; 1024]);
    assert_eq!(initramfs::parse(&escape).unwrap_err(), TarError::BadPath);
    let mut link = Vec::new();
    tar_entry(&mut link, "link", b'2', &[]);
    link.extend_from_slice(&[0; 1024]);
    assert_eq!(
        initramfs::parse(&link).unwrap_err(),
        TarError::UnsupportedType(b'2')
    );
}

#[test]
fn ramdisk_checks_bounds_and_sizes() {
    assert!(RamDisk::new(100, 4).is_err());
    let disk = RamDisk::new(512, 4).unwrap();
    assert_eq!((disk.block_size(), disk.block_count()), (512, 4));
    disk.write_block(3, &[0xab; 512]).unwrap();
    let mut block = [0u8; 512];
    disk.read_block(3, &mut block).unwrap();
    assert_eq!(block, [0xab; 512]);
    assert_eq!(disk.read_block(4, &mut block), Err(Errno::EIO));
    assert_eq!(disk.write_block(0, &[0; 100]), Err(Errno::EINVAL));
}

#[test]
fn concurrent_writers_keep_their_own_data() {
    let (vfs, _) = setup();
    let vfs = Arc::new(vfs);
    let threads = if cfg!(miri) { 2 } else { 4 };
    let rounds = if cfg!(miri) { 5 } else { 200 };
    std::thread::scope(|scope| {
        for worker in 0..threads {
            let vfs = vfs.clone();
            scope.spawn(move || {
                let files = FileTable::new();
                let path = format!("/worker-{worker}");
                for round in 0..rounds {
                    let data = vec![(worker * 31 + round) as u8; 100 + round];
                    write_file(&vfs, &files, &path, &data);
                    assert_eq!(read_file(&vfs, &files, &path).unwrap(), data);
                    // Shared directory churn at the same time.
                    let _ = vfs.mkdir("/shared");
                    let _ = vfs.rmdir("/shared");
                }
            });
        }
    });
}
