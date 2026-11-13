//! Narrowly scoped, owned, expiring security review exceptions.
//!
//! An exception matches one scanner fingerprint and one exact scope. There is
//! no glob, pattern, severity, package-family or query-class waiver. Every
//! record of the selected source scope must be valid even when its scanner is
//! not part of the current check. A review of vendored source is enforced only
//! when that scope is selected; dependency and image reviews always apply.

use crate::{
    Fault, Result, files,
    record::{self, MAX_DOCUMENT},
    require,
    scope::vendor_path,
};
use serde_json::Value;
use std::path::Path;

const MAX_EXCEPTIONS: usize = 256;
const SCANNERS: [&str; 11] = [
    "zizmor",
    "gitleaks",
    "semgrep",
    "cargo-audit",
    "cargo-deny",
    "pip-audit",
    "npm-audit",
    "squawk",
    "grype",
    "image-license",
    "codeql",
];
/// The scanners whose scope is a source path.
const SOURCE_SCANNERS: [&str; 4] = ["codeql", "gitleaks", "semgrep", "zizmor"];
const FIELDS: [&str; 8] = [
    "scanner",
    "fingerprint",
    "scope",
    "owner",
    "rationale",
    "reachability",
    "expires",
    "review",
];
/// Words that name no particular finding, scope or owner.
const VAGUE: [&str; 4] = ["all", "any", "unknown", "todo"];
const EXPIRY: Fault = Fault("invalid_exception_expiry");

/// A calendar day.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Date {
    year: u16,
    month: u8,
    day: u8,
}

impl Date {
    /// The first day there is: nothing has expired by it.
    pub const MIN: Self = Self {
        year: 1,
        month: 1,
        day: 1,
    };

    /// The day `year-month-day`, when the calendar has one.
    pub fn new(year: u16, month: u8, day: u8) -> Option<Self> {
        let leap =
            year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
        let days = match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if leap => 29,
            2 => 28,
            _ => return None,
        };
        ((1..=9999).contains(&year) && (1..=days).contains(&day)).then_some(Self {
            year,
            month,
            day,
        })
    }

    /// `YYYY-MM-DD` in ASCII digits and nothing else.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        let digits = |range: std::ops::Range<usize>| {
            let part = bytes.get(range)?;
            part.iter()
                .all(u8::is_ascii_digit)
                .then(|| std::str::from_utf8(part).ok())
                .flatten()
        };
        if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
            return None;
        }
        Self::new(
            digits(0..4)?.parse().ok()?,
            digits(5..7)?.parse().ok()?,
            digits(8..10)?.parse().ok()?,
        )
    }

    /// The day it is now in UTC.
    pub fn today() -> Self {
        let seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        Self::of_epoch_second(seconds).unwrap_or(Self::MIN)
    }

    /// The number of days from 1970-01-01 to this day.
    pub(crate) fn epoch_day(self) -> i64 {
        let (month, day) = (i64::from(self.month), i64::from(self.day));
        let year = i64::from(self.year) - i64::from(month <= 2);
        let (era, year_of_era) = (year.div_euclid(400), year.rem_euclid(400));
        let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
        let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
        era * 146_097 + day_of_era - 719_468
    }

    /// The UTC day of the second `seconds` after the epoch.
    fn of_epoch_second(seconds: u64) -> Option<Self> {
        Self::of_epoch_day(seconds / 86_400)
    }

    /// The day `days` after 1970-01-01.
    fn of_epoch_day(days: u64) -> Option<Self> {
        // Days are counted from 0000-03-01 so that a leap day ends its year.
        let shifted = days.checked_add(719_468)?;
        let (era, day_of_era) = (shifted / 146_097, shifted % 146_097);
        let year_of_era =
            (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let shifted_month = (5 * day_of_year + 2) / 153;
        let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
        let month = if shifted_month < 10 {
            shifted_month + 3
        } else {
            shifted_month - 9
        };
        let year = year_of_era + era * 400 + u64::from(month <= 2);
        Self::new(
            u16::try_from(year).ok()?,
            u8::try_from(month).ok()?,
            u8::try_from(day).ok()?,
        )
    }
}

impl std::fmt::Display for Date {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{:04}-{:02}-{:02}",
            self.year, self.month, self.day
        )
    }
}

/// A complete review bound to one finding in one place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exception {
    pub scanner: String,
    pub fingerprint: String,
    pub scope: String,
    pub owner: String,
    pub rationale: String,
    pub reachability: String,
    pub expires: Date,
    pub review: String,
}

/// White space as the reference strips and matches it: Unicode white space
/// and the four information separators.
fn space(character: char) -> bool {
    character.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&character)
}

/// The bytes that `%XX` escapes stand for, with every other byte as it is.
fn unescaped(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let escape = bytes
            .get(index + 1..index + 3)
            .filter(|_| bytes[index] == b'%')
            .and_then(|pair| std::str::from_utf8(pair).ok())
            .and_then(|pair| u8::from_str_radix(pair, 16).ok());
        match escape {
            Some(byte) => {
                decoded.push(byte);
                index += 3;
            }
            None => {
                decoded.push(bytes[index]);
                index += 1;
            }
        }
    }
    decoded
}

/// Characters that would make a scope a pattern or more than one line.
fn patterned(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .any(|byte| matches!(byte, b'*' | b'?' | b'\n' | b'\r' | 0))
}

/// One qualifier's key and value as text, a plus standing for a space.
fn qualifier(field: &str) -> Option<(String, String)> {
    let (key, value) = field.split_once('=')?;
    let text =
        |part: &str| String::from_utf8_lossy(&unescaped(&part.replace('+', " "))).into_owned();
    Some((text(key), text(value)))
}

/// A package URL with qualifiers, each of them literal: the one scope that may
/// hold a query mark, and only for the two scanners of image contents.
fn qualified(value: &str, scanner: &str) -> bool {
    let class = |text: &str, more: &[u8]| {
        !text.is_empty()
            && text
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || more.contains(&byte))
    };
    let escapes = value.match_indices('%').all(|(at, _)| {
        value
            .as_bytes()
            .get(at + 1..at + 3)
            .is_some_and(|pair| pair.iter().all(u8::is_ascii_hexdigit))
    });
    let parts = value.strip_prefix("pkg:").and_then(|rest| {
        let (path, query) = rest.split_once('?')?;
        let (kind, named) = path.split_once('/')?;
        let (name, version) = named.split_once('@')?;
        Some((path, kind, name, version, query))
    });
    let Some((path, kind, name, version, query)) = parts else {
        return false;
    };
    let shaped = kind.starts_with(|first: char| first.is_ascii_lowercase())
        && kind.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b".+-".contains(&byte)
        })
        && class(name, b"._~%/+-")
        && class(version, b"._~%+:-")
        && !query.contains('#')
        && !query.contains(space);
    if !matches!(scanner, "image-license" | "grype") || !shaped || !escapes {
        return false;
    }
    let decoded = unescaped(path);
    if patterned(&decoded)
        || decoded
            .split(|byte| *byte == b'/')
            .any(|part| matches!(part, b"." | b".."))
    {
        return false;
    }
    let Some(pairs) = query.split('&').map(qualifier).collect::<Option<Vec<_>>>() else {
        return false;
    };
    let mut keys: Vec<&str> = pairs.iter().map(|(key, _)| key.as_str()).collect();
    keys.sort_unstable();
    keys.dedup();
    keys.len() == pairs.len()
        && pairs.iter().all(|(key, item)| {
            let mut letters = key.bytes();
            letters
                .next()
                .is_some_and(|first| first.is_ascii_lowercase())
                && letters
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
                && !item.is_empty()
                && !patterned(item.as_bytes())
                && item.trim_matches(space) == item
        })
}

/// One finding, one place or one person, spelled without a pattern.
fn narrow(key: &str, value: &str, scanner: &str) -> bool {
    value.trim_matches(space) == value
        && !value.contains(['*', '\n', '\r', '\0'])
        && (!value.contains('?') || (key == "scope" && qualified(value, scanner)))
        && !VAGUE.contains(&value.to_lowercase().as_str())
}

/// An HTTPS address of a named host with a path, and no credentials. It is
/// spelled plainly: no white space or control character anywhere, an ASCII
/// host, and brackets only around an IPv6 address.
fn linked(review: &str) -> bool {
    let plain = !review
        .chars()
        .any(|character| character <= ' ' || character == '\u{7f}');
    let located = review.split_once(':').and_then(|(scheme, rest)| {
        let rest = rest.strip_prefix("//")?;
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        scheme
            .eq_ignore_ascii_case("https")
            .then_some(rest.split_at(end))
    });
    let Some((authority, tail)) = located else {
        return false;
    };
    let host = match authority.strip_prefix('[') {
        Some(bracketed) => bracketed.split_once(']').is_some_and(|(address, port)| {
            address.parse::<std::net::Ipv6Addr>().is_ok()
                && (port.is_empty() || port.starts_with(':'))
        }),
        None => {
            let name = authority.split(':').next().unwrap_or("");
            !name.is_empty() && !authority.contains(['[', ']'])
        }
    };
    let path = tail.split(['?', '#']).next().unwrap_or("");
    plain && authority.is_ascii() && !authority.contains('@') && host && !matches!(path, "" | "/")
}

/// One record, complete and current on `today`.
pub(crate) fn parse(value: &Value, today: Date) -> Result<Exception> {
    let raw = record::record(value, &FIELDS)?;
    let mut fields = Vec::with_capacity(FIELDS.len());
    for name in FIELDS {
        fields.push(record::string(&raw[name])?);
    }
    let [
        scanner,
        fingerprint,
        scope,
        owner,
        rationale,
        reachability,
        expires,
        review,
    ] = fields[..]
    else {
        return Err(Fault("invalid_tool_record_fields"));
    };
    require(SCANNERS.contains(&scanner), "unknown_exception_scanner")?;
    for (key, value) in [
        ("fingerprint", fingerprint),
        ("scope", scope),
        ("owner", owner),
    ] {
        require(narrow(key, value, scanner), "ambiguous_exception_scope")?;
    }
    let expires = Date::parse(expires).ok_or(EXPIRY)?;
    require(expires >= today, "expired_security_exception")?;
    require(linked(review), "invalid_exception_review_link")?;
    Ok(Exception {
        scanner: scanner.to_owned(),
        fingerprint: fingerprint.to_owned(),
        scope: scope.to_owned(),
        owner: owner.to_owned(),
        rationale: rationale.to_owned(),
        reachability: reachability.to_owned(),
        expires,
        review: review.to_owned(),
    })
}

/// The whole policy at `path`, valid on `today`: the reviews of the selected
/// source scope in the file's order. One invalid record, or two with the same
/// identity, fails the whole policy.
pub fn read(path: &Path, today: Date, include_vendor: bool) -> Result<Vec<Exception>> {
    let content = files::bounded(path, MAX_DOCUMENT, false)?;
    let value = record::document(&content)?;
    let raw = record::record(&value, &["schemaVersion", "exceptions"])?;
    require(
        raw["schemaVersion"].as_u64() == Some(1),
        "exception_policy_schema",
    )?;
    let Value::Array(entries) = &raw["exceptions"] else {
        return Err(Fault("invalid_exception_list"));
    };
    require(
        entries.len() <= MAX_EXCEPTIONS,
        "too_many_security_exceptions",
    )?;
    let mut identities = Vec::with_capacity(entries.len());
    let mut selected = Vec::new();
    for entry in entries {
        let fields = record::record(entry, &FIELDS)?;
        let vendor = SOURCE_SCANNERS.contains(&record::string(&fields["scanner"])?)
            && vendor_path(record::string(&fields["scope"])?);
        // An unselected vendor review is still held to its shape, never to
        // its expiry.
        let on = if vendor && !include_vendor {
            Date::MIN
        } else {
            today
        };
        let parsed = parse(entry, on)?;
        identities.push((
            parsed.scanner.clone(),
            parsed.fingerprint.clone(),
            parsed.scope.clone(),
        ));
        if include_vendor || !vendor {
            selected.push(parsed);
        }
    }
    let count = identities.len();
    identities.sort_unstable();
    identities.dedup();
    require(identities.len() == count, "duplicate_security_exception")?;
    Ok(selected)
}

/// Whether a review names exactly this finding of this scanner in this scope.
/// A similar version, file or finding is not waived.
pub fn permitted(records: &[Exception], scanner: &str, fingerprint: &str, scope: &str) -> bool {
    records.iter().any(|item| {
        item.scanner == scanner && item.fingerprint == fingerprint && item.scope == scope
    })
}

#[cfg(test)]
mod tests;
