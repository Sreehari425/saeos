//! `/bin/sh`, the SaeOS shell.
//!
//! ```text
//! sh              interactive: prompt `saeos:CWD$ `, one line at a time
//! sh FILE         run a script
//! sh -c COMMAND   run one command line
//! ```
//!
//! Each line is parsed by `parse.rs` (quotes, `$VAR`, `$?`, `>`, `>>`,
//! `<`, `;`). Builtins: `cd`, `pwd`, `echo`, `export`, `unset`, `exit`,
//! `help`. Anything else is looked up in `PATH` and run with `fork` +
//! `execve` + `wait4`; its redirections are applied in the child with
//! `dup2`. The exit status is the last command's (`$?`): its exit code,
//! 127 if not found, or 128 + the signal that killed it.

#![no_std]
#![no_main]

extern crate alloc;

mod parse;

use alloc::collections::BTreeMap;
use libsaeos::fs::FileType;
use libsaeos::io::File;
use libsaeos::prelude::*;
use libsaeos::process::{self, Fork};
use libsaeos::{Args, entry, env, fs, io, raw};
use parse::{Command, RedirectKind};

entry!(main);

struct Shell {
    variables: BTreeMap<String, String>,
    status: i32,
    /// Set by `exit`: stop and exit with this.
    exit: Option<i32>,
}

const HELP: &str = "\
builtins: cd [DIR], pwd, echo [-n] ARGS, export NAME=VALUE, unset NAME,
          exit [CODE], help
programs in $PATH run with fork + exec; redirect with > >> <, chain with ;
quote with '...' or \"...\"; $NAME and $? expand
";

fn main(_args: Args) -> i32 {
    let arguments: Vec<&str> = env::args().collect();
    let mut shell = Shell {
        variables: env::vars()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect(),
        status: 0,
        exit: None,
    };
    match arguments.get(1).copied() {
        Some("-c") => match arguments.get(2) {
            Some(line) => shell.run_line(line),
            None => {
                eprintln!("sh: -c needs a command");
                return 2;
            }
        },
        Some(path) => match fs::read_to_string(path) {
            Ok(script) => {
                for line in script.lines() {
                    shell.run_line(line);
                    if shell.exit.is_some() {
                        break;
                    }
                }
            }
            Err(error) => {
                eprintln!("sh: {path}: {error}");
                return 127;
            }
        },
        None => shell.interactive(),
    }
    shell.exit.unwrap_or(shell.status)
}

/// Whether fd 0 is a terminal (the console answers TIOCGWINSZ).
fn stdin_is_terminal() -> bool {
    let mut size = [0u16; 4];
    raw::ioctl(0, raw::TIOCGWINSZ, size.as_mut_ptr() as u64) == 0
}

fn signal_name(signal: i32) -> &'static str {
    match signal {
        4 => "SIGILL",
        5 => "SIGTRAP",
        7 => "SIGBUS",
        8 => "SIGFPE",
        11 => "SIGSEGV",
        _ => "a signal",
    }
}

impl Shell {
    fn interactive(&mut self) {
        let prompt = stdin_is_terminal();
        loop {
            if prompt {
                let cwd = process::current_dir().unwrap_or_else(|_| "?".to_string());
                print!("saeos:{cwd}$ ");
                io::flush();
            }
            let mut line = String::new();
            match io::stdin_read_line(&mut line) {
                Ok(0) => {
                    // End of file (Ctrl-D).
                    if prompt {
                        println!();
                    }
                    return;
                }
                Ok(_) => self.run_line(&line),
                Err(error) => {
                    eprintln!("sh: reading input: {error}");
                    return;
                }
            }
            if self.exit.is_some() {
                return;
            }
        }
    }

    fn run_line(&mut self, line: &str) {
        let status = self.status;
        let lookup = |name: &str| -> Option<String> {
            match name {
                "?" => Some(status.to_string()),
                name => self.variables.get(name).cloned(),
            }
        };
        let commands = match parse::parse(line, &lookup) {
            Ok(commands) => commands,
            Err(error) => {
                eprintln!("sh: {}", error.message());
                self.status = 2;
                return;
            }
        };
        for command in commands {
            self.status = self.run(&command);
            if self.exit.is_some() {
                return;
            }
        }
    }

    fn run(&mut self, command: &Command) -> i32 {
        let Some(name) = command.words.first() else {
            // Only redirections: `> file` creates (or truncates) the file.
            return match self.open_output(command) {
                Ok(_) => 0,
                Err(()) => 1,
            };
        };
        let arguments = &command.words[1..];
        match name.as_str() {
            "cd" => {
                let target = arguments
                    .first()
                    .cloned()
                    .or_else(|| self.variables.get("HOME").cloned())
                    .unwrap_or_else(|| "/".to_string());
                match process::set_current_dir(&target) {
                    Ok(()) => 0,
                    Err(error) => {
                        eprintln!("sh: cd: {target}: {error}");
                        1
                    }
                }
            }
            "exit" => {
                let code = match arguments.first() {
                    None => self.status,
                    Some(code) => code.parse().unwrap_or(2),
                };
                self.exit = Some(code);
                code
            }
            "export" => {
                for assignment in arguments {
                    match assignment.split_once('=') {
                        Some((name, value)) => {
                            self.variables.insert(name.to_string(), value.to_string());
                        }
                        // `export NAME` keeps a variable as it is.
                        None => {
                            self.variables.entry(assignment.clone()).or_default();
                        }
                    }
                }
                0
            }
            "unset" => {
                for name in arguments {
                    self.variables.remove(name);
                }
                0
            }
            "echo" | "pwd" | "help" => self.output_builtin(command),
            _ => self.run_program(command),
        }
    }

    /// The last `>`/`>>` target, opened; `None` means stdout.
    fn open_output(&self, command: &Command) -> core::result::Result<Option<File>, ()> {
        let mut output = None;
        for redirect in &command.redirects {
            let opened = match redirect.kind {
                RedirectKind::Output => File::create(&redirect.target),
                RedirectKind::Append => File::append(&redirect.target),
                RedirectKind::Input => continue,
            };
            match opened {
                Ok(file) => output = Some(file),
                Err(error) => {
                    eprintln!("sh: {}: {error}", redirect.target);
                    return Err(());
                }
            }
        }
        Ok(output)
    }

    /// Builtins that only print: run in the shell, writing to the
    /// redirection target if there is one.
    fn output_builtin(&self, command: &Command) -> i32 {
        let Ok(mut output) = self.open_output(command) else {
            return 1;
        };
        let arguments = &command.words[1..];
        let text = match command.words[0].as_str() {
            "echo" => {
                let (newline, words) = match arguments.first().map(String::as_str) {
                    Some("-n") => (false, &arguments[1..]),
                    _ => (true, arguments),
                };
                let mut text = words.join(" ");
                if newline {
                    text.push('\n');
                }
                text
            }
            "pwd" => match process::current_dir() {
                Ok(cwd) => format!("{cwd}\n"),
                Err(error) => {
                    eprintln!("sh: pwd: {error}");
                    return 1;
                }
            },
            _ => HELP.to_string(),
        };
        let written = match output.as_mut() {
            Some(file) => file.write_all(text.as_bytes()),
            None => {
                print!("{text}");
                Ok(())
            }
        };
        if written.is_err() { 1 } else { 0 }
    }

    /// `name` as a path to run: as given if it has a `/`, else the first
    /// match in `PATH`.
    fn find_program(&self, name: &str) -> Option<String> {
        let is_program = |path: &str| {
            fs::metadata(path).is_ok_and(|metadata| metadata.file_type == FileType::File)
        };
        if name.contains('/') {
            return is_program(name).then(|| name.to_string());
        }
        let path = self
            .variables
            .get("PATH")
            .map_or("/bin:/sbin", String::as_str);
        path.split(':')
            .filter(|directory| !directory.is_empty())
            .map(|directory| format!("{}/{name}", directory.trim_end_matches('/')))
            .find(|candidate| is_program(candidate))
    }

    fn run_program(&self, command: &Command) -> i32 {
        let name = &command.words[0];
        let Some(path) = self.find_program(name) else {
            eprintln!("sh: {name}: not found");
            return 127;
        };
        let environment: Vec<String> = self
            .variables
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect();
        match process::fork() {
            Ok(Fork::Child) => {
                if redirect_child(command).is_err() {
                    process::exit(1);
                }
                let arguments: Vec<&str> = command.words.iter().map(String::as_str).collect();
                let environment: Vec<&str> = environment.iter().map(String::as_str).collect();
                let error = process::exec(&path, &arguments, &environment);
                eprintln!("sh: {name}: {error}");
                process::exit(126)
            }
            Ok(Fork::Parent(child)) => match process::wait(Some(child)) {
                Ok((_, status)) => match (status.code(), status.signal()) {
                    (Some(code), _) => code,
                    (None, Some(signal)) => {
                        eprintln!("sh: {name}: killed by {}", signal_name(signal));
                        128 + signal
                    }
                    (None, None) => 1,
                },
                Err(error) => {
                    eprintln!("sh: wait: {error}");
                    1
                }
            },
            Err(error) => {
                eprintln!("sh: fork: {error}");
                1
            }
        }
    }
}

/// In the child, before exec: point stdin/stdout at the redirection files.
fn redirect_child(command: &Command) -> core::result::Result<(), ()> {
    for redirect in &command.redirects {
        let (file, target_fd) = match redirect.kind {
            RedirectKind::Output => (File::create(&redirect.target), 1),
            RedirectKind::Append => (File::append(&redirect.target), 1),
            RedirectKind::Input => (File::open(&redirect.target), 0),
        };
        let file = file.map_err(|error| eprintln!("sh: {}: {error}", redirect.target))?;
        if raw::dup2(file.fd(), target_fd) < 0 {
            eprintln!("sh: {}: cannot redirect", redirect.target);
            return Err(());
        }
        // `file` closes its own descriptor; the dup stays.
    }
    Ok(())
}
