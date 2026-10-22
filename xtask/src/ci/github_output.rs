//! Append one typed cache key to the runner's existing output file. The fixed
//! output name and exact scope/trust/hash framing prevent output-line injection.

use super::{Context, Scope, require};
use anyhow::Result;
use rustix::fs::{Mode, OFlags};
use std::fs::File;
use std::io::Write;
use std::path::Path;

pub(super) struct Target(File);

impl Target {
    /// The runner supplies an existing absolute regular file. Never create,
    /// truncate, follow a final symlink or wait for a special-file peer. Open
    /// before fingerprinting so a missing runner boundary launches no Git child.
    pub(super) fn open(context: &Context) -> Result<Self> {
        let value = context.env("GITHUB_OUTPUT");
        require(!value.is_empty(), "ci_cache_github_output_missing")?;
        let path = Path::new(value);
        require(path.is_absolute(), "ci_cache_github_output_path")?;
        let descriptor = rustix::fs::open(
            path,
            OFlags::WRONLY | OFlags::APPEND | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| anyhow::anyhow!("ci_cache_github_output_open"))?;
        let file = File::from(descriptor);
        require(
            file.metadata().is_ok_and(|metadata| metadata.is_file()),
            "ci_cache_github_output_regular",
        )?;
        Ok(Self(file))
    }

    /// Append only after the entire key is validated. Explicitly close even on
    /// a write failure; the first failure wins and no path or OS error escapes.
    pub(super) fn append(self, scope: Scope, trusted: bool, key: &str) -> Result<()> {
        let record = record(scope, trusted, key)?;
        let mut file = self.0;
        let written = write(&mut file, &record);
        let closed =
            nix::unistd::close(file).map_err(|_| anyhow::anyhow!("ci_cache_github_output_close"));
        written?;
        closed
    }
}

fn record(scope: Scope, trusted: bool, key: &str) -> Result<Vec<u8>> {
    let trust = if trusted { "main" } else { "untrusted" };
    let prefix = format!("ci-verified-v1-{}-{trust}-", scope.name());
    require(
        key.strip_prefix(&prefix).is_some_and(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        }),
        "ci_cache_github_output_key",
    )?;
    Ok(format!("key={key}\n").into_bytes())
}

fn write(output: &mut impl Write, record: &[u8]) -> Result<()> {
    output
        .write_all(record)
        .map_err(|_| anyhow::anyhow!("ci_cache_github_output_write"))
}

#[cfg(test)]
#[path = "github_output_tests.rs"]
mod tests;
