//! Linux x86_64 user ABI layouts: `open` flags, `struct stat`,
//! `struct linux_dirent64`, and `wait` status words. Pure byte encoding,
//! tested in the host model.

use crate::fs::{DirEntry, Errno, InodeKind, OpenFlags, Stat, Whence};
use alloc::vec::Vec;

pub const O_ACCMODE: u64 = 0o3;
pub const O_RDONLY: u64 = 0o0;
pub const O_WRONLY: u64 = 0o1;
pub const O_RDWR: u64 = 0o2;
pub const O_CREAT: u64 = 0o100;
pub const O_EXCL: u64 = 0o200;
pub const O_TRUNC: u64 = 0o1000;
pub const O_APPEND: u64 = 0o2000;
pub const O_DIRECTORY: u64 = 0o200000;

/// Translate Linux `open` flags. Flags SaeOS does not act on (`O_CLOEXEC`,
/// `O_NOCTTY`, `O_LARGEFILE`, ...) are accepted and ignored, as Linux
/// ignores flags that do not apply.
pub fn open_flags(flags: u64) -> Result<OpenFlags, Errno> {
    let mut result = match flags & O_ACCMODE {
        O_RDONLY => OpenFlags::READ,
        O_WRONLY => OpenFlags::WRITE,
        O_RDWR => OpenFlags::READ_WRITE,
        _ => return Err(Errno::EINVAL),
    };
    for (bit, flag) in [
        (O_CREAT, OpenFlags::CREATE),
        (O_EXCL, OpenFlags::EXCL),
        (O_TRUNC, OpenFlags::TRUNC),
        (O_APPEND, OpenFlags::APPEND),
        (O_DIRECTORY, OpenFlags::DIRECTORY),
    ] {
        if flags & bit != 0 {
            result = result | flag;
        }
    }
    Ok(result)
}

pub fn whence(value: u64) -> Result<Whence, Errno> {
    match value {
        0 => Ok(Whence::Set),
        1 => Ok(Whence::Current),
        2 => Ok(Whence::End),
        _ => Err(Errno::EINVAL),
    }
}

pub const S_IFCHR: u32 = 0o020000;
pub const S_IFDIR: u32 = 0o040000;
pub const S_IFREG: u32 = 0o100000;

/// `st_mode`: file type and the permissions every SaeOS file has (there
/// are no owners or permission checks yet).
pub fn mode(kind: InodeKind) -> u32 {
    match kind {
        InodeKind::File => S_IFREG | 0o644,
        InodeKind::Directory => S_IFDIR | 0o755,
        InodeKind::CharDevice => S_IFCHR | 0o666,
    }
}

/// `sizeof(struct stat)` on x86_64.
pub const STAT_SIZE: usize = 144;

/// Encode `struct stat` (x86_64 layout). Times are zero: SaeOS keeps none.
pub fn encode_stat(stat: &Stat) -> [u8; STAT_SIZE] {
    let mut bytes = [0u8; STAT_SIZE];
    let mut put = |offset: usize, value: &[u8]| {
        bytes[offset..offset + value.len()].copy_from_slice(value);
    };
    put(0, &stat.fs_id.to_le_bytes()); // st_dev
    put(8, &stat.ino.to_le_bytes()); // st_ino
    put(16, &u64::from(stat.nlink).to_le_bytes()); // st_nlink
    put(24, &mode(stat.kind).to_le_bytes()); // st_mode
    // st_uid, st_gid, __pad0, st_rdev: 0.
    put(48, &stat.size.to_le_bytes()); // st_size
    put(56, &4096u64.to_le_bytes()); // st_blksize
    put(64, &stat.size.div_ceil(512).to_le_bytes()); // st_blocks
    bytes
}

const DT_CHR: u8 = 2;
const DT_DIR: u8 = 4;
const DT_REG: u8 = 8;

/// Size of one `struct linux_dirent64` for `name`: 19 header bytes, the
/// name and its NUL, padded to 8.
pub fn dirent_size(name: &str) -> usize {
    (19 + name.len() + 1).next_multiple_of(8)
}

/// Append one `struct linux_dirent64`. `next_offset` is the `d_off` a
/// later `lseek` would use to resume after this entry.
pub fn encode_dirent(buffer: &mut Vec<u8>, entry: &DirEntry, next_offset: u64) {
    let size = dirent_size(&entry.name);
    let start = buffer.len();
    buffer.extend_from_slice(&entry.ino.to_le_bytes()); // d_ino
    buffer.extend_from_slice(&next_offset.to_le_bytes()); // d_off
    buffer.extend_from_slice(&(size as u16).to_le_bytes()); // d_reclen
    buffer.push(match entry.kind {
        InodeKind::File => DT_REG,
        InodeKind::Directory => DT_DIR,
        InodeKind::CharDevice => DT_CHR,
    }); // d_type
    buffer.extend_from_slice(entry.name.as_bytes());
    buffer.resize(start + size, 0);
}

/// The `wait` status word for an `ExitStatus` value: a normal exit is
/// `code << 8` (`WIFEXITED`, `WEXITSTATUS`); a process killed by a signal
/// has the signal number in the low 7 bits (`WIFSIGNALED`, `WTERMSIG`).
pub fn exit_status_word(status: i32) -> i32 {
    if status < 0 {
        (-status) & 0x7f
    } else {
        (status & 0xff) << 8
    }
}

/// How an `ioctl` request moves its argument, from the `_IOC` encoding
/// (`include/uapi/asm-generic/ioctl.h`): bits 30-31 the direction, 16-29
/// the size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoctlInt {
    /// Copy the `int` in from user memory first (`_IOC_WRITE`).
    pub copy_in: bool,
    /// Copy it back out afterwards (`_IOC_READ`).
    pub copy_out: bool,
}

/// The copies for an integer `ioctl`: `None` for a request with no
/// argument (`_IO`, like `SNDCTL_DSP_SYNC`), `ENOTTY` for an argument that
/// is not one `int`.
pub fn ioctl_int(request: u64) -> Result<Option<IoctlInt>, Errno> {
    const IOC_WRITE: u64 = 1;
    const IOC_READ: u64 = 2;
    let direction = (request >> 30) & 0x3;
    let size = (request >> 16) & 0x3fff;
    if request >> 32 != 0 {
        return Err(Errno::ENOTTY);
    }
    if direction == 0 {
        return Ok(None);
    }
    if size != 4 {
        return Err(Errno::ENOTTY);
    }
    Ok(Some(IoctlInt {
        copy_in: direction & IOC_WRITE != 0,
        copy_out: direction & IOC_READ != 0,
    }))
}

pub const TCGETS: u64 = 0x5401;
pub const TIOCGWINSZ: u64 = 0x5413;

/// `sizeof(struct termios)` as the kernel copies it (x86_64).
pub const TERMIOS_SIZE: usize = 36;

/// The console's settings for `TCGETS`: canonical mode with echo, the
/// editing characters `drivers/tty.rs` implements, CR mapped to NL on
/// input and NL to CR-NL on output. Changing them (`TCSETS`) is not
/// supported yet.
pub fn console_termios() -> [u8; TERMIOS_SIZE] {
    const ICRNL: u32 = 0o400;
    const OPOST: u32 = 0o1;
    const ONLCR: u32 = 0o4;
    const B38400: u32 = 0o17;
    const CS8: u32 = 0o60;
    const CREAD: u32 = 0o200;
    const ICANON: u32 = 0o2;
    const ECHO: u32 = 0o10;
    const ECHOE: u32 = 0o20;
    const ECHOK: u32 = 0o40;
    let mut termios = [0u8; TERMIOS_SIZE];
    termios[0..4].copy_from_slice(&ICRNL.to_le_bytes()); // c_iflag
    termios[4..8].copy_from_slice(&(OPOST | ONLCR).to_le_bytes()); // c_oflag
    termios[8..12].copy_from_slice(&(B38400 | CS8 | CREAD).to_le_bytes()); // c_cflag
    termios[12..16].copy_from_slice(&(ICANON | ECHO | ECHOE | ECHOK).to_le_bytes()); // c_lflag
    // c_line = 0; c_cc[]: VINTR, VQUIT, VERASE, VKILL, VEOF, VTIME, VMIN,
    // ..., VWERASE (index 14).
    let control = &mut termios[17..];
    control[0] = 0x03;
    control[1] = 0x1c;
    control[2] = 0x7f;
    control[3] = 0x15;
    control[4] = 0x04;
    control[6] = 1;
    control[14] = 0x17;
    termios
}

/// `struct winsize` for `TIOCGWINSZ`: 25 rows, 80 columns.
pub fn console_window_size() -> [u8; 8] {
    let mut size = [0u8; 8];
    size[0..2].copy_from_slice(&25u16.to_le_bytes());
    size[2..4].copy_from_slice(&80u16.to_le_bytes());
    size
}

pub const FBIOGET_VSCREENINFO: u64 = 0x4600;
pub const FBIOGET_FSCREENINFO: u64 = 0x4602;
pub const FB_VAR_SCREENINFO_SIZE: usize = 160;
pub const FB_FIX_SCREENINFO_SIZE: usize = 80;
const FB_TYPE_PACKED_PIXELS: u32 = 0;
const FB_VISUAL_TRUECOLOR: u32 = 2;

/// A framebuffer mode as the fbdev ioctls describe it: 32-bit pixels with
/// 8-bit colour fields at these bit offsets in the little-endian `u32`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenInfo {
    pub width: u32,
    pub height: u32,
    /// Bytes per row.
    pub stride: u32,
    /// Bytes of framebuffer (`stride * height`).
    pub size: u32,
    /// Physical address of the first pixel.
    pub physical: u64,
    pub red_offset: u32,
    pub green_offset: u32,
    pub blue_offset: u32,
}

/// `struct fb_var_screeninfo` (`linux/fb.h`): the visible mode, virtual
/// size = visible, 32 bits per pixel; the rest (timings, margins) 0.
pub fn encode_var_screeninfo(info: &ScreenInfo) -> [u8; FB_VAR_SCREENINFO_SIZE] {
    let mut bytes = [0u8; FB_VAR_SCREENINFO_SIZE];
    let mut put = |offset: usize, value: u32| {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    };
    put(0, info.width); // xres
    put(4, info.height); // yres
    put(8, info.width); // xres_virtual
    put(12, info.height); // yres_virtual
    put(24, 32); // bits_per_pixel
    // red, green, blue: { offset, length, msb_right }; transp stays 0.
    for (field, offset) in [
        (32, info.red_offset),
        (44, info.green_offset),
        (56, info.blue_offset),
    ] {
        put(field, offset);
        put(field + 4, 8);
    }
    bytes
}

/// `struct fb_fix_screeninfo` (x86_64 layout): name, physical memory,
/// packed true-colour pixels, bytes per line.
pub fn encode_fix_screeninfo(info: &ScreenInfo) -> [u8; FB_FIX_SCREENINFO_SIZE] {
    let mut bytes = [0u8; FB_FIX_SCREENINFO_SIZE];
    bytes[..8].copy_from_slice(b"saeos-fb"); // id[16]
    bytes[16..24].copy_from_slice(&info.physical.to_le_bytes()); // smem_start
    bytes[24..28].copy_from_slice(&info.size.to_le_bytes()); // smem_len
    bytes[28..32].copy_from_slice(&FB_TYPE_PACKED_PIXELS.to_le_bytes()); // type
    bytes[36..40].copy_from_slice(&FB_VISUAL_TRUECOLOR.to_le_bytes()); // visual
    bytes[48..52].copy_from_slice(&info.stride.to_le_bytes()); // line_length
    bytes
}
