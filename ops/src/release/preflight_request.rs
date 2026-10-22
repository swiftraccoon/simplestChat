//! A tiny strict JSON admission boundary; helpers never select filesystem paths.

use super::{Failure, Result};
use crate::json;
use std::fmt;

pub(super) const BASE: [&str; 10] = [
    "bounded_process.py",
    "release-public.py",
    "release_public.py",
    "release_artifact.py",
    "release_assets.py",
    "browser_assets.py",
    "release_json.py",
    "runtime_profile.py",
    "reboot-public.py",
    "reboot_public.py",
];
pub(super) const FETCH: [&str; 3] = [
    "fetch-release.py",
    "release_fetch_receiver.py",
    "release_trust.py",
];

/// One of the exact complete helper sets, or the ordinary empty selection.
/// Raw JSON, private values and digests are deliberately absent from Debug.
pub struct ExpectedHelpers {
    pub(super) values: Vec<(String, [u8; 32])>,
}
impl fmt::Debug for ExpectedHelpers {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExpectedHelpers")
            .field("prepared", &!self.values.is_empty())
            .finish()
    }
}
impl ExpectedHelpers {
    /// Absent input means ordinary reconciliation. UTF-8 JSON is at most 4096
    /// bytes, with the shared strict Unicode-scalar/depth/duplicate/finite domain.
    /// A BOM is refused because the reference receives a JSON string argument.
    pub fn decode(raw: Option<&str>) -> Result<Self> {
        let raw = raw.unwrap_or("{}");
        if raw.len() > 4096 {
            return Err(Failure::Request);
        }
        if raw.starts_with('\u{feff}') {
            return Err(Failure::Unavailable);
        }
        let value = json::decode_limited(raw.as_bytes(), 4096).map_err(|_| Failure::Unavailable)?;
        let object = value.object().ok_or(Failure::HelperSet)?;
        let names: Vec<_> = object.keys().map(String::as_str).collect();
        let mut valid = names.is_empty();
        for extra in [&[][..], &FETCH[..], &["force_release.py"][..]] {
            let mut expected: Vec<_> = BASE.into_iter().chain(extra.iter().copied()).collect();
            expected.sort_unstable();
            valid |= names == expected;
        }
        if !valid {
            return Err(Failure::HelperSet);
        }
        let json::Json::Object(_, order) = &value else {
            return Err(Failure::HelperSet);
        };
        let mut values = Vec::new();
        for name in order {
            let value = &object[name];
            let text = value.string().ok_or(Failure::HelperDigest)?;
            if text.len() != 64
                || !text
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err(Failure::HelperDigest);
            }
            let mut digest = [0; 32];
            for (index, pair) in text.as_bytes().as_chunks::<2>().0.iter().enumerate() {
                let digit = |byte| {
                    if byte <= b'9' {
                        byte - b'0'
                    } else {
                        byte - b'a' + 10
                    }
                };
                digest[index] = digit(pair[0]) * 16 + digit(pair[1]);
            }
            values.push((name.clone(), digest));
        }
        Ok(Self { values })
    }
}
