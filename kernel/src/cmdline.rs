//! The kernel command line, Linux style.
//!
//! ```text
//! BIOS   qemu -kernel saeos.bin -append "loglevel=debug quiet"   (Multiboot)
//! UEFI   BOOTX64.EFI loglevel=debug   (load options)  or  \saeos\cmdline.txt
//!   -> BootInfo.cmdline -> cmdline::init -> /proc/cmdline
//! ```
//!
//! Words are separated by spaces; each is `key=value` or a bare flag, and
//! double quotes keep spaces inside a value (`key="a b"`). A key given twice
//! takes its last value. Unknown parameters are kept (they show in
//! `/proc/cmdline`) and only logged at debug level, as Linux does.
//!
//! Parameters the kernel acts on:
//!
//! | parameter         | effect                                            |
//! |-------------------|---------------------------------------------------|
//! | `loglevel=LEVEL`  | log threshold: `error`/`warn`/`info`/`debug`, or Linux's 0-8 |
//! | `quiet`           | same as `loglevel=warn`                           |
//! | `init=PATH`       | the program PID 1 runs (`kernel/src/init.rs`) |
//! | `saeos.kshell`    | run the in-kernel debug shell instead of userspace init |
//! | `-- ARGS...`      | everything after `--` is passed to init as arguments |
//!
//! The parser below is pure and is tested in the host model.

/// Parameters beyond this are ignored (with a warning).
pub const MAX_PARAMS: usize = 64;

/// The parsed command line. Borrows the raw text; never allocates.
pub struct Params<'a> {
    raw: &'a str,
    params: [(&'a str, Option<&'a str>); MAX_PARAMS],
    count: usize,
    dropped: usize,
    /// Everything after a `--` word: arguments for init.
    init_args: &'a str,
}

/// Strip one pair of surrounding double quotes (or a lone leading one).
fn unquote(text: &str) -> &str {
    let text = text.strip_prefix('"').unwrap_or(text);
    text.strip_suffix('"').unwrap_or(text)
}

impl<'a> Params<'a> {
    pub fn parse(raw: &'a str) -> Self {
        let mut params = Self {
            raw,
            params: [("", None); MAX_PARAMS],
            count: 0,
            dropped: 0,
            init_args: "",
        };
        let mut rest = raw;
        loop {
            rest = rest.trim_start();
            if rest.is_empty() {
                break;
            }
            // A word ends at whitespace outside double quotes.
            let mut quoted = false;
            let end = rest
                .char_indices()
                .find(|&(_, character)| {
                    if character == '"' {
                        quoted = !quoted;
                    }
                    character.is_whitespace() && !quoted
                })
                .map_or(rest.len(), |(index, _)| index);
            let (word, tail) = rest.split_at(end);
            rest = tail;
            if word == "--" {
                params.init_args = rest.trim();
                break;
            }
            // `"key=value"` and `key="value"` both mean key=value.
            let word = word
                .strip_prefix('"')
                .map_or(word, |inner| inner.strip_suffix('"').unwrap_or(inner));
            let param = match word.split_once('=') {
                Some((key, value)) => (key, Some(unquote(value))),
                None => (word, None),
            };
            if param.0.is_empty() {
                continue;
            }
            if params.count == MAX_PARAMS {
                params.dropped += 1;
            } else {
                params.params[params.count] = param;
                params.count += 1;
            }
        }
        params
    }

    /// The text as given, for `/proc/cmdline`.
    pub fn as_str(&self) -> &'a str {
        self.raw
    }

    /// The last value given for `key`.
    pub fn get(&self, key: &str) -> Option<&'a str> {
        self.iter()
            .filter(|&(name, _)| name == key)
            .filter_map(|(_, value)| value)
            .next_back()
    }

    /// True if `name` appears as a bare flag.
    pub fn flag(&self, name: &str) -> bool {
        self.iter().any(|param| param == (name, None))
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = (&'a str, Option<&'a str>)> + '_ {
        self.params[..self.count].iter().copied()
    }

    /// The words after `--`, which Linux passes to init as its arguments
    /// (`init=/bin/sh -- -c "echo hi"` is not supported: words are split on
    /// spaces only).
    pub fn init_args(&self) -> impl Iterator<Item = &'a str> + 'a {
        self.init_args.split_whitespace()
    }

    /// Parameters past [`MAX_PARAMS`] that were ignored.
    pub fn dropped(&self) -> usize {
        self.dropped
    }
}

/// Normalize a `loglevel=` value to a level name: names pass through, and
/// Linux's numbers map to the nearest level (console messages more
/// important than N are shown: 4 shows errors, 5 warnings, 7 info, 8 all).
pub fn level_name(value: &str) -> Option<&'static str> {
    match value {
        "error" => Some("error"),
        "warn" => Some("warn"),
        "info" => Some("info"),
        "debug" => Some("debug"),
        number => Some(match number.parse::<u8>().ok()? {
            0..=4 => "error",
            5..=6 => "warn",
            7 => "info",
            _ => "debug",
        }),
    }
}

/// The parameters the kernel itself understands.
pub const KNOWN: &[&str] = &["loglevel", "quiet", "init", "saeos.kshell"];

#[cfg(not(host_model))]
mod kernel {
    use super::{KNOWN, Params, level_name};
    use crate::boot::boot_info::Cmdline;
    use crate::klog::{self, Level};
    use core::cell::UnsafeCell;
    use core::sync::atomic::{AtomicBool, Ordering};

    struct BootCmdline {
        line: UnsafeCell<Cmdline>,
        ready: AtomicBool,
    }

    // Written once during single-threaded boot, before `ready` is set;
    // read-only afterwards.
    unsafe impl Sync for BootCmdline {}

    static CMDLINE: BootCmdline = BootCmdline {
        line: UnsafeCell::new(Cmdline::EMPTY),
        ready: AtomicBool::new(false),
    };

    /// Keep the boot command line and act on it. Called once, early in
    /// `kernel_main_high`; needs no heap.
    pub fn init(line: &Cmdline) {
        if CMDLINE.ready.load(Ordering::Acquire) {
            return;
        }
        unsafe { *CMDLINE.line.get() = *line };
        CMDLINE.ready.store(true, Ordering::Release);

        let params = params();
        crate::kinfo!("cmdline: {}", params.as_str());
        if params.dropped() != 0 {
            crate::kwarn!(
                "cmdline: {} parameters past the first {} ignored",
                params.dropped(),
                super::MAX_PARAMS
            );
        }
        if params.flag("quiet") {
            klog::set_level(Level::Warn);
        }
        if let Some(value) = params.get("loglevel") {
            match level_name(value).and_then(Level::parse) {
                Some(level) => klog::set_level(level),
                None => crate::kwarn!("cmdline: bad loglevel={}", value),
            }
        }
        for (key, _) in params.iter().filter(|(key, _)| !KNOWN.contains(key)) {
            crate::kdebug!("cmdline: unknown parameter {}", key);
        }
    }

    /// The raw command line (empty before `init`).
    pub fn raw() -> &'static str {
        if CMDLINE.ready.load(Ordering::Acquire) {
            unsafe { (*CMDLINE.line.get()).as_str() }
        } else {
            ""
        }
    }

    pub fn params() -> Params<'static> {
        Params::parse(raw())
    }
}

#[cfg(not(host_model))]
pub use kernel::{init, params, raw};
