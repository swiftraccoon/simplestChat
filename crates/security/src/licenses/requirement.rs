//! One line of a hash-locked requirement file: an exact pin of a named
//! version, with the SHA-256 of every artifact that may satisfy it and, at
//! most, an environment marker.
//!
//! Only that form is read. The reference parsed every requirement its
//! library knows and then refused what was no exact pin; here whatever is no
//! plain `name==version` is refused in one step, under the same code.

use super::version::normal;
use crate::{Fault, Result, policy::space, record::quoted, require};
use std::collections::BTreeSet;

const HASH: &str = "--hash=sha256:";
const INEXACT: Fault = Fault("dependency_python_exact_version");

/// A pin as it was declared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pin {
    /// The name in its canonical spelling.
    pub name: String,
    /// The version as it is written.
    pub version: String,
    /// The requirement in its one spelling and the sorted hashes: what makes
    /// two declarations the same.
    pub identity: String,
    /// The SHA-256 of each artifact.
    pub hashes: BTreeSet<String>,
}

/// A name in its canonical spelling: lower case, each run of separators one
/// hyphen.
pub(crate) fn canonical(name: &str) -> String {
    let mut spelled = String::with_capacity(name.len());
    let mut separated = false;
    for character in name.chars() {
        if matches!(character, '-' | '_' | '.') {
            separated = true;
        } else {
            if std::mem::take(&mut separated) {
                spelled.push('-');
            }
            spelled.extend(character.to_lowercase());
        }
    }
    if separated {
        spelled.push('-');
    }
    spelled
}

/// The names a marker may ask about, as they are spelled today.
fn variable(name: &str) -> Option<&'static str> {
    const NAMES: [&str; 14] = [
        "python_version",
        "python_full_version",
        "os_name",
        "sys_platform",
        "platform_release",
        "platform_system",
        "platform_version",
        "platform_machine",
        "platform_python_implementation",
        "implementation_name",
        "implementation_version",
        "extra",
        "extras",
        "dependency_groups",
    ];
    let spelled = match name {
        "python_implementation" | "platform.python_implementation" => {
            "platform_python_implementation"
        }
        "os.name" => "os_name",
        "sys.platform" => "sys_platform",
        "platform.version" => "platform_version",
        "platform.machine" => "platform_machine",
        other => other,
    };
    NAMES.into_iter().find(|known| *known == spelled)
}

/// One side of a comparison: a name, or a value with its quotes taken off.
enum Side {
    Name(&'static str),
    Value(String),
}

impl Side {
    fn spelled(&self) -> String {
        match self {
            Self::Name(name) => (*name).to_owned(),
            Self::Value(value) if value.contains('"') => format!("'{value}'"),
            Self::Value(value) => format!("\"{value}\""),
        }
    }
}

/// A marker as it was written: comparisons, the words that join them, and
/// groups in parentheses.
enum Node {
    Compared(String),
    Joined(&'static str),
    Group(Vec<Node>),
}

/// The one spelling of a list of nodes, as the reference's library writes
/// it: a list that is one comparison or one group is spelled as that member
/// is, and any other list is in parentheses unless it is the outermost.
fn spelled(list: &[Node], outermost: bool) -> String {
    match list {
        [Node::Compared(comparison)] => return comparison.clone(),
        [Node::Group(inner)] => return spelled(inner, outermost),
        _ => {}
    }
    let members: Vec<String> = list
        .iter()
        .map(|node| match node {
            Node::Compared(comparison) => comparison.clone(),
            Node::Joined(word) => (*word).to_owned(),
            Node::Group(inner) => spelled(inner, false),
        })
        .collect();
    let joined = members.join(" ");
    if outermost {
        joined
    } else {
        format!("({joined})")
    }
}

/// A marker being read from its text.
struct Marker<'a>(&'a str);

impl Marker<'_> {
    fn blank(&mut self) {
        self.0 = self.0.trim_start_matches([' ', '\t']);
    }

    /// Take `word` when it stands alone at the start.
    fn word(&mut self, word: &str) -> bool {
        let after = self.0.strip_prefix(word);
        let alone = after.is_some_and(|after| !after.starts_with(char::is_alphanumeric));
        if alone {
            self.0 = &self.0[word.len()..];
        }
        alone
    }

    fn side(&mut self) -> Option<Side> {
        let quote = self
            .0
            .chars()
            .next()
            .filter(|quote| matches!(quote, '\'' | '"'));
        if let Some(quote) = quote {
            let end = self.0[1..].find(quote)?;
            let value = self.0[1..=end].to_owned();
            self.0 = &self.0[end + 2..];
            return Some(Side::Value(value));
        }
        let named = |character: char| character.is_ascii_alphanumeric() || "._".contains(character);
        let length = self
            .0
            .find(|character| !named(character))
            .unwrap_or(self.0.len());
        let name = variable(&self.0[..length])?;
        self.0 = &self.0[length..];
        Some(Side::Name(name))
    }

    fn operator(&mut self) -> Option<&'static str> {
        if self.word("in") {
            return Some("in");
        }
        if self.word("not") {
            self.blank();
            return self.word("in").then_some("not in");
        }
        let found = ["===", "==", "~=", "!=", "<=", ">=", "<", ">"]
            .into_iter()
            .find(|operator| self.0.starts_with(operator))?;
        self.0 = &self.0[found.len()..];
        Some(found)
    }

    /// A group in parentheses or one comparison. The value compared with
    /// `extra` is a name and is spelled canonically.
    fn atom(&mut self) -> Option<Node> {
        self.blank();
        if let Some(inside) = self.0.strip_prefix('(') {
            self.0 = inside;
            let inner = self.list()?;
            self.blank();
            self.0 = self.0.strip_prefix(')')?;
            return Some(Node::Group(inner));
        }
        let mut left = self.side()?;
        self.blank();
        let operator = self.operator()?;
        self.blank();
        let mut right = self.side()?;
        match (&mut left, &mut right) {
            (Side::Name("extra"), Side::Value(value))
            | (Side::Value(value), Side::Name("extra")) => *value = canonical(value),
            _ => {}
        }
        let comparison = format!("{} {operator} {}", left.spelled(), right.spelled());
        Some(Node::Compared(comparison))
    }

    /// Atoms joined by `and` and `or`.
    fn list(&mut self) -> Option<Vec<Node>> {
        let mut list = vec![self.atom()?];
        loop {
            self.blank();
            let Some(word) = ["and", "or"].into_iter().find(|word| self.word(word)) else {
                return Some(list);
            };
            list.push(Node::Joined(word));
            list.push(self.atom()?);
        }
    }
}

/// A marker in its one spelling.
fn marker(text: &str) -> Option<String> {
    let mut marker = Marker(text);
    let list = marker.list()?;
    marker.blank();
    marker.0.is_empty().then(|| spelled(&list, true))
}

/// The pin on `line`: `name==version`, a marker or none, and after it one
/// `--hash=sha256:` and sixty-four lower-case hexadecimal digits for each
/// artifact.
pub(crate) fn pin(line: &str) -> Result<Pin> {
    let pieces = split(line);
    let digest = |text: &&str| {
        let text = text.trim_matches(space);
        text.len() == 64
            && text
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    };
    require(
        pieces.len() > 1 && pieces[1..].iter().all(digest),
        "dependency_python_hash",
    )?;
    let head = pieces[0].trim_matches(space);
    let named = |byte: u8| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.');
    let length = head.bytes().take_while(|byte| named(*byte)).count();
    let name = &head[..length];
    let ends = |byte: Option<u8>| byte.is_some_and(|byte| byte.is_ascii_alphanumeric());
    require(
        ends(name.bytes().next()) && ends(name.bytes().last()),
        INEXACT.0,
    )?;
    let after = head[length..].trim_start_matches([' ', '\t']);
    let after = after.strip_prefix("==").ok_or(INEXACT)?;
    let after = after.trim_start_matches([' ', '\t']);
    let stop = |character: char| matches!(character, ' ' | '\t' | ';');
    let length = after.find(stop).unwrap_or(after.len());
    let (written, after) = after.split_at(length);
    require(normal(written).is_some(), INEXACT.0)?;
    let after = after.trim_start_matches([' ', '\t']);
    let mut requirement = format!("{name}=={written}");
    if let Some(conditions) = after.strip_prefix(';') {
        requirement.push_str("; ");
        requirement.push_str(&marker(conditions).ok_or(INEXACT)?);
    } else {
        require(after.is_empty(), INEXACT.0)?;
    }
    let hashes: BTreeSet<String> = pieces[1..]
        .iter()
        .map(|piece| piece.trim_matches(space).to_owned())
        .collect();
    let listed: Vec<String> = hashes.iter().map(|hash| quoted(hash)).collect();
    Ok(Pin {
        name: canonical(name),
        version: written.to_owned(),
        identity: format!("[{},[{}]]", quoted(&requirement), listed.join(",")),
        hashes,
    })
}

/// `line` cut at every `--hash=sha256:` that has space before it; the space
/// belongs to neither side.
fn split(line: &str) -> Vec<&str> {
    let mut pieces = Vec::new();
    let (mut start, mut from) = (0, 0);
    while let Some(found) = line[from..].find(HASH) {
        let at = from + found;
        let before = line[start..at].trim_end_matches(space);
        if before.len() < at - start {
            pieces.push(before);
            start = at + HASH.len();
        }
        from = at + HASH.len();
    }
    pieces.push(&line[start..]);
    pieces
}
