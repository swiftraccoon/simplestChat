//! The difference of two versions of a vendored file, as evidence.
//!
//! Text is compared line by line and written as a unified difference with
//! three lines of context. The lines, the blocks that are found to match and
//! the grouping are the reference's, which used Python's `difflib`, so that
//! the evidence of a deviation reads the same whichever made it. Anything
//! that is no UTF-8 text without a zero byte is kept whole instead, both
//! sides in base64 beside their digests.

use crate::digest;
use std::collections::HashMap;

/// How many lines around a change are shown.
const CONTEXT: usize = 3;
/// From how many lines on a line that makes up more than a hundredth of
/// them is too common to start a match from.
const POPULAR_FROM: usize = 200;

/// Whether a line ends with `character` for the reference's interpreter.
pub(crate) fn ends(character: char) -> bool {
    matches!(
        character,
        '\n' | '\r'
            | '\u{b}'
            | '\u{c}'
            | '\u{1c}'
            | '\u{1d}'
            | '\u{1e}'
            | '\u{85}'
            | '\u{2028}'
            | '\u{2029}'
    )
}

/// The lines of a text with their ends, split where the reference's
/// interpreter splits lines: after a line feed, a carriage return (with the
/// line feed that follows it), a vertical tab, a form feed, the three
/// information separators, a next-line character and the separators of
/// lines and of paragraphs.
pub(crate) fn lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut characters = text.char_indices().peekable();
    while let Some((at, character)) = characters.next() {
        if !ends(character) {
            continue;
        }
        let mut end = at + character.len_utf8();
        if character == '\r' && characters.next_if(|(_, next)| *next == '\n').is_some() {
            end += 1;
        }
        lines.push(&text[start..end]);
        start = end;
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// A run of lines that both versions have: where it begins in each and how
/// long it is.
type Block = (usize, usize, usize);

/// What matches between the old lines and the new ones.
struct Matcher<'a> {
    old: &'a [&'a str],
    new: &'a [&'a str],
    /// Where each line occurs among the new ones, unless it is too common.
    places: HashMap<&'a str, Vec<usize>>,
}

impl<'a> Matcher<'a> {
    fn new(old: &'a [&'a str], new: &'a [&'a str]) -> Self {
        let mut places: HashMap<&str, Vec<usize>> = HashMap::new();
        for (at, line) in new.iter().enumerate() {
            places.entry(line).or_default().push(at);
        }
        if new.len() >= POPULAR_FROM {
            let most = new.len() / 100 + 1;
            places.retain(|_, found| found.len() <= most);
        }
        Self { old, new, places }
    }

    /// The longest run that the two ranges share: the first of the longest
    /// that begins at a line that is not too common, grown over every equal
    /// line before and after it.
    fn longest(&self, old: (usize, usize), new: (usize, usize)) -> Block {
        let (mut first, mut second, mut size) = (old.0, new.0, 0);
        let mut lengths: HashMap<usize, usize> = HashMap::new();
        for at in old.0..old.1 {
            let mut next = HashMap::new();
            for place in self.places.get(self.old[at]).map_or(&[][..], Vec::as_slice) {
                if *place < new.0 {
                    continue;
                }
                if *place >= new.1 {
                    break;
                }
                let before = place.checked_sub(1).and_then(|before| lengths.get(&before));
                let length = before.copied().unwrap_or(0) + 1;
                next.insert(*place, length);
                if length > size {
                    (first, second, size) = (at + 1 - length, place + 1 - length, length);
                }
            }
            lengths = next;
        }
        while first > old.0 && second > new.0 && self.old[first - 1] == self.new[second - 1] {
            (first, second, size) = (first - 1, second - 1, size + 1);
        }
        while first + size < old.1
            && second + size < new.1
            && self.old[first + size] == self.new[second + size]
        {
            size += 1;
        }
        (first, second, size)
    }

    /// Every run both versions share, in order, with neighbours joined, and
    /// an empty one at the end.
    fn blocks(&self) -> Vec<Block> {
        let mut waiting = vec![(0, self.old.len(), 0, self.new.len())];
        let mut found = Vec::new();
        while let Some((old_low, old_high, new_low, new_high)) = waiting.pop() {
            let (first, second, size) = self.longest((old_low, old_high), (new_low, new_high));
            if size == 0 {
                continue;
            }
            found.push((first, second, size));
            if old_low < first && new_low < second {
                waiting.push((old_low, first, new_low, second));
            }
            if first + size < old_high && second + size < new_high {
                waiting.push((first + size, old_high, second + size, new_high));
            }
        }
        found.sort_unstable();
        let mut joined: Vec<Block> = Vec::new();
        for (first, second, size) in found {
            match joined.last_mut() {
                Some(last) if last.0 + last.2 == first && last.1 + last.2 == second => {
                    last.2 += size
                }
                _ => joined.push((first, second, size)),
            }
        }
        joined.push((self.old.len(), self.new.len(), 0));
        joined
    }
}

/// One step from the old version to the new: whether the lines stay, and
/// the ranges of the old and of the new lines.
type Step = (bool, usize, usize, usize, usize);

/// The steps from the old version to the new one.
fn steps(blocks: &[Block]) -> Vec<Step> {
    let mut steps = Vec::new();
    let (mut old, mut new) = (0, 0);
    for (first, second, size) in blocks {
        if old < *first || new < *second {
            steps.push((false, old, *first, new, *second));
        }
        (old, new) = (first + size, second + size);
        if *size > 0 {
            steps.push((true, *first, old, *second, new));
        }
    }
    steps
}

/// The steps in groups, each a change with its context: unchanged lines
/// further than the context from every change are left out, and a run of
/// more than twice the context ends a group.
fn groups(mut steps: Vec<Step>) -> Vec<Vec<Step>> {
    if steps.is_empty() {
        steps.push((true, 0, 1, 0, 1));
    }
    if let Some((true, old, old_end, new, new_end)) = steps.first_mut() {
        (*old, *new) = (
            (*old).max(old_end.saturating_sub(CONTEXT)),
            (*new).max(new_end.saturating_sub(CONTEXT)),
        );
    }
    if let Some((true, old, old_end, new, new_end)) = steps.last_mut() {
        (*old_end, *new_end) = (
            (*old_end).min(*old + CONTEXT),
            (*new_end).min(*new + CONTEXT),
        );
    }
    let mut groups = Vec::new();
    let mut group: Vec<Step> = Vec::new();
    for (stays, mut old, old_end, mut new, new_end) in steps {
        if stays && old_end - old > 2 * CONTEXT {
            group.push((
                true,
                old,
                old_end.min(old + CONTEXT),
                new,
                new_end.min(new + CONTEXT),
            ));
            groups.push(std::mem::take(&mut group));
            (old, new) = (
                old.max(old_end.saturating_sub(CONTEXT)),
                new.max(new_end.saturating_sub(CONTEXT)),
            );
        }
        group.push((stays, old, old_end, new, new_end));
    }
    if !matches!(group[..], [] | [(true, ..)]) {
        groups.push(group);
    }
    groups
}

/// A range of lines as a unified difference names it.
fn range(start: usize, end: usize) -> String {
    match end - start {
        1 => (start + 1).to_string(),
        0 => format!("{start},0"),
        length => format!("{},{length}", start + 1),
    }
}

/// The unified difference of two texts: its lines, each with whatever end
/// the line it shows has.
fn unified(old: &str, new: &str, from: &str, to: &str) -> Vec<String> {
    let (old, new) = (lines(old), lines(new));
    let mut written = Vec::new();
    for group in groups(steps(&Matcher::new(&old, &new).blocks())) {
        if written.is_empty() {
            written.push(format!("--- {from}\n"));
            written.push(format!("+++ {to}\n"));
        }
        let (Some(first), Some(last)) = (group.first(), group.last()) else {
            continue;
        };
        written.push(format!(
            "@@ -{} +{} @@\n",
            range(first.1, last.2),
            range(first.3, last.4)
        ));
        for (stays, old_start, old_end, new_start, new_end) in group {
            if stays {
                written.extend(
                    old[old_start..old_end]
                        .iter()
                        .map(|line| format!(" {line}")),
                );
                continue;
            }
            written.extend(
                old[old_start..old_end]
                    .iter()
                    .map(|line| format!("-{line}")),
            );
            written.extend(
                new[new_start..new_end]
                    .iter()
                    .map(|line| format!("+{line}")),
            );
        }
    }
    written
}

/// Standard base64 with padding.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let group = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let sextets = [
            group[0] >> 2,
            (group[0] & 0x03) << 4 | group[1] >> 4,
            (group[1] & 0x0f) << 2 | group[2] >> 6,
            group[2] & 0x3f,
        ];
        for (place, sextet) in sextets.iter().enumerate() {
            if place <= chunk.len() {
                text.push(char::from(ALPHABET[usize::from(*sextet)]));
            } else {
                text.push('=');
            }
        }
    }
    text
}

/// The whole difference between two versions of the file at `path`, where
/// a version that is not there is nothing: a unified difference of text, in
/// which a shown line without a line feed at its end is marked as git marks
/// it, or both versions whole when one is no text.
pub(crate) fn rendered(path: &str, old: Option<&[u8]>, new: Option<&[u8]>) -> Vec<u8> {
    let (before, after) = (old.unwrap_or_default(), new.unwrap_or_default());
    let mut written = format!("diff --git a/{path} b/{path}\n");
    if old.is_none() {
        written.push_str("new file\n");
    }
    if new.is_none() {
        written.push_str("deleted file\n");
    }
    let from = old.map_or_else(|| "/dev/null".to_owned(), |_| format!("a/{path}"));
    let to = new.map_or_else(|| "/dev/null".to_owned(), |_| format!("b/{path}"));
    let text = |bytes| {
        std::str::from_utf8(bytes)
            .ok()
            .filter(|text: &&str| !text.contains('\0'))
    };
    let (Some(old), Some(new)) = (text(before), text(after)) else {
        written.push_str(&format!("--- {from}\n+++ {to}\n"));
        for (side, bytes) in [("old", before), ("new", after)] {
            let whole = base64(bytes);
            written.push_str(&format!(
                "Binary {side} sha256={} base64={whole}\n",
                digest(bytes)
            ));
        }
        return written.into_bytes();
    };
    for line in unified(old, new, &from, &to) {
        written.push_str(&line);
        if !line.ends_with('\n') {
            written.push_str("\n\\ No newline at end of file\n");
        }
    }
    written.into_bytes()
}

#[cfg(test)]
#[path = "diff_tests.rs"]
mod tests;
