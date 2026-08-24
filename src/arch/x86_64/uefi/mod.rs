pub mod proto;

use crate::arch::x86_64::acpi;
use crate::boot::boot_info::{
    BootInfo, BootMode, DisplayMode, FramebufferInfo, MAX_MEMORY_REGIONS, PhysicalMemoryKind,
    PhysicalMemoryMap, PhysicalMemoryRegion, PixelFormat,
};
use proto::{
    EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID, EFI_LOADED_IMAGE_PROTOCOL_GUID, EFI_SUCCESS,
    EfiGraphicsOutputProtocol, EfiHandle, EfiLoadedImageProtocol, EfiStatus, EfiSystemTable,
};

// Static handle buffer — enough for up to 64 GOP handles (8 bytes each on x86_64)
const HANDLE_BUF_SIZE: usize = 64 * 8;
static mut HANDLE_BUF: [u8; HANDLE_BUF_SIZE] = [0u8; HANDLE_BUF_SIZE];

// Memory map buffer
const MMAP_BUFFER_SIZE: usize = 32768;
static mut MMAP_BUFFER: [u8; MMAP_BUFFER_SIZE] = [0u8; MMAP_BUFFER_SIZE];
static mut UEFI_MEMORY_MAP: PhysicalMemoryMap = PhysicalMemoryMap::empty();
static mut UEFI_DESCRIPTOR_COUNT: usize = 0;
static mut UEFI_DISCARDED: usize = 0;

// ----------------------------------------------------------------
// Minimal inline serial writer for pre-kernel debug (COM1 = 0x3F8)
// ----------------------------------------------------------------
const COM1: u16 = 0x3F8;

unsafe fn serial_putc(c: u8) {
    unsafe {
        loop {
            let lsr: u8;
            core::arch::asm!("in al, dx", out("al") lsr, in("dx") COM1 + 5, options(nomem, nostack));
            if lsr & 0x20 != 0 {
                break;
            }
        }
        core::arch::asm!("out dx, al", in("dx") COM1, in("al") c, options(nomem, nostack));
    }
}

unsafe fn serial_str(s: &str) {
    for b in s.bytes() {
        if b == b'\n' {
            unsafe { serial_putc(b'\r') };
        }
        unsafe { serial_putc(b) };
    }
}

unsafe fn serial_hex(mut value: u64) {
    let mut digits = [b'0'; 16];
    for digit in digits.iter_mut().rev() {
        let nibble = (value & 0xf) as u8;
        *digit = if nibble < 10 {
            b'0' + nibble
        } else {
            b'a' + nibble - 10
        };
        value >>= 4;
    }
    unsafe { serial_str(core::str::from_utf8_unchecked(&digits)) };
}

// ----------------------------------------------------------------
// Bochs VBE DISPI register access (I/O ports 0x01CE / 0x01CF)
// QEMU with -vga std always provides a Bochs VBE 2.0 adapter.
// The linear framebuffer is at PCI BAR0: typically 0xE0000000.
//
// VBE_DISPI registers:
//   0 = ID, 1 = XRES, 2 = YRES, 3 = BPP, 4 = ENABLE,
//   5 = BANK, 6 = VIRT_WIDTH, 7 = VIRT_HEIGHT, 8 = X_OFFSET, 9 = Y_OFFSET
// ----------------------------------------------------------------
const VBE_DISPI_IOPORT_INDEX: u16 = 0x01CE;
const VBE_DISPI_IOPORT_DATA: u16 = 0x01CF;
const VBE_DISPI_INDEX_ID: u16 = 0;
const VBE_DISPI_INDEX_XRES: u16 = 1;
const VBE_DISPI_INDEX_YRES: u16 = 2;
const VBE_DISPI_INDEX_BPP: u16 = 3;
const VBE_DISPI_INDEX_VIRT_WIDTH: u16 = 6;

/// Read a 16-bit Bochs VBE DISPI register.
///
/// # Safety
/// Performs I/O port access.
unsafe fn vbe_read(index: u16) -> u16 {
    let val: u16;
    unsafe {
        core::arch::asm!(
            "out dx, ax",
            in("dx") VBE_DISPI_IOPORT_INDEX,
            in("ax") index,
            options(nomem, nostack)
        );
        core::arch::asm!(
            "in ax, dx",
            out("ax") val,
            in("dx") VBE_DISPI_IOPORT_DATA,
            options(nomem, nostack)
        );
    }
    val
}

/// Probe the Bochs VBE adapter directly and build a `FramebufferInfo`.
///
/// Works with QEMU's `-vga std` (Bochs VBE 2.0), returns `None` if the
/// adapter is not present or no mode is set.
///
/// # Safety
/// Performs I/O port and memory-mapped I/O.
unsafe fn probe_bochs_vbe() -> Option<FramebufferInfo> {
    // Check VBE ID: must be 0xB0C0..0xB0C5
    let id = unsafe { vbe_read(VBE_DISPI_INDEX_ID) };
    if !(0xB0C0..=0xB0C5).contains(&id) {
        return None;
    }

    let width = unsafe { vbe_read(VBE_DISPI_INDEX_XRES) } as usize;
    let height = unsafe { vbe_read(VBE_DISPI_INDEX_YRES) } as usize;
    let bpp = unsafe { vbe_read(VBE_DISPI_INDEX_BPP) } as usize;
    let virtual_width = unsafe { vbe_read(VBE_DISPI_INDEX_VIRT_WIDTH) } as usize;

    if width == 0 || height == 0 || bpp == 0 {
        return None;
    }

    // With -vga std, QEMU maps the Bochs VBE framebuffer at PCI BAR0.
    // On i440FX + PIIX (default QEMU machine), the Bochs VBE adapter is at
    // PCI 00:01.0 (ISA bridge) — actually it's at 00:02.0 in some configs.
    // The standard linear framebuffer base is 0xE0000000 (128 MiB aperture).
    //
    // We probe PCI config space to find the actual BAR0 of the VGA device.
    let fb_base = unsafe { probe_vga_bar0() }.unwrap_or(0xE000_0000);

    let bytes_per_pixel = bpp.div_ceil(8);
    let stride = if virtual_width > 0 {
        virtual_width
    } else {
        width
    };
    let fb_size = stride * height * bytes_per_pixel;

    // QEMU's -vga std uses BGR 32-bit packed pixels (BGRX)
    let pixel_format = PixelFormat::Bgr;

    Some(FramebufferInfo {
        base_addr: fb_base as u64,
        size: fb_size,
        width,
        height,
        stride,
        pixel_format,
    })
}

/// Read a 32-bit PCI config space register via I/O ports 0xCF8/0xCFC.
///
/// # Safety
/// Performs I/O port access.
unsafe fn pci_read32(bus: u8, dev: u8, func: u8, offset: u8) -> u32 {
    let addr: u32 = 0x8000_0000
        | ((bus as u32) << 16)
        | ((dev as u32) << 11)
        | ((func as u32) << 8)
        | ((offset as u32) & 0xFC);
    let val: u32;
    unsafe {
        core::arch::asm!(
            "out dx, eax",
            in("dx") 0x0CF8u16,
            in("eax") addr,
            options(nomem, nostack)
        );
        core::arch::asm!(
            "in eax, dx",
            out("eax") val,
            in("dx") 0x0CFCu16,
            options(nomem, nostack)
        );
    }
    val
}

/// Scan PCI bus 0 for a VGA-class device (class=0x03, subclass=0x00) and
/// return its BAR0 (memory aperture base), or `None` if not found.
///
/// # Safety
/// Performs PCI I/O port access.
unsafe fn probe_vga_bar0() -> Option<u32> {
    for dev in 0u8..32 {
        let vendor_device = unsafe { pci_read32(0, dev, 0, 0x00) };
        if vendor_device == 0xFFFF_FFFF || vendor_device == 0x0000_0000 {
            continue;
        }
        let class_rev = unsafe { pci_read32(0, dev, 0, 0x08) };
        let class_code = (class_rev >> 16) as u16;
        // Display controller (0x0300) or VGA-compatible (0x0300)
        if class_code == 0x0300 || class_code == 0x0302 {
            let bar0 = unsafe { pci_read32(0, dev, 0, 0x10) };
            // Memory BAR: bit 0 = 0, return aligned address
            if bar0 & 1 == 0 && bar0 > 0x1000 {
                return Some(bar0 & 0xFFFF_F000);
            }
        }
    }
    None
}

/// UEFI entrypoint — called by OVMF firmware.
///
/// # Safety
/// `image_handle` and `system_table` must be valid pointers provided by UEFI firmware.
#[unsafe(no_mangle)]
pub unsafe extern "efiapi" fn efi_main(
    image_handle: EfiHandle,
    system_table: *mut EfiSystemTable,
) -> EfiStatus {
    unsafe {
        // Init COM1 for early debug (115200 8N1)
        let ports: [(u16, u8); 6] = [
            (COM1 + 1, 0x00),
            (COM1 + 3, 0x80),
            (COM1, 0x01),
            (COM1 + 1, 0x00),
            (COM1 + 3, 0x03),
            (COM1 + 2, 0xC7),
        ];
        for (port, val) in ports {
            core::arch::asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack));
        }

        serial_str("[UEFI] efi_main entered\n");

        if system_table.is_null() {
            serial_str("[UEFI] ERROR: system_table is null\n");
            return 1;
        }

        let st = &*system_table;

        let rsdp_addr = acpi::find_rsdp_uefi(st.configuration_table, st.number_of_table_entries);

        if st.boot_services.is_null() {
            serial_str("[UEFI] ERROR: boot_services is null\n");
            return 1;
        }

        let bs = &*st.boot_services;

        // The loaded-image protocol is the authoritative image extent. It
        // avoids reserving an arbitrary region around one function symbol.
        let mut image_base = 0u64;
        let mut image_end = 0u64;
        let open_protocol: unsafe extern "efiapi" fn(
            EfiHandle,
            *const proto::EfiGuid,
            *mut *mut (),
            EfiHandle,
            EfiHandle,
            u32,
            *mut usize,
        ) -> EfiStatus = core::mem::transmute(bs.open_protocol);
        let mut image_iface: *mut () = core::ptr::null_mut();
        if open_protocol(
            image_handle,
            &EFI_LOADED_IMAGE_PROTOCOL_GUID,
            &mut image_iface,
            image_handle,
            core::ptr::null_mut(),
            1,
            core::ptr::null_mut(),
        ) == EFI_SUCCESS
            && !image_iface.is_null()
        {
            let image = &*(image_iface as *const EfiLoadedImageProtocol);
            image_base = image.image_base as u64;
            image_end = image_base.saturating_add(image.image_size);
        }

        // ----------------------------------------------------------------
        // Step 1: Try GOP via LocateHandle (ByProtocol)
        // ----------------------------------------------------------------
        let mut gop_ptr: *mut EfiGraphicsOutputProtocol = core::ptr::null_mut();
        let mut buf_size: usize = HANDLE_BUF_SIZE;
        let handles_ptr = core::ptr::addr_of_mut!(HANDLE_BUF) as *mut EfiHandle;

        let locate_status = (bs.locate_handle)(
            2, // ByProtocol
            &EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID,
            core::ptr::null_mut(),
            &mut buf_size,
            handles_ptr,
        );

        if locate_status == EFI_SUCCESS && buf_size > 0 {
            let num_handles = buf_size / core::mem::size_of::<EfiHandle>();
            let handles_slice = core::slice::from_raw_parts(handles_ptr, num_handles);

            for &handle in handles_slice {
                if handle.is_null() {
                    continue;
                }
                let mut iface: *mut () = core::ptr::null_mut();
                let hp_status =
                    (bs.handle_protocol)(handle, &EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID, &mut iface);
                if hp_status == EFI_SUCCESS && !iface.is_null() {
                    let candidate = iface as *mut EfiGraphicsOutputProtocol;
                    if !(*candidate).mode.is_null() && (*(*candidate).mode).frame_buffer_base != 0 {
                        gop_ptr = candidate;
                        break;
                    }
                }
            }
        }

        // Step 1b: Try console_out_handle
        if gop_ptr.is_null() && !st.console_out_handle.is_null() {
            let mut iface: *mut () = core::ptr::null_mut();
            let hp_status = (bs.handle_protocol)(
                st.console_out_handle,
                &EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID,
                &mut iface,
            );
            if hp_status == EFI_SUCCESS && !iface.is_null() {
                let candidate = iface as *mut EfiGraphicsOutputProtocol;
                if !(*candidate).mode.is_null() && (*(*candidate).mode).frame_buffer_base != 0 {
                    gop_ptr = candidate;
                }
            }
        }

        // Step 1c: LocateProtocol
        if gop_ptr.is_null() {
            let mut iface: *mut () = core::ptr::null_mut();
            let lp_status = (bs.locate_protocol)(
                &EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID,
                core::ptr::null_mut(),
                &mut iface,
            );
            if lp_status == EFI_SUCCESS && !iface.is_null() {
                let candidate = iface as *mut EfiGraphicsOutputProtocol;
                if !(*candidate).mode.is_null() && (*(*candidate).mode).frame_buffer_base != 0 {
                    gop_ptr = candidate;
                }
            }
        }

        // ----------------------------------------------------------------
        // Step 2: Extract GOP framebuffer (if found)
        // ----------------------------------------------------------------
        let gop_fb = extract_framebuffer(gop_ptr);

        // ----------------------------------------------------------------
        // Step 3: ExitBootServices
        // ----------------------------------------------------------------
        let mmap_ptr = core::ptr::addr_of_mut!(MMAP_BUFFER) as *mut u8;
        let mut memory_map_size = MMAP_BUFFER_SIZE;
        let mut map_key = 0usize;
        let mut descriptor_size = 0usize;
        let mut descriptor_version = 0u32;

        let mmap_status = (bs.get_memory_map)(
            &mut memory_map_size,
            mmap_ptr,
            &mut map_key,
            &mut descriptor_size,
            &mut descriptor_version,
        );

        if mmap_status != EFI_SUCCESS {
            serial_str("[UEFI] GetMemoryMap failed\n");
            return mmap_status;
        }

        parse_memory_map(
            mmap_ptr,
            memory_map_size,
            descriptor_size,
            descriptor_version,
        );

        let mut exit_status = (bs.exit_boot_services)(image_handle, map_key);
        if exit_status != EFI_SUCCESS {
            let mut retry_size = MMAP_BUFFER_SIZE;
            let _ = (bs.get_memory_map)(
                &mut retry_size,
                mmap_ptr,
                &mut map_key,
                &mut descriptor_size,
                &mut descriptor_version,
            );
            parse_memory_map(mmap_ptr, retry_size, descriptor_size, descriptor_version);
            exit_status = (bs.exit_boot_services)(image_handle, map_key);
            if exit_status != EFI_SUCCESS {
                serial_str("[UEFI] ExitBootServices failed\n");
                return exit_status;
            }
        }

        // ----------------------------------------------------------------
        // Step 4: If GOP was unavailable, probe Bochs VBE hardware directly
        // (works with QEMU's -vga std after ExitBootServices)
        // ----------------------------------------------------------------
        let fb_info = gop_fb.or_else(|| {
            let bochs = probe_bochs_vbe();
            if bochs.is_some() {
                serial_str("[UEFI] Using Bochs VBE hardware framebuffer.\n");
            } else {
                serial_str("[UEFI] No framebuffer found, using VGA text fallback.\n");
            }
            bochs
        });

        {
            let map = core::ptr::read(&raw const UEFI_MEMORY_MAP);
            serial_str("[UEFI] descriptors=");
            serial_hex(UEFI_DESCRIPTOR_COUNT as u64);
            serial_str(" discarded=");
            serial_hex(UEFI_DISCARDED as u64);
            serial_str(" usable_mib=");
            let usable: u64 = map.usable().map(|r| r.length).sum();
            serial_hex(usable / (1024 * 1024));
            serial_str(" highest=");
            serial_hex(map.usable().map(|r| r.end()).max().unwrap_or(0));
            serial_str("\n[UEFI] Transferring to kernel_main.\n");
        }

        // ----------------------------------------------------------------
        // Step 5: Build BootInfo and hand off
        // ----------------------------------------------------------------
        let display = if let Some(fb) = fb_info {
            serial_str("[UEFI] Display: GOP/VBE Framebuffer\n");
            DisplayMode::GopFramebuffer(fb)
        } else {
            serial_str("[UEFI] Display: VGA Text fallback\n");
            DisplayMode::VgaText {
                buffer_addr: 0xb8000,
            }
        };

        let boot_info = BootInfo {
            display,
            boot_mode: BootMode::Uefi,
            memory_map: UEFI_MEMORY_MAP,
            kernel_physical_start: image_base,
            kernel_physical_end: image_end,
            rsdp_addr,
            memory_map_descriptor_count: UEFI_DESCRIPTOR_COUNT,
            memory_map_discarded: UEFI_DISCARDED,
        };

        crate::kernel_main(&boot_info);
    }
}

/// EFI memory descriptor layout shared by UEFI 2.x implementations.
#[repr(C)]
struct EfiMemoryDescriptor {
    memory_type: u32,
    _pad: u32,
    physical_start: u64,
    virtual_start: u64,
    number_of_pages: u64,
    attribute: u64,
}

unsafe fn parse_memory_map(
    ptr: *mut u8,
    size: usize,
    descriptor_size: usize,
    descriptor_version: u32,
) {
    let minimum = core::mem::size_of::<EfiMemoryDescriptor>();
    if descriptor_size < minimum || !descriptor_size.is_multiple_of(8) || descriptor_version == 0 {
        unsafe {
            UEFI_DISCARDED = UEFI_DISCARDED.saturating_add(1);
        }
        return;
    }
    unsafe {
        UEFI_MEMORY_MAP = PhysicalMemoryMap::empty();
        UEFI_DESCRIPTOR_COUNT = 0;
        UEFI_DISCARDED = 0;
    }
    let mut offset = 0;
    while offset <= size && size - offset >= minimum {
        let descriptor = unsafe { &*(ptr.add(offset) as *const EfiMemoryDescriptor) };
        let Some(length) = descriptor.number_of_pages.checked_mul(4096) else {
            unsafe {
                UEFI_DISCARDED += 1;
            }
            break;
        };
        let Some(end) = descriptor.physical_start.checked_add(length) else {
            unsafe {
                UEFI_DISCARDED += 1;
            }
            break;
        };
        let kind = match descriptor.memory_type {
            CONVENTIONAL_MEMORY | LOADER_CODE | LOADER_DATA | BOOT_SERVICES_CODE
            | BOOT_SERVICES_DATA => PhysicalMemoryKind::Usable,
            ACPI_RECLAIM => PhysicalMemoryKind::AcpiReclaimable,
            ACPI_NVS => PhysicalMemoryKind::AcpiNvs,
            RUNTIME_CODE | RUNTIME_DATA => PhysicalMemoryKind::Runtime,
            MMIO | MMIO_PORT_SPACE => PhysicalMemoryKind::Mmio,
            _ => PhysicalMemoryKind::Reserved,
        };
        unsafe {
            UEFI_DESCRIPTOR_COUNT += 1;
            (&raw mut UEFI_MEMORY_MAP)
                .as_mut()
                .unwrap()
                .push_merged(PhysicalMemoryRegion {
                    start: descriptor.physical_start,
                    length: end - descriptor.physical_start,
                    kind,
                });
            if UEFI_MEMORY_MAP.count == MAX_MEMORY_REGIONS {
                UEFI_DISCARDED += 1;
            }
        }
        let Some(next) = offset.checked_add(descriptor_size) else {
            break;
        };
        if next <= offset {
            break;
        }
        offset = next;
    }
}

const CONVENTIONAL_MEMORY: u32 = 7;
const LOADER_CODE: u32 = 1;
const LOADER_DATA: u32 = 2;
const BOOT_SERVICES_CODE: u32 = 3;
const BOOT_SERVICES_DATA: u32 = 4;
const ACPI_RECLAIM: u32 = 9;
const ACPI_NVS: u32 = 10;
const MMIO: u32 = 11;
const MMIO_PORT_SPACE: u32 = 12;
const RUNTIME_CODE: u32 = 5;
const RUNTIME_DATA: u32 = 6;

/// Extract `FramebufferInfo` from a GOP pointer, returning `None` if GOP is null or unusable.
///
/// # Safety
/// `gop_ptr` must be a valid GOP pointer or null.
unsafe fn extract_framebuffer(gop_ptr: *mut EfiGraphicsOutputProtocol) -> Option<FramebufferInfo> {
    if gop_ptr.is_null() {
        return None;
    }
    let gop = unsafe { &*gop_ptr };
    if gop.mode.is_null() {
        return None;
    }
    let mode = unsafe { &*gop.mode };
    if mode.frame_buffer_base == 0 || mode.info.is_null() {
        return None;
    }
    let info = unsafe { &*mode.info };

    let pixel_format = if info.pixel_format == 0 {
        PixelFormat::Rgb
    } else {
        PixelFormat::Bgr
    };

    Some(FramebufferInfo {
        base_addr: mode.frame_buffer_base,
        size: mode.frame_buffer_size,
        width: info.horizontal_resolution as usize,
        height: info.vertical_resolution as usize,
        stride: info.pixels_per_scan_line as usize,
        pixel_format,
    })
}
