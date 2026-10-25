//! User-pointer range checks and string reads.

use crate::fake_user::FakeUser;
use crate::fs::Errno;
use crate::uaccess::{USER_TOP, UserMemory, check_range};

#[test]
fn ranges_must_stay_in_the_user_half() {
    assert_eq!(check_range(0, 0), Ok(()));
    assert_eq!(check_range(0x1000, 4096), Ok(()));
    assert_eq!(check_range(USER_TOP - 1, 1), Ok(()), "last user byte");
    assert_eq!(check_range(USER_TOP, 0), Ok(()), "empty range at the top");
    assert_eq!(check_range(USER_TOP, 1), Err(Errno::EFAULT));
    assert_eq!(
        check_range(USER_TOP - 1, 2),
        Err(Errno::EFAULT),
        "straddles the top"
    );
    assert_eq!(
        check_range(0xffff_8000_0000_0000, 1),
        Err(Errno::EFAULT),
        "kernel half"
    );
    assert_eq!(
        check_range(0x0000_8000_0000_0000, 1),
        Err(Errno::EFAULT),
        "non-canonical"
    );
    assert_eq!(check_range(u64::MAX, 1), Err(Errno::EFAULT), "wraps");
    assert_eq!(check_range(u64::MAX - 10, 100), Err(Errno::EFAULT), "wraps");
    assert_eq!(check_range(1, usize::MAX), Err(Errno::EFAULT));
}

#[test]
fn strings_stop_at_nul_page_ends_and_limits() {
    // Two pages; the string straddles the boundary.
    let memory = FakeUser::new(0x10_0000, 8192);
    let start = 0x10_0000 + 4096 - 5;
    memory.write(start, b"/etc/hello\0").unwrap();
    assert_eq!(memory.read_cstr(start, 4096).as_deref(), Ok("/etc/hello"));
    assert_eq!(
        memory.read_cstr(start, 10).as_deref(),
        Ok("/etc/hello"),
        "exactly max"
    );
    assert_eq!(memory.read_cstr(start, 9), Err(Errno::ENAMETOOLONG));
    assert_eq!(memory.read_cstr(start + 10, 4096).as_deref(), Ok(""));
    // No NUL before the end of the mapping: the next read faults.
    let end = 0x10_0000 + 8192;
    memory.write(end - 3, b"abc").unwrap();
    assert_eq!(memory.read_cstr(end - 3, 4096), Err(Errno::EFAULT));
    // A NUL as the very last mapped byte is found without reading past it.
    memory.write(end - 3, b"ab\0").unwrap();
    assert_eq!(memory.read_cstr(end - 3, 4096).as_deref(), Ok("ab"));
    // Bad pointers and bad text.
    assert_eq!(memory.read_cstr(0, 10), Err(Errno::EFAULT));
    assert_eq!(memory.read_cstr(USER_TOP - 1, 10), Err(Errno::EFAULT));
    memory.write(0x10_0000, b"\xff\xfe\0").unwrap();
    assert_eq!(memory.read_cstr(0x10_0000, 10), Err(Errno::EINVAL));
    // A long string, read in several chunks.
    let long: Vec<u8> = (0..3000)
        .map(|index| b'a' + (index % 26) as u8)
        .chain([0])
        .collect();
    memory.write(0x10_0000 + 100, &long).unwrap();
    let read = memory.read_cstr(0x10_0000 + 100, 4096).unwrap();
    assert_eq!(read.as_bytes(), &long[..3000]);
}
