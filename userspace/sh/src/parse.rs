//! Shell command lines: quoting, expansion, redirections, `;`.
//!
//! ```text
//! echo "a $HOME" 'no $expansion' x\ y > out.txt ; cat < out.txt
//! └─ words ─────────────────────────┘ └ redirect ┘ │ └ second command ┘
//!                                                   separator
//! ```
//!
//! | syntax           | meaning                                             |
//! |------------------|-----------------------------------------------------|
//! | `'...'`          | literal text                                        |
//! | `"..."`          | text with `$` expansion; `\"`, `\\`, `\$` escapes   |
//! | `\c`             | the character `c`, literally                        |
//! | `$NAME` `${NAME}`| a variable (empty if unset)                         |
//! | `$?`             | the last command's exit status                      |
//! | `> f` `>> f` `< f` | redirect stdout (truncate / append) or stdin      |
//! | `;`              | run one command after another                       |
//! | `# ...`          | a comment, to the end of the line                   |
//!
//! Expansions are not split into several words (a variable holding spaces
//! stays one word), but an unquoted word that expands to nothing
//! disappears, as in sh. Pure (it gets variables through a function), so
//! the host model tests it.

use alloc::string::String;
use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectKind {
    /// `> file`
    Output,
    /// `>> file`
    Append,
    /// `< file`
    Input,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redirect {
    pub kind: RedirectKind,
    pub target: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Command {
    pub words: Vec<String>,
    pub redirects: Vec<Redirect>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    UnterminatedQuote,
    /// A redirection with no file name after it.
    MissingRedirectTarget,
    /// `${` without `}`.
    BadSubstitution,
}

impl ParseError {
    pub fn message(self) -> &'static str {
        match self {
            Self::UnterminatedQuote => "unterminated quote",
            Self::MissingRedirectTarget => "redirection without a file name",
            Self::BadSubstitution => "bad ${...} substitution",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Token {
    /// The text, and whether any of it was quoted (an empty quoted word
    /// is still a word).
    Word(String, bool),
    Redirect(RedirectKind),
    Separator,
}

fn is_name_start(character: char) -> bool {
    character == '_' || character.is_ascii_alphabetic()
}

fn is_name(character: char) -> bool {
    character == '_' || character.is_ascii_alphanumeric()
}

/// Expand a `$...` reference starting after the `$`; returns the value.
fn expand(
    characters: &mut core::iter::Peekable<core::str::Chars>,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<String, ParseError> {
    match characters.peek().copied() {
        Some('?') => {
            characters.next();
            Ok(lookup("?").unwrap_or_default())
        }
        Some('{') => {
            characters.next();
            let mut name = String::new();
            loop {
                match characters.next() {
                    Some('}') => break,
                    Some(character) if is_name(character) => name.push(character),
                    _ => return Err(ParseError::BadSubstitution),
                }
            }
            if name.is_empty() {
                return Err(ParseError::BadSubstitution);
            }
            Ok(lookup(&name).unwrap_or_default())
        }
        Some(character) if is_name_start(character) => {
            let mut name = String::new();
            while let Some(&character) = characters.peek() {
                if !is_name(character) {
                    break;
                }
                name.push(character);
                characters.next();
            }
            Ok(lookup(&name).unwrap_or_default())
        }
        // A lone `$` is just a dollar sign.
        _ => Ok(String::from("$")),
    }
}

fn tokenize(line: &str, lookup: &dyn Fn(&str) -> Option<String>) -> Result<Vec<Token>, ParseError> {
    let mut tokens = Vec::new();
    let mut characters = line.chars().peekable();
    let mut word = String::new();
    let mut in_word = false;
    let mut quoted = false;
    let finish =
        |tokens: &mut Vec<Token>, word: &mut String, in_word: &mut bool, quoted: &mut bool| {
            if *in_word && (!word.is_empty() || *quoted) {
                tokens.push(Token::Word(core::mem::take(word), *quoted));
            }
            word.clear();
            *in_word = false;
            *quoted = false;
        };
    while let Some(character) = characters.next() {
        match character {
            ' ' | '\t' | '\n' | '\r' => finish(&mut tokens, &mut word, &mut in_word, &mut quoted),
            '#' if !in_word => break,
            ';' => {
                finish(&mut tokens, &mut word, &mut in_word, &mut quoted);
                tokens.push(Token::Separator);
            }
            '<' => {
                finish(&mut tokens, &mut word, &mut in_word, &mut quoted);
                tokens.push(Token::Redirect(RedirectKind::Input));
            }
            '>' => {
                finish(&mut tokens, &mut word, &mut in_word, &mut quoted);
                let kind = if characters.peek() == Some(&'>') {
                    characters.next();
                    RedirectKind::Append
                } else {
                    RedirectKind::Output
                };
                tokens.push(Token::Redirect(kind));
            }
            '\'' => {
                in_word = true;
                quoted = true;
                loop {
                    match characters.next() {
                        Some('\'') => break,
                        Some(character) => word.push(character),
                        None => return Err(ParseError::UnterminatedQuote),
                    }
                }
            }
            '"' => {
                in_word = true;
                quoted = true;
                loop {
                    match characters.next() {
                        Some('"') => break,
                        Some('\\') => match characters.next() {
                            Some(escaped @ ('"' | '\\' | '$')) => word.push(escaped),
                            Some(other) => {
                                word.push('\\');
                                word.push(other);
                            }
                            None => return Err(ParseError::UnterminatedQuote),
                        },
                        Some('$') => word.push_str(&expand(&mut characters, lookup)?),
                        Some(character) => word.push(character),
                        None => return Err(ParseError::UnterminatedQuote),
                    }
                }
            }
            '\\' => {
                in_word = true;
                quoted = true;
                if let Some(escaped) = characters.next() {
                    word.push(escaped);
                }
            }
            '$' => {
                in_word = true;
                word.push_str(&expand(&mut characters, lookup)?);
            }
            character => {
                in_word = true;
                word.push(character);
            }
        }
    }
    finish(&mut tokens, &mut word, &mut in_word, &mut quoted);
    Ok(tokens)
}

/// Parse one line into the commands to run, in order. Empty commands
/// (blank lines, `;;`, comments) are dropped.
pub fn parse(
    line: &str,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<Vec<Command>, ParseError> {
    let mut commands = Vec::new();
    let mut current = Command::default();
    let mut tokens = tokenize(line, lookup)?.into_iter();
    while let Some(token) = tokens.next() {
        match token {
            Token::Word(word, _) => current.words.push(word),
            Token::Redirect(kind) => match tokens.next() {
                Some(Token::Word(target, _)) => current.redirects.push(Redirect { kind, target }),
                _ => return Err(ParseError::MissingRedirectTarget),
            },
            Token::Separator => {
                if !current.words.is_empty() || !current.redirects.is_empty() {
                    commands.push(core::mem::take(&mut current));
                }
            }
        }
    }
    if !current.words.is_empty() || !current.redirects.is_empty() {
        commands.push(current);
    }
    Ok(commands)
}
