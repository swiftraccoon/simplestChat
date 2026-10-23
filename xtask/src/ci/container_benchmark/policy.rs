//! Closed selected evidence from the unchanged container benchmark predicates.
//! Unknown values stay raw/ignored; no private producer object becomes output.
use crate::tooling::Result;
use serde::{
    Deserialize, Serialize,
    de::{IgnoredAny, MapAccess, Visitor},
};
use serde_json::{Value, value::RawValue};
use std::{collections::BTreeMap, fmt};

const SUMMARY_BYTES: usize = 8 * 1024 * 1024;
const METRIC_BYTES: usize = 64 * 1024;
const SELECTED: [&str; 30] = [
    "schemaVersion",
    "run",
    "completed",
    "passed",
    "configuration",
    "numClients",
    "durationSecs",
    "numRooms",
    "rampUpSecs",
    "warmupSecs",
    "runLabel",
    "provenance",
    "serverRevision",
    "generatorRevision",
    "attemptCoverage",
    "version",
    "scope",
    "available",
    "attempts",
    "passedAttempts",
    "failedAttempts",
    "missingCoverageAttempts",
    "skippedShortTailAttempts",
    "requestedChurners",
    "validatedChurners",
    "totalErrors",
    "failedConnections",
    "failedConsumers",
    "validatedConsumers",
    "skippedShortLivedConsumers",
];

pub(super) struct SummarySelection {
    pub clients: u32,
    pub rooms: u32,
    pub duration: u32,
    pub ramp_up: u32,
    pub warmup: u32,
    pub run_id: String,
    pub server_image: String,
    pub generator_image: String,
}
impl fmt::Debug for SummarySelection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SelectedContainerBenchmark")
    }
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SafeSummaryProjection {
    schema_version: u8,
    completed: bool,
    passed: bool,
    total_errors: u8,
    failed_connections: u8,
    failed_consumers: u8,
    validated_consumers: u32,
    skipped_short_lived_consumers: u8,
    attempts: u32,
    passed_attempts: u32,
}

/// Only this profile's fixed selected keys allocate retained map entries.
/// Repeated keys replace the prior value as in jq. Unknown values are skipped
/// without parsing their numbers into a finite Rust float.
struct Fields(BTreeMap<&'static str, Box<RawValue>>);
impl<'de> Deserialize<'de> for Fields {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct SelectedFields;
        impl<'de> Visitor<'de> for SelectedFields {
            type Value = Fields;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a selected container benchmark object")
            }
            fn visit_map<M: MapAccess<'de>>(
                self,
                mut map: M,
            ) -> std::result::Result<Fields, M::Error> {
                let mut values = BTreeMap::new();
                while let Some(key) = map.next_key::<String>()? {
                    if let Some(name) = SELECTED.iter().find(|name| **name == key) {
                        values.insert(*name, map.next_value::<Box<RawValue>>()?);
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                Ok(Fields(values))
            }
        }
        deserializer.deserialize_map(SelectedFields)
    }
}
impl Fields {
    fn decode(bytes: &[u8]) -> Result<Self> {
        serde_json::from_slice(bytes).map_err(|_| super::failure(1))
    }
    fn object(&self, key: &str) -> Result<Self> {
        Self::decode(
            self.0
                .get(key)
                .ok_or_else(|| super::failure(1))?
                .get()
                .as_bytes(),
        )
    }
    fn number(&self, key: &str, expected: u32) -> bool {
        self.0
            .get(key)
            .is_some_and(|raw| number(raw.get(), expected))
    }
    fn yes(&self, key: &str) -> bool {
        self.0.get(key).is_some_and(|raw| raw.get() == "true")
    }
    fn string(&self, key: &str, expected: &str) -> bool {
        self.0.get(key).is_some_and(|raw| {
            serde_json::from_str::<String>(raw.get()).is_ok_and(|value| value == expected)
        })
    }
}

/// Compare a valid JSON number to a small integer using decimal identity, not
/// binary-float rounding/underflow. jq keeps decimal literal precision: neither
/// 1.00000000000000000001 == 1 nor 1e-999 == 0. Zero exponents and -0 still equal 0.
fn number(raw: &str, expected: u32) -> bool {
    if !raw
        .as_bytes()
        .first()
        .is_some_and(|b| b.is_ascii_digit() || *b == b'-')
    {
        return false;
    }
    let negative = raw.starts_with('-');
    let raw = raw.strip_prefix('-').unwrap_or(raw);
    let (coefficient, exponent) = raw
        .find(['e', 'E'])
        .map_or((raw, "0"), |at| (&raw[..at], &raw[at + 1..]));
    let all_zero = coefficient
        .bytes()
        .filter(|b| *b != b'.')
        .all(|b| b == b'0');
    if expected == 0 {
        return all_zero;
    }
    if negative || all_zero {
        return false;
    }
    let bound = SUMMARY_BYTES as i64 + 32;
    let (sign, exponent) = if let Some(value) = exponent.strip_prefix('-') {
        (-1, value)
    } else {
        (1, exponent.strip_prefix('+').unwrap_or(exponent))
    };
    let mut power = 0i64;
    for digit in exponent.bytes() {
        if !digit.is_ascii_digit() || power > bound / 10 {
            return false;
        }
        power = power * 10 + i64::from(digit - b'0');
        if power > bound {
            return false;
        }
    }
    power *= sign;
    let fraction = coefficient
        .find('.')
        .map_or(0, |at| coefficient.len() - at - 1);
    let trailing = coefficient
        .bytes()
        .rev()
        .filter(|b| *b != b'.')
        .take_while(|b| *b == b'0')
        .count();
    let digits = coefficient.bytes().filter(|b| *b != b'.');
    let total = digits.clone().count();
    let leading = digits.clone().take_while(|b| *b == b'0').count();
    let significant = total - leading - trailing;
    let expected = expected.to_string();
    let selected = expected.trim_end_matches('0');
    significant == selected.len()
        && digits.skip(leading).take(significant).eq(selected.bytes())
        && power - fraction as i64 + trailing as i64 == (expected.len() - selected.len()) as i64
}
fn image(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|body| hex(body, 64))
}
fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn expected_consumers(selection: &SummarySelection) -> Result<u32> {
    if !(2..=30).contains(&selection.clients)
        || !(1..=4).contains(&selection.rooms)
        || selection.rooms > selection.clients
        || selection.clients > selection.rooms * 10
        || !(3..=180).contains(&selection.duration)
        || !(1..=120).contains(&selection.ramp_up)
        || !(10..=60).contains(&selection.warmup)
        || !hex(&selection.run_id, 32)
        || !image(&selection.server_image)
        || !image(&selection.generator_image)
    {
        return Err(super::failure(1));
    }
    Ok((0..selection.rooms)
        .map(|room| {
            let n = selection.clients / selection.rooms
                + u32::from(room < selection.clients % selection.rooms);
            n * n.saturating_sub(1).min(4) * 2
        })
        .sum())
}

pub(super) fn admit_summary(
    bytes: &[u8],
    selection: &SummarySelection,
) -> Result<SafeSummaryProjection> {
    if bytes.is_empty() || bytes.len() > SUMMARY_BYTES {
        return Err(super::failure(1));
    }
    let expected = expected_consumers(selection)?;
    let fields = Fields::decode(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes))?;
    let run = fields.object("run")?;
    let config = run.object("configuration")?;
    let provenance = run.object("provenance")?;
    let coverage = fields.object("attemptCoverage")?;
    let matches = fields.number("schemaVersion", 2)
        && run.yes("completed")
        && run.yes("passed")
        && fields.number("totalErrors", 0)
        && fields.number("failedConnections", 0)
        && fields.number("failedConsumers", 0)
        && expected > 0
        && fields.number("validatedConsumers", expected)
        && fields.number("skippedShortLivedConsumers", 0)
        && config.number("numClients", selection.clients)
        && config.number("durationSecs", selection.duration)
        && config.number("numRooms", selection.rooms)
        && config.number("rampUpSecs", selection.ramp_up)
        && config.number("warmupSecs", selection.warmup)
        && config.string("runLabel", &selection.run_id)
        && provenance.string("serverRevision", &selection.server_image)
        && provenance.string("generatorRevision", &selection.generator_image)
        && coverage.number("version", 1)
        && coverage.string("scope", "stable-publishers")
        && coverage.yes("available")
        && coverage.number("attempts", selection.clients)
        && coverage.number("passedAttempts", selection.clients)
        && [
            "failedAttempts",
            "missingCoverageAttempts",
            "skippedShortTailAttempts",
            "requestedChurners",
            "validatedChurners",
        ]
        .iter()
        .all(|key| coverage.number(key, 0));
    if !matches {
        return Err(super::failure(1));
    }
    Ok(SafeSummaryProjection {
        schema_version: 2,
        completed: true,
        passed: true,
        total_errors: 0,
        failed_connections: 0,
        failed_consumers: 0,
        validated_consumers: expected,
        skipped_short_lived_consumers: 0,
        attempts: selection.clients,
        passed_attempts: selection.clients,
    })
}

/// Exact old identity predicate. Native authority additionally checks intent,
/// immutable name, daemon and namespace observations outside this pure function.
pub(super) fn ownership(value: &Value, id: &str, image: &str, run: &str, role: &str) -> bool {
    [("id", id), ("image", image), ("run", run), ("role", role)]
        .iter()
        .all(|(key, expected)| value[*key].as_str() == Some(*expected))
}
pub(super) fn exit_passed(value: &Value) -> bool {
    value["Running"] == false
        && value["OOMKilled"] == false
        && value["Restarting"] == false
        && value["Error"] == ""
        && value["ExitCode"]
            .as_number()
            .is_some_and(|n| number(&n.to_string(), 0))
}

pub(super) fn zero_counts(bytes: &[u8]) -> Result<()> {
    if bytes.len() > METRIC_BYTES {
        return Err(super::failure(1));
    }
    let names = [
        b"simplestchat_rooms_active".as_slice(),
        b"simplestchat_participants_active",
        b"simplestchat_connections_active",
        b"simplestchat_participants_snapshot_complete",
    ];
    let mut counts = [0u8; 4];
    for line in bytes.split(|b| *b == b'\n') {
        let index = names
            .iter()
            .position(|name| line.starts_with(name) && line.get(name.len()) == Some(&b' '));
        let Some(index) = index else {
            continue;
        };
        let mut fields = line
            .split(|b| matches!(b, b' ' | b'\t'))
            .filter(|v| !v.is_empty());
        let (_, Some(value), None) = (fields.next(), fields.next(), fields.next()) else {
            return Err(super::failure(1));
        };
        if counts[index] != 0 {
            return Err(super::failure(1));
        }
        let valid = if index < 3 {
            !value.is_empty() && value.iter().all(|b| *b == b'0')
        } else {
            snapshot_one(value)
        };
        if !valid {
            return Err(super::failure(1));
        }
        counts[index] = 1;
    }
    if counts != [1; 4] {
        return Err(super::failure(1));
    }
    Ok(())
}

/// The retained LC_ALL=C AWK accepts decimal strnum coercion for this fourth
/// field, unlike its three explicitly digit-only counters. Keep that decimal
/// profile (including binary-float rounding near one); hex and other provider
/// extensions are outside this profile. Observed C leading whitespace and
/// trailing CR are admitted, not Unicode whitespace or trailing VT/FF.
fn snapshot_one(value: &[u8]) -> bool {
    let start = value
        .iter()
        .position(|byte| !matches!(byte, b'\t'..=b'\r' | b' '))
        .unwrap_or(value.len());
    let value = &value[start..];
    let value = value.strip_suffix(b"\r").map_or(value, |_| {
        let end = value
            .iter()
            .rposition(|byte| *byte != b'\r')
            .map_or(0, |at| at + 1);
        &value[..end]
    });
    let Ok(text) = std::str::from_utf8(value) else {
        return false;
    };
    let unsigned = text.strip_prefix(['+', '-']).unwrap_or(text);
    let (coefficient, exponent) = unsigned.find(['e', 'E']).map_or((unsigned, None), |at| {
        (&unsigned[..at], Some(&unsigned[at + 1..]))
    });
    let mut dot = false;
    let mut digits = 0usize;
    for byte in coefficient.bytes() {
        if byte == b'.' && !dot {
            dot = true;
        } else if byte.is_ascii_digit() {
            digits += 1;
        } else {
            return false;
        }
    }
    if digits == 0
        || exponent.is_some_and(|value| {
            let value = value.strip_prefix(['+', '-']).unwrap_or(value);
            value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit())
        })
    {
        return false;
    }
    text.parse::<f64>().ok() == Some(1.0)
}

#[cfg(test)]
#[path = "policy_tests.rs"]
mod tests;
