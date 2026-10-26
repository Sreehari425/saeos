//! The framebuffer behind `/dev/fb0`: one per boot, from whichever driver
//! found the display.
//!
//! ```text
//! UEFI: the GOP framebuffer the firmware set up (registered at boot)
//! BIOS: QEMU's VGA through Bochs VBE (drivers/bochs_vbe.rs), switched to
//!       its graphics mode when /dev/fb0 is first opened
//! ```
//!
//! While `/dev/fb0` is open the console stops drawing on the screen
//! ([`crate::drivers::console::take_screen`]); after the last close it
//! clears it and draws again, and a Bochs VBE display goes back to VGA text
//! mode. **Keep the descriptor open while drawing**, also through a
//! mapping: the screen belongs to whoever has it open (a forked child
//! shares the parent's open file).
//!
//! The device file: `read`/`write` at byte offsets, `mmap(MAP_SHARED)`
//! (write-combining, shared with forked children), and the fbdev queries
//! `FBIOGET_VSCREENINFO` and `FBIOGET_FSCREENINFO`. The mode is fixed per
//! boot. Pixels are 32-bit; [`PixelOrder`] says where each colour is.

use crate::fs::{Errno, FsResult, Inode, InodeKind, Stat};
use crate::sync::SpinMutex;
use crate::syscall::abi::{
    FBIOGET_FSCREENINFO, FBIOGET_VSCREENINFO, ScreenInfo, encode_fix_screeninfo,
    encode_var_screeninfo,
};
use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;

/// Where the colours are in a pixel's little-endian `u32`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelOrder {
    /// `0x00RRGGBB` (bytes B, G, R, x): VGA, Bochs, most GOP modes.
    Bgrx,
    /// `0x00BBGGRR` (bytes R, G, B, x).
    Rgbx,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mode {
    /// Physical address of the first pixel (page-aligned).
    pub physical: u64,
    /// Kernel address of the first pixel.
    pub mapped: u64,
    pub width: usize,
    pub height: usize,
    /// Bytes per row.
    pub stride: usize,
    pub order: PixelOrder,
}

impl Mode {
    pub fn size(&self) -> usize {
        self.stride * self.height
    }

    fn screen_info(&self) -> ScreenInfo {
        let (red, blue) = match self.order {
            PixelOrder::Bgrx => (16, 0),
            PixelOrder::Rgbx => (0, 16),
        };
        ScreenInfo {
            width: self.width as u32,
            height: self.height as u32,
            stride: self.stride as u32,
            size: self.size() as u32,
            physical: self.physical,
            red_offset: red,
            green_offset: 8,
            blue_offset: blue,
        }
    }
}

/// A display a driver found.
pub trait Display: Send {
    fn name(&self) -> &'static str;
    fn mode(&self) -> Mode;
    /// The first open: switch the display to `mode()` if it is not.
    fn activate(&mut self) {}
    /// The last close: give the screen back to the text console.
    fn deactivate(&mut self) {}
}

struct Registered {
    display: Box<dyn Display>,
    /// Open `/dev/fb0` handles.
    users: usize,
}

static DISPLAY: SpinMutex<Option<Registered>> = SpinMutex::new(None);

/// Whether a display is already `/dev/fb0`.
pub fn registered() -> Option<&'static str> {
    DISPLAY
        .lock()
        .as_ref()
        .map(|registered| registered.display.name())
}

/// Offer a display; the first one stays (on UEFI the GOP framebuffer comes
/// before PCI drivers). Returns whether it was taken.
pub fn register(display: Box<dyn Display>) -> bool {
    let mut registered = DISPLAY.lock();
    if let Some(existing) = registered.as_ref() {
        crate::kinfo!(
            "fb: {} not used, {} is /dev/fb0",
            display.name(),
            existing.display.name()
        );
        return false;
    }
    let mode = display.mode();
    crate::kinfo!(
        "fb: {} {}x{} is /dev/fb0",
        display.name(),
        mode.width,
        mode.height
    );
    *registered = Some(Registered { display, users: 0 });
    true
}

/// The firmware's GOP framebuffer (UEFI boots).
struct Gop {
    mode: Mode,
}

impl Display for Gop {
    fn name(&self) -> &'static str {
        "UEFI GOP"
    }

    fn mode(&self) -> Mode {
        self.mode
    }
}

pub fn register_gop(info: crate::boot::boot_info::FramebufferInfo) {
    use crate::boot::boot_info::PixelFormat;
    let order = match info.pixel_format {
        PixelFormat::Rgb => PixelOrder::Rgbx,
        PixelFormat::Bgr | PixelFormat::Unknown => PixelOrder::Bgrx,
    };
    let physical = crate::mm::PhysAddr(info.base_addr);
    register(Box::new(Gop {
        mode: Mode {
            physical: info.base_addr,
            mapped: crate::mm::paging::phys_to_virt(physical).0,
            width: info.width,
            height: info.height,
            // GOP counts the stride in pixels.
            stride: info.stride * 4,
            order,
        },
    }));
}

/// `/proc/fb`.
pub fn proc_text() -> Vec<u8> {
    let registered = DISPLAY.lock();
    let Some(registered) = registered.as_ref() else {
        return b"no framebuffer\n".to_vec();
    };
    let mode = registered.display.mode();
    alloc::format!(
        "display: {}\nmode: {}x{} 32 bpp ({:?})\nstride: {} bytes\nmemory: {:#x}+{:#x}\nopen: {}\n",
        registered.display.name(),
        mode.width,
        mode.height,
        mode.order,
        mode.stride,
        mode.physical,
        mode.size(),
        registered.users
    )
    .into_bytes()
}

/// `open("/dev/fb0")`: the first opener takes the screen.
pub fn open(ino: u64, fs_id: u64) -> FsResult<Arc<dyn Inode>> {
    let mut registered = DISPLAY.lock();
    let registered = registered.as_mut().ok_or(Errno::ENODEV)?;
    if registered.users == 0 {
        crate::drivers::console::take_screen();
        registered.display.activate();
        let mode = registered.display.mode();
        // Start from black, not from whatever the console drew.
        unsafe { core::ptr::write_bytes(mode.mapped as *mut u8, 0, mode.size()) };
    }
    registered.users += 1;
    Ok(Arc::new(FbHandle {
        ino,
        fs_id,
        mode: registered.display.mode(),
    }))
}

/// One open of `/dev/fb0`.
struct FbHandle {
    ino: u64,
    fs_id: u64,
    mode: Mode,
}

impl FbHandle {
    /// The part of `offset..offset + length` inside the framebuffer.
    fn range(&self, offset: u64, length: usize) -> core::ops::Range<usize> {
        let size = self.mode.size();
        let start = usize::try_from(offset).unwrap_or(usize::MAX).min(size);
        start..start.saturating_add(length).min(size)
    }
}

impl Inode for FbHandle {
    fn stat(&self) -> Stat {
        Stat {
            ino: self.ino,
            fs_id: self.fs_id,
            kind: InodeKind::CharDevice,
            size: self.mode.size() as u64,
            nlink: 1,
        }
    }

    fn kind(&self) -> InodeKind {
        InodeKind::CharDevice
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> FsResult<usize> {
        let range = self.range(offset, buf.len());
        let count = range.len();
        unsafe {
            core::ptr::copy_nonoverlapping(
                (self.mode.mapped as usize + range.start) as *const u8,
                buf.as_mut_ptr(),
                count,
            )
        };
        Ok(count)
    }

    fn write_at(&self, offset: u64, buf: &[u8]) -> FsResult<usize> {
        let range = self.range(offset, buf.len());
        if range.is_empty() && !buf.is_empty() {
            return Err(Errno::ENOSPC);
        }
        unsafe {
            core::ptr::copy_nonoverlapping(
                buf.as_ptr(),
                (self.mode.mapped as usize + range.start) as *mut u8,
                range.len(),
            )
        };
        Ok(range.len())
    }

    fn truncate(&self, _size: u64) -> FsResult<()> {
        Err(Errno::EINVAL)
    }

    fn seekable(&self) -> bool {
        true
    }

    fn ioctl_out(&self, request: u64) -> FsResult<Option<Vec<u8>>> {
        let info = self.mode.screen_info();
        Ok(match request {
            FBIOGET_VSCREENINFO => Some(encode_var_screeninfo(&info).to_vec()),
            FBIOGET_FSCREENINFO => Some(encode_fix_screeninfo(&info).to_vec()),
            _ => None,
        })
    }

    fn mmap(&self, offset: u64, length: u64) -> FsResult<u64> {
        let size = (self.mode.size() as u64).next_multiple_of(4096);
        if offset.checked_add(length).is_none_or(|end| end > size) {
            return Err(Errno::EINVAL);
        }
        Ok(self.mode.physical + offset)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Drop for FbHandle {
    fn drop(&mut self) {
        let mut registered = DISPLAY.lock();
        let Some(registered) = registered.as_mut() else {
            return;
        };
        registered.users -= 1;
        if registered.users == 0 {
            registered.display.deactivate();
            crate::drivers::console::release_screen();
        }
    }
}

/// For the fatal path: back to a screen the console can draw on.
pub fn reclaim_for_panic() {
    if let Some(mut registered) = DISPLAY.try_lock()
        && let Some(registered) = registered.as_mut()
        && registered.users > 0
    {
        registered.display.deactivate();
    }
}
