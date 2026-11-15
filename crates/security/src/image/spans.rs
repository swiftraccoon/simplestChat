//! Where a finding of the secret scanner lies in a projection, proven
//! without publishing what lies there. The scanner reports lines and
//! columns of the fragments it cut a file into, not of the file's own
//! lines, so the cutting is repeated here as the pinned scanner does it.
//! A region that the coordinates name once is hashed. One that they name
//! twice, on a long line that was cut, is ambiguous, and one that they do
//! not name is unresolved: nothing is guessed, and only a resolved region
//! can carry an exact review.

use crate::{Fault, Result, document::Document, hexadecimal, require, secrets};
use sha2::{Digest as _, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::Read,
    os::unix::fs::OpenOptionsExt,
    path::Path,
    time::{Duration, Instant},
};

/// The version of the scanner whose cutting this repeats.
pub const VERSION: &str = "8.30.1";
/// The name of what the evidence holds.
pub const FORMAT: &str = "gitleaks-8.30.1-projection-match";
/// How many bytes the scanner reads at once.
const BUFFER: usize = 100_000;
/// How far it reads on for the end of a paragraph.
const PEEK: usize = 25_000;
/// How many bytes its reader holds while it reads on byte by byte.
const READER: usize = 4096;
const COORDINATES: [&str; 4] = ["StartLine", "EndLine", "StartColumn", "EndColumn"];
/// The report is no list of records, or a finding lacks a field.
const REPORT: Fault = Fault("secret_span_report");
/// The map of the projections lacks what a finding's file must have.
const PATHS: Fault = Fault("secret_span_paths");

/// The bounds of a collection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The most bytes a projection, and a coordinate, may have.
    pub file: u64,
    /// The most bytes that may be read in all.
    pub total: u64,
    /// The most findings.
    pub findings: usize,
    /// The most regions that may be tried.
    pub candidates: usize,
    /// The seconds the collection may take; fewer than none is no time.
    pub seconds: i64,
}

impl Limits {
    /// The reviewed bounds.
    pub const REVIEWED: Self = Self {
        file: 256 * 1024 * 1024 + secrets::PREFIX_BYTES,
        total: 4 * 1024 * 1024 * 1024,
        findings: 20_000,
        candidates: 100_000,
        seconds: 90,
    };
}

/// A file as the pinned scanner's buffered reader hands it out: whole
/// blocks, then single bytes out of a small buffer of its own, and what is
/// left of that buffer as the next block.
struct Reader<R> {
    source: R,
    pending: Vec<u8>,
    position: usize,
}

impl<R: Read> Reader<R> {
    /// As many bytes as `most`, fewer only where the file ends.
    fn filled(&mut self, most: usize) -> std::io::Result<Vec<u8>> {
        let mut data = Vec::new();
        (&mut self.source)
            .take(most as u64)
            .read_to_end(&mut data)?;
        Ok(data)
    }

    fn block(&mut self) -> std::io::Result<Vec<u8>> {
        if self.position < self.pending.len() {
            let data = self.pending.split_off(self.position);
            self.position = self.pending.len();
            return Ok(data);
        }
        self.filled(BUFFER)
    }

    fn byte(&mut self) -> std::io::Result<Option<u8>> {
        if self.position == self.pending.len() {
            self.pending = self.filled(READER)?;
            self.position = 0;
        }
        let byte = self.pending.get(self.position).copied();
        self.position += usize::from(byte.is_some());
        Ok(byte)
    }
}

fn space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n')
}

/// The next fragment of the file: a block, read on byte by byte, unless
/// it ends a paragraph already, until two line ends with nothing but
/// white space between them, the longest look ahead or the file's end.
fn fragment<R: Read>(reader: &mut Reader<R>) -> std::io::Result<Option<Vec<u8>>> {
    let mut part = reader.block()?;
    let read = part.len();
    let trailing = part.iter().rev().take_while(|byte| space(**byte));
    if trailing.filter(|byte| **byte == b'\n').count() >= 2 {
        return Ok(Some(part));
    }
    let mut newlines = 0;
    while let Some(last) = part.last().copied() {
        if last == b'\n' {
            newlines += 1;
        } else if !space(last) {
            newlines = 0;
        }
        if newlines >= 2 || part.len() - read >= PEEK {
            break;
        }
        let Some(byte) = reader.byte()? else { break };
        part.push(byte);
    }
    Ok((!part.is_empty()).then_some(part))
}

/// The bytes that coordinates name in a fragment whose line ends are at
/// `lines`: the first and the last line counted from the fragment's own
/// first, and the columns as the scanner counts them, the line end before
/// a line as its first column. Coordinates that do not lead back to
/// themselves name nothing.
fn region<'a>(data: &'a [u8], lines: &[usize], coordinates: [i64; 4]) -> Option<&'a [u8]> {
    let [first, last, column, end_column] = coordinates;
    // A line before the fragment and a column of nothing do not lead back
    // to themselves below; only the lines that are looked up are held here.
    if !(first <= last && last <= lines.len() as i64) {
        return None;
    }
    let before = |line: i64| match usize::try_from(line - 1) {
        Ok(index) => lines[index] as i64,
        Err(_) => 0,
    };
    let (start, end) = (before(first) + column - 1, before(last) + end_column);
    if !(start < end && end <= data.len() as i64) {
        return None;
    }
    let (start, end) = (usize::try_from(start).ok()?, usize::try_from(end).ok()?);
    // A match that ends after the last line end may be reported with an
    // end column of nothing: that is not guessed.
    let actual_first = lines.partition_point(|at| *at <= start) as i64;
    let actual_last = lines.partition_point(|at| *at < end) as i64;
    (actual_first == first && actual_last == last).then(|| &data[start..end])
}

/// One finding: where the scanner says it lies, what the evidence says of
/// it, and at most one region that its coordinates name, by size and
/// digest alone.
struct Finding {
    coordinates: [i64; 4],
    evidence: Vec<(String, Document)>,
    candidate: Option<(usize, String)>,
    ambiguous: bool,
}

/// What a collection may still spend.
struct Budget<'a> {
    limits: &'a Limits,
    deadline: Option<Instant>,
    candidates: usize,
    bytes: u64,
}

impl Budget<'_> {
    fn check(&self) -> Result<()> {
        let timely = self
            .deadline
            .is_some_and(|deadline| Instant::now() <= deadline);
        require(timely, "secret_span_deadline")?;
        let tried = self.candidates <= self.limits.candidates;
        require(tried, "secret_span_candidate_budget")?;
        require(self.bytes <= self.limits.total, "secret_span_read_budget")
    }
}

fn small_hexadecimal(text: &str) -> bool {
    let digit = |byte: u8| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte);
    text.len() == 64 && text.bytes().all(digit)
}

/// Read the whole projection at `path`, which must be the file that the
/// map of projections describes, and give every finding of it the regions
/// that its coordinates name.
fn inspect(
    path: &Path,
    expected: &Document,
    findings: &mut [&mut Finding],
    budget: &mut Budget<'_>,
) -> Result<()> {
    const CHANGED: Fault = Fault("secret_span_file_changed");
    const KIND: Fault = Fault("secret_span_file_kind");
    let size = expected.get("projectionBytes").and_then(Document::whole);
    let size = size.and_then(|size| u64::try_from(size).ok());
    let size = size.filter(|size| (1..=budget.limits.file).contains(size));
    let size = size.ok_or(Fault("secret_span_file_size"))?;
    let checksum = expected.get("projectionSha256").and_then(Document::text);
    let checksum = checksum.filter(|checksum| small_hexadecimal(checksum));
    let checksum = checksum.ok_or(Fault("secret_span_file_digest"))?;
    let mut options = OpenOptions::new();
    let flags = rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK;
    options.read(true).custom_flags(flags.bits() as i32);
    let source: File = options.open(path).map_err(|_| KIND)?;
    let before = source.metadata().map_err(|_| KIND)?;
    require(before.is_file() && before.len() == size, KIND.0)?;
    findings.sort_by_key(|finding| finding.coordinates[0]);
    let mut reader = Reader {
        source: &source,
        pending: Vec::new(),
        position: 0,
    };
    let (mut digest, mut total, mut line) = (Sha256::new(), 0_u64, 1_i64);
    while let Some(data) = fragment(&mut reader).map_err(|_| CHANGED)? {
        total += data.len() as u64;
        budget.bytes += data.len() as u64;
        budget.check()?;
        require(total <= size, CHANGED.0)?;
        digest.update(&data);
        let lines: Vec<usize> = (0..data.len()).filter(|at| data[*at] == b'\n').collect();
        let through = line + lines.len() as i64;
        let from = findings.partition_point(|finding| finding.coordinates[0] < line);
        let to = findings.partition_point(|finding| finding.coordinates[0] <= through);
        for finding in &mut findings[from..to] {
            budget.candidates += 1;
            budget.check()?;
            let [first, last, column, end_column] = finding.coordinates;
            let relative = [first - line, last - line, column, end_column];
            if let Some(found) = region(&data, &lines, relative) {
                // Two regions stay two although they may hold the same.
                finding.ambiguous |= finding.candidate.is_some();
                let seen = (found.len(), hexadecimal(&Sha256::digest(found)));
                finding.candidate.get_or_insert(seen);
            }
        }
        line = through;
    }
    let after = source.metadata().map_err(|_| CHANGED)?;
    let named = path.symlink_metadata().map_err(|_| CHANGED)?;
    let same = secrets::identity(&before) == secrets::identity(&after)
        && secrets::identity(&before) == secrets::identity(&named);
    let whole = total == size && hexadecimal(&digest.finalize()) == checksum;
    require(whole && same, CHANGED.0)
}

/// The evidence of where every finding of the scanner's `report` lies:
/// `paths` maps the neutral names of the projections in `directory` to
/// what they are projections of, and `version` is the scanner that made
/// the report. The findings keep their order.
pub fn collect(
    report: &Document,
    paths: &Document,
    directory: &Path,
    version: &str,
    limits: &Limits,
) -> Result<Document> {
    require(version == VERSION, "secret_span_scanner_version")?;
    let rows = report.items().ok_or(REPORT)?;
    require(rows.len() <= limits.findings, "secret_span_finding_count")?;
    let mut findings = Vec::new();
    let mut files: Vec<(&str, &Document)> = Vec::new();
    let mut placed = Vec::new();
    for row in rows {
        let name = row.get("File").and_then(Document::text).ok_or(REPORT)?;
        let name = name.strip_prefix("/layers/").unwrap_or(name);
        let numbered = name.strip_prefix("content-").is_some_and(|number| {
            number.len() == 6 && number.bytes().all(|byte| byte.is_ascii_digit())
        });
        let listed = paths.get(name).filter(|_| numbered);
        let expected = listed.ok_or(Fault("secret_span_file"))?;
        require(expected.fields().is_some(), PATHS.0)?;
        let format = expected.get("projectionFormat").and_then(Document::text);
        require(format == Some(secrets::FORMAT), "secret_span_format")?;
        let mut coordinates = [0_i64; 4];
        for (coordinate, key) in coordinates.iter_mut().zip(COORDINATES) {
            let number = row.get(key).ok_or(REPORT)?.whole();
            let number = number.filter(|number| (0..=i128::from(limits.file)).contains(number));
            let number = number.and_then(|number| i64::try_from(number).ok());
            *coordinate = number.ok_or(Fault("secret_span_coordinates"))?;
        }
        let [first, last, column, end_column] = coordinates.map(|number| number as u64);
        let held = |name: &str| expected.get(name).cloned().ok_or(PATHS);
        let evidence = vec![
            ("rule".to_owned(), row.get("RuleID").cloned().ok_or(REPORT)?),
            ("path".to_owned(), held("path")?),
            ("fileSha256".to_owned(), held("sha256")?),
            ("projectionSha256".to_owned(), held("projectionSha256")?),
            ("startLine".to_owned(), first.into()),
            ("endLine".to_owned(), last.into()),
            ("startColumn".to_owned(), column.into()),
            ("endColumn".to_owned(), end_column.into()),
        ];
        let file = files.iter().position(|(held, _)| *held == name);
        placed.push(file.unwrap_or(files.len()));
        if file.is_none() {
            files.push((name, expected));
        }
        findings.push(Finding {
            coordinates,
            evidence,
            candidate: None,
            ambiguous: false,
        });
    }
    let seconds = u64::try_from(limits.seconds).ok().map(Duration::from_secs);
    let deadline = seconds.and_then(|seconds| Instant::now().checked_add(seconds));
    let mut budget = Budget {
        limits,
        deadline,
        candidates: 0,
        bytes: 0,
    };
    let held = directory.symlink_metadata().is_ok_and(|held| held.is_dir());
    require(rows.is_empty() || held, "secret_span_directory")?;
    for (index, (name, expected)) in files.iter().enumerate() {
        let of_file = findings.iter_mut().zip(&placed);
        let mut of_file: Vec<&mut Finding> = of_file
            .filter(|(_, file)| **file == index)
            .map(|(finding, _)| finding)
            .collect();
        inspect(&directory.join(name), expected, &mut of_file, &mut budget)?;
    }
    let results = findings.into_iter().map(|finding| {
        let mut entry = finding.evidence;
        match finding.candidate.filter(|_| !finding.ambiguous) {
            Some((size, digest)) => {
                entry.push(("status".to_owned(), "resolved".into()));
                entry.push(("spanBytes".to_owned(), size.into()));
                entry.push(("spanSha256".to_owned(), digest.into()));
            }
            None => {
                let status = if finding.ambiguous {
                    "ambiguous"
                } else {
                    "unresolved"
                };
                entry.push(("status".to_owned(), status.into()));
            }
        }
        Document::Record(entry)
    });
    Ok(Document::record([
        ("format", FORMAT.into()),
        ("findings", Document::List(results.collect())),
    ]))
}

#[cfg(test)]
#[path = "spans_tests.rs"]
mod tests;
