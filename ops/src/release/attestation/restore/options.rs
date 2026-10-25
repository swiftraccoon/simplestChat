//! One release attempt and explicit native whole/finalization budgets.
use std::{collections::BTreeMap, ffi::OsString, time::Duration};

pub(super) enum Parse {
    Syntax,
    Selection,
}

pub(super) struct Options {
    pub attempt: String,
    pub whole: Duration,
    pub reserve: Duration,
}
impl Options {
    pub fn parse(arguments: &[OsString]) -> Result<Self, Parse> {
        let mut values = BTreeMap::new();
        let mut at = 0;
        while at < arguments.len() {
            let argument = arguments[at].to_str().ok_or(Parse::Syntax)?;
            let (name, value) = if let Some(pair) = argument.split_once('=') {
                pair
            } else {
                at += 1;
                (
                    argument,
                    arguments
                        .get(at)
                        .and_then(|v| v.to_str())
                        .ok_or(Parse::Syntax)?,
                )
            };
            if ![
                "--release-attempt",
                "--timeout-seconds",
                "--finalization-seconds",
            ]
            .contains(&name)
                || values.insert(name, value).is_some()
            {
                return Err(Parse::Syntax);
            }
            at += 1;
        }
        let attempt = *values.get("--release-attempt").ok_or(Parse::Syntax)?;
        let suffix = attempt.strip_prefix("release.").ok_or(Parse::Selection)?;
        if !(6..=64).contains(&suffix.len())
            || !suffix
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        {
            return Err(Parse::Selection);
        }
        let seconds = |key| -> Result<u64, Parse> {
            let raw = *values.get(key).ok_or(Parse::Syntax)?;
            if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
                return Err(Parse::Syntax);
            }
            raw.parse().map_err(|_| Parse::Syntax)
        };
        let whole = seconds("--timeout-seconds")?;
        let reserve = seconds("--finalization-seconds")?;
        if reserve == 0 || reserve >= whole || whole > 1800 {
            return Err(Parse::Syntax);
        }
        Ok(Self {
            attempt: attempt.into(),
            whole: Duration::from_secs(whole),
            reserve: Duration::from_secs(reserve),
        })
    }
}
