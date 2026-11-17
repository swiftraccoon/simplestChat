//! The CodeQL tier's own steps beside its analyzer: how many compilers a
//! fresh extraction may run, the proof that the compiler was observed on
//! the native sources, the pinned analyzer with its bundled query suites,
//! the key of the Rust extractor's dependency build and its reuse, the
//! exact committed and vendored bytes that a finding's location is bound
//! to, and the triage of the findings against their exact reviews, with
//! the authorized dismissal of reviewed false positives on GitHub. Each
//! step keeps the reference's fixed codes and bounds.

pub mod cargo;
pub mod cli;
pub mod coverage;
pub mod rust_sources;
pub mod sources;
pub mod tools;
pub mod triage;

pub use cli::run;

use crate::{Fault, document::Document, policy::space, vendor::Problem};

/// Why a step that reads committed or vendored sources stopped: a fixed
/// code, or what the vendor's verifier complains of in its words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stop {
    /// A fixed code.
    Code(&'static str),
    /// What the vendor's verifier complains of.
    Problem(Problem),
}

impl std::fmt::Display for Stop {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Code(code) => formatter.write_str(code),
            Self::Problem(problem) => formatter.write_str(&problem.0),
        }
    }
}

impl std::error::Error for Stop {}

impl From<Fault> for Stop {
    fn from(fault: Fault) -> Self {
        Self::Code(fault.0)
    }
}

impl From<Problem> for Stop {
    fn from(problem: Problem) -> Self {
        Self::Problem(problem)
    }
}

/// An answer of a step that reads sources, or why it stopped.
pub type Sourced<T> = std::result::Result<T, Stop>;

/// The bytes of a JSON document as the reference's reader takes bytes: a
/// byte order mark of UTF-8 before it is passed over.
pub(crate) fn unmarked(data: &[u8]) -> &[u8] {
    data.strip_prefix(b"\xef\xbb\xbf").unwrap_or(data)
}

/// A number of a document as the reference's interpreter holds one: a
/// whole number exactly, any other as a fraction, and a truth value as one
/// or nothing.
enum Numeric {
    Whole(i128),
    Fraction(f64),
}

fn numeric(value: &Document) -> Option<Numeric> {
    match value {
        Document::Bool(held) => Some(Numeric::Whole(i128::from(*held))),
        Document::Number(number) => Some(match value.whole() {
            Some(whole) => Numeric::Whole(whole),
            None => Numeric::Fraction(number.as_f64()?),
        }),
        _ => None,
    }
}

/// Whether `left` equals `right` as the reference's interpreter compares
/// two decoded values: a truth value equals the number it counts as, a
/// whole number the fraction of the same value, and records equal whatever
/// the order of their fields.
pub(crate) fn alike(left: &Document, right: &Document) -> bool {
    if let (Some(left), Some(right)) = (numeric(left), numeric(right)) {
        return match (left, right) {
            (Numeric::Whole(left), Numeric::Whole(right)) => left == right,
            (Numeric::Whole(whole), Numeric::Fraction(fraction))
            | (Numeric::Fraction(fraction), Numeric::Whole(whole)) => whole as f64 == fraction,
            (Numeric::Fraction(left), Numeric::Fraction(right)) => left == right,
        };
    }
    match (left, right) {
        (Document::Null, Document::Null) => true,
        (Document::Text(left), Document::Text(right)) => left == right,
        (Document::List(left), Document::List(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| alike(left, right))
        }
        (Document::Record(left), Document::Record(right)) => {
            left.len() == right.len()
                && left.iter().all(|(name, value)| {
                    right
                        .iter()
                        .any(|(other, held)| other == name && alike(value, held))
                })
        }
        _ => false,
    }
}

/// The lines of `text` as the reference's interpreter splits text into
/// lines: at a line feed, a carriage return, both together, the vertical
/// tab, the form feed, the file, group and record separators, the next
/// line and the line and paragraph separators, with no line after the last
/// break.
pub(crate) fn lines(text: &str) -> Vec<&str> {
    let breaking = |character: char| {
        matches!(
            character,
            '\n' | '\r' | '\u{b}' | '\u{c}' | '\u{1c}'
                ..='\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}'
        )
    };
    let mut found = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let Some(at) = rest.find(breaking) else {
            found.push(rest);
            break;
        };
        found.push(&rest[..at]);
        let after = &rest[at..];
        let width = if after.starts_with("\r\n") {
            2
        } else {
            after.chars().next().map_or(1, char::len_utf8)
        };
        rest = &after[width..];
    }
    found
}

/// The words of `text` as the reference's interpreter splits text at its
/// white space.
pub(crate) fn words(text: &str) -> Vec<&str> {
    text.split(space).filter(|word| !word.is_empty()).collect()
}

#[cfg(test)]
pub(crate) mod tests;
