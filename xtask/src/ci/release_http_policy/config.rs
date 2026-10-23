//! Original five fixed-fixture inputs; no origin, endpoint or credential option.

use super::{Result, fail};
use std::path::PathBuf;

pub(super) struct Config {
    pub token: String,
    pub caddy: String,
    pub app: String,
    pub ca: PathBuf,
    pub sha256: String,
}
pub(super) fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
impl Config {
    pub fn parse(arguments: &[String]) -> Result<Self> {
        let mut fields = std::collections::BTreeMap::new();
        if arguments.len() != 10 {
            return Err(fail(2, "fixture_arguments"));
        }
        for pair in arguments.as_chunks::<2>().0 {
            if !matches!(
                pair[0].as_str(),
                "--fixture-token" | "--caddy-id" | "--app-id" | "--ca" | "--ca-sha256"
            ) || fields.insert(pair[0].as_str(), &pair[1]).is_some()
            {
                return Err(fail(2, "fixture_arguments"));
            }
        }
        let get = |name| {
            fields
                .get(name)
                .copied()
                .ok_or_else(|| fail(2, "fixture_arguments"))
        };
        let value = Self {
            token: get("--fixture-token")?.clone(),
            caddy: get("--caddy-id")?.clone(),
            app: get("--app-id")?.clone(),
            ca: PathBuf::from(get("--ca")?),
            sha256: get("--ca-sha256")?.clone(),
        };
        if !hex(&value.token, 32) {
            return Err(fail(1, "fixture_token"));
        }
        if value.caddy == value.app {
            return Err(fail(1, "fixture_distinct_services"));
        }
        if !hex(&value.caddy, 64) || !hex(&value.app, 64) {
            return Err(fail(1, "fixture_container_identity"));
        }
        if !hex(&value.sha256, 64) {
            return Err(fail(1, "fixture_ca_identity"));
        }
        if !value.ca.is_absolute()
            || value.ca.as_os_str().len() > 4096
            || value.ca.components().count() > 17
            || value
                .ca
                .to_str()
                .is_none_or(|s| s.chars().any(char::is_control))
            || value.ca.components().collect::<PathBuf>() != value.ca
        {
            return Err(fail(1, "fixture_ca_file"));
        }
        Ok(value)
    }
}
