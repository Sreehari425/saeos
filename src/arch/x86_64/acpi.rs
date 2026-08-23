//! Minimal ACPI discovery for interrupt-controller setup.
//!
//! This module intentionally uses fixed-size arrays and raw physical pointers:
//! APIC setup happens before the kernel heap is initialized.

const RSDP_SIGNATURE: &[u8; 8] = b"RSD PTR ";
const MAX_IOAPICS: usize = 8;
const MAX_OVERRIDES: usize = 16;

const ACPI2_GUID: super::uefi::proto::EfiGuid = super::uefi::proto::EFI_ACPI_20_TABLE_GUID;
const ACPI1_GUID: super::uefi::proto::EfiGuid = super::uefi::proto::EFI_ACPI_TABLE_GUID;

#[derive(Debug, Clone, Copy)]
pub struct IoApicInfo {
    pub id: u8,
    pub address: u64,
    pub gsi_base: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct InterruptOverride {
    pub source_irq: u8,
    pub gsi: u32,
    pub flags: u16,
}

#[derive(Debug, Clone, Copy)]
pub struct InterruptTopology {
    pub lapic_address: u64,
    pub ioapics: [IoApicInfo; MAX_IOAPICS],
    pub ioapic_count: usize,
    pub overrides: [InterruptOverride; MAX_OVERRIDES],
    pub override_count: usize,
}

impl InterruptTopology {
    const fn empty() -> Self {
        Self {
            lapic_address: 0,
            ioapics: [IoApicInfo {
                id: 0,
                address: 0,
                gsi_base: 0,
            }; MAX_IOAPICS],
            ioapic_count: 0,
            overrides: [InterruptOverride {
                source_irq: 0,
                gsi: 0,
                flags: 0,
            }; MAX_OVERRIDES],
            override_count: 0,
        }
    }

    pub fn gsi_for_irq(&self, irq: u8) -> (u32, u16) {
        for entry in &self.overrides[..self.override_count] {
            if entry.source_irq == irq {
                return (entry.gsi, entry.flags);
            }
        }
        (irq as u32, 0)
    }

    pub fn ioapic_for_gsi(&self, gsi: u32) -> Option<IoApicInfo> {
        for ioapic in self.ioapics[..self.ioapic_count].iter() {
            let next_base = self.ioapics[..self.ioapic_count]
                .iter()
                .filter(|candidate| candidate.gsi_base > ioapic.gsi_base)
                .map(|candidate| candidate.gsi_base)
                .min()
                .unwrap_or(u32::MAX);
            if ioapic.gsi_base <= gsi && gsi < next_base {
                return Some(*ioapic);
            }
        }
        None
    }
}

#[derive(Debug, Clone, Copy)]
pub enum AcpiError {
    InvalidRsdp,
    InvalidRootTable,
    NoMadt,
    InvalidMadt,
    TooManyEntries,
}

fn read_u32(address: usize) -> u32 {
    unsafe { core::ptr::read_unaligned(address as *const u32) }
}

fn read_u64(address: usize) -> u64 {
    unsafe { core::ptr::read_unaligned(address as *const u64) }
}

fn read_u16(address: usize) -> u16 {
    unsafe { core::ptr::read_unaligned(address as *const u16) }
}

fn mapped_address(address: u64) -> usize {
    if crate::mm::paging::is_active() {
        crate::mm::paging::phys_to_virt(crate::mm::PhysAddr(address)).0 as usize
    } else {
        address as usize
    }
}

fn checksum_ok(address: usize, length: usize) -> bool {
    let mut sum = 0u8;
    for offset in 0..length {
        sum = sum.wrapping_add(unsafe { (address as *const u8).add(offset).read() });
    }
    sum == 0
}

fn signature_equals(address: usize, signature: &[u8; 4]) -> bool {
    for (offset, expected) in signature.iter().enumerate() {
        if unsafe { (address as *const u8).add(offset).read() } != *expected {
            return false;
        }
    }
    true
}

fn rsdp_valid(address: usize) -> bool {
    for (offset, expected) in RSDP_SIGNATURE.iter().enumerate() {
        if unsafe { (address as *const u8).add(offset).read() } != *expected {
            return false;
        }
    }

    if !checksum_ok(address, 20) {
        return false;
    }

    let revision = unsafe { (address as *const u8).add(15).read() };
    if revision >= 2 {
        let length = read_u32(address + 20) as usize;
        length >= 36 && checksum_ok(address, length)
    } else {
        true
    }
}

pub fn find_rsdp_bios() -> Option<u64> {
    // The EBDA segment pointer lives in the BIOS data area at 0x40E.
    let ebda = (read_u16(0x40E) as usize) << 4;
    if ebda != 0
        && let Some(address) = scan(ebda, ebda.saturating_add(1024))
    {
        return Some(address as u64);
    }

    scan(0xE0000, 0x100000).map(|address| address as u64)
}

fn scan(start: usize, end: usize) -> Option<usize> {
    let mut address = (start + 15) & !15;
    while address + 20 <= end {
        if rsdp_valid(address) {
            return Some(address);
        }
        address += 16;
    }
    None
}

/// # Safety
/// `tables` must point to `count` valid UEFI configuration-table entries.
pub unsafe fn find_rsdp_uefi(
    tables: *const super::uefi::proto::EfiConfigurationTable,
    count: usize,
) -> Option<u64> {
    if tables.is_null() {
        return None;
    }

    let entries = unsafe { core::slice::from_raw_parts(tables, count) };
    let mut acpi1 = None;
    for entry in entries {
        if entry.vendor_guid == ACPI2_GUID && !entry.vendor_table.is_null() {
            let address = entry.vendor_table as usize;
            if rsdp_valid(address) {
                return Some(address as u64);
            }
        }
        if entry.vendor_guid == ACPI1_GUID && !entry.vendor_table.is_null() {
            acpi1 = Some(entry.vendor_table as usize);
        }
    }

    acpi1
        .filter(|address| rsdp_valid(*address))
        .map(|address| address as u64)
}

/// # Safety
/// `address` must point to a readable ACPI RSDP and its referenced tables.
pub unsafe fn parse_rsdp(address: u64) -> Result<InterruptTopology, AcpiError> {
    let rsdp = mapped_address(address);
    if !rsdp_valid(rsdp) {
        return Err(AcpiError::InvalidRsdp);
    }

    let revision = unsafe { (rsdp as *const u8).add(15).read() };
    let root = if revision >= 2 && read_u64(rsdp + 24) != 0 {
        (read_u64(rsdp + 24), true)
    } else {
        (read_u32(rsdp + 16) as u64, false)
    };

    let root_address = mapped_address(root.0);
    if root_address == 0 {
        return Err(AcpiError::InvalidRootTable);
    }
    if !signature_equals(root_address, if root.1 { b"XSDT" } else { b"RSDT" }) {
        return Err(AcpiError::InvalidRootTable);
    }

    let root_length = read_u32(root_address + 4) as usize;
    if root_length < 36 || !checksum_ok(root_address, root_length) {
        return Err(AcpiError::InvalidRootTable);
    }

    let entry_size = if root.1 { 8 } else { 4 };
    let entry_count = (root_length - 36) / entry_size;
    for index in 0..entry_count {
        let entry_address = if root.1 {
            mapped_address(read_u64(root_address + 36 + index * 8))
        } else {
            mapped_address(read_u32(root_address + 36 + index * 4) as u64)
        };
        if entry_address == 0 || !signature_equals(entry_address, b"APIC") {
            continue;
        }

        let length = read_u32(entry_address + 4) as usize;
        if length < 44 || !checksum_ok(entry_address, length) {
            return Err(AcpiError::InvalidMadt);
        }
        return unsafe { parse_madt(entry_address, length) };
    }

    Err(AcpiError::NoMadt)
}

unsafe fn parse_madt(address: usize, length: usize) -> Result<InterruptTopology, AcpiError> {
    let mut topology = InterruptTopology::empty();
    topology.lapic_address = read_u32(address + 36) as u64;

    let mut offset = 44;
    while offset + 2 <= length {
        let entry_type = unsafe { (address as *const u8).add(offset).read() };
        let entry_length = unsafe { (address as *const u8).add(offset + 1).read() } as usize;
        if entry_length < 2 || offset + entry_length > length {
            return Err(AcpiError::InvalidMadt);
        }

        match entry_type {
            1 if entry_length >= 12 => {
                if topology.ioapic_count == MAX_IOAPICS {
                    return Err(AcpiError::TooManyEntries);
                }
                topology.ioapics[topology.ioapic_count] = IoApicInfo {
                    id: unsafe { (address as *const u8).add(offset + 2).read() },
                    address: read_u32(address + offset + 4) as u64,
                    gsi_base: read_u32(address + offset + 8),
                };
                topology.ioapic_count += 1;
            }
            2 if entry_length >= 10 => {
                let bus = unsafe { (address as *const u8).add(offset + 2).read() };
                if bus == 0 {
                    if topology.override_count == MAX_OVERRIDES {
                        return Err(AcpiError::TooManyEntries);
                    }
                    topology.overrides[topology.override_count] = InterruptOverride {
                        source_irq: unsafe { (address as *const u8).add(offset + 3).read() },
                        gsi: read_u32(address + offset + 4),
                        flags: read_u16(address + offset + 8),
                    };
                    topology.override_count += 1;
                }
            }
            _ => {}
        }

        offset += entry_length;
    }

    if topology.lapic_address == 0 || topology.ioapic_count == 0 {
        return Err(AcpiError::InvalidMadt);
    }
    Ok(topology)
}
