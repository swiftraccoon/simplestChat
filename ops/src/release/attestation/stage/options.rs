//! Fixed revision and explicit whole-operation budgets.
use super::super::control::{Cause, Result};
use std::{collections::BTreeMap, ffi::OsString, time::Duration};

pub(super) const USAGE: &str = concat!(
    "Usage: simplestchat-ops release stage --revision SHA40 ",
    "--timeout-seconds N --finalization-seconds M\n",
    "       simplestchat-ops release stage --help|--licenses\n",
    "Explicit positive ASCII seconds: 0 < M < N <= 600. Fixed root host paths.",
);
pub(super) struct Options {
    pub revision: String,
    pub whole: Duration,
    pub reserve: Duration,
}
impl Options {
    pub fn parse(args: &[OsString]) -> Result<Option<Self>> {
        if args.len() == 1 && matches!(args[0].to_str(), Some("--help" | "-h")) {
            return Ok(None);
        }
        let mut values = BTreeMap::new();
        let mut at = 0;
        while at < args.len() {
            let arg = args[at].to_str().ok_or(Cause::Input)?;
            let (key, value) = if let Some(pair) = arg.split_once('=') {
                pair
            } else {
                at += 1;
                (
                    arg,
                    args.get(at).and_then(|v| v.to_str()).ok_or(Cause::Input)?,
                )
            };
            if !["--revision", "--timeout-seconds", "--finalization-seconds"].contains(&key)
                || values.insert(key, value).is_some()
            {
                return Err(Cause::Input);
            }
            at += 1;
        }
        let revision = *values.get("--revision").ok_or(Cause::Input)?;
        if revision.len() != 40
            || !revision
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Cause::Input);
        }
        let seconds = |key| -> Result<u64> {
            let raw = *values.get(key).ok_or(Cause::Input)?;
            if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
                return Err(Cause::Input);
            }
            raw.parse().map_err(|_| Cause::Input)
        };
        let whole = seconds("--timeout-seconds")?;
        let reserve = seconds("--finalization-seconds")?;
        if reserve == 0 || reserve >= whole || whole > 600 {
            return Err(Cause::Input);
        }
        Ok(Some(Self {
            revision: revision.into(),
            whole: Duration::from_secs(whole),
            reserve: Duration::from_secs(reserve),
        }))
    }
}
