//! The start of a C interface: the functions a C program (or a C library
//! linked in) expects, with C names and conventions: `-1` and `errno` on
//! failure, NUL-terminated strings, `malloc`'d memory.
//!
//! `memcpy`, `memmove`, `memset` and `memcmp` come from compiler-builtins
//! (built with its `mem` feature) and are callable from C as they are.
//!
//! Each function's safety contract is its C standard one (valid
//! NUL-terminated strings, buffers of the given size, `malloc`'d blocks).
#![allow(clippy::missing_safety_doc)]

use crate::errno::{Errno, check, set_errno};
use crate::raw;
use crate::startinfo::{c_string, entry};
use alloc::alloc::{Layout, alloc, dealloc, realloc as resize};
use core::ffi::{c_char, c_int};

/// A raw result as C wants it: the value, or -1 with `errno` set.
fn c_result(result: i64) -> isize {
    match check(result) {
        Ok(value) => value as isize,
        Err(error) => {
            set_errno(error);
            -1
        }
    }
}

unsafe fn c_bytes<'a>(text: *const c_char) -> &'a [u8] {
    unsafe { c_string(text as *const u8) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn read(fd: c_int, buffer: *mut u8, count: usize) -> isize {
    c_result(unsafe { raw::syscall3(raw::nr::READ, fd as u64, buffer as u64, count as u64) })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn write(fd: c_int, buffer: *const u8, count: usize) -> isize {
    c_result(unsafe { raw::syscall3(raw::nr::WRITE, fd as u64, buffer as u64, count as u64) })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn open(path: *const c_char, flags: c_int, mode: u32) -> c_int {
    c_result(unsafe { raw::syscall3(raw::nr::OPEN, path as u64, flags as u64, u64::from(mode)) })
        as c_int
}

#[unsafe(no_mangle)]
pub extern "C" fn close(fd: c_int) -> c_int {
    c_result(raw::close(fd as u32)) as c_int
}

/// Flush stdout and exit.
#[unsafe(no_mangle)]
pub extern "C" fn exit(code: c_int) -> ! {
    crate::process::exit(code)
}

/// Exit at once, without flushing.
#[unsafe(no_mangle)]
pub extern "C" fn _exit(code: c_int) -> ! {
    raw::exit_now(code)
}

/// `malloc` blocks carry their size just before the returned pointer (C's
/// `free` does not pass it); 16 bytes keep the 16-byte alignment C expects.
const HEADER: usize = 16;

fn layout(total: usize) -> Option<Layout> {
    Layout::from_size_align(total, HEADER).ok()
}

#[unsafe(no_mangle)]
pub extern "C" fn malloc(size: usize) -> *mut u8 {
    let Some((total, layout)) = size
        .checked_add(HEADER)
        .and_then(|total| Some((total, layout(total)?)))
    else {
        set_errno(Errno::ENOMEM);
        return core::ptr::null_mut();
    };
    let base = unsafe { alloc(layout) };
    if base.is_null() {
        set_errno(Errno::ENOMEM);
        return base;
    }
    unsafe {
        (base as *mut usize).write(total);
        base.add(HEADER)
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn free(block: *mut u8) {
    if block.is_null() {
        return;
    }
    unsafe {
        let base = block.sub(HEADER);
        let total = (base as *const usize).read();
        dealloc(base, layout(total).expect("malloc'd block"));
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn calloc(count: usize, size: usize) -> *mut u8 {
    let Some(bytes) = count.checked_mul(size) else {
        set_errno(Errno::ENOMEM);
        return core::ptr::null_mut();
    };
    let block = malloc(bytes);
    if !block.is_null() {
        unsafe { core::ptr::write_bytes(block, 0, bytes) };
    }
    block
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn realloc(block: *mut u8, size: usize) -> *mut u8 {
    if block.is_null() {
        return malloc(size);
    }
    if size == 0 {
        unsafe { free(block) };
        return core::ptr::null_mut();
    }
    let Some(total) = size.checked_add(HEADER) else {
        set_errno(Errno::ENOMEM);
        return core::ptr::null_mut();
    };
    unsafe {
        let base = block.sub(HEADER);
        let old = (base as *const usize).read();
        let new = resize(base, layout(old).expect("malloc'd block"), total);
        if new.is_null() {
            set_errno(Errno::ENOMEM);
            return new;
        }
        (new as *mut usize).write(total);
        new.add(HEADER)
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn strlen(text: *const c_char) -> usize {
    unsafe { c_bytes(text) }.len()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn strcmp(left: *const c_char, right: *const c_char) -> c_int {
    let (left, right) = unsafe { (c_bytes(left), c_bytes(right)) };
    match left.cmp(right) {
        core::cmp::Ordering::Less => -1,
        core::cmp::Ordering::Equal => 0,
        core::cmp::Ordering::Greater => 1,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn strcpy(destination: *mut c_char, source: *const c_char) -> *mut c_char {
    unsafe {
        let source = c_bytes(source);
        core::ptr::copy_nonoverlapping(source.as_ptr(), destination as *mut u8, source.len());
        *destination.add(source.len()) = 0;
    }
    destination
}

/// The value of environment variable `name`, or NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn getenv(name: *const c_char) -> *const c_char {
    let name = unsafe { c_bytes(name) };
    let environment = crate::env::raw_environment();
    if environment.is_null() {
        return core::ptr::null();
    }
    for index in 0.. {
        let pointer = unsafe { entry(environment, index) };
        if pointer.is_null() {
            break;
        }
        let variable = unsafe { c_string(pointer) };
        if variable.len() > name.len() && variable.starts_with(name) && variable[name.len()] == b'='
        {
            return unsafe { pointer.add(name.len() + 1) } as *const c_char;
        }
    }
    core::ptr::null()
}

/// Write `text` and a newline to stdout (after anything buffered).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn puts(text: *const c_char) -> c_int {
    crate::io::flush();
    let text = unsafe { c_bytes(text) };
    match crate::io::write_all(1, text).and_then(|()| crate::io::write_all(1, b"\n")) {
        Ok(()) => 0,
        Err(error) => {
            set_errno(error);
            -1
        }
    }
}
