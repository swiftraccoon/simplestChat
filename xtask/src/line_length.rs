//! Over-length line ratchet for first-party Rust.
//!
//! rustfmt wraps every statement it can; a statement it cannot wrap (a string
//! literal longer than the line) is left as is and silently exceeds the
//! width. This check counts such lines in production code and refuses to let
//! the count grow. `--update` records the current count so a cleanup can
//! lower the allowance in the same commit.

use anyhow::{Context, Result, bail};
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) const MAX_WIDTH: usize = 100;
const ALLOWANCE_FILE: &str = "xtask/line-length-allowance.txt";
const ROOTS: [&str; 3] = ["src", "load_tests/src", "xtask/src"];

pub(crate) fn run(root: &Path, arguments: &[String]) -> Result<()> {
    let update = match arguments {
        [] => false,
        [flag] if flag == "--update" => true,
        _ => bail!("line-length takes no argument other than --update"),
    };
    let allowance_path = root.join(ALLOWANCE_FILE);
    let mut offenders = Vec::new();
    for directory in ROOTS {
        collect(&root.join(directory), root, &mut offenders)?;
    }
    offenders.sort();
    let count = offenders.len();
    if update {
        fs::write(&allowance_path, format!("{count}\n"))
            .with_context(|| format!("writing {}", allowance_path.display()))?;
        println!("line-length: allowance set to {count}");
        return Ok(());
    }
    let allowance: usize = fs::read_to_string(&allowance_path)
        .with_context(|| format!("reading {}", allowance_path.display()))?
        .trim()
        .parse()
        .context("the allowance file holds one integer")?;
    if count > allowance {
        for offender in &offenders {
            eprintln!("{offender}");
        }
        bail!(
            "{count} production lines exceed {MAX_WIDTH} columns; the allowance is {allowance}. \
             Move long SQL and messages into multi-line literals, or lower the debt and run \
             `cargo run -q -p xtask -- line-length --update`"
        );
    }
    println!("line-length: {count} over-length lines within the allowance of {allowance}");
    Ok(())
}

/// Production code only: test-only files (`*_tests.rs`) and inline `mod tests`
/// blocks introduced by `#[cfg(test)]` are excluded, like the review metrics.
fn collect(directory: &Path, root: &Path, offenders: &mut Vec<String>) -> Result<()> {
    let mut entries: Vec<PathBuf> = fs::read_dir(directory)
        .with_context(|| format!("reading {}", directory.display()))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<_>>()?;
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect(&path, root, offenders)?;
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.ends_with(".rs") || name.ends_with("_tests.rs") {
            continue;
        }
        let text =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .display()
            .to_string();
        for (index, line) in production_lines(&text) {
            if line.chars().count() > MAX_WIDTH {
                offenders.push(format!(
                    "{relative}:{}: {} columns",
                    index + 1,
                    line.chars().count()
                ));
            }
        }
    }
    Ok(())
}

/// Lines outside `#[cfg(test)] mod … { … }` blocks, with their zero-based index.
fn production_lines(text: &str) -> Vec<(usize, &str)> {
    let lines: Vec<&str> = text.lines().collect();
    let mut result = Vec::with_capacity(lines.len());
    let mut index = 0;
    while index < lines.len() {
        if lines[index].starts_with("#[cfg(test)]") {
            let mut header = index + 1;
            while header < lines.len() && header < index + 4 && lines[header].starts_with("#[") {
                header += 1;
            }
            if header < lines.len() && is_module_start(lines[header]) {
                let mut end = header + 1;
                while end < lines.len() && !lines[end].starts_with('}') {
                    end += 1;
                }
                index = end + 1;
                continue;
            }
        }
        result.push((index, lines[index]));
        index += 1;
    }
    result
}

fn is_module_start(line: &str) -> bool {
    let rest = line
        .strip_prefix("pub(crate) ")
        .or_else(|| line.strip_prefix("pub(super) "))
        .or_else(|| line.strip_prefix("pub "))
        .unwrap_or(line);
    rest.starts_with("mod ") && rest.trim_end().ends_with('{')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_test_modules_are_excluded_from_production_lines() {
        let text = "fn a() {}\n#[cfg(test)]\nmod tests {\n    fn long() {}\n}\nfn b() {}\n";
        let lines: Vec<&str> = production_lines(text).into_iter().map(|(_, l)| l).collect();
        assert_eq!(lines, ["fn a() {}", "fn b() {}"]);
    }

    #[test]
    fn a_cfg_test_item_that_is_not_a_module_stays_in_production_text() {
        let text = "#[cfg(test)]\nuse std::fmt;\nfn a() {}\n";
        assert_eq!(production_lines(text).len(), 3);
    }
}
