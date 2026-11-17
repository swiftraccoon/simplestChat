//! The committed literal include closure of a Rust source: the extractor
//! can place an included expression at its parent's `include!`, so a
//! review of the parent alone would leave that expression unbound. Nothing
//! is expanded or run; an include that is not a plain local string refuses
//! the source.

use super::{Sourced, sources::Shown, sources::tracked_bytes};
use crate::{Fault, digest, policy::space, require};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

/// The most files of one closure.
pub const MAX_FILES: usize = 64;
/// The most includes of one closure, and of one file.
pub const MAX_EDGES: usize = 256;
/// The most bytes of one closure.
pub const MAX_BYTES: u64 = 16 * 1024 * 1024;
/// The deepest an include may be below the source.
pub const MAX_DEPTH: usize = 16;
/// The most tokens of one file.
pub const MAX_TOKENS: usize = 500_000;
/// The most characters of one included path.
pub const MAX_PATH: usize = 1024;
/// The most a raw string's opening may be marked with.
const MAX_HASHES: usize = 255;
/// The fewest tokens an include takes after its name.
const MIN_INVOCATION: usize = 4;
/// Where an included file's bytes are read.
const MAX_INCLUDED: u64 = super::sources::MAX_SOURCE;

/// What one token of Rust source is: a name, a plain string literal and its
/// contents as written, another string literal, which is never read as a
/// path, or any other mark.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Token {
    /// A name, `r#` and all.
    Identifier(String),
    /// A plain string literal's contents, escapes as written.
    Literal(String),
    /// A byte or C string literal's contents, or nothing for a raw one.
    Opaque(String),
    /// A character literal, or any one character that is no white space.
    Other(String),
}

/// The length of a name at the start of `text`, optionally after `r#`: a
/// letter or an underscore, then letters, digits and underscores.
fn identifier(text: &str) -> Option<usize> {
    let raw = if text.starts_with("r#") { 2 } else { 0 };
    let named = |rest: &str| {
        let first = rest
            .bytes()
            .next()
            .filter(|byte| byte.is_ascii_alphabetic() || *byte == b'_');
        first.map(|_| {
            rest.bytes()
                .take_while(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
                .count()
        })
    };
    named(&text[raw..])
        .map(|length| raw + length)
        .or_else(|| named(text))
}

/// The length of a character literal at the start of `text`, `b` before it
/// or not: one character that is no quote, backslash or line end, or a
/// backslash and any one character but a line end.
fn character(text: &str) -> Option<usize> {
    let byte = usize::from(text.starts_with("b'"));
    let rest = text[byte..].strip_prefix('\'')?;
    let mut characters = rest.chars();
    let inner = match characters.next()? {
        '\\' => 1 + characters.next().filter(|next| *next != '\n')?.len_utf8(),
        '\'' | '\n' => return None,
        other => other.len_utf8(),
    };
    rest[inner..]
        .starts_with('\'')
        .then_some(byte + 1 + inner + 1)
}

/// The raw string literal's opening at the start of `text`: its length,
/// and how many `#` mark it.
fn raw_opening(text: &str) -> Option<(usize, usize)> {
    let prefix = ["br", "cr", "r"]
        .into_iter()
        .find(|prefix| text.starts_with(prefix))?;
    let rest = &text[prefix.len()..];
    let hashes = rest.bytes().take_while(|byte| *byte == b'#').count();
    let hashes = hashes.min(MAX_HASHES);
    rest[hashes..]
        .starts_with('"')
        .then_some((prefix.len() + hashes + 1, hashes))
}

/// The tokens of the Rust source `text`, with comments, white space and
/// the contents of every literal but a plain string passed over.
pub fn tokens(text: &str) -> Sourced<Vec<Token>> {
    let mut result = Vec::new();
    let mut position = 0;
    while position < text.len() {
        let rest = &text[position..];
        if rest.starts_with(space) {
            position += rest.len() - rest.trim_start_matches(space).len();
            continue;
        }
        if rest.starts_with("//") {
            position += rest.find('\n').unwrap_or(rest.len());
            continue;
        }
        if rest.starts_with("/*") {
            // Openings and closings are found left to right, none inside
            // another, as the reference's pattern finds them.
            let mut depth = 1;
            let mut at = position + 2;
            while depth > 0 && at < text.len() {
                let marked = &text.as_bytes()[at..];
                if marked.starts_with(b"/*") {
                    depth += 1;
                    at += 2;
                } else if marked.starts_with(b"*/") {
                    depth -= 1;
                    at += 2;
                } else {
                    at += 1;
                }
            }
            require(depth == 0, "codeql_rust_comment")?;
            position = at;
            continue;
        }
        if let Some((opening, hashes)) = raw_opening(rest) {
            let closing = format!("\"{}", "#".repeat(hashes));
            let end = text[position + opening..].find(&closing);
            let end = end.ok_or(Fault("codeql_rust_string"))?;
            position += opening + end + closing.len();
            result.push(Token::Opaque(String::new()));
        } else if let Some(quote) = ["\"", "b\"", "c\""]
            .into_iter()
            .find(|quote| rest.starts_with(quote))
        {
            let start = position + quote.len();
            let bytes = text.as_bytes();
            let mut at = start;
            while at < bytes.len() && bytes[at] != b'"' {
                at += if bytes[at] == b'\\' { 2 } else { 1 };
            }
            require(at < bytes.len(), "codeql_rust_string")?;
            let contents = text[start..at].to_owned();
            result.push(if quote.len() == 1 {
                Token::Literal(contents)
            } else {
                Token::Opaque(contents)
            });
            position = at + 1;
        } else if let Some(length) = character(rest) {
            result.push(Token::Other(rest[..length].to_owned()));
            position += length;
        } else if let Some(length) = identifier(rest) {
            result.push(Token::Identifier(rest[..length].to_owned()));
            position += length;
        } else {
            let width = rest.chars().next().map_or(1, char::len_utf8);
            result.push(Token::Other(rest[..width].to_owned()));
            position += width;
        }
        require(result.len() <= MAX_TOKENS, "codeql_rust_token_limit")?;
    }
    Ok(result)
}

/// Whether `token` is the mark `mark`.
fn marked(token: Option<&Token>, mark: &str) -> bool {
    matches!(token, Some(Token::Other(held)) if held == mark)
}

/// The local paths that `text` includes: each `include!` must be called
/// with one plain string of letters, digits, `_`, `.`, `/` and `-` that is
/// no absolute path, with a comma after it or not, and comments anywhere
/// between.
pub fn literal_includes(text: &str) -> Sourced<Vec<String>> {
    let stream = tokens(text)?;
    let mut result = Vec::new();
    for (index, token) in stream.iter().enumerate() {
        let Token::Identifier(name) = token else {
            continue;
        };
        if name != "include" && name != "r#include" {
            continue;
        }
        let following = &stream[index + 1..(index + 6).min(stream.len())];
        if !marked(following.first(), "!") {
            continue;
        }
        let path = match following.get(2) {
            Some(Token::Literal(path)) => Some(path),
            _ => None,
        };
        let closed = marked(following.get(3), ")")
            || (following.len() == 5
                && marked(following.get(3), ",")
                && marked(following.get(4), ")"));
        let invoked = following.len() >= MIN_INVOCATION && marked(following.get(1), "(") && closed;
        let path = path
            .filter(|_| invoked)
            .ok_or(Fault("codeql_rust_include_literal"))?;
        let plain = |byte: u8| byte.is_ascii_alphanumeric() || b"_./-".contains(&byte);
        let local = (1..=MAX_PATH).contains(&path.chars().count())
            && path.bytes().all(plain)
            && !path.starts_with('/');
        require(local, "codeql_rust_include_path")?;
        result.push(path.clone());
        require(result.len() <= MAX_EDGES, "codeql_rust_include_edges")?;
    }
    Ok(result)
}

/// The SHA-256 of every file the committed source `name` includes, however
/// deep, by its path; an edited, linked or escaping file, a cycle and any
/// bound passed all refuse it.
pub fn included_sources(
    root: &Path,
    revision: &str,
    name: &str,
    git: &dyn Shown,
) -> Sourced<BTreeMap<String, String>> {
    let mut pending: Vec<(String, usize, BTreeSet<String>)> =
        vec![(name.to_owned(), 0, BTreeSet::new())];
    let mut hashes: BTreeMap<String, String> = BTreeMap::new();
    let (mut total, mut edges) = (0_u64, 0_usize);
    while let Some((selected, depth, ancestors)) = pending.pop() {
        require(!ancestors.contains(&selected), "codeql_rust_include_cycle")?;
        require(depth <= MAX_DEPTH, "codeql_rust_include_depth")?;
        if hashes.contains_key(&selected) {
            continue;
        }
        require(hashes.len() < MAX_FILES, "codeql_rust_include_limit")?;
        let data = tracked_bytes(root, revision, &selected, MAX_INCLUDED, git)?;
        total += data.len() as u64;
        require(total <= MAX_BYTES, "codeql_rust_include_bytes")?;
        hashes.insert(selected.clone(), digest(&data));
        let text = std::str::from_utf8(&data).map_err(|_| Fault("codeql_rust_source_text"))?;
        for relative in literal_includes(text)? {
            edges += 1;
            require(edges <= MAX_EDGES, "codeql_rust_include_edges")?;
            let dependency = include_path(root, &selected, &relative)?;
            let mut above = ancestors.clone();
            above.insert(selected.clone());
            pending.push((dependency, depth + 1, above));
        }
    }
    hashes.remove(name);
    Ok(hashes)
}

/// The path that `relative` names from the file `selected`, its parent
/// steps taken without leaving the checkout `root`, and with no step that
/// is a link.
pub fn include_path(root: &Path, selected: &str, relative: &str) -> Sourced<String> {
    let mut parts: Vec<&str> = selected
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    parts.pop();
    for part in relative.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                require(!parts.is_empty(), "codeql_rust_include_escape")?;
                parts.pop();
            }
            _ => {
                parts.push(part);
                let path = parts
                    .iter()
                    .fold(root.to_path_buf(), |path, part| path.join(part));
                require(!path.is_symlink(), "codeql_source_symlink")?;
            }
        }
    }
    require(!parts.is_empty(), "codeql_rust_include_escape")?;
    Ok(parts.join("/"))
}

#[cfg(test)]
#[path = "rust_sources_tests.rs"]
mod tests;
