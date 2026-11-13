//! The file names of the artifacts of a Python package.
//!
//! A wheel is named `name-version-interpreter-abi-platform.whl`, with a
//! build tag after the version when it has one, and a source distribution
//! `name-version.tar.gz` or `.zip`. A file name is read into the canonical
//! name and the normal version it is of, or it names no artifact.

use super::{requirement::canonical, version::normal};
use crate::policy::space;

/// Whether `word` is written as the tag of an interpreter is: as a name in
/// a program.
fn identifier(word: &str) -> bool {
    let mut bytes = word.bytes();
    let starts = bytes
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_');
    starts && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// The canonical name and the normal version that the wheel `filename` is
/// of. The name is letters, digits, points and single underscores, a build
/// tag starts with a digit, and no part of a tag is empty.
pub(crate) fn wheel(filename: &str) -> Option<(String, String)> {
    let stem = filename.strip_suffix(".whl")?;
    let hyphens = stem.matches('-').count();
    if !matches!(hyphens, 4 | 5) {
        return None;
    }
    let mut parts = stem.splitn(hyphens - 1, '-');
    let (name, version) = (parts.next()?, parts.next()?);
    let lettered = |byte: u8| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.');
    let named = !name.is_empty() && !name.contains("__") && name.bytes().all(lettered);
    let built = hyphens == 4
        || parts
            .next()?
            .starts_with(|first: char| first.is_ascii_digit());
    let mut tags = parts.next()?.split('-').map(|tag| tag.split('.'));
    let interpreted = tags.next()?.all(identifier);
    let whole = tags.all(|mut tag| tag.all(|part| !part.is_empty()));
    if !(named && built && interpreted && whole) {
        return None;
    }
    Some((canonical(name), normal(version.trim_matches(space))?))
}

/// The canonical name and the normal version that the source distribution
/// `filename` is of: the version is what follows the last hyphen.
pub(crate) fn sdist(filename: &str) -> Option<(String, String)> {
    let stem = filename
        .strip_suffix(".tar.gz")
        .or_else(|| filename.strip_suffix(".zip"))?;
    let (name, version) = stem.rsplit_once('-')?;
    if name.is_empty() {
        return None;
    }
    Some((canonical(name), normal(version.trim_matches(space))?))
}
