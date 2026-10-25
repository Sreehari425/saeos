//! The kernel command line: the boot-time copy and the Linux-style parser.

use crate::boot_info::{COMMAND_LINE_SIZE, Cmdline};
use crate::cmdline::{MAX_PARAMS, Params, level_name};

fn pairs<'a>(params: &Params<'a>) -> Vec<(&'a str, Option<&'a str>)> {
    params.iter().collect()
}

#[test]
fn words_values_flags_and_quotes() {
    let params =
        Params::parse("  loglevel=debug quiet  init=/bin/init key=\"a b  c\" \"x=y z\" empty= =v ");
    assert_eq!(
        pairs(&params),
        [
            ("loglevel", Some("debug")),
            ("quiet", None),
            ("init", Some("/bin/init")),
            ("key", Some("a b  c")),
            ("x", Some("y z")),
            ("empty", Some("")),
        ],
        "a word with an empty key is skipped"
    );
    assert_eq!(params.get("init"), Some("/bin/init"));
    assert_eq!(params.get("quiet"), None, "a flag has no value");
    assert!(params.flag("quiet"));
    assert!(!params.flag("loglevel"), "a key with a value is not a flag");
    assert_eq!(params.get("missing"), None);
    assert!(params.as_str().contains("x=y z"), "raw text is kept");
}

#[test]
fn edge_cases() {
    assert_eq!(Params::parse("").iter().count(), 0);
    assert_eq!(Params::parse(" \t ").iter().count(), 0);
    // The last value wins.
    let params = Params::parse("loglevel=info loglevel=warn loglevel");
    assert_eq!(params.get("loglevel"), Some("warn"));
    assert!(params.flag("loglevel"));
    // An unterminated quote runs to the end of the line.
    assert_eq!(
        pairs(&Params::parse("a=\"open b c")),
        [("a", Some("open b c"))]
    );
    // Values may contain `=`.
    assert_eq!(
        Params::parse("root=UUID=1234").get("root"),
        Some("UUID=1234")
    );
    // Non-ASCII text is just text.
    assert_eq!(Params::parse("name=ünï").get("name"), Some("ünï"));
}

#[test]
fn too_many_parameters_are_counted_and_dropped() {
    let line: Vec<String> = (0..MAX_PARAMS + 5)
        .map(|index| format!("p{index}=1"))
        .collect();
    let line = line.join(" ");
    let params = Params::parse(&line);
    assert_eq!(params.iter().count(), MAX_PARAMS);
    assert_eq!(params.dropped(), 5);
    assert_eq!(params.get("p63"), Some("1"));
    assert_eq!(params.get("p64"), None);
}

#[test]
fn log_levels_accept_names_and_linux_numbers() {
    for (value, expected) in [
        ("error", Some("error")),
        ("warn", Some("warn")),
        ("debug", Some("debug")),
        ("0", Some("error")),
        ("4", Some("error")),
        ("5", Some("warn")),
        ("7", Some("info")),
        ("8", Some("debug")),
        ("15", Some("debug")),
        ("loud", None),
        ("-1", None),
        ("", None),
    ] {
        assert_eq!(level_name(value), expected, "{value:?}");
    }
}

#[test]
fn boot_copy_trims_truncates_and_stays_utf8() {
    assert_eq!(Cmdline::new(b"").as_str(), "");
    assert_eq!(Cmdline::new(b"  quiet\n").as_str(), "quiet");
    assert_eq!(
        Cmdline::new(b"a=1\0garbage").as_str(),
        "a=1",
        "stops at NUL"
    );
    assert_eq!(
        Cmdline::new(b"a=1\nb=2\r\n").as_str(),
        "a=1 b=2",
        "newlines become spaces"
    );
    let long = vec![b'x'; COMMAND_LINE_SIZE + 100];
    assert_eq!(Cmdline::new(&long).as_str().len(), COMMAND_LINE_SIZE);
    // Truncation never splits a character.
    let mut text = vec![b'x'; COMMAND_LINE_SIZE - 1];
    text.extend_from_slice("é".as_bytes());
    assert_eq!(Cmdline::new(&text).as_str().len(), COMMAND_LINE_SIZE - 1);
    // Invalid UTF-8 keeps the valid prefix.
    assert_eq!(Cmdline::new(b"ok \xff bad").as_str(), "ok ");
}

#[test]
fn words_after_double_dash_are_init_arguments() {
    let params = Params::parse("quiet init=/bin/sh -- -x  script.sh loglevel=debug");
    assert!(params.flag("quiet"));
    assert_eq!(params.get("init"), Some("/bin/sh"));
    assert_eq!(
        params.init_args().collect::<Vec<_>>(),
        ["-x", "script.sh", "loglevel=debug"]
    );
    assert_eq!(
        params.get("loglevel"),
        None,
        "after -- is not a kernel parameter"
    );
    assert_eq!(Params::parse("quiet").init_args().count(), 0);
    assert_eq!(Params::parse("quiet --").init_args().count(), 0);
    // `--x` is an ordinary word.
    assert!(Params::parse("--x").flag("--x"));
}
