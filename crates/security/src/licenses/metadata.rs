//! The licence that a wheel's core metadata declares.
//!
//! Core metadata is a block of fields in the form of a mail header, ended
//! by a blank line; a description may follow and is never read. The licence
//! is the one `License-Expression` of metadata that names the locked
//! package and version exactly once each. Older free-text fields are never
//! interpreted: a wheel without an expression passes only when its exact
//! metadata and its exact bytes were reviewed.

use super::{Dependency, requirement::canonical};
use crate::{Fault, Result, digest, require};

const FORMAT: Fault = Fault("dependency_python_metadata_format");
const FIELDS: Fault = Fault("dependency_python_metadata_fields");

/// A licence that was reviewed in the texts a wheel carries, for metadata
/// that declares none as an expression.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reviewed<'a> {
    /// The canonical name of the package.
    pub name: &'a str,
    /// Its version.
    pub version: &'a str,
    /// The SHA-256 of the metadata that was read.
    pub metadata: &'a str,
    /// The SHA-256 of the wheel that was read.
    pub wheel: &'a str,
    /// The licence that was found.
    pub license: &'a str,
}

/// The reviews recorded in `security/reviews.md`. Other metadata or another
/// wheel of the same version needs a review of its own.
pub const REVIEWED: [Reviewed<'static>; 3] = [
    Reviewed {
        name: "certifi",
        version: "2026.10.19",
        metadata: "a5c0be3a42daeef0edfaf58e6ea5b7550f3e4e8eb35420f72e902df9bcf00f5c",
        wheel: "42d35340cc5b9ba78e818ddbff0c5b1c2b779de448a340fd1b989f5fa8670e1b",
        license: "MPL-2.0",
    },
    Reviewed {
        name: "basedpyright",
        version: "1.40.2",
        metadata: "913fdc12587d87d82fb487ecc3e5a209dabab7be28982168f86d04c8518b7b8c",
        wheel: "3a53aeb93a86675ded0e12bc27bd8476c1742356de07ad8cab67bff18110c690",
        license: "MIT",
    },
    Reviewed {
        name: "meson",
        version: "1.12.1",
        metadata: "690a6190f8fe2f15822a341150963bcb79615657460e0b8cab755ce1912aeb22",
        wheel: "930bc7542cbd9f57009e182fd014eba48cf1a0180a7b9006d2d32d8f168d8b02",
        license: "Apache-2.0",
    },
];

/// The values of the fields that are read, each as often as it is given.
#[derive(Default)]
struct Fields {
    names: Vec<String>,
    versions: Vec<String>,
    expressions: Vec<String>,
}

/// The first line of `text` without its end, and what follows it. A line
/// ends with a line feed, a carriage return or both.
fn line(text: &[u8]) -> (&[u8], &[u8]) {
    let end = text.iter().position(|byte| matches!(byte, b'\r' | b'\n'));
    let Some(end) = end else {
        return (text, &[]);
    };
    let ended = &text[end..];
    let width = if ended.starts_with(b"\r\n") { 2 } else { 1 };
    (&text[..end], &ended[width..])
}

/// The fields of `content` up to its first blank line. A field is a name
/// in visible ASCII, a colon and a value, which lines that start with a
/// space or a tab continue; no other line is among the fields, so neither
/// is the envelope line of a mailbox. A content type is refused, because
/// its parts would be another document, and so is an encoded word in a
/// value that is read, because it would be another spelling.
fn fields(content: &[u8]) -> Result<Fields> {
    const READ: [&str; 4] = ["name", "version", "license-expression", "content-type"];
    let mut values: Vec<(usize, Vec<u8>)> = Vec::new();
    let mut continued = None;
    let mut rest = content;
    while !rest.is_empty() {
        let (text, after) = line(rest);
        rest = after;
        if text.is_empty() {
            break;
        }
        if text.starts_with(b" ") || text.starts_with(b"\t") {
            // The first line continues nothing.
            let field: Option<usize> = continued.ok_or(FORMAT)?;
            if let Some(field) = field {
                values[field].1.extend(text);
            }
            continue;
        }
        let named = |byte: &u8| byte.is_ascii_graphic() && *byte != b':';
        let length = text.iter().take_while(|byte| named(byte)).count();
        require(length > 0 && text.get(length) == Some(&b':'), FORMAT.0)?;
        let name = &text[..length];
        let read = READ
            .iter()
            .position(|read| name.eq_ignore_ascii_case(read.as_bytes()));
        continued = Some(read.map(|read| {
            values.push((read, text[length + 1..].to_vec()));
            values.len() - 1
        }));
    }
    let mut fields = Fields::default();
    for (read, value) in values {
        let value = String::from_utf8_lossy(&value);
        let value = value.trim_start_matches([' ', '\t']);
        require(!value.contains("=?"), FORMAT.0)?;
        match read {
            0 => fields.names.push(value.to_owned()),
            1 => fields.versions.push(value.to_owned()),
            2 => fields.expressions.push(value.to_owned()),
            _ => return Err(FORMAT),
        }
    }
    Ok(fields)
}

/// The licence expression of the metadata `content` of at most `limit`
/// bytes, which must be that of `dependency`; or, when it declares none,
/// the licence `reviewed` holds for exactly this metadata and one of the
/// locked wheels.
pub(crate) fn metadata_license(
    content: &[u8],
    dependency: &Dependency,
    reviewed: &[Reviewed<'_>],
    limit: u64,
) -> Result<String> {
    require(
        !content.is_empty() && content.len() as u64 <= limit,
        "dependency_python_metadata_size",
    )?;
    let fields = fields(content)?;
    let ([name], [version]) = (fields.names.as_slice(), fields.versions.as_slice()) else {
        return Err(FIELDS);
    };
    require(
        canonical(name) == dependency.name && *version == dependency.version,
        "dependency_python_metadata_identity",
    )?;
    match fields.expressions.as_slice() {
        [expression] => Ok(expression.clone()),
        [] => {
            let read = digest(content);
            let same = |review: &&Reviewed<'_>| {
                review.name == dependency.name
                    && review.version == dependency.version
                    && review.metadata == read
            };
            let review = reviewed.iter().find(same).ok_or(FIELDS)?;
            require(
                dependency.hashes.contains(review.wheel),
                "dependency_python_reviewed_wheel_identity",
            )?;
            Ok(review.license.to_owned())
        }
        _ => Err(FIELDS),
    }
}
