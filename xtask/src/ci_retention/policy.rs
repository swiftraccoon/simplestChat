//! Pure retention decisions. Unknown families and non-main refs are protected.

use anyhow::{Result, bail};
use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use regex::Regex;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::LazyLock;

pub(super) const REPOSITORY: &str = "swiftraccoon/simplestChat";
pub(super) const REFERENCE: &str = "refs/heads/main";
pub(super) const MAX_CACHES: usize = 1000;
pub(super) const MAX_DELETIONS: usize = 100;
pub(super) const PAGE_SIZE: usize = 100;
pub(super) const SOFT_BYTES: u64 = 8 * 1024 * 1024 * 1024;

pub(super) fn require(condition: bool, code: &str) -> Result<()> {
    if condition { Ok(()) } else { bail!("{code}") }
}

const DIGEST: &str = "[a-f0-9]{64}";
const NAMESPACE: &str = "(?:main|untrusted)-[a-zA-Z0-9_]+-[a-zA-Z0-9_]+";

static FAMILIES: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        format!("buildx-v[23]-({NAMESPACE})-{DIGEST}"),
        format!("codeql-analyzed-v2-(rust|c-cpp)-({NAMESPACE})-{DIGEST}"),
        format!("codeql-analyzed-v1-(rust|c-cpp)-{DIGEST}"),
        format!("grype-db-v2-(main|untrusted)-{DIGEST}-{DIGEST}"),
        format!(r"grype-db-v1-(main|untrusted)-{DIGEST}-\d{{4}}-\d{{2}}-\d{{2}}"),
        format!("(codeql-rust-cargo-v1-{NAMESPACE})-{DIGEST}"),
        format!(
            "((?:codeql-cli-v1|codeql-openssl-v1|openssl-v3|native-dtls-v2)-{NAMESPACE})-{DIGEST}"
        ),
        format!("(vendored-v2-{NAMESPACE}-rust-(?:check|build|test))-{DIGEST}"),
        format!("(native-prepared-v1-(?:main|untrusted)-(?:amd64|arm64))-{DIGEST}"),
        format!(
            "(native-compiled-v1-(?:asan|ubsan|replay)-(?:main|untrusted)-(?:amd64|arm64))-{DIGEST}"
        ),
        concat!(
            "(v3-rust-(?:main|untrusted)-(?:X64|ARM64)-[a-z-]{1,40}-",
            "(?:Linux|macOS|Windows)-(?:x64|arm64))-[a-f0-9]{8}-[a-f0-9]{8}"
        )
        .into(),
    ]
    .into_iter()
    .map(|pattern| {
        Regex::new(&format!(r"\A(?:{pattern})\z")).expect("fixed reviewed retention pattern")
    })
    .collect()
});

pub(super) fn family(key: &str) -> Option<(String, usize)> {
    for (index, expression) in FAMILIES.iter().enumerate() {
        let Some(captures) = expression.captures(key) else {
            continue;
        };
        let family = match index {
            0 => format!("buildx-{}", &captures[1]),
            1 => format!("codeql-{}-{}", &captures[1], &captures[2]),
            2 => format!("codeql-{}-main-linux-x86_64", &captures[1]),
            3 | 4 => format!("grype-{}", &captures[1]),
            _ => captures[1].to_owned(),
        };
        return Some((family, if index == 0 { 1 } else { 2 }));
    }
    None
}

pub(super) fn digest(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

// Match the maintained Python 3.14 reference's ISO dates and reduced/basic times,
// including its microsecond truncation. GitHub normally sends full RFC 3339.
static ISO_TIMESTAMP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"\A(?P<date>[0-9]{4}-[0-9]{2}-[0-9]{2}|[0-9]{8}|",
        r"[0-9]{4}-W[0-9]{2}(?:-[0-9])?|[0-9]{4}W[0-9]{2}[0-9]?)",
        r"(?s:.)(?P<time>(?:[0-9]{2}(?::[0-9]{2}(?::[0-9]{2})?)?|",
        r"[0-9]{2}(?:[0-9]{2}(?:[0-9]{2})?)?)(?:[.,][0-9]+)?)Z\z",
    ))
    .expect("fixed reviewed ISO timestamp pattern")
});

fn timestamp(value: &Value) -> Result<DateTime<Utc>> {
    let value = value
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("cache_retention_timestamp"))?;
    require(
        value.chars().count() <= 40 && value.ends_with('Z'),
        "cache_retention_timestamp",
    )?;
    let invalid = || anyhow::anyhow!("cache_retention_timestamp");
    let parts = ISO_TIMESTAMP.captures(value).ok_or_else(invalid)?;
    let date = &parts["date"];
    let date = if date.contains('W') {
        let mut date = date.replace('-', "");
        if date.len() == 7 {
            date.push('1');
        }
        NaiveDate::parse_from_str(&date, "%GW%V%u")
    } else {
        NaiveDate::parse_from_str(
            date,
            if date.contains('-') {
                "%Y-%m-%d"
            } else {
                "%Y%m%d"
            },
        )
    }
    .map_err(|_| invalid())?;
    require(
        (1..=9999).contains(&date.year()),
        "cache_retention_timestamp",
    )?;
    let (time, fraction) = parts["time"]
        .split_once(['.', ','])
        .unwrap_or((&parts["time"], ""));
    let time = time.replace(':', "");
    require(
        fraction.is_empty() || time.len() == 6,
        "cache_retention_timestamp",
    )?;
    let number = |start, end| {
        time.get(start..end)
            .unwrap_or("00")
            .parse::<u32>()
            .map_err(|_| invalid())
    };
    let microseconds = format!("{fraction:0<6}")
        .chars()
        .take(6)
        .collect::<String>()
        .parse::<u32>()
        .map_err(|_| invalid())?;
    let (hour, minute, second) = (number(0, 2)?, number(2, 4)?, number(4, 6)?);
    let (date, hour) = if hour == 24 && minute == 0 && second == 0 && microseconds == 0 {
        let date = date
            .succ_opt()
            .filter(|date| date.year() <= 9999)
            .ok_or_else(invalid)?;
        (date, 0)
    } else {
        (date, hour)
    };
    date.and_hms_micro_opt(hour, minute, second, microseconds)
        .map(|time| time.and_utc())
        .ok_or_else(invalid)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Entry {
    pub id: u64,
    pub key: String,
    pub reference: String,
    pub version: String,
    pub size: u64,
    pub created: DateTime<Utc>,
    pub accessed: DateTime<Utc>,
}

impl Entry {
    pub(super) fn parse(value: &Value) -> Result<Self> {
        let text = |key: &str| {
            value[key]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| anyhow::anyhow!("cache_retention_metadata"))
        };
        let number = |key: &str| {
            value[key]
                .as_u64()
                .ok_or_else(|| anyhow::anyhow!("cache_retention_metadata"))
        };
        let entry = Self {
            id: number("id")?,
            key: text("key")?,
            reference: text("ref")?,
            version: text("version")?,
            size: number("size_in_bytes")?,
            created: timestamp(&value["created_at"])?,
            accessed: timestamp(&value["last_accessed_at"])?,
        };
        require(
            entry.id > 0
                && entry.size <= 100 * 1024 * 1024 * 1024
                && !entry.key.is_empty()
                && entry.key.chars().count() <= 512
                && entry.reference.chars().count() <= 512
                && digest(&entry.version, 64),
            "cache_retention_metadata",
        )?;
        Ok(entry)
    }

    pub(super) fn recent(&self, now: DateTime<Utc>) -> bool {
        self.created.max(self.accessed) >= now - Duration::hours(2)
    }

    pub(super) fn summary(&self) -> Value {
        json!({"id": self.id, "key": self.key, "bytes": self.size})
    }

    pub(super) fn unchanged(&self, other: &Self) -> bool {
        self.id == other.id
            && self.key == other.key
            && self.reference == other.reference
            && self.version == other.version
            && self.size == other.size
            && self.created == other.created
    }
}

pub(super) fn plan(entries: &[Entry], now: DateTime<Utc>) -> Vec<Entry> {
    plan_with_limits(entries, now, SOFT_BYTES, MAX_DELETIONS)
}

pub(super) fn plan_with_limits(
    entries: &[Entry],
    now: DateTime<Utc>,
    soft: u64,
    maximum: usize,
) -> Vec<Entry> {
    let mut groups: BTreeMap<String, (usize, Vec<&Entry>)> = BTreeMap::new();
    for entry in entries {
        if entry.reference == REFERENCE
            && let Some((name, keep)) = family(&entry.key)
        {
            groups
                .entry(name)
                .or_insert_with(|| (keep, Vec::new()))
                .1
                .push(entry);
        }
    }
    let mut removals = Vec::new();
    let mut optional = Vec::new();
    for (keep, mut group) in groups.into_values() {
        group.sort_by_key(|entry| std::cmp::Reverse((entry.created, entry.id)));
        for (index, entry) in group.into_iter().enumerate() {
            if index == 0 || entry.recent(now) {
                continue;
            }
            if index >= keep {
                removals.push(entry);
            } else {
                optional.push(entry);
            }
        }
    }
    let mut retained = entries.iter().map(|entry| entry.size).sum::<u64>()
        - removals.iter().map(|entry| entry.size).sum::<u64>();
    optional.sort_by_key(|entry| (entry.accessed, entry.created, entry.id));
    for entry in optional {
        if retained <= soft {
            break;
        }
        removals.push(entry);
        retained -= entry.size;
    }
    removals.sort_by_key(|entry| (entry.accessed, entry.id));
    removals.into_iter().take(maximum).cloned().collect()
}
