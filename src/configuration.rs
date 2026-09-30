//! Strict parsing for operator-supplied numeric controls.
//!
//! Only absence selects a default. An explicit empty, malformed, non-UTF-8 or
//! out-of-range value is an error, so an intended small limit cannot silently
//! become a larger default. Callers choose units and bounds at their boundary.

use anyhow::{Result, bail};

/// Parse an optional integer without reading or modifying process environment.
pub fn parse_usize(
    name: &str,
    value: Option<&str>,
    default: usize,
    min: usize,
    max: usize,
) -> Result<usize> {
    let Some(value) = value else {
        return Ok(default);
    };
    match value.trim().parse::<usize>() {
        Ok(parsed) if (min..=max).contains(&parsed) => Ok(parsed),
        _ => bail!("{name} must be a whole number from {min} to {max}"),
    }
}

/// Read a bounded integer; error messages identify the setting, never its value.
pub fn read_usize(name: &str, default: usize, min: usize, max: usize) -> Result<usize> {
    let value = match std::env::var(name) {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => bail!("{name} must be valid UTF-8"),
    };
    parse_usize(name, value.as_deref(), default, min, max)
}

/// Validate numeric capacity controls before opening workers or a database.
/// Returned values are public limits only; credentials are never included.
pub fn numeric_settings() -> Result<std::collections::BTreeMap<&'static str, usize>> {
    let mut values = std::collections::BTreeMap::new();
    for (name, default, min, max) in [
        ("PORT", 3000, 1, 65535),
        ("MAX_CONNECTIONS", 10000, 1, 1000000),
        ("MAX_CONNECTIONS_PER_IP", 50, 1, 1000000),
        ("MAX_ROOMS", 1000, 1, 1000000),
        ("MAX_PERSISTED_ROOMS", 10000, 1, 1000000),
        ("MAX_USERS", 100000, 1, 10000000),
        ("WS_HANDSHAKES_PER_MINUTE", 120, 1, 1000000),
        ("AUTH_REQUESTS_PER_MINUTE", 60, 1, 1000000),
        ("PROFILE_REQUESTS_PER_MINUTE", 600, 1, 1000000),
        ("AUTH_REQUESTS_PER_ACCOUNT_PER_MINUTE", 20, 1, 1000000),
        ("REGISTRATIONS_PER_IP_PER_HOUR", 5, 1, 1000000),
        ("AUTH_MAX_CONCURRENCY", 16, 1, 1000000),
        ("ROOM_API_REQUESTS_PER_MINUTE", 120, 1, 1000000),
        ("ROOM_API_MAX_CONCURRENCY", 32, 1, 1000000),
        ("ROOM_CREATIONS_PER_ACCOUNT_PER_MINUTE", 10, 1, 1000000),
        ("MAX_PRODUCERS_PER_PARTICIPANT", 8, 1, 10000),
        ("MAX_CONSUMERS_PER_PARTICIPANT", 64, 1, 10000),
        ("DATABASE_MAX_CONNECTIONS", 20, 1, 1000),
        ("DATABASE_MIN_CONNECTIONS", 2, 0, 1000),
        ("DATABASE_ACQUIRE_TIMEOUT_SECS", 3, 1, 60),
    ] {
        values.insert(name, read_usize(name, default, min, max)?);
    }
    // Absence deliberately leaves the two password lanes' CPU-derived defaults
    // intact; an explicit override has the same bounds for both lanes.
    if std::env::var_os("MAX_PASSWORD_WORKERS").is_some() {
        values.insert(
            "MAX_PASSWORD_WORKERS",
            read_usize("MAX_PASSWORD_WORKERS", 2, 1, 32)?,
        );
    }
    anyhow::ensure!(
        values["DATABASE_MIN_CONNECTIONS"] <= values["DATABASE_MAX_CONNECTIONS"],
        "DATABASE_MIN_CONNECTIONS must not exceed DATABASE_MAX_CONNECTIONS"
    );
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_defaults_but_invalid_explicit_limits_never_do() {
        assert_eq!(
            parse_usize("LIMIT", None, 10000, 1, 1000000).unwrap(),
            10000
        );
        assert_eq!(
            parse_usize("LIMIT", Some(" 12 "), 10000, 1, 1000000).unwrap(),
            12
        );
        for value in [
            "",
            " ",
            "0",
            "-1",
            "12x",
            "1.5",
            "1000001",
            "99999999999999999999999999999999",
        ] {
            let error = parse_usize("LIMIT", Some(value), 10000, 1, 1000000).unwrap_err();
            assert_eq!(
                error.to_string(),
                "LIMIT must be a whole number from 1 to 1000000"
            );
        }
        assert_eq!(parse_usize("FLOOR", Some("0"), 2, 0, 1000).unwrap(), 0);
    }
}
