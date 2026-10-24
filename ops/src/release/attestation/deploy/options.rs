//! Explicit host revision and bounded scheduling; no caller or host selectors.

use super::{Options, budget};
use std::{ffi::OsString, time::Duration};

fn number(value: &std::ffi::OsStr) -> Result<u64, ()> {
    let value = value.to_str().ok_or(())?;
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(());
    }
    value.parse().map_err(|_| ())
}
impl Options {
    pub(super) fn parse(arguments: &[OsString]) -> Result<Self, ()> {
        let mut revision = None;
        let mut quiet = None;
        let mut whole = None;
        let mut reserve = None;
        let mut at = 0;
        while at < arguments.len() {
            let word = arguments[at].to_str().ok_or(())?;
            let (name, value) = if let Some((name, value)) = word.split_once('=') {
                (name, OsString::from(value))
            } else {
                at += 1;
                (word, arguments.get(at).ok_or(())?.clone())
            };
            match name {
                "--revision" if revision.is_none() => revision = Some(value),
                "--quiet-seconds" if quiet.is_none() => quiet = Some(number(&value)?),
                "--timeout-seconds" if whole.is_none() => whole = Some(number(&value)?),
                "--finalization-seconds" if reserve.is_none() => reserve = Some(number(&value)?),
                _ => return Err(()),
            }
            at += 1;
        }
        let revision = revision.ok_or(())?.into_string().map_err(|_| ())?;
        let whole = Duration::from_secs(whole.ok_or(())?);
        let reserve = Duration::from_secs(reserve.ok_or(())?);
        let quiet_seconds = quiet.unwrap_or(0);
        if revision.len() != 40
            || !revision
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || quiet_seconds > 600
            || whole.is_zero()
            || whole > budget::MAX_WHOLE
            || reserve.is_zero()
            || reserve >= whole
        {
            return Err(());
        }
        Ok(Self {
            revision,
            quiet_seconds,
            whole,
            reserve,
        })
    }
}
