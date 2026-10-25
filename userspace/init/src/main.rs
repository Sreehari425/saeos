//! `/sbin/init`: PID 1. Starts the shell on the console, starts it again
//! whenever it exits, and collects every process that ends (orphans are
//! handed to init by the kernel).

#![no_std]
#![no_main]

extern crate alloc;

use libsaeos::prelude::*;
use libsaeos::process::{self, Fork};
use libsaeos::{Args, entry};

entry!(main);

const SHELL: &str = "/bin/sh";

fn start_shell() -> Option<u64> {
    match process::fork() {
        Ok(Fork::Child) => {
            let error = process::exec_inherit(SHELL, &["sh"]);
            eprintln!("init: cannot run {SHELL}: {error}");
            process::exit(127)
        }
        Ok(Fork::Parent(pid)) => Some(pid),
        Err(error) => {
            eprintln!("init: fork: {error}");
            None
        }
    }
}

fn main(_args: Args) -> i32 {
    println!("init: SaeOS userspace is up (pid {})", process::id());
    let mut shell = start_shell();
    loop {
        match process::wait(None) {
            Ok((pid, status)) if Some(pid) == shell => {
                if status.code() == Some(127) {
                    eprintln!("init: {SHELL} cannot start; not retrying");
                    shell = None;
                    continue;
                }
                println!("init: shell exited; starting a new one");
                shell = start_shell();
            }
            // An orphan, collected.
            Ok(_) => {}
            // No children at all: nothing left to wait for, so restart.
            Err(_) => {
                if shell.is_none() {
                    loop {
                        core::hint::spin_loop();
                    }
                }
                shell = start_shell();
            }
        }
    }
}
