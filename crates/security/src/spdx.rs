//! Licence expressions in the SPDX form, judged against an allowlist.
//!
//! An expression names licences, joins them with `AND` and `OR`, attaches
//! an exception to one with `WITH`, and groups with parentheses. It is
//! allowed when every licence of some alternative is on the list. A licence
//! with an exception is one entry of the list, under both names. The
//! grammar is bounded and nothing but the grammar is read: an expression in
//! any other form is refused and never guessed at.

use crate::{Fault, Result, policy::space, require};
use std::collections::BTreeSet;

/// The most characters of an expression.
const LONGEST: usize = 4096;
/// How many groups may be open at once.
const DEEPEST: usize = 32;

/// The licences a policy allows, as expressions are compared with them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Allowed(BTreeSet<String>);

/// A licence as it is compared: a reference of one's own exactly as it is
/// written, any other name whatever the case of its letters.
fn identifier(name: &str) -> String {
    if name.starts_with("LicenseRef-") || name.starts_with("DocumentRef-") {
        name.to_owned()
    } else {
        name.to_ascii_lowercase()
    }
}

impl Allowed {
    /// The list of `licenses`.
    pub fn new<'a>(licenses: impl IntoIterator<Item = &'a str>) -> Self {
        Self(licenses.into_iter().map(identifier).collect())
    }
}

/// The words of `expression`: names, which start with a letter or a digit
/// and go on with those, points, plus signs, colons and hyphens, and the
/// two parentheses. Space between words is skipped and nothing else is.
/// Every word takes at least its first character, so the reading ends.
fn words(expression: &str) -> Result<Vec<&str>> {
    let mut words = Vec::new();
    let mut rest = expression.trim_start_matches(space);
    while let Some(first) = rest.chars().next() {
        let length = if matches!(first, '(' | ')') {
            1
        } else {
            let named = |byte: &u8| byte.is_ascii_alphanumeric() || b".+:-".contains(byte);
            require(first.is_ascii_alphanumeric(), "image_license_grammar")?;
            1 + rest.bytes().skip(1).take_while(named).count()
        };
        words.push(&rest[..length]);
        rest = rest[length..].trim_start_matches(space);
    }
    Ok(words)
}

/// An expression being read word by word.
struct Reader<'a> {
    words: Vec<&'a str>,
    at: usize,
    allowed: &'a Allowed,
}

impl Reader<'_> {
    fn take(&mut self, word: &str) -> bool {
        let next = self.words.get(self.at) == Some(&word);
        self.at += usize::from(next);
        next
    }

    /// A group, or a licence with the exception that follows it.
    fn atom(&mut self, depth: usize) -> Result<bool> {
        require(
            depth < DEEPEST && self.at < self.words.len(),
            "image_license_depth",
        )?;
        if self.take("(") {
            let allowed = self.choice(depth + 1)?;
            require(self.take(")"), "image_license_parenthesis")?;
            return Ok(allowed);
        }
        let name = self.words[self.at];
        require(
            !matches!(name, "AND" | "OR" | "WITH" | ")"),
            "image_license_atom",
        )?;
        self.at += 1;
        if !self.take("WITH") {
            return Ok(self.allowed.0.contains(&identifier(name)));
        }
        let exception = self.words.get(self.at);
        let exception = exception.ok_or(Fault("image_license_exception"))?;
        self.at += 1;
        let whole = format!("{name} WITH {exception}");
        Ok(self.allowed.0.contains(&identifier(&whole)))
    }

    /// Licences that all apply: each must be allowed, and each is read.
    fn conjunction(&mut self, depth: usize) -> Result<bool> {
        let mut allowed = self.atom(depth)?;
        while self.take("AND") {
            allowed &= self.atom(depth)?;
        }
        Ok(allowed)
    }

    /// Alternatives: one that is allowed is enough, and each is read.
    fn choice(&mut self, depth: usize) -> Result<bool> {
        let mut allowed = self.conjunction(depth)?;
        while self.take("OR") {
            allowed |= self.conjunction(depth)?;
        }
        Ok(allowed)
    }
}

/// Whether the policy allows `expression`. An expression that is no
/// expression is an error, whatever was allowed before the fault.
pub fn permits(expression: &str, allowed: &Allowed) -> Result<bool> {
    let length = expression.chars().count();
    require((1..=LONGEST).contains(&length), "image_license_expression")?;
    let mut reader = Reader {
        words: words(expression)?,
        at: 0,
        allowed,
    };
    let permitted = reader.choice(0)?;
    require(
        reader.at == reader.words.len(),
        "image_license_trailing_tokens",
    )?;
    Ok(permitted)
}

#[cfg(test)]
mod tests;
