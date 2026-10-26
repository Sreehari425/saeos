//! The shell's command-line parser.

use crate::sh_parse::{Command, ParseError, Redirect, RedirectKind, parse};

fn variables(name: &str) -> Option<String> {
    match name {
        "HOME" => Some("/home/user".into()),
        "?" => Some("3".into()),
        "SPACED" => Some("a  b".into()),
        "EMPTY" => Some(String::new()),
        _ => None,
    }
}

fn words(line: &str) -> Vec<Vec<String>> {
    parse(line, &variables)
        .unwrap()
        .into_iter()
        .map(|command| command.words)
        .collect()
}

fn one(line: &str) -> Vec<String> {
    let mut commands = words(line);
    assert_eq!(commands.len(), 1, "{line}");
    commands.remove(0)
}

#[test]
fn words_and_whitespace() {
    assert_eq!(one("ls -l   /etc"), ["ls", "-l", "/etc"]);
    assert_eq!(one("  echo\thi  "), ["echo", "hi"]);
    assert!(words("").is_empty());
    assert!(words("   ").is_empty());
    assert!(words("# just a comment").is_empty());
    assert_eq!(one("echo a # comment"), ["echo", "a"]);
    assert_eq!(one("echo a#b"), ["echo", "a#b"], "# inside a word is text");
}

#[test]
fn quoting_and_escapes() {
    assert_eq!(one("echo 'a  $HOME  b'"), ["echo", "a  $HOME  b"]);
    assert_eq!(one("echo \"a  $HOME\""), ["echo", "a  /home/user"]);
    assert_eq!(
        one(r#"echo "say \"hi\" \\ \$HOME""#),
        ["echo", r#"say "hi" \ $HOME"#]
    );
    assert_eq!(
        one(r#"echo "keep \n""#),
        ["echo", r"keep \n"],
        "other escapes stay"
    );
    assert_eq!(one(r"echo a\ b \$HOME"), ["echo", "a b", "$HOME"]);
    assert_eq!(
        one("echo 'one'\"two\"three"),
        ["echo", "onetwothree"],
        "parts join"
    );
    assert_eq!(
        one("echo '' \"\""),
        ["echo", "", ""],
        "empty quoted words stay"
    );
    assert_eq!(
        one("echo ';' '>' \"<\""),
        ["echo", ";", ">", "<"],
        "quoted operators are text"
    );
}

#[test]
fn expansion() {
    assert_eq!(
        one("echo $HOME/x ${HOME}y $?"),
        ["echo", "/home/user/x", "/home/usery", "3"]
    );
    assert_eq!(one("echo $SPACED"), ["echo", "a  b"], "no word splitting");
    assert_eq!(
        one("echo $NOPE $EMPTY end"),
        ["echo", "end"],
        "empty unquoted words vanish"
    );
    assert_eq!(
        one("echo \"$NOPE\""),
        ["echo", ""],
        "a quoted empty word stays"
    );
    assert_eq!(
        one("echo $ cost$"),
        ["echo", "$", "cost$"],
        "a lone $ is literal"
    );
    assert_eq!(one("echo $HOME_DIR"), ["echo"], "the longest name is used");
}

#[test]
fn redirections_and_separators() {
    let commands = parse("cat < in.txt > out.txt ; echo x >> log", &variables).unwrap();
    assert_eq!(
        commands,
        [
            Command {
                words: vec!["cat".into()],
                redirects: vec![
                    Redirect {
                        kind: RedirectKind::Input,
                        target: "in.txt".into()
                    },
                    Redirect {
                        kind: RedirectKind::Output,
                        target: "out.txt".into()
                    },
                ],
            },
            Command {
                words: vec!["echo".into(), "x".into()],
                redirects: vec![Redirect {
                    kind: RedirectKind::Append,
                    target: "log".into()
                }],
            },
        ]
    );
    // No spaces needed around operators; the target may be expanded.
    let commands = parse("echo hi>$HOME/f;;true", &variables).unwrap();
    assert_eq!(commands.len(), 2, "empty commands between ;; are dropped");
    assert_eq!(commands[0].redirects[0].target, "/home/user/f");
    assert_eq!(commands[1].words, ["true"]);
    // Only a redirection: allowed (it creates the file).
    let commands = parse("> new.txt", &variables).unwrap();
    assert!(commands[0].words.is_empty() && commands[0].redirects.len() == 1);
}

#[test]
fn errors() {
    assert_eq!(
        parse("echo 'open", &variables),
        Err(ParseError::UnterminatedQuote)
    );
    assert_eq!(
        parse("echo \"open", &variables),
        Err(ParseError::UnterminatedQuote)
    );
    assert_eq!(
        parse("echo >", &variables),
        Err(ParseError::MissingRedirectTarget)
    );
    assert_eq!(
        parse("echo > ; ls", &variables),
        Err(ParseError::MissingRedirectTarget)
    );
    assert_eq!(
        parse("echo ${HOME", &variables),
        Err(ParseError::BadSubstitution)
    );
    assert_eq!(
        parse("echo ${}", &variables),
        Err(ParseError::BadSubstitution)
    );
}
