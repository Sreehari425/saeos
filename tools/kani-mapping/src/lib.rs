#![cfg(kani)]

// Import the production mapping planner and its real input types, rather than
// maintaining a verification-only copy of either implementation.
#[path = "../../../kernel/src/boot/boot_info.rs"]
pub mod boot_info;

pub mod boot {
    pub use crate::boot_info;
}

#[path = "../../../kernel/src/mm/mapping_model.rs"]
mod mapping_model;

use boot_info::PhysicalMemoryKind;
use mapping_model::{
    FOUR_LEVEL_LIMIT, GIGABYTE, MappingError, PAGE_SIZE, PageSize, TWO_MIB, direct_virtual_address,
    identity_virtual_address, plan_range,
};

#[kani::proof]
fn direct_and_identity_addresses_respect_four_level_limit() {
    let physical: u64 = kani::any();
    kani::assume(physical < FOUR_LEVEL_LIMIT);

    assert_eq!(identity_virtual_address(physical), Some(physical));
    assert_eq!(
        direct_virtual_address(physical),
        Some(0xffff_8000_0000_0000 + physical)
    );
    assert_eq!(identity_virtual_address(FOUR_LEVEL_LIMIT), None);
    assert_eq!(direct_virtual_address(FOUR_LEVEL_LIMIT), None);
}

#[kani::proof]
#[kani::unwind(4)]
fn plans_cover_bounded_ranges_without_gaps() {
    let start: u64 = kani::any();
    let length: u16 = kani::any();
    let direct: bool = kani::any();
    let mmio: bool = kani::any();
    let gigabyte_pages: bool = kani::any();

    // Keep every symbolic range short enough for tractable loop unwinding,
    // while allowing starts near both huge-page alignment boundaries.
    kani::assume(start < FOUR_LEVEL_LIMIT - 8192);
    kani::assume(start % PAGE_SIZE == 0);
    kani::assume(length > 0 && length <= 8192 && length % PAGE_SIZE as u16 == 0);
    let end = start + length as u64;
    let kind = if mmio {
        PhysicalMemoryKind::Mmio
    } else {
        PhysicalMemoryKind::Usable
    };

    let plan = plan_range(start, end, direct, kind, gigabyte_pages).unwrap();
    assert!(plan.count > 0);
    assert!(plan.count <= 8192 / PAGE_SIZE as usize + 1);

    let mut cursor = start;
    for index in 0..3 {
        if index >= plan.count {
            break;
        }
        let chunk = &plan.chunks[index];
        assert_eq!(chunk.physical_start, cursor);
        assert!(chunk.size > 0);
        assert!(chunk.size <= end - cursor);
        assert_eq!(
            chunk.virtual_start,
            if direct {
                0xffff_8000_0000_0000 + cursor
            } else {
                cursor
            }
        );
        match chunk.page_size {
            PageSize::OneGiB => {
                assert!(!mmio && gigabyte_pages);
                assert_eq!(chunk.size, GIGABYTE);
                assert_eq!(cursor % GIGABYTE, 0);
                assert!(end - cursor >= GIGABYTE);
            }
            PageSize::TwoMiB => {
                assert!(!mmio);
                assert_eq!(chunk.size, TWO_MIB);
                assert_eq!(cursor % TWO_MIB, 0);
                assert!(end - cursor >= TWO_MIB);
                assert!(!gigabyte_pages || cursor % GIGABYTE != 0 || end - cursor < GIGABYTE);
            }
            PageSize::FourKiB => {
                assert_eq!(chunk.size, PAGE_SIZE);
                if mmio {
                    assert_eq!(chunk.page_size, PageSize::FourKiB);
                }
            }
        }
        cursor += chunk.size;
    }
    assert_eq!(cursor, end);
}

#[kani::proof]
fn aligned_ranges_select_eligible_large_pages() {
    let two_mib = plan_range(TWO_MIB, 2 * TWO_MIB, true, PhysicalMemoryKind::Usable, true).unwrap();
    assert_eq!(two_mib.count, 1);
    assert_eq!(two_mib.chunks[0].page_size, PageSize::TwoMiB);
    assert_eq!(two_mib.chunks[0].size, TWO_MIB);

    let one_gib = plan_range(0, GIGABYTE, false, PhysicalMemoryKind::Usable, true).unwrap();
    assert_eq!(one_gib.count, 1);
    assert_eq!(one_gib.chunks[0].page_size, PageSize::OneGiB);
    assert_eq!(one_gib.chunks[0].size, GIGABYTE);
}

#[kani::proof]
fn planner_rejects_invalid_range_endpoints() {
    assert!(matches!(
        plan_range(0, 0, true, PhysicalMemoryKind::Usable, false),
        Err(MappingError::EmptyRange)
    ));
    assert!(matches!(
        plan_range(
            FOUR_LEVEL_LIMIT - 1,
            FOUR_LEVEL_LIMIT + 1,
            false,
            PhysicalMemoryKind::Usable,
            false
        ),
        Err(MappingError::NonCanonicalRange)
    ));
}
