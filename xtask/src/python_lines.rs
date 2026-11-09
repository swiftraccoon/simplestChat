//! Per-file line ratchet for the repository's Python.
//!
//! New code is written in Rust and the remaining Python is being translated,
//! so tracked Python may shrink but never grow. The allowance is the exact
//! inventory of tracked `*.py` and `*.pyi` outside `vendor/`, one line count
//! per file, its rows unique and sorted by path in byte order with LF endings.
//! The check fails on a tracked file without a row, a row whose file is no
//! longer tracked, a file above its row, or a total above the recorded one.
//! `--update` only removes rows and lowers counts; a renamed file's row is
//! edited by hand.

use anyhow::{Context, Result, anyhow, bail};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command;

const ALLOWANCE_FILE: &str = "xtask/python-allowance.txt";

/// The recorded total and the per-file counts written with it.
struct Allowance {
    total: usize,
    files: BTreeMap<String, usize>,
}

pub(crate) fn run(root: &Path, arguments: &[String]) -> Result<()> {
    ratchet(root, arguments, &mut std::io::stderr())
}

/// `run`, writing each offending path to `diagnostics`.
fn ratchet(root: &Path, arguments: &[String], diagnostics: &mut impl Write) -> Result<()> {
    let update = match arguments {
        [] => false,
        [flag] if flag == "--update" => true,
        _ => bail!("python-lines takes no argument other than --update"),
    };
    let allowance_path = root.join(ALLOWANCE_FILE);
    let allowance = read_allowance(&allowance_path)?;
    let counted = tracked(root)?;
    let total: usize = counted.values().sum();
    // A stale row fails the check; `--update` removes it.
    let offenders = offenders(&counted, &allowance, !update);
    if offenders.is_empty() && total <= allowance.total {
        if update {
            let mut text = format!("total {total}\n");
            for (path, lines) in &counted {
                text.push_str(&format!("{lines}\t{path}\n"));
            }
            fs::write(&allowance_path, text)
                .with_context(|| format!("writing {}", allowance_path.display()))?;
            println!(
                "python-lines: allowance set to {} in {}",
                plural(total, "line"),
                plural(counted.len(), "file")
            );
        } else {
            println!(
                "python-lines: {} in {} within the allowance of {}",
                plural(total, "line"),
                plural(counted.len(), "tracked Python file"),
                allowance.total
            );
        }
        return Ok(());
    }
    for offender in &offenders {
        writeln!(diagnostics, "{offender}")?;
    }
    let summary = format!(
        "{} in {}, {}; the allowance is {}",
        plural(total, "line"),
        plural(counted.len(), "tracked Python file"),
        plural(offenders.len(), "offending path"),
        allowance.total
    );
    if update {
        bail!(
            "{summary}, and `--update` only removes rows and lowers counts. Write new code in \
             Rust, or edit a renamed file's row by hand"
        );
    }
    bail!(
        "{summary}. Write new code in Rust, edit a renamed file's row by hand, or after \
         removing Python run `cargo run -q -p xtask -- python-lines --update`"
    );
}

/// Every tracked `*.py` and `*.pyi` outside `vendor/`, with its line count.
fn tracked(root: &Path) -> Result<BTreeMap<String, usize>> {
    let output = Command::new("git")
        .args(["ls-files", "-z", "--", "*.py", "*.pyi", ":!vendor/"])
        .current_dir(root)
        .output()
        .context("running git ls-files")?;
    if !output.status.success() {
        bail!(
            "git ls-files failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim_end()
        );
    }
    // An unmerged path is listed once per stage; the map counts it once.
    let mut counted = BTreeMap::new();
    for name in output
        .stdout
        .split(|&byte| byte == 0)
        .filter(|name| !name.is_empty())
    {
        let path = std::str::from_utf8(name)
            .ok()
            .filter(|path| !path.contains('\n'))
            .with_context(|| format!("the allowance cannot record {}", name.escape_ascii()))?;
        let bytes = fs::read(root.join(path)).with_context(|| format!("reading {path}"))?;
        counted.insert(path.to_owned(), line_count(&bytes));
    }
    Ok(counted)
}

/// LF-terminated lines, plus one for an unterminated last line.
fn line_count(bytes: &[u8]) -> usize {
    let terminated = bytes.iter().filter(|&&byte| byte == b'\n').count();
    terminated + usize::from(bytes.last().is_some_and(|&byte| byte != b'\n'))
}

/// The parsed allowance. A missing file is an error in both modes: the
/// committed allowance is the only starting point. Lines are read in order and
/// the first malformed one is refused by its number.
fn read_allowance(path: &Path) -> Result<Allowance> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let refuse = |line: usize, cause: &str| anyhow!("{}:{line}: {cause}", path.display());
    let mut total = None;
    let mut files = BTreeMap::new();
    let mut previous: Option<&str> = None;
    for (row, line) in text.split_terminator('\n').zip(1..) {
        if row.ends_with('\r') {
            return Err(refuse(line, "expected LF line endings, found CRLF"));
        }
        if line == 1 {
            let recorded = row
                .strip_prefix("total ")
                .and_then(|total| total.parse().ok());
            total = Some(recorded.ok_or_else(|| refuse(line, "expected `total <lines>`"))?);
            continue;
        }
        let (count, file) = row
            .split_once('\t')
            .and_then(|(count, file)| Some((count.parse().ok()?, file)))
            .ok_or_else(|| refuse(line, "expected `<lines>\\t<path>`"))?;
        if previous.is_some_and(|previous| previous >= file) {
            return Err(refuse(line, "rows must be unique and sorted by path"));
        }
        previous = Some(file);
        files.insert(file.to_owned(), count);
    }
    let total = total.ok_or_else(|| refuse(1, "expected `total <lines>`"))?;
    Ok(Allowance { total, files })
}

/// One line per offending path: each tracked file without a row or above its
/// row, then, when `stale` is set, each row whose file is no longer tracked.
fn offenders(counted: &BTreeMap<String, usize>, allowance: &Allowance, stale: bool) -> Vec<String> {
    let mut offenders: Vec<String> = counted
        .iter()
        .filter_map(|(path, &lines)| match allowance.files.get(path) {
            None => Some(format!(
                "{path}: {}, not in the allowance",
                plural(lines, "line")
            )),
            Some(&recorded) if lines > recorded => Some(format!(
                "{path}: {}, {recorded} recorded",
                plural(lines, "line")
            )),
            Some(_) => None,
        })
        .collect();
    if stale {
        let untracked = allowance
            .files
            .keys()
            .filter(|path| !counted.contains_key(*path));
        offenders.extend(untracked.map(|path| format!("{path}: listed but not tracked")));
    }
    offenders
}

/// `count` followed by `noun`, which takes an `s` unless the count is one.
fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

#[cfg(test)]
#[path = "python_lines_tests.rs"]
mod tests;
