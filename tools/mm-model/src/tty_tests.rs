//! The console line discipline: editing, echo, line-at-a-time reads, EOF.

use crate::tty::{LINE_MAX, LineDiscipline};

/// Feed `input`; return (everything read, line by line, until no line is
/// ready) and the echo.
fn run(input: &[u8], read_size: usize) -> (Vec<Vec<u8>>, Vec<u8>) {
    let mut tty = LineDiscipline::new();
    let mut echo = Vec::new();
    for &byte in input {
        tty.input(byte, &mut echo);
    }
    let mut reads = Vec::new();
    let mut buffer = vec![0u8; read_size];
    while let Some(count) = tty.read(&mut buffer) {
        reads.push(buffer[..count].to_vec());
    }
    (reads, echo)
}

#[test]
fn lines_complete_on_enter_and_echo() {
    let (reads, echo) = run(b"hello\rworld\n", 64);
    assert_eq!(
        reads,
        [b"hello\n".to_vec(), b"world\n".to_vec()],
        "CR and LF both end a line"
    );
    assert_eq!(echo, b"hello\nworld\n");
    // Nothing is readable until the line is complete.
    let mut tty = LineDiscipline::new();
    let mut echo = Vec::new();
    for &byte in b"partial" {
        tty.input(byte, &mut echo);
    }
    assert!(!tty.has_input());
    assert_eq!(tty.read(&mut [0; 8]), None);
}

#[test]
fn editing_keys() {
    let (reads, echo) = run(b"abX\x08c\n", 64);
    assert_eq!(reads, [b"abc\n".to_vec()]);
    assert_eq!(echo, b"abX\x08 \x08c\n");
    assert_eq!(
        run(b"abX\x7fc\n", 64).0,
        [b"abc\n".to_vec()],
        "DEL erases too"
    );
    assert_eq!(
        run(b"\x08\x08ok\n", 64).0,
        [b"ok\n".to_vec()],
        "erasing an empty line"
    );
    assert_eq!(
        run(b"wrong line\x15right\n", 64).0,
        [b"right\n".to_vec()],
        "Ctrl-U"
    );
    assert_eq!(
        run(b"cat file  \x17x\n", 64).0,
        [b"cat x\n".to_vec()],
        "Ctrl-W"
    );
    let (reads, echo) = run(b"oops\x03fine\n", 64);
    assert_eq!(reads, [b"fine\n".to_vec()], "Ctrl-C discards");
    assert!(echo.windows(3).any(|window| window == b"^C\n"));
    // Other control characters are ignored; Tab is text.
    assert_eq!(run(b"a\x01\x02\tb\n", 64).0, [b"a\tb\n".to_vec()]);
    // Erasing a multi-byte character takes all of its bytes.
    assert_eq!(run("é!\x08\x08\n".as_bytes(), 64).0, [b"\n".to_vec()]);
}

#[test]
fn end_of_file_and_partial_reads() {
    // Ctrl-D on an empty line: one read returns 0; mid-line it ends the
    // line without a newline.
    let (reads, _) = run(b"one\n\x04two\x04", 64);
    assert_eq!(reads, [b"one\n".to_vec(), Vec::new(), b"two".to_vec()]);
    // A small buffer reads a line in pieces, never past its end.
    let (reads, _) = run(b"abcdef\nxy\n", 4);
    assert_eq!(
        reads,
        [b"abcd".to_vec(), b"ef\n".to_vec(), b"xy\n".to_vec()]
    );
}

#[test]
fn lines_are_limited() {
    let mut input = vec![b'x'; LINE_MAX + 100];
    input.push(b'\n');
    let (reads, echo) = run(&input, LINE_MAX * 2);
    assert_eq!(reads.len(), 1);
    assert_eq!(
        reads[0].len(),
        LINE_MAX,
        "LINE_MAX - 1 characters and the newline"
    );
    assert_eq!(reads[0].last(), Some(&b'\n'));
    assert_eq!(echo.len(), LINE_MAX, "dropped characters are not echoed");
}
