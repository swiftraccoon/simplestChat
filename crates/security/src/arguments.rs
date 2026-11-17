//! Command lines as the reference's parsers read them: an option named
//! whole or by a beginning that is no other's, its value after it or after
//! an `=`, `--` before words only, and the help wherever it is asked for.

/// The two names of the help, which every parser of the reference's knew.
const HELP: [&str; 2] = ["-h", "--help"];

/// How the reference's parser read one argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Read<'a> {
    /// A word that names no option: a value, or one too many.
    Word,
    /// `--`, after which every argument is a word.
    End,
    /// An option, with the value given after its `=` where there is one.
    Named(&'static str, Option<&'a str>),
    /// The help: asked for, or refused for a value given to it.
    Help(bool),
    /// The beginning of more than one option.
    Ambiguous,
    /// An option that the parser does not take.
    Unknown,
}

/// How the reference's parser read `argument` where its options are
/// `options` and the help: `--`; an option named whole, or a long one by a
/// beginning that is no other's, each with or without a value after `=`;
/// the short help with more after it; and otherwise a word, unless it begins
/// like an option and is no negative number and holds no space.
pub(crate) fn read<'a>(argument: &'a str, options: &[&'static str]) -> Read<'a> {
    if argument == "--" {
        return Read::End;
    }
    if !argument.starts_with('-') {
        return Read::Word;
    }
    if HELP.contains(&argument) {
        return Read::Help(true);
    }
    if let Some(option) = options.iter().find(|option| **option == argument) {
        return Read::Named(option, None);
    }
    if argument.len() == 1 {
        return Read::Word;
    }
    if let Some((name, value)) = argument.split_once('=') {
        if HELP.contains(&name) {
            return Read::Help(false);
        }
        if let Some(option) = options.iter().find(|option| **option == name) {
            return Read::Named(option, Some(value));
        }
    }
    if argument.starts_with("--") {
        let (beginning, value) = match argument.split_once('=') {
            Some((beginning, value)) => (beginning, Some(value)),
            None => (argument, None),
        };
        let all = options.iter().copied().chain(["--help"]);
        let begun: Vec<&'static str> = all.filter(|option| option.starts_with(beginning)).collect();
        match begun[..] {
            [] => {}
            ["--help"] => return Read::Help(value.is_none()),
            [option] => return Read::Named(option, value),
            _ => return Read::Ambiguous,
        }
    } else if argument.starts_with("-=") {
        // Every option begins with the one prefix before the `=`.
        return Read::Ambiguous;
    } else if let Some(rest) = argument.strip_prefix("-h") {
        // The parser took what follows the short help for more short
        // options, unless it began with a prefix.
        return Read::Help(!rest.starts_with('-'));
    }
    let number = argument[1..].strip_prefix('.').unwrap_or(&argument[1..]);
    if number.starts_with(|letter: char| letter.is_ascii_digit()) || argument.contains(' ') {
        return Read::Word;
    }
    Read::Unknown
}
