//! `play`'s WAV parser, and the ioctl argument decoding `/dev/dsp` relies on.

use crate::fs::Errno;
use crate::syscall::abi::{IoctlInt, ioctl_int};
use crate::wav::{WavError, parse, parse_header};

fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut bytes = id.to_vec();
    bytes.extend_from_slice(&(body.len() as u32).to_le_bytes());
    bytes.extend_from_slice(body);
    if body.len() % 2 == 1 {
        bytes.push(0);
    }
    bytes
}

fn fmt(tag: u16, channels: u16, rate: u32, bits: u16) -> Vec<u8> {
    let align = channels * bits / 8;
    let mut body = Vec::new();
    for field in [tag, channels] {
        body.extend_from_slice(&field.to_le_bytes());
    }
    body.extend_from_slice(&rate.to_le_bytes());
    body.extend_from_slice(&(rate * u32::from(align)).to_le_bytes());
    body.extend_from_slice(&align.to_le_bytes());
    body.extend_from_slice(&bits.to_le_bytes());
    chunk(b"fmt ", &body)
}

fn riff(chunks: &[Vec<u8>]) -> Vec<u8> {
    let body: Vec<u8> = chunks.concat();
    let mut bytes = b"RIFF".to_vec();
    bytes.extend_from_slice(&(4 + body.len() as u32).to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(&body);
    bytes
}

#[test]
fn parses_pcm_and_skips_other_chunks() {
    let samples: Vec<u8> = (0..40).collect();
    let file = riff(&[
        chunk(b"JUNK", b"odd"),
        fmt(1, 2, 44_100, 16),
        chunk(b"LIST", b"INFOISFT\x05\x00\x00\x00test\x00"),
        chunk(b"data", &samples),
    ]);
    let wav = parse(&file).unwrap();
    assert_eq!((wav.rate, wav.channels, wav.bits), (44_100, 2, 16));
    assert_eq!(wav.data, samples.as_slice());
}

#[test]
fn data_is_cut_to_the_file_and_to_whole_frames() {
    let mut file = riff(&[fmt(1, 2, 8_000, 16), chunk(b"data", &[1; 10])]);
    // A streamed WAV: the data size says more than there is.
    let size_at = file.len() - 10 - 4;
    file[size_at..size_at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    let wav = parse(&file).unwrap();
    // 10 bytes of 4-byte frames: 8.
    assert_eq!(wav.data.len(), 8);
}

#[test]
fn the_header_alone_says_where_the_samples_are() {
    let samples = vec![7u8; 1000];
    let file = riff(&[
        fmt(1, 2, 22_050, 16),
        chunk(b"LIST", b"odd"),
        chunk(b"data", &samples),
    ]);
    let data_offset = file.len() - samples.len();
    // Only up to the data chunk's header: enough to stream the rest.
    let header = parse_header(&file[..data_offset]).unwrap();
    assert_eq!((header.rate, header.channels, header.bits), (22_050, 2, 16));
    assert_eq!(header.data_offset, data_offset);
    assert_eq!(header.data_length, 1000);
    assert_eq!(header.frame_bytes(), 4);
    assert_eq!(header.playable(file.len()), 1000);
    // A file cut short, and a size streamed WAVs leave unset.
    assert_eq!(header.playable(data_offset + 999), 996);
    let unset = crate::wav::Header {
        data_length: u32::MAX as usize,
        ..header
    };
    assert_eq!(unset.playable(data_offset + 10), 8);
    assert_eq!(unset.playable(10), 0, "a file ending before its samples");
    // The prefix must reach the data chunk's header.
    assert_eq!(
        parse_header(&file[..data_offset - 8]),
        Err(WavError::NoData)
    );
}

#[test]
fn rejects_what_it_cannot_play() {
    let data = chunk(b"data", &[0; 8]);
    let cases = [
        (b"RIFX....WAVE".to_vec(), WavError::NotWave),
        (b"RIFF".to_vec(), WavError::NotWave),
        (riff(&[data.clone()]), WavError::NoFormat),
        (riff(&[fmt(1, 1, 8_000, 8)]), WavError::NoData),
        (
            riff(&[fmt(3, 1, 8_000, 32), data.clone()]),
            WavError::Unsupported("not plain PCM"),
        ),
        (
            riff(&[fmt(1, 1, 8_000, 24), data.clone()]),
            WavError::Unsupported("only 8 and 16-bit samples"),
        ),
        (
            riff(&[fmt(1, 3, 8_000, 16), data.clone()]),
            WavError::Unsupported("only mono and stereo"),
        ),
        (
            riff(&[fmt(1, 1, 0, 16), data.clone()]),
            WavError::Unsupported("a rate of 0"),
        ),
        (riff(&[b"fmt \x10\x00".to_vec()]), WavError::Truncated),
        (riff(&[chunk(b"fmt ", &[1, 0, 1, 0])]), WavError::Truncated),
    ];
    for (file, error) in cases {
        assert_eq!(parse(&file), Err(error));
    }
}

#[test]
fn ioctl_arguments_decode_from_the_request() {
    const SNDCTL_DSP_SPEED: u64 = 0xc004_5002;
    const SNDCTL_DSP_SYNC: u64 = 0x5001;
    let both = Some(IoctlInt {
        copy_in: true,
        copy_out: true,
    });
    assert_eq!(ioctl_int(SNDCTL_DSP_SPEED), Ok(both));
    assert_eq!(ioctl_int(SNDCTL_DSP_SYNC), Ok(None));
    // _IOW('P', 1, int) and _IOR('P', 1, int).
    assert_eq!(
        ioctl_int(0x4004_5001),
        Ok(Some(IoctlInt {
            copy_in: true,
            copy_out: false
        }))
    );
    assert_eq!(
        ioctl_int(0x8004_5001),
        Ok(Some(IoctlInt {
            copy_in: false,
            copy_out: true
        }))
    );
    // An 8-byte argument, and a request with bits above 32.
    assert_eq!(ioctl_int(0x8008_7801), Err(Errno::ENOTTY));
    assert_eq!(ioctl_int(1 << 40), Err(Errno::ENOTTY));
}
