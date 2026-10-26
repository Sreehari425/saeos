//! The player's BAV decoder against a file written by `scripts/bav.py`
//! (`--fixture`): the Python encoder and the Rust decoder must agree.

use crate::bav::{BavError, Decoder, parse_header};

const FIXTURE: &[u8] = include_bytes!("../fixtures/bav/pattern.bav");

/// `bav.py`'s `fixture_frames`: 23x9, frame i white where
/// (x + 2y + 3k) % 7 < 3, k = 0 on every third row (they never change)
/// and i elsewhere, but frame 6 repeats frame 5; then one all-white frame.
/// The unchanged rows are what make it a test of deltas.
fn expected(index: u32, x: usize, y: usize) -> u8 {
    let shift = match (index, y % 3) {
        (12, _) => return 1,
        (_, 0) => 0,
        (6, _) => 5,
        _ => index as usize,
    };
    u8::from((x + 2 * y + 3 * shift) % 7 < 3)
}

#[test]
fn decodes_what_the_python_encoder_wrote() {
    let mut decoder = Decoder::new(FIXTURE).unwrap();
    let header = decoder.header();
    assert_eq!((header.width, header.height, header.frames), (23, 9, 13));
    assert_eq!(header.frame_time_ns(30), 1_000_000_000);
    let mut changed = Vec::new();
    let mut previous = vec![0u8; 23 * 9];
    for index in 0..13 {
        assert!(decoder.next_frame(&mut changed).unwrap());
        for y in 0..9 {
            let row = decoder.row(y);
            for (x, &pixel) in row.iter().enumerate() {
                assert_eq!(pixel, expected(index, x, y), "frame {index} ({x}, {y})");
            }
            // Exactly the rows that differ from the previous frame.
            let differs = row != &previous[y * 23..(y + 1) * 23];
            assert_eq!(
                changed.contains(&(y as u16)),
                differs,
                "frame {index} row {y}"
            );
            previous[y * 23..(y + 1) * 23].copy_from_slice(row);
        }
    }
    assert!(!decoder.next_frame(&mut changed).unwrap(), "no 14th frame");
    assert_eq!(decoder.decoded(), 13);
}

/// A header plus one hand-made frame.
fn file_with(width: u16, height: u16, frames: u32, frame: &[u8]) -> Vec<u8> {
    let mut bytes = b"BAV1".to_vec();
    for value in [width, height, 30, 1] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes.extend_from_slice(&frames.to_le_bytes());
    bytes.push(frame.len() as u8);
    bytes.extend_from_slice(frame);
    bytes
}

fn first_frame(file: &[u8]) -> Result<bool, BavError> {
    Decoder::new(file)?.next_frame(&mut Vec::new())
}

#[test]
fn rejects_malformed_files() {
    assert_eq!(parse_header(b"BAV2............"), Err(BavError::NotBav));
    assert_eq!(parse_header(b"BAV1"), Err(BavError::NotBav));
    // One changed row (row 1), runs 2 black + 2 white: fine.
    assert_eq!(first_frame(&file_with(4, 2, 1, &[1, 1, 2, 2, 2])), Ok(true));
    let cases: [(&[u8], BavError); 5] = [
        // Runs that do not fill the 4-pixel row.
        (&[1, 0, 1, 3], BavError::BadRow),
        // Runs past the row's end.
        (&[1, 0, 2, 3, 3], BavError::BadRow),
        // A row past the bottom.
        (&[1, 2, 1, 4], BavError::BadRow),
        // The frame says it is longer than its rows: bytes left over.
        (&[1, 0, 1, 4, 9], BavError::BadLength),
        // A varint that never ends.
        (
            &[1, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01],
            BavError::BadVarint,
        ),
    ];
    for (frame, error) in cases {
        assert_eq!(
            first_frame(&file_with(4, 2, 1, frame)),
            Err(error),
            "{frame:?}"
        );
    }
    // The file ends inside the frame.
    let mut cut = file_with(4, 2, 1, &[1, 0, 1, 4]);
    cut.truncate(cut.len() - 1);
    assert_eq!(first_frame(&cut), Err(BavError::Truncated));
}
