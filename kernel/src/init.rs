//! PID 1: start the userspace init program, as Linux's `kernel_init` does.
//!
//! ```text
//! saeos.kshell           -> the in-kernel debug shell instead of userspace
//! init=PATH              -> run PATH; if it cannot start, panic
//! otherwise              -> the first of /sbin/init /etc/init /bin/init /bin/sh
//! nothing could start    -> "No working init found" panic
//! ```
//!
//! The init task becomes the program: it builds the image, installs it as
//! its own process's address space, opens fds 0, 1, 2 on `/dev/console`, and
//! drops to ring 3. Words after `--` on the command line are init's
//! arguments. If PID 1 ever exits the kernel panics, again as Linux does
//! (`user-test` builds turn that exit into the QEMU test verdict instead).

use crate::fs::{Errno, OpenFlags};
use crate::task::ExitStatus;
use alloc::vec::Vec;

const DEFAULT_INITS: [&str; 4] = ["/sbin/init", "/etc/init", "/bin/init", "/bin/sh"];
const ENVIRONMENT: [&str; 3] = ["HOME=/", "PATH=/bin:/sbin", "TERM=linux"];

pub fn kernel_init() -> ! {
    let params = crate::cmdline::params();
    if params.flag("saeos.kshell") {
        crate::kinfo!("init: saeos.kshell given; running the kernel shell");
        crate::shell::run();
    }
    let arguments: Vec<&str> = params.init_args().collect();
    if let Some(path) = params.get("init") {
        let error = run_init(path, &arguments);
        no_working_init(format_args!(
            "Requested init {} failed (error -{}).",
            path,
            error.code()
        ));
    }
    for path in DEFAULT_INITS {
        match run_init(path, &arguments) {
            Errno::ENOENT => {}
            error => crate::kwarn!(
                "init: {} exists but could not be executed (error -{})",
                path,
                error.code()
            ),
        }
    }
    no_working_init(format_args!("No init found in {:?}.", DEFAULT_INITS));
}

/// Start `path` as init. Only returns if it could not be started.
fn run_init(path: &str, arguments: &[&str]) -> Errno {
    let mut argv = alloc::vec![path];
    argv.extend_from_slice(arguments);
    let image = match crate::exec::prepare(path, &argv, &ENVIRONMENT) {
        Ok(image) => image,
        Err(error) => return error,
    };
    if let Err(error) = open_console() {
        return error;
    }
    crate::kinfo!("init: starting {}", path);
    let (entry, stack_pointer) = crate::exec::install_current(image);
    unsafe { crate::arch::x86_64::syscall::enter_user(entry, stack_pointer) }
}

/// fds 0, 1 and 2 on the console, once.
fn open_console() -> Result<(), Errno> {
    let files = crate::fs::current_files()?;
    if files.open_count() >= 3 {
        return Ok(());
    }
    for _ in 0..3 {
        crate::fs::VFS.open(&files, "/dev/console", OpenFlags::READ_WRITE)?;
    }
    Ok(())
}

fn no_working_init(reason: core::fmt::Arguments) -> ! {
    crate::kerror!("init: {}", reason);
    let expected = cfg!(feature = "user-test")
        && crate::cmdline::params().get("saeos.test") == Some("no-init");
    crate::fatal::report(
        format_args!(
            "Kernel panic - not syncing: No working init found. Try passing init= option to kernel."
        ),
        expected,
    )
}

/// Called (interrupts off) when PID 1 exits or is killed.
pub fn init_exited(status: ExitStatus) -> ! {
    let description = describe(status);
    crate::kinfo!(
        "init exited: {} (user-mode preemptions: {})",
        description,
        crate::scheduler::user_preemptions()
    );
    #[cfg(feature = "user-test")]
    {
        // The test expects `saeos.test.exit=N` (default 0) or `sigN`.
        let wanted = crate::cmdline::params()
            .get("saeos.test.exit")
            .unwrap_or("0");
        let expected = match wanted.strip_prefix("sig") {
            Some(signal) => signal.parse().ok().map(ExitStatus::killed),
            None => wanted.parse().ok().map(ExitStatus),
        };
        crate::serial_println!(
            "user-test: init {} (wanted {}) -> {}",
            description,
            wanted,
            if expected == Some(status) {
                "PASS"
            } else {
                "FAIL"
            }
        );
        crate::arch::x86_64::qemu::exit(expected == Some(status));
    }
    #[allow(unreachable_code)]
    crate::fatal::report(
        format_args!(
            "Kernel panic - not syncing: Attempted to kill init! ({})",
            description
        ),
        false,
    )
}

fn describe(status: ExitStatus) -> alloc::string::String {
    match status.signal() {
        Some(signal) => alloc::format!("killed by {}", signal_name(signal)),
        None => alloc::format!("exit code {}", status.0),
    }
}

pub fn signal_name(signal: i32) -> &'static str {
    match signal {
        4 => "SIGILL",
        5 => "SIGTRAP",
        7 => "SIGBUS",
        8 => "SIGFPE",
        11 => "SIGSEGV",
        _ => "signal",
    }
}
