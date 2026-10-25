//! Processes: fork, exec, wait, exit, and the current directory.

use crate::errno::{Errno, Result, check};
use crate::io::c_path;
use crate::raw;
use alloc::string::String;
use alloc::vec::Vec;

pub enum Fork {
    /// In the new process.
    Child,
    /// In the original process, with the child's pid.
    Parent(u64),
}

pub fn fork() -> Result<Fork> {
    match check(raw::fork())? {
        0 => Ok(Fork::Child),
        pid => Ok(Fork::Parent(pid)),
    }
}

/// NUL-terminated copies of `strings` and a NULL-terminated pointer array.
fn c_strings(strings: &[&str]) -> Result<(Vec<Vec<u8>>, Vec<*const u8>)> {
    let owned = strings
        .iter()
        .map(|string| c_path(string))
        .collect::<Result<Vec<_>>>()?;
    let mut pointers: Vec<*const u8> = owned.iter().map(|string| string.as_ptr()).collect();
    pointers.push(core::ptr::null());
    Ok((owned, pointers))
}

fn execve(path: &str, args: &[&str], environment: *const *const u8) -> Errno {
    let result = (|| {
        let path = c_path(path)?;
        let (_strings, argv) = c_strings(args)?;
        check(unsafe {
            raw::syscall3(
                raw::nr::EXECVE,
                path.as_ptr() as u64,
                argv.as_ptr() as u64,
                environment as u64,
            )
        })
    })();
    match result {
        Err(error) => error,
        Ok(_) => Errno::EINVAL,
    }
}

/// Replace this program with `path`, run with `args` (`args[0]` is its
/// name) and `environment` (`NAME=value`). Only returns on failure.
pub fn exec(path: &str, args: &[&str], environment: &[&str]) -> Errno {
    match c_strings(environment) {
        Ok((_strings, envp)) => execve(path, args, envp.as_ptr()),
        Err(error) => error,
    }
}

/// [`exec`] with this program's own environment.
pub fn exec_inherit(path: &str, args: &[&str]) -> Errno {
    execve(path, args, crate::env::raw_environment())
}

/// How a child ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitStatus(pub i32);

impl ExitStatus {
    /// The exit code, if it exited normally.
    pub fn code(self) -> Option<i32> {
        raw::exited_with(self.0)
    }

    /// The signal that killed it, if any.
    pub fn signal(self) -> Option<i32> {
        raw::killed_by(self.0)
    }

    pub fn success(self) -> bool {
        self.code() == Some(0)
    }
}

/// Wait for `pid` (or any child) to end.
pub fn wait(pid: Option<u64>) -> Result<(u64, ExitStatus)> {
    let mut status = 0;
    let target = pid.map_or(-1, |pid| pid as i64);
    let child = check(raw::wait4(target, &mut status, 0))?;
    Ok((child, ExitStatus(status)))
}

/// Like [`wait`], but `None` if no such child has ended yet.
pub fn try_wait(pid: Option<u64>) -> Result<Option<(u64, ExitStatus)>> {
    let mut status = 0;
    let target = pid.map_or(-1, |pid| pid as i64);
    match check(raw::wait4(target, &mut status, raw::WNOHANG))? {
        0 => Ok(None),
        child => Ok(Some((child, ExitStatus(status)))),
    }
}

/// End the program: flush stdout, then exit with `code`.
pub fn exit(code: i32) -> ! {
    crate::io::flush();
    raw::exit_now(code)
}

pub fn id() -> u64 {
    raw::getpid() as u64
}

pub fn parent_id() -> u64 {
    raw::getppid() as u64
}

pub fn set_current_dir(path: &str) -> Result<()> {
    check(raw::chdir(&c_path(path)?)).map(|_| ())
}

pub fn current_dir() -> Result<String> {
    let mut buffer = [0u8; 4096];
    let length = check(raw::getcwd(&mut buffer))? as usize;
    Ok(String::from_utf8_lossy(&buffer[..length.saturating_sub(1)]).into_owned())
}
