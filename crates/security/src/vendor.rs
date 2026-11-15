//! The vendored sources against their record: every upstream byte is
//! authenticated and every maintained deviation is accounted for.
//!
//! What the verifier complains of is a line for whoever maintains the
//! vendored tree: it names the path or the record that is wrong, in the
//! reference's words.

pub mod compare;
pub(crate) mod diff;
pub mod manifest;
mod wrap;

/// What the verifier complains of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem(pub String);

impl std::fmt::Display for Problem {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// An answer of the verifier, or what it complains of.
pub type Verdict<T> = Result<T, Problem>;

fn problem(text: impl Into<String>) -> Problem {
    Problem(text.into())
}

/// A text between quotes as the reference showed one in a complaint: single
/// quotes, unless the text holds one and no double quote; a backslash
/// before a backslash and before the quote that is used; and an escape for
/// whatever is no printable ASCII.
fn repr(text: &str) -> String {
    use std::fmt::Write as _;
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut shown = String::from(quote);
    for character in text.chars() {
        match character {
            '\\' => shown.push_str("\\\\"),
            '\t' => shown.push_str("\\t"),
            '\n' => shown.push_str("\\n"),
            '\r' => shown.push_str("\\r"),
            _ if character == quote => {
                shown.push('\\');
                shown.push(quote);
            }
            ' '..='~' => shown.push(character),
            _ if u32::from(character) < 0x100 => {
                let _ = write!(shown, "\\x{:02x}", u32::from(character));
            }
            _ if u32::from(character) < 0x1_0000 => {
                let _ = write!(shown, "\\u{:04x}", u32::from(character));
            }
            _ => {
                let _ = write!(shown, "\\U{:08x}", u32::from(character));
            }
        }
    }
    shown.push(quote);
    shown
}

/// Texts as the reference showed a list of them in a complaint.
fn listed(texts: &[&str]) -> String {
    let shown: Vec<String> = texts.iter().map(|text| repr(text)).collect();
    format!("[{}]", shown.join(", "))
}

#[cfg(test)]
mod tests;
