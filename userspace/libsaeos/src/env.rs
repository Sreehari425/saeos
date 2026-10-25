//! Arguments and environment variables, from the initial stack.

use crate::startinfo::{StartInfo, c_string, entry};
use core::cell::UnsafeCell;

struct Saved(UnsafeCell<Option<StartInfo>>);
unsafe impl Sync for Saved {}

static START: Saved = Saved(UnsafeCell::new(None));

pub(crate) fn init(info: StartInfo) {
    unsafe { *START.0.get() = Some(info) };
}

fn info() -> Option<StartInfo> {
    unsafe { *START.0.get() }
}

/// The raw `envp` array, for passing the environment on to `exec`.
pub(crate) fn raw_environment() -> *const *const u8 {
    info().map_or(core::ptr::null(), |info| info.envp)
}

fn text(bytes: &'static [u8]) -> &'static str {
    core::str::from_utf8(bytes).unwrap_or("")
}

/// The program's arguments, `args().next()` being its name.
pub fn args() -> impl Iterator<Item = &'static str> {
    let info = info();
    let count = info.map_or(0, |info| info.argc);
    (0..count).map(move |index| {
        let info = info.unwrap();
        text(unsafe { c_string(entry(info.argv, index)) })
    })
}

/// Every `NAME=value` in the environment.
pub fn vars() -> impl Iterator<Item = (&'static str, &'static str)> {
    let environment = raw_environment();
    let mut index = 0;
    core::iter::from_fn(move || {
        if environment.is_null() {
            return None;
        }
        let pointer = unsafe { entry(environment, index) };
        if pointer.is_null() {
            return None;
        }
        index += 1;
        let variable = text(unsafe { c_string(pointer) });
        Some(variable.split_once('=').unwrap_or((variable, "")))
    })
}

pub fn var(name: &str) -> Option<&'static str> {
    vars().find(|(key, _)| *key == name).map(|(_, value)| value)
}
