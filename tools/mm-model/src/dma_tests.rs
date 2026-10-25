//! DMA buffer planning: which buddy block satisfies a device's
//! constraints. The allocator places a block of order `n` at a multiple of
//! its size, so every such start must be aligned and stay inside one
//! boundary window.

use crate::dma::{DmaConstraints, DmaError, plan, within_boundary};

const PAGE: u64 = 4096;

fn constraints(alignment: u64, boundary: Option<u64>) -> DmaConstraints {
    DmaConstraints {
        limit: 1 << 32,
        alignment,
        boundary,
    }
}

#[test]
fn order_is_the_smallest_block_that_holds_the_buffer() {
    let page = DmaConstraints::BELOW_4G;
    assert_eq!(plan(1, page), Ok(0));
    assert_eq!(plan(4096, page), Ok(0));
    assert_eq!(plan(4097, page), Ok(1));
    assert_eq!(plan(3 * 4096, page), Ok(2));
    assert_eq!(plan(64 << 10, page), Ok(4));
    assert_eq!(plan((1 << 18) * 4096, page), Ok(18));
    assert_eq!(plan((1 << 18) * 4096 + 1, page), Err(DmaError::TooLarge));
    assert_eq!(plan(0, page), Err(DmaError::TooLarge));
}

#[test]
fn alignment_raises_the_order() {
    assert_eq!(plan(100, constraints(64, None)), Ok(0));
    assert_eq!(plan(100, constraints(PAGE, None)), Ok(0));
    assert_eq!(plan(100, constraints(64 << 10, None)), Ok(4));
    assert_eq!(plan(5 * 4096, constraints(8 * 4096, None)), Ok(3));
}

#[test]
fn boundaries() {
    // 64 KiB fits a 64 KiB boundary exactly; a byte more crosses it.
    assert_eq!(plan(64 << 10, constraints(PAGE, Some(64 << 10))), Ok(4));
    assert_eq!(
        plan((64 << 10) + 1, constraints(PAGE, Some(64 << 10))),
        Err(DmaError::CrossesBoundary)
    );
    // Smaller than a page: a page-aligned start keeps the first bytes
    // inside the first window.
    assert_eq!(plan(256, constraints(PAGE, Some(256))), Ok(0));
    assert_eq!(
        plan(257, constraints(PAGE, Some(256))),
        Err(DmaError::CrossesBoundary)
    );
}

#[test]
fn bad_constraints() {
    let bad = [
        DmaConstraints {
            limit: 0,
            ..DmaConstraints::BELOW_4G
        },
        constraints(3, None),
        constraints(0, None),
        constraints(PAGE, Some(3 << 12)),
        constraints(PAGE, Some(0)),
    ];
    for constraints in bad {
        assert_eq!(plan(1, constraints), Err(DmaError::BadConstraints));
    }
}

#[test]
fn every_placement_of_a_planned_block_meets_the_constraints() {
    let lengths: &[u64] = if cfg!(miri) {
        &[1, 4096, 5000, 65536]
    } else {
        &[
            1,
            255,
            256,
            4095,
            4096,
            4097,
            8192,
            12288,
            65535,
            65536,
            65537,
            1 << 20,
        ]
    };
    let alignments = [16, PAGE, 2 * PAGE, 64 << 10];
    let boundaries = [None, Some(256), Some(PAGE), Some(64 << 10), Some(1 << 20)];
    for &len in lengths {
        for alignment in alignments {
            for boundary in boundaries {
                let constraints = constraints(alignment, boundary);
                let Ok(order) = plan(len as usize, constraints) else {
                    // Rejected only when it really cannot fit.
                    assert!(boundary.is_some_and(|boundary| len > boundary));
                    continue;
                };
                let size = PAGE << order;
                assert!(size >= len, "{len} bytes in order {order}");
                assert!(order == 0 || size / 2 < len.max(alignment));
                for slot in 0..8 {
                    let start = slot * size;
                    assert_eq!(start % alignment, 0);
                    if let Some(boundary) = boundary {
                        assert!(
                            within_boundary(start, len, boundary),
                            "{len} bytes at {start:#x} cross {boundary:#x}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn boundary_check() {
    assert!(within_boundary(0, 0x1_0000, 0x1_0000));
    assert!(!within_boundary(0xffff, 2, 0x1_0000));
    assert!(within_boundary(0x1_0000, 1, 0x1_0000));
    assert!(within_boundary(5, 0, 16));
}
