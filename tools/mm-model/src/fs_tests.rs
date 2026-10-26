//! Unit tests for the VFS, ramfs, devfs, the cpio parser, and RamDisk.

use crate::fs::block::{BlockDevice, RamDisk};
use crate::fs::devfs::{CONSOLE_CAPTURE, CONSOLE_INPUT, DevFs};
use crate::fs::initramfs::{self, CpioError, EntryKind};
use crate::fs::procfs::{Generator, ProcFs};
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

fn sample_image() -> mkinitramfs::Image {
    let mut image = mkinitramfs::Image::default();
    image
        .add_file("/etc/hello", 0o644, b"hello\n".to_vec())
        .unwrap();
    image
        .add_file("/deep/a/b/file", 0o644, vec![5; 1300])
        .unwrap();
    image.add_directory("/empty").unwrap();
    image
}

fn unpack_into_vfs(archive: &[u8]) -> (Vfs, Result<(usize, u64), CpioError>) {
    let (vfs, result) = unpack_with_limit(archive, crate::fs::ramfs::DEFAULT_LIMIT);
    (
        vfs,
        result.map(|unpacked| {
            assert!(
                unpacked.skipped.is_empty(),
                "skipped {:?}",
                unpacked.skipped
            );
            (unpacked.files, unpacked.bytes)
        }),
    )
}

fn unpack_with_limit(archive: &[u8], limit: u64) -> (Vfs, Result<initramfs::Unpacked, CpioError>) {
    let filesystem = RamFs::with_limit(limit);
    let vfs = Vfs::new();
    vfs.mount("/", filesystem.clone()).unwrap();
    let result = initramfs::unpack(&filesystem.root(), archive);
    (vfs, result)
}

#[test]
fn cpio_parses_and_unpacks() {
    let archive = sample_image().to_cpio();
    let entries = initramfs::parse(&archive).unwrap();
    let summary: Vec<_> = entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry.kind, entry.data.len()))
        .collect();
    assert_eq!(
        summary,
        [
            ("deep", EntryKind::Directory, 0),
            ("deep/a", EntryKind::Directory, 0),
            ("deep/a/b", EntryKind::Directory, 0),
            ("deep/a/b/file", EntryKind::File, 1300),
            ("empty", EntryKind::Directory, 0),
            ("etc", EntryKind::Directory, 0),
            ("etc/hello", EntryKind::File, 6),
        ]
    );
    let (vfs, result) = unpack_into_vfs(&archive);
    assert_eq!(result, Ok((2, 1306)));
    let files = FileTable::new();
    assert_eq!(read_file(&vfs, &files, "/etc/hello").unwrap(), b"hello\n");
    assert_eq!(
        read_file(&vfs, &files, "/deep/a/b/file").unwrap(),
        vec![5; 1300]
    );
    assert_eq!(vfs.stat("/empty").unwrap().kind, InodeKind::Directory);
}

#[test]
fn a_file_that_does_not_fit_is_skipped_not_left_empty() {
    let mut image = mkinitramfs::Image::default();
    image.add_file("/bin/init", 0o755, vec![1; 100]).unwrap();
    image
        .add_file("/music/huge.wav", 0o644, vec![2; 5000])
        .unwrap();
    image.add_file("/sbin/after", 0o755, vec![3; 100]).unwrap();
    let (vfs, result) = unpack_with_limit(&image.to_cpio(), 1000);
    let unpacked = result.unwrap();
    assert_eq!((unpacked.files, unpacked.bytes), (2, 200));
    assert_eq!(
        unpacked.skipped,
        [("music/huge.wav".to_string(), Errno::ENOSPC, 5000)]
    );
    // No empty file pretends to be it, and entries after it still unpack.
    assert_eq!(vfs.stat("/music/huge.wav"), Err(Errno::ENOENT));
    let files = FileTable::new();
    assert_eq!(
        read_file(&vfs, &files, "/sbin/after").unwrap(),
        vec![3; 100]
    );
    assert_eq!(read_file(&vfs, &files, "/bin/init").unwrap(), vec![1; 100]);
}

#[test]
fn cpio_fixture_image_unpacks_with_matching_hashes() {
    let mut image = mkinitramfs::Image::default();
    image.add_test_fixtures().unwrap();
    let (vfs, result) = unpack_into_vfs(&image.to_cpio());
    result.unwrap();
    let files = FileTable::new();
    let manifest = read_file(&vfs, &files, "/test/MANIFEST").unwrap();
    for line in std::str::from_utf8(&manifest).unwrap().lines() {
        let fields: Vec<_> = line.split(' ').collect();
        let data = read_file(&vfs, &files, fields[0]).unwrap();
        assert_eq!(data.len().to_string(), fields[1], "{line}");
        assert_eq!(
            format!("{:016x}", mkinitramfs::fnv1a(&data)),
            fields[2],
            "{line}"
        );
    }
}

#[test]
fn concatenated_archives_override_earlier_files() {
    let mut first = mkinitramfs::Image::default();
    first
        .add_file("/etc/hello", 0o644, b"a much longer original".to_vec())
        .unwrap();
    first
        .add_file("/etc/kept", 0o644, b"kept".to_vec())
        .unwrap();
    let mut second = mkinitramfs::Image::default();
    second
        .add_file("/etc/hello", 0o644, b"new".to_vec())
        .unwrap();
    second.add_file("/extra", 0o644, b"x".to_vec()).unwrap();
    let mut archive = first.to_cpio();
    // Linux allows zero padding between concatenated archives.
    archive.extend_from_slice(&[0; 512]);
    archive.extend_from_slice(&second.to_cpio());
    let (vfs, result) = unpack_into_vfs(&archive);
    assert_eq!(result, Ok((4, 22 + 4 + 3 + 1)));
    let files = FileTable::new();
    assert_eq!(
        read_file(&vfs, &files, "/etc/hello").unwrap(),
        b"new",
        "replaced and truncated"
    );
    assert_eq!(read_file(&vfs, &files, "/etc/kept").unwrap(), b"kept");
    assert_eq!(read_file(&vfs, &files, "/extra").unwrap(), b"x");
    // A file cannot be replaced by a directory (or the reverse).
    let mut clash = mkinitramfs::Image::default();
    clash.add_file("/extra/inside", 0o644, Vec::new()).unwrap();
    archive.extend_from_slice(&clash.to_cpio());
    assert_eq!(
        unpack_into_vfs(&archive).1,
        Err(CpioError::Filesystem(Errno::ENOTDIR))
    );
}

/// One raw entry followed by a trailer.
fn raw_archive(mode: u32, path: &str, data: &[u8]) -> Vec<u8> {
    let mut archive = Vec::new();
    mkinitramfs::cpio::push_entry(&mut archive, 1, mode, path, data);
    mkinitramfs::cpio::push_entry(&mut archive, 0, 0, mkinitramfs::cpio::TRAILER, &[]);
    archive
}

#[test]
fn cpio_rejects_every_kind_of_damage() {
    let archive = sample_image().to_cpio();
    // Any truncation (losing the trailer or part of an entry) fails.
    let step = if cfg!(miri) { 37 } else { 1 };
    for length in (0..archive.len()).step_by(step) {
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
    assert_eq!(corrupt(0, b'X'), Some(CpioError::BadMagic));
    assert_eq!(
        corrupt(5, b'2'),
        Some(CpioError::BadMagic),
        "070702 (crc) variant"
    );
    assert_eq!(corrupt(6, b'g'), Some(CpioError::BadNumber), "ino field");
    assert_eq!(
        corrupt(6 + 6 * 8, b' '),
        Some(CpioError::BadNumber),
        "filesize field"
    );
    // Trailing garbage after the trailer is not another archive.
    let mut garbage = archive.clone();
    garbage.extend_from_slice(b"junk");
    assert_eq!(initramfs::parse(&garbage).err(), Some(CpioError::Truncated));
    garbage.extend_from_slice(&[b'j'; 200]);
    assert_eq!(initramfs::parse(&garbage).err(), Some(CpioError::BadMagic));

    let regular = mkinitramfs::cpio::S_IFREG | 0o644;
    // Sizes far beyond the archive.
    let mut huge = raw_archive(regular, "f", b"abcd");
    huge[6 + 6 * 8..14 + 6 * 8].copy_from_slice(b"ffffffff");
    assert_eq!(initramfs::parse(&huge).err(), Some(CpioError::Truncated));
    let mut long_name = raw_archive(regular, "f", b"");
    long_name[6 + 11 * 8..14 + 11 * 8].copy_from_slice(b"ffffffff");
    assert_eq!(
        initramfs::parse(&long_name).err(),
        Some(CpioError::Truncated)
    );
    // namesize must include exactly one NUL, at the end.
    let mut no_nul = raw_archive(regular, "f", b"");
    no_nul[6 + 11 * 8..14 + 11 * 8].copy_from_slice(b"00000001");
    assert_eq!(initramfs::parse(&no_nul).err(), Some(CpioError::BadPath));
    let mut zero_name = raw_archive(regular, "f", b"");
    zero_name[6 + 11 * 8..14 + 11 * 8].copy_from_slice(b"00000000");
    assert_eq!(initramfs::parse(&zero_name).err(), Some(CpioError::BadPath));

    for path in ["../etc/passwd", "a/../../b", "/"] {
        assert_eq!(
            initramfs::parse(&raw_archive(regular, path, b"x")).err(),
            Some(CpioError::BadPath),
            "{path}"
        );
    }
    for mode in [0o120777, 0o020644, 0o060644, 0o010644, 0o140755] {
        assert_eq!(
            initramfs::parse(&raw_archive(mode, "special", b"")).err(),
            Some(CpioError::UnsupportedType(mode)),
            "{mode:o}"
        );
    }
    let directory = mkinitramfs::cpio::S_IFDIR | 0o755;
    assert_eq!(
        initramfs::parse(&raw_archive(directory, "d", b"data")).err(),
        Some(CpioError::BadNumber),
        "directory with a body"
    );
    // `.` and `./x` (from `find . | cpio`) are accepted.
    let mut dotted = Vec::new();
    mkinitramfs::cpio::push_entry(&mut dotted, 1, directory, ".", &[]);
    mkinitramfs::cpio::push_entry(&mut dotted, 2, regular, "./x", b"1");
    mkinitramfs::cpio::push_entry(&mut dotted, 0, 0, mkinitramfs::cpio::TRAILER, &[]);
    let entries = initramfs::parse(&dotted).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path, "x");
}

#[test]
fn procfs_regenerates_on_read_and_is_read_only() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let (vfs, files) = setup();
    vfs.mkdir("/proc").unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let counting: Generator = Arc::new(move || {
        let call = counter.fetch_add(1, Ordering::SeqCst);
        format!("call {call}\n").into_bytes()
    });
    let fixed: Generator = Arc::new(|| b"0123456789".to_vec());
    vfs.mount(
        "/proc",
        ProcFs::new(vec![("count", counting), ("fixed", fixed)]),
    )
    .unwrap();

    // Offsets and partial reads are served from the generated contents.
    let fd = vfs.open(&files, "/proc/fixed", OpenFlags::READ).unwrap();
    let mut buffer = [0u8; 4];
    assert_eq!(vfs.read(&files, fd, &mut buffer).unwrap(), 4);
    assert_eq!(&buffer, b"0123");
    vfs.lseek(&files, fd, 8, Whence::Set).unwrap();
    assert_eq!(vfs.read(&files, fd, &mut buffer).unwrap(), 2);
    assert_eq!(&buffer[..2], b"89");
    assert_eq!(vfs.read(&files, fd, &mut buffer).unwrap(), 0, "end of file");
    vfs.lseek(&files, fd, 1 << 40, Whence::Set).unwrap();
    assert_eq!(
        vfs.read(&files, fd, &mut buffer).unwrap(),
        0,
        "far past the end"
    );
    vfs.close(&files, fd).unwrap();
    assert_eq!(
        read_file(&vfs, &files, "/proc/fixed").unwrap(),
        b"0123456789"
    );

    // Each read generates afresh.
    assert_eq!(read_file(&vfs, &files, "/proc/count").unwrap(), b"call 0\n");
    let before = calls.load(Ordering::SeqCst);
    let again = read_file(&vfs, &files, "/proc/count").unwrap();
    assert_eq!(again, format!("call {before}\n").into_bytes());

    // Listing, stat (size 0 as on Linux), and nothing writable.
    let fd = vfs
        .open(&files, "/proc", OpenFlags::READ | OpenFlags::DIRECTORY)
        .unwrap();
    let names: Vec<_> = vfs
        .readdir(&files, fd)
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    assert_eq!(names, ["count", "fixed"]);
    vfs.close(&files, fd).unwrap();
    let stat = vfs.stat("/proc/fixed").unwrap();
    assert_eq!((stat.kind, stat.size), (InodeKind::File, 0));
    let writable = vfs.open(&files, "/proc/fixed", OpenFlags::WRITE).unwrap();
    assert_eq!(vfs.write(&files, writable, b"x"), Err(Errno::EROFS));
    vfs.close(&files, writable).unwrap();
    assert_eq!(
        vfs.open(&files, "/proc/fixed", OpenFlags::WRITE | OpenFlags::TRUNC),
        Err(Errno::EROFS)
    );
    assert_eq!(
        vfs.open(&files, "/proc/new", OpenFlags::WRITE | CREATE),
        Err(Errno::EROFS)
    );
    assert_eq!(vfs.unlink("/proc/fixed"), Err(Errno::EROFS));
    assert_eq!(vfs.mkdir("/proc/dir"), Err(Errno::EROFS));
    assert_eq!(vfs.rename("/proc/fixed", "/proc/other"), Err(Errno::EROFS));
    assert_eq!(vfs.stat("/proc/missing"), Err(Errno::ENOENT));
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

#[test]
fn dup_dup2_and_fork_share_open_files() {
    let (vfs, files) = setup();
    write_file(&vfs, &files, "/f", b"0123456789");
    let fd = vfs.open(&files, "/f", OpenFlags::READ).unwrap();
    let copy = files.dup(fd).unwrap();
    assert_eq!(copy, fd + 1, "lowest free");
    let mut buffer = [0u8; 3];
    vfs.read(&files, fd, &mut buffer).unwrap();
    vfs.read(&files, copy, &mut buffer).unwrap();
    assert_eq!(&buffer, b"345", "dup shares the offset");
    // dup2 onto a free high descriptor, onto an open one, onto itself.
    assert_eq!(files.dup2(fd, 40), Ok(40));
    vfs.read(&files, 40, &mut buffer).unwrap();
    assert_eq!(&buffer, b"678");
    let other = vfs.open(&files, "/dev/zero", OpenFlags::READ).unwrap();
    assert_eq!(files.dup2(fd, other), Ok(other));
    assert_eq!(
        vfs.fstat(&files, other).unwrap().kind,
        InodeKind::File,
        "replaced"
    );
    assert_eq!(files.dup2(fd, fd), Ok(fd));
    assert_eq!(files.dup2(99, 3), Err(Errno::EBADF));
    assert_eq!(
        files.dup2(fd, crate::fs::file::MAX_FDS as u32),
        Err(Errno::EBADF)
    );
    assert_eq!(files.dup(99), Err(Errno::EBADF));
    // fork: same descriptors, shared offsets, independent tables.
    let child = files.fork();
    assert_eq!(child.open_count(), files.open_count());
    vfs.lseek(&files, fd, 0, Whence::Set).unwrap();
    vfs.read(&child, fd, &mut buffer).unwrap();
    assert_eq!(&buffer, b"012", "the child sees the parent's offset");
    vfs.read(&files, fd, &mut buffer).unwrap();
    assert_eq!(&buffer, b"345", "and moves it for both");
    vfs.close(&child, fd).unwrap();
    assert!(
        vfs.read(&files, fd, &mut buffer).is_ok(),
        "closing in the child leaves the parent's"
    );
}

#[test]
fn relative_paths_resolve_against_the_cwd() {
    use crate::fs::path::resolve;
    for (cwd, path, expected) in [
        ("/", "etc/hello", "/etc/hello"),
        ("/etc", "hello", "/etc/hello"),
        ("/etc", "/bin/sh", "/bin/sh"),
        ("/a/b", "../c", "/a/c"),
        ("/a/b", "../../../..", "/"),
        ("/", "..", "/"),
        ("/a", ".", "/a"),
        ("/a", "", "/a"),
        ("/a/", "b//c/./", "/a/b/c"),
        ("/a/b", "./../b/./c", "/a/b/c"),
    ] {
        assert_eq!(resolve(cwd, path), expected, "{cwd} + {path}");
    }
}

#[test]
fn console_reads_lines_through_the_discipline() {
    let (vfs, files) = setup();
    let console = vfs
        .open(&files, "/dev/console", OpenFlags::READ_WRITE)
        .unwrap();
    assert!(vfs.lookup("/dev/console").unwrap().is_terminal());
    assert!(!vfs.lookup("/dev/null").unwrap().is_terminal());
    CONSOLE_CAPTURE.lock().unwrap().clear();
    CONSOLE_INPUT
        .lock()
        .unwrap()
        .extend_from_slice(b"ls -l\x7f\x7fa\rnext\n");
    let mut buffer = [0u8; 64];
    let count = vfs.read(&files, console, &mut buffer).unwrap();
    assert_eq!(&buffer[..count], b"ls a\n");
    let count = vfs.read(&files, console, &mut buffer).unwrap();
    assert_eq!(&buffer[..count], b"next\n");
    assert!(
        CONSOLE_CAPTURE
            .lock()
            .unwrap()
            .starts_with(b"ls -l\x08 \x08"),
        "echoed"
    );
}
