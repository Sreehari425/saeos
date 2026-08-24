//! Pure mapping policy used to validate the runtime memory-map decisions.
//!
//! This module deliberately does not touch page tables or CPU state.  It is
//! also useful to boot code as a compact description of what the hardware
//! mapper is expected to do.

use crate::boot::boot_info::{PhysicalMemoryKind, PhysicalMemoryMap};

pub const PAGE_SIZE: u64 = 4096;
pub const TWO_MIB: u64 = 1 << 21;
pub const GIGABYTE: u64 = 1 << 30;
pub const FOUR_LEVEL_LIMIT: u64 = 1 << 47;
pub const KERNEL_VIRT_BASE: u64 = 0xffff_8000_0000_0000;
pub const MAX_CHUNKS: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MappingError {
    EmptyRange,
    NonCanonicalRange,
    TooManyChunks,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageSize {
    OneGiB,
    TwoMiB,
    FourKiB,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MappingChunk {
    pub physical_start: u64,
    pub virtual_start: u64,
    pub size: u64,
    pub page_size: PageSize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MappingPlan {
    pub chunks: [MappingChunk; MAX_CHUNKS],
    pub count: usize,
}

impl MappingPlan {
    pub const fn empty() -> Self {
        Self {
            chunks: [MappingChunk {
                physical_start: 0,
                virtual_start: 0,
                size: 0,
                page_size: PageSize::FourKiB,
            }; MAX_CHUNKS],
            count: 0,
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &MappingChunk> {
        self.chunks[..self.count].iter()
    }
}

pub fn direct_virtual_address(physical: u64) -> Option<u64> {
    if physical < FOUR_LEVEL_LIMIT {
        Some(KERNEL_VIRT_BASE + physical)
    } else {
        None
    }
}

pub fn identity_virtual_address(physical: u64) -> Option<u64> {
    if physical < FOUR_LEVEL_LIMIT {
        Some(physical)
    } else {
        None
    }
}

pub fn plan_range(
    start: u64,
    end: u64,
    direct: bool,
    kind: PhysicalMemoryKind,
    gigabyte_pages: bool,
) -> Result<MappingPlan, MappingError> {
    if start >= end {
        return Err(MappingError::EmptyRange);
    }
    if end > FOUR_LEVEL_LIMIT {
        return Err(MappingError::NonCanonicalRange);
    }

    let mut plan = MappingPlan::empty();
    let mut physical = start;
    while physical < end {
        let remaining = end - physical;
        let (size, page_size) = if kind != PhysicalMemoryKind::Mmio
            && gigabyte_pages
            && physical.is_multiple_of(GIGABYTE)
            && remaining >= GIGABYTE
        {
            (GIGABYTE, PageSize::OneGiB)
        } else if kind != PhysicalMemoryKind::Mmio
            && physical.is_multiple_of(TWO_MIB)
            && remaining >= TWO_MIB
        {
            (TWO_MIB, PageSize::TwoMiB)
        } else {
            (PAGE_SIZE, PageSize::FourKiB)
        };
        if plan.count == MAX_CHUNKS {
            return Err(MappingError::TooManyChunks);
        }
        plan.chunks[plan.count] = MappingChunk {
            physical_start: physical,
            virtual_start: if direct {
                direct_virtual_address(physical).ok_or(MappingError::NonCanonicalRange)?
            } else {
                identity_virtual_address(physical).ok_or(MappingError::NonCanonicalRange)?
            },
            size,
            page_size,
        };
        plan.count += 1;
        physical += size;
    }
    Ok(plan)
}

pub fn plan_memory_map(
    map: &PhysicalMemoryMap,
    direct: bool,
    gigabyte_pages: bool,
) -> Result<MappingPlan, MappingError> {
    let mut combined = MappingPlan::empty();
    for region in map.regions[..map.count].iter().filter(|region| {
        matches!(
            region.kind,
            PhysicalMemoryKind::Usable
                | PhysicalMemoryKind::AcpiReclaimable
                | PhysicalMemoryKind::AcpiNvs
                | PhysicalMemoryKind::Runtime
                | PhysicalMemoryKind::Mmio
        )
    }) {
        let plan = plan_range(
            region.start,
            region.end(),
            direct,
            region.kind,
            gigabyte_pages,
        )?;
        for chunk in plan.iter() {
            if combined.count == MAX_CHUNKS {
                return Err(MappingError::TooManyChunks);
            }
            combined.chunks[combined.count] = *chunk;
            combined.count += 1;
        }
    }
    Ok(combined)
}

pub fn usable_region_contains(map: &PhysicalMemoryMap, frame: u64) -> bool {
    map.usable()
        .any(|region| frame >= region.start && frame.saturating_add(PAGE_SIZE) <= region.end())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::boot::boot_info::{PhysicalMemoryKind, PhysicalMemoryRegion};

    fn region(start: u64, length: u64, kind: PhysicalMemoryKind) -> PhysicalMemoryRegion {
        PhysicalMemoryRegion {
            start,
            length,
            kind,
        }
    }

    #[test]
    fn direct_map_and_canonical_limits() {
        assert_eq!(
            direct_virtual_address(0x1234),
            Some(KERNEL_VIRT_BASE + 0x1234)
        );
        assert_eq!(direct_virtual_address(1 << 47), None);
        assert_eq!(identity_virtual_address(1 << 47), None);
    }

    #[test]
    fn large_pages_split_at_alignment_and_holes() {
        let mut map = PhysicalMemoryMap::empty();
        map.push(region(0, 512 * GIGABYTE, PhysicalMemoryKind::Usable));
        map.push(region(
            512 * GIGABYTE,
            GIGABYTE,
            PhysicalMemoryKind::Reserved,
        ));
        map.push(region(513 * GIGABYTE, GIGABYTE, PhysicalMemoryKind::Usable));
        let plan = plan_memory_map(&map, true, true).unwrap();
        assert!(plan.iter().any(|chunk| chunk.page_size == PageSize::OneGiB));
        assert!(
            plan.iter()
                .all(|chunk| chunk.physical_start < 512 * GIGABYTE
                    || chunk.physical_start >= 513 * GIGABYTE)
        );
    }

    #[test]
    fn mmio_is_always_four_kib() {
        let plan = plan_range(
            0x1_0000,
            0x1_0000 + TWO_MIB,
            true,
            PhysicalMemoryKind::Mmio,
            true,
        )
        .unwrap();
        assert_eq!(plan.count, 512);
        assert!(
            plan.iter()
                .all(|chunk| chunk.page_size == PageSize::FourKiB)
        );
    }

    #[test]
    fn rejects_ranges_above_four_level_coverage() {
        assert_eq!(
            plan_range(
                FOUR_LEVEL_LIMIT - PAGE_SIZE,
                FOUR_LEVEL_LIMIT + PAGE_SIZE,
                true,
                PhysicalMemoryKind::Usable,
                true
            ),
            Err(MappingError::NonCanonicalRange)
        );
    }

    #[test]
    fn high_memory_and_firmware_identity_cases_remain_supported() {
        let relocated_image = plan_range(
            4 * GIGABYTE,
            4 * GIGABYTE + TWO_MIB,
            true,
            PhysicalMemoryKind::Reserved,
            true,
        )
        .unwrap();
        assert_eq!(
            relocated_image.chunks[0].virtual_start,
            KERNEL_VIRT_BASE + 4 * GIGABYTE
        );

        let firmware = plan_range(
            1 * GIGABYTE,
            1 * GIGABYTE + PAGE_SIZE,
            false,
            PhysicalMemoryKind::Mmio,
            true,
        )
        .unwrap();
        assert_eq!(firmware.chunks[0].virtual_start, 1 * GIGABYTE);
    }

    #[test]
    fn allocator_usable_frame_check_excludes_reserved_holes() {
        let mut map = PhysicalMemoryMap::empty();
        map.push(region(0, GIGABYTE, PhysicalMemoryKind::Usable));
        map.push(region(GIGABYTE, PAGE_SIZE, PhysicalMemoryKind::Reserved));
        assert!(usable_region_contains(&map, 0x2000));
        assert!(!usable_region_contains(&map, GIGABYTE));
    }
}
