//! Calendar-checked wall-clock anchors and nominal lifecycle phases. Nanosecond
//! ordering remains integer based; elapsed-clock drift uses the JS reference's
//! floating-point millisecond policy and 250ms tolerance.

use chrono::{Datelike, NaiveDate};
use regex::Regex;
use serde::Serialize;
use serde_json::value::RawValue;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

pub(super) const ANCHORS: &[&str] = &[
    "run_started",
    "server_ready",
    "generator_started",
    "generator_exited",
    "cleanup_observed",
    "server_stop_requested",
    "server_exited",
];
pub(super) const MAX_TIMELINE_BYTES: usize = 64 * 1024;
pub(super) type Issues = BTreeSet<&'static str>;
type Fields = BTreeMap<String, Box<RawValue>>;

static INSTANT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
    r"^([0-9]{4})-([0-9]{2})-([0-9]{2})T([0-9]{2}):([0-9]{2}):([0-9]{2})(?:\.([0-9]{1,9}))?(?:Z|\+00:00)$",
).expect("fixed UTC instant grammar")
});

#[derive(Clone, Debug)]
pub(super) struct Instant {
    pub(super) at: String,
    pub(super) ms: f64,
    pub(super) ns: i128,
}

/// Validate calendar fields instead of accepting Date.parse's normalization of
/// impossible dates. Only the reference's UTC spellings and 1..9 digits apply.
pub(super) fn instant(value: &str) -> Option<Instant> {
    let captures = INSTANT.captures(value)?;
    let date = NaiveDate::from_ymd_opt(
        captures[1].parse().ok()?,
        captures[2].parse().ok()?,
        captures[3].parse().ok()?,
    )?;
    let whole = date
        .and_hms_opt(
            captures[4].parse().ok()?,
            captures[5].parse().ok()?,
            captures[6].parse().ok()?,
        )?
        .and_utc()
        .timestamp_millis();
    let fraction = captures.get(7).map_or("0", |value| value.as_str());
    let nanos: u32 = format!("{fraction:0<9}").parse().ok()?;
    let fractional_ms = format!("0.{fraction}").parse::<f64>().ok()? * 1000.0;
    Some(Instant {
        at: value.to_owned(),
        ms: whole as f64 + fractional_ms,
        ns: i128::from(whole) * 1_000_000 + i128::from(nanos),
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Output {
    pub(super) departure: Option<&'static str>,
    events: Vec<Anchor>,
    nominal_measurement: Option<Measurement>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Anchor {
    event: &'static str,
    at: String,
    elapsed_ms: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Measurement {
    started_at: String,
    finished_at: String,
}

pub(super) struct View {
    pub(super) output: Output,
    known: BTreeMap<&'static str, Instant>,
    safe: bool,
    start: Option<i128>,
    end: Option<i128>,
}

impl View {
    pub(super) fn parse(input: &str, issues: &mut Issues) -> Self {
        let mut view = Self {
            output: Output {
                departure: None,
                events: Vec::new(),
                nominal_measurement: None,
            },
            known: BTreeMap::new(),
            safe: true,
            start: None,
            end: None,
        };
        let Some(fields) = (input.len() <= MAX_TIMELINE_BYTES)
            .then(|| object(input))
            .flatten()
        else {
            issues.insert("invalid_timeline");
            view.safe = false;
            return view;
        };
        let departure = match string(&fields, "departure").as_deref() {
            Some("abrupt") => Some("abrupt"),
            Some("explicit-leave") => Some("explicit-leave"),
            _ => None,
        };
        let events = fields
            .get("events")
            .and_then(|raw| serde_json::from_str::<Vec<Box<RawValue>>>(raw.get()).ok());
        if !exact(
            &fields,
            &["schemaVersion", "departure", "events", "workload"],
        ) || number(&fields, "schemaVersion") != Some(1.0)
            || departure.is_none()
            || events
                .as_ref()
                .is_none_or(|events| events.len() > ANCHORS.len())
        {
            issues.insert("invalid_timeline");
            view.safe = false;
            return view;
        }
        view.output.departure = departure;
        let mut previous: Option<(&'static str, f64, Instant)> = None;
        for raw in events.expect("validated timeline event list") {
            let Some(fields) = object(raw.get()) else {
                issues.insert("invalid_timeline");
                view.safe = false;
                continue;
            };
            let event = string(&fields, "event")
                .and_then(|event| ANCHORS.iter().copied().find(|value| *value == event));
            let time = string(&fields, "at").and_then(|at| instant(&at));
            let elapsed = number(&fields, "elapsedMs").filter(|value| finite(*value));
            let valid = exact(&fields, &["event", "at", "elapsedMs"])
                && event.is_some_and(|event| !view.known.contains_key(event))
                && time.is_some()
                && elapsed.is_some();
            if !valid {
                issues.insert("invalid_timeline");
                view.safe = false;
                continue;
            }
            let event = event.expect("validated anchor name");
            let time = time.expect("validated instant");
            let elapsed = elapsed.expect("validated elapsed time");
            if let Some((previous_event, previous_elapsed, previous_time)) = &previous {
                if ANCHORS.iter().position(|value| *value == event)
                    <= ANCHORS.iter().position(|value| value == previous_event)
                    || elapsed < *previous_elapsed
                {
                    issues.insert("timeline_order");
                    view.safe = false;
                }
                if (time.ms - previous_time.ms - (elapsed - previous_elapsed)).abs() > 250.0 {
                    issues.insert("timeline_clock_shift");
                    view.safe = false;
                }
            }
            view.known.insert(event, time.clone());
            view.output.events.push(Anchor {
                event,
                at: time.at.clone(),
                elapsed_ms: if elapsed == 0.0 { 0.0 } else { elapsed },
            });
            previous = Some((event, elapsed, time));
        }
        if ANCHORS.iter().any(|event| !view.known.contains_key(event)) {
            issues.insert("missing_timeline_anchor");
        }
        view.workload(
            fields.get("workload").map_or("null", |raw| raw.get()),
            issues,
        );
        view
    }

    fn workload(&mut self, raw: &str, issues: &mut Issues) {
        let parsed = (|| {
            let fields = object(raw)?;
            if !exact(
                &fields,
                &[
                    "startedAt",
                    "finishedAt",
                    "rampUpSecs",
                    "warmupSecs",
                    "durationSecs",
                ],
            ) {
                return None;
            }
            let start = instant(&string(&fields, "startedAt")?)?;
            let finish = instant(&string(&fields, "finishedAt")?)?;
            let ramp = integer(&fields, "rampUpSecs")?;
            let warmup = integer(&fields, "warmupSecs")?;
            let duration = integer(&fields, "durationSecs")?;
            if finish.ns < start.ns || ramp > 600 || warmup > 60 || !(3..=180).contains(&duration) {
                return None;
            }
            Some((start, finish, ramp, warmup, duration))
        })();
        let Some((start, finish, ramp, warmup, duration)) = parsed else {
            issues.insert("invalid_workload_timeline");
            return;
        };
        let nominal_start = start.ns + i128::from(ramp + warmup) * 1_000_000_000;
        let nominal_end = nominal_start + i128::from(duration) * 1_000_000_000;
        self.start = Some(nominal_start);
        self.end = Some(nominal_end);
        self.output.nominal_measurement = Some(Measurement {
            started_at: iso(nominal_start),
            finished_at: iso(nominal_end),
        });
        if finish.ns < nominal_end
            || self
                .known
                .get("generator_started")
                .is_some_and(|time| start.ms < time.ms - 250.0)
            || self
                .known
                .get("generator_exited")
                .is_some_and(|time| finish.ms > time.ms + 250.0)
            || self
                .known
                .get("cleanup_observed")
                .is_some_and(|time| time.ns < nominal_end)
        {
            issues.insert("workload_timeline_order");
            self.safe = false;
        }
    }

    pub(super) fn phase(&self, time: &Instant) -> &'static str {
        if !self.safe
            || self
                .known
                .get("run_started")
                .is_none_or(|start| time.ns < start.ns)
            || self
                .known
                .get("server_exited")
                .is_some_and(|end| time.ns > end.ns)
        {
            return "unknown";
        }
        if self
            .known
            .get("server_stop_requested")
            .is_some_and(|start| time.ns >= start.ns)
        {
            return "shutdown";
        }
        if self
            .known
            .get("cleanup_observed")
            .is_some_and(|start| time.ns >= start.ns)
        {
            return "post_cleanup";
        }
        if self.end.is_some_and(|end| time.ns >= end) {
            return "departure";
        }
        if self.start.is_some_and(|start| time.ns >= start) {
            return "measurement";
        }
        if self.start.is_some() {
            "setup"
        } else {
            "unknown"
        }
    }
}

fn iso(ns: i128) -> String {
    // BigInt division in the reference truncates toward zero before Date reads
    // integer milliseconds, including instants before the Unix epoch.
    let ms =
        i64::try_from(ns / 1_000_000).expect("validated four-digit UTC dates and bounded offsets");
    let date = chrono::DateTime::from_timestamp_millis(ms).expect("validated Gregorian instant");
    let year = date.year();
    let year = if (0..=9999).contains(&year) {
        format!("{year:04}")
    } else {
        format!("{year:+07}")
    };
    format!("{year}{}", date.format("-%m-%dT%H:%M:%S%.3fZ"))
}

fn object(raw: &str) -> Option<Fields> {
    serde_json::from_str(raw).ok()
}
fn exact(fields: &Fields, keys: &[&str]) -> bool {
    fields.len() == keys.len() && keys.iter().all(|key| fields.contains_key(*key))
}
fn string(fields: &Fields, key: &str) -> Option<String> {
    serde_json::from_str(fields.get(key)?.get()).ok()
}
fn number(fields: &Fields, key: &str) -> Option<f64> {
    fields.get(key)?.get().parse().ok()
}
fn finite(value: f64) -> bool {
    value.is_finite() && (0.0..=9_007_199_254_740_991.0).contains(&value)
}
fn integer(fields: &Fields, key: &str) -> Option<u64> {
    let value = number(fields, key)?;
    (finite(value) && value.fract() == 0.0).then_some(value as u64)
}
