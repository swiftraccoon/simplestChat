//! Two explicit original-clock budgets; fixed host paths and no ambient selectors.
use super::super::control::{Cause, Result};
use std::{ffi::OsString, time::Duration};
pub(super) const USAGE: &str = concat!(
    "Usage: simplestchat-ops release receive --timeout-seconds N --finalization-seconds M\n",
    "       simplestchat-ops release receive --help|--licenses\n",
    "Requires Linux/OpenSSL curl 8.4+, root and the fixed private host layout.\n",
    "N and M are positive ASCII seconds, 0 < M < N <= 300; no default budget.\n",
    "Stdin supplies one envelope line, then a fresh URL only after Ready.\n",
    "No installed receiver, service, deployment or bootstrap route is changed.",
);
pub(super) struct Options {
    pub whole: Duration,
    pub reserve: Duration,
}
impl Options {
    pub fn parse(arguments: &[OsString]) -> Result<Option<Self>> {
        if arguments.len() == 1 && matches!(arguments[0].to_str(), Some("--help" | "-h")) {
            return Ok(None);
        }
        let mut whole = None;
        let mut reserve = None;
        let mut at = 0;
        while at < arguments.len() {
            let value = arguments[at].to_str().ok_or(Cause::Input)?;
            let (name, value) = match value.split_once('=') {
                Some(parts) => parts,
                None => {
                    at += 1;
                    (
                        value,
                        arguments
                            .get(at)
                            .and_then(|s| s.to_str())
                            .ok_or(Cause::Input)?,
                    )
                }
            };
            let target = match name {
                "--timeout-seconds" => &mut whole,
                "--finalization-seconds" => &mut reserve,
                _ => return Err(Cause::Input),
            };
            if target.is_some() || value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err(Cause::Input);
            }
            *target = Some(value.parse::<u64>().map_err(|_| Cause::Input)?);
            at += 1;
        }
        let whole = whole.ok_or(Cause::Input)?;
        let reserve = reserve.ok_or(Cause::Input)?;
        if whole == 0 || whole > 300 || reserve == 0 || reserve >= whole {
            return Err(Cause::Input);
        }
        Ok(Some(Self {
            whole: Duration::from_secs(whole),
            reserve: Duration::from_secs(reserve),
        }))
    }
}
