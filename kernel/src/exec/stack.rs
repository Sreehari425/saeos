//! The initial user stack, laid out as the SysV x86_64 ABI (and Linux)
//! specify. At `_start`, rsp points at argc, and rsp is 16-byte aligned:
//!
//! ```text
//! high  strings: argv[], envp[], then 16 AT_RANDOM bytes
//!       (padding to 16)
//!       auxv: (type, value) pairs ... AT_NULL
//!       envp[0..n], NULL
//!       argv[0..n], NULL
//! rsp-> argc
//! ```

use crate::fs::Errno;
use alloc::vec::Vec;

pub const AT_NULL: u64 = 0;
pub const AT_PHDR: u64 = 3;
pub const AT_PHENT: u64 = 4;
pub const AT_PHNUM: u64 = 5;
pub const AT_PAGESZ: u64 = 6;
pub const AT_ENTRY: u64 = 9;
pub const AT_RANDOM: u64 = 25;

/// The bytes to place so they end at `top`, and the initial rsp.
pub struct InitialStack {
    pub bytes: Vec<u8>,
    pub stack_pointer: u64,
}

/// Build the stack. `auxv` excludes `AT_RANDOM` and `AT_NULL`, which are
/// added here. `E2BIG` if it would not fit in `limit` bytes.
pub fn build(
    top: u64,
    argv: &[&str],
    envp: &[&str],
    auxv: &[(u64, u64)],
    random: [u8; 16],
    limit: usize,
) -> Result<InitialStack, Errno> {
    // The string area ends at `top`: argv strings, envp strings, then the
    // random bytes, each at a known offset from its start.
    let mut strings = Vec::new();
    let offsets = |values: &[&str], strings: &mut Vec<u8>| -> Vec<usize> {
        values
            .iter()
            .map(|value| {
                let offset = strings.len();
                strings.extend_from_slice(value.as_bytes());
                strings.push(0);
                offset
            })
            .collect()
    };
    let argv_offsets = offsets(argv, &mut strings);
    let envp_offsets = offsets(envp, &mut strings);
    let random_offset = strings.len();
    strings.extend_from_slice(&random);
    let strings_start = top.checked_sub(strings.len() as u64).ok_or(Errno::E2BIG)?;
    let address = |offset: usize| strings_start + offset as u64;
    let random_address = address(random_offset);
    let argv_addresses: Vec<u64> = argv_offsets.into_iter().map(address).collect();
    let envp_addresses: Vec<u64> = envp_offsets.into_iter().map(address).collect();

    let mut auxv: Vec<(u64, u64)> = auxv.to_vec();
    auxv.push((AT_RANDOM, random_address));
    auxv.push((AT_NULL, 0));
    let words = 1 + argv.len() + 1 + envp.len() + 1 + auxv.len() * 2;
    // rsp must be 16-byte aligned at argc.
    let stack_pointer = strings_start
        .checked_sub((words * 8) as u64)
        .ok_or(Errno::E2BIG)?
        & !0xf;
    let total = (top - stack_pointer) as usize;
    if total > limit {
        return Err(Errno::E2BIG);
    }
    let mut bytes = Vec::with_capacity(total);
    let mut word = |value: u64| bytes.extend_from_slice(&value.to_le_bytes());
    word(argv.len() as u64);
    argv_addresses.iter().for_each(|&value| word(value));
    word(0);
    envp_addresses.iter().for_each(|&value| word(value));
    word(0);
    for (kind, value) in &auxv {
        word(*kind);
        word(*value);
    }
    bytes.resize(total - strings.len(), 0);
    bytes.extend_from_slice(&strings);
    Ok(InitialStack {
        bytes,
        stack_pointer,
    })
}
