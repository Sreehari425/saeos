//! Files SaeOS reads from its own boot volume before `ExitBootServices`:
//!
//! ```text
//! \EFI\BOOT\BOOTX64.EFI      the kernel (loaded by the firmware)
//! \saeos\initramfs.cpio      the initramfs, read into LoaderData pages
//! \saeos\cmdline.txt         the command line, when the loader passed none
//! ```
//!
//! A missing file just means no initramfs or an empty command line; the
//! kernel still boots.

use super::proto::{
    ALLOCATE_MAX_ADDRESS, EFI_FILE_INFO_GUID, EFI_FILE_MODE_READ, EFI_LOADER_DATA,
    EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID, EFI_SUCCESS, EfiAllocatePages, EfiBootServices,
    EfiFileInfo, EfiFileProtocol, EfiHandle, EfiSimpleFileSystemProtocol,
};
use super::{serial_hex, serial_str};
use crate::boot::boot_info::{COMMAND_LINE_SIZE, Cmdline, PhysicalRange};

/// A NUL-terminated UTF-16 path from ASCII, at compile time.
const fn utf16<const N: usize>(text: &str) -> [u16; N] {
    let bytes = text.as_bytes();
    let mut path = [0u16; N];
    let mut index = 0;
    while index < bytes.len() {
        path[index] = bytes[index] as u16;
        index += 1;
    }
    path
}

const INITRAMFS_PATH: [u16; 22] = utf16("\\saeos\\initramfs.cpio");
const CMDLINE_PATH: [u16; 19] = utf16("\\saeos\\cmdline.txt");

/// Keep the archive below 4 GiB, inside the range the frame allocator and
/// the direct map always cover.
const MAX_ADDRESS: u64 = 0xffff_ffff;

/// An open file on the boot volume, closed on drop.
struct File(*mut EfiFileProtocol);

impl File {
    unsafe fn open(root: &File, path: &[u16]) -> Option<File> {
        let mut handle = core::ptr::null_mut();
        let status =
            unsafe { ((*root.0).open)(root.0, &mut handle, path.as_ptr(), EFI_FILE_MODE_READ, 0) };
        (status == EFI_SUCCESS && !handle.is_null()).then(|| File(handle))
    }

    unsafe fn size(&self) -> Option<u64> {
        // EFI_FILE_INFO plus room for the file name.
        #[repr(C, align(8))]
        struct InfoBuffer([u8; 512]);
        let mut buffer = InfoBuffer([0; 512]);
        let mut length = buffer.0.len();
        let status = unsafe {
            ((*self.0).get_info)(
                self.0,
                &EFI_FILE_INFO_GUID,
                &mut length,
                buffer.0.as_mut_ptr(),
            )
        };
        (status == EFI_SUCCESS && length >= size_of::<EfiFileInfo>())
            .then(|| unsafe { (*(buffer.0.as_ptr() as *const EfiFileInfo)).file_size })
    }

    /// Fill `buffer`; returns the bytes read (short only at end of file).
    unsafe fn read(&self, buffer: &mut [u8]) -> Option<usize> {
        let mut filled = 0;
        while filled < buffer.len() {
            let mut length = buffer.len() - filled;
            let status =
                unsafe { ((*self.0).read)(self.0, &mut length, buffer[filled..].as_mut_ptr()) };
            if status != EFI_SUCCESS {
                return None;
            }
            if length == 0 {
                break;
            }
            filled += length;
        }
        Some(filled)
    }
}

impl Drop for File {
    fn drop(&mut self) {
        unsafe {
            ((*self.0).close)(self.0);
        }
    }
}

/// Open the root directory of the volume the kernel was loaded from.
unsafe fn open_boot_volume(boot_services: &EfiBootServices, device: EfiHandle) -> Option<File> {
    if device.is_null() {
        return None;
    }
    let mut interface: *mut () = core::ptr::null_mut();
    let status = unsafe {
        (boot_services.handle_protocol)(
            device,
            &EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID,
            &mut interface,
        )
    };
    if status != EFI_SUCCESS || interface.is_null() {
        return None;
    }
    let filesystem = interface as *mut EfiSimpleFileSystemProtocol;
    let mut root = core::ptr::null_mut();
    let status = unsafe { ((*filesystem).open_volume)(filesystem, &mut root) };
    (status == EFI_SUCCESS && !root.is_null()).then(|| File(root))
}

/// Read `\saeos\initramfs.cpio` into freshly allocated LoaderData pages.
unsafe fn load_initramfs(boot_services: &EfiBootServices, root: &File) -> Option<PhysicalRange> {
    let Some(file) = (unsafe { File::open(root, &INITRAMFS_PATH) }) else {
        unsafe { serial_str("[UEFI] no \\saeos\\initramfs.cpio\n") };
        return None;
    };
    let size = unsafe { file.size() }.filter(|&size| size != 0)?;
    let pages = usize::try_from(size.div_ceil(4096)).ok()?;
    let allocate_pages: EfiAllocatePages =
        unsafe { core::mem::transmute(boot_services.allocate_pages) };
    let mut address = MAX_ADDRESS;
    let status =
        unsafe { allocate_pages(ALLOCATE_MAX_ADDRESS, EFI_LOADER_DATA, pages, &mut address) };
    if status != EFI_SUCCESS {
        unsafe {
            serial_str("[UEFI] initramfs: cannot allocate ");
            serial_hex(size);
            serial_str(" bytes in one piece; give the machine more memory (QEMU -m)\n");
        }
        return None;
    }
    // Firmware still identity maps all memory.
    let buffer = unsafe { core::slice::from_raw_parts_mut(address as *mut u8, size as usize) };
    if unsafe { file.read(buffer) } != Some(buffer.len()) {
        unsafe { serial_str("[UEFI] initramfs: read failed\n") };
        return None;
    }
    Some(PhysicalRange {
        start: address,
        end: address + size,
    })
}

/// The command line from the loader's options (`BOOTX64.EFI loglevel=debug`
/// from the UEFI shell or a boot entry). Options that are not plain text
/// (boot managers may pass binary data) are ignored.
unsafe fn cmdline_from_load_options(options: *const u16, size: u32) -> Option<Cmdline> {
    if options.is_null() || size < 2 {
        return None;
    }
    let units = unsafe { core::slice::from_raw_parts(options, size as usize / 2) };
    let units = units.split(|&unit| unit == 0).next().unwrap_or(&[]);
    let mut text = [0u8; COMMAND_LINE_SIZE];
    let length = units.len().min(COMMAND_LINE_SIZE);
    for (byte, &unit) in text.iter_mut().zip(&units[..length]) {
        if !(0x20..0x7f).contains(&unit) && unit != u16::from(b'\t') {
            return None;
        }
        *byte = unit as u8;
    }
    let mut line = text[..length].trim_ascii();
    // Drop the image path the shell passes as the first word.
    let word = line
        .iter()
        .position(u8::is_ascii_whitespace)
        .unwrap_or(line.len());
    if word >= 4 && line[word - 4..word].eq_ignore_ascii_case(b".efi") {
        line = line[word..].trim_ascii_start();
    }
    (!line.is_empty()).then(|| Cmdline::new(line))
}

unsafe fn cmdline_from_file(root: &File) -> Option<Cmdline> {
    let file = unsafe { File::open(root, &CMDLINE_PATH) }?;
    let mut text = [0u8; COMMAND_LINE_SIZE];
    let length = unsafe { file.read(&mut text) }?;
    Some(Cmdline::new(text[..length].trim_ascii()))
}

/// Load the initramfs and command line. Must run before the memory map is
/// read for `ExitBootServices`, since it allocates pages.
pub unsafe fn load(
    boot_services: &EfiBootServices,
    device: EfiHandle,
    load_options: *const u16,
    load_options_size: u32,
) -> (Option<PhysicalRange>, Cmdline) {
    let from_options = unsafe { cmdline_from_load_options(load_options, load_options_size) };
    let Some(root) = (unsafe { open_boot_volume(boot_services, device) }) else {
        unsafe { serial_str("[UEFI] boot volume not readable; no initramfs\n") };
        return (None, from_options.unwrap_or(Cmdline::EMPTY));
    };
    let initramfs = unsafe { load_initramfs(boot_services, &root) };
    // On failure, load_initramfs logged why.
    if let Some(range) = initramfs {
        unsafe {
            serial_str("[UEFI] initramfs: ");
            serial_hex(range.end - range.start);
            serial_str(" bytes at ");
            serial_hex(range.start);
            serial_str("\n");
        }
    }
    let cmdline = from_options
        .or_else(|| unsafe { cmdline_from_file(&root) })
        .unwrap_or(Cmdline::EMPTY);
    (initramfs, cmdline)
}
