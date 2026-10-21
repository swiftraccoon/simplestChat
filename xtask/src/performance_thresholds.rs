//! Evaluate one same-host benchmark comparison against the existing coarse budgets.
//!
//! The benchmark runner owns workload/shutdown validation and measurement. This
//! reader preserves scenario/metric order and only summarizes and budgets its
//! comparison document; it never starts a workload or publishes new measurements.

use anyhow::{Context, Result};
use serde::de::{Error, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use std::fmt::{self, Display};
use std::marker::PhantomData;
use std::path::Path;
use std::process::ExitCode;

const DEFAULT_THRESHOLDS: [(&str, f64); 4] = [
    ("receiveReadyP99Ms", 0.25),
    ("sendReadyP99Ms", 0.25),
    ("serverCpuPercent", 0.25),
    ("serverPeakRssMiB", 0.15),
];

/// JSON object entries in their source order, without changing serde_json's
/// workspace-wide map representation or the server's JSON serialization.
#[derive(Debug)]
struct Entries<T>(Vec<(String, T)>);

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Entries<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct EntryVisitor<T>(PhantomData<T>);
        impl<'de, T: Deserialize<'de>> Visitor<'de> for EntryVisitor<T> {
            type Value = Entries<T>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an object of named comparison entries")
            }

            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut entries: Vec<(String, T)> = Vec::new();
                while let Some((key, value)) = map.next_entry()? {
                    // JSON.parse retains the first position and the last value
                    // when a document repeats an object key.
                    if let Some(entry) = entries.iter_mut().find(|entry| entry.0 == key) {
                        entry.1 = value;
                    } else {
                        entries.push((key, value));
                    }
                }
                // Object.entries visits array-index keys first, in numeric order.
                entries.sort_by_key(|(key, _)| {
                    let index = key
                        .parse::<u32>()
                        .ok()
                        .filter(|index| *index != u32::MAX && index.to_string() == *key);
                    (index.is_none(), index.unwrap_or(0))
                });
                Ok(Entries(entries))
            }
        }
        deserializer.deserialize_map(EntryVisitor(PhantomData))
    }
}

/// Missing and null medians are reported as unusable, preserving diagnostic copy.
#[derive(Debug, Default)]
enum Median {
    Number(f64),
    Null,
    #[default]
    Missing,
}

impl<'de> Deserialize<'de> for Median {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Box::<serde_json::value::RawValue>::deserialize(deserializer)?;
        if raw.get() == "null" {
            Ok(Self::Null)
        } else {
            raw.get()
                .parse()
                .map(Self::Number)
                .map_err(D::Error::custom)
        }
    }
}

/// The standard float parser matches JSON.parse's correctly rounded decimal
/// conversion. serde_json's default fast path can differ by one ULP, which can
/// turn an exact threshold boundary into a violation. Raw tokens keep this
/// precision choice local instead of changing the server's serde_json features.
#[derive(Debug)]
struct ExactNumber(f64);

impl<'de> Deserialize<'de> for ExactNumber {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Box::<serde_json::value::RawValue>::deserialize(deserializer)?;
        raw.get().parse().map(Self).map_err(D::Error::custom)
    }
}

impl Median {
    fn number(&self) -> Option<f64> {
        match self {
            Self::Number(value) => Some(*value),
            Self::Null | Self::Missing => None,
        }
    }
}

impl Display for Median {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Number(value) => formatter.write_str(&number(*value)),
            Self::Null => formatter.write_str("null"),
            Self::Missing => formatter.write_str("undefined"),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Metric {
    #[serde(default)]
    baseline_median: Median,
    #[serde(default)]
    candidate_median: Median,
    baseline_range: Vec<Option<ExactNumber>>,
    candidate_range: Vec<Option<ExactNumber>>,
    delta_percent: ExactNumber,
}

#[derive(Debug, Deserialize)]
struct Comparison {
    passed: Option<bool>,
    comparison: Option<Entries<Entries<Metric>>>,
}

impl Comparison {
    fn scenarios(&self) -> &[(String, Entries<Metric>)] {
        self.comparison.as_ref().map_or(&[], |entries| &entries.0)
    }
}

fn evaluate(comparison: &Comparison, thresholds: &[(&str, f64)]) -> Vec<String> {
    if comparison.passed != Some(true) {
        return vec!["comparison did not pass: workload or server shutdown failed".to_owned()];
    }
    let mut violations = Vec::new();
    for (scenario, metrics) in comparison.scenarios() {
        for &(metric, limit) in thresholds {
            let Some((_, entry)) = metrics.0.iter().find(|entry| entry.0 == metric) else {
                violations.push(format!(
                    "{scenario}: metric {metric} missing from comparison"
                ));
                continue;
            };
            let baseline = entry.baseline_median.number().filter(|value| *value > 0.0);
            let candidate = entry
                .candidate_median
                .number()
                .filter(|value| value.is_finite());
            let (Some(baseline), Some(candidate)) = (baseline, candidate) else {
                violations.push(format!(
                    "{scenario}: {metric} has no usable baseline ({}) or candidate ({})",
                    entry.baseline_median, entry.candidate_median
                ));
                continue;
            };
            let change = (candidate - baseline) / baseline;
            if change > limit {
                violations.push(format!(
                    "{scenario}: {metric} regressed {}% (baseline {}, candidate {}, budget +{}%)",
                    fixed(change * 100.0, 1),
                    entry.baseline_median,
                    entry.candidate_median,
                    fixed(limit * 100.0, 0)
                ));
            }
        }
    }
    violations
}

fn range(values: &[Option<ExactNumber>]) -> String {
    values
        .iter()
        .map(|value| {
            value
                .as_ref()
                .map_or(String::new(), |value| number(value.0))
        })
        .collect::<Vec<_>>()
        .join("–")
}

fn summarize(comparison: &Comparison) -> String {
    let mut lines = Vec::new();
    for (scenario, metrics) in comparison.scenarios() {
        lines.push(format!("{scenario}:"));
        for (metric, entry) in &metrics.0 {
            lines.push(format!(
                "  {metric}: baseline {} [{}] candidate {} [{}] ({}{}%)",
                entry.baseline_median,
                range(&entry.baseline_range),
                entry.candidate_median,
                range(&entry.candidate_range),
                if entry.delta_percent.0 >= 0.0 {
                    "+"
                } else {
                    ""
                },
                fixed(entry.delta_percent.0, 1)
            ));
        }
    }
    lines.join("\n")
}

/// Match the previous JavaScript reader's toFixed and shortest number text.
fn fixed(value: f64, decimals: u8) -> String {
    ryu_js::Buffer::new()
        .format_to_fixed(value, decimals)
        .to_owned()
}

fn number(value: f64) -> String {
    ryu_js::Buffer::new().format(value).to_owned()
}

pub(crate) fn run(root: &Path, arguments: &[String]) -> Result<ExitCode> {
    let [path] = arguments else {
        eprintln!("usage: cargo run -q -p xtask -- performance-thresholds <comparison.json>");
        return Ok(ExitCode::from(2));
    };
    let source = std::fs::read_to_string(root.join(path))
        .with_context(|| format!("Read benchmark comparison {path}"))?;
    let comparison: Comparison = serde_json::from_str(&source)
        .with_context(|| format!("Decode benchmark comparison {path}"))?;
    println!("{}", summarize(&comparison));
    let violations = evaluate(&comparison, &DEFAULT_THRESHOLDS);
    if violations.is_empty() {
        println!("\nWithin regression budget.");
        Ok(ExitCode::SUCCESS)
    } else {
        eprintln!("\nRegression budget violated:");
        for violation in violations {
            eprintln!("- {violation}");
        }
        Ok(ExitCode::FAILURE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn entry(baseline: f64, candidate: f64) -> Value {
        json!({
            "baselineMedian": baseline, "candidateMedian": candidate,
            "baselineRange": [baseline, baseline], "candidateRange": [candidate, candidate],
            "deltaPercent": ((candidate - baseline) / baseline) * 100.0,
        })
    }

    fn comparison(overrides: Value) -> Comparison {
        let mut metrics = json!({
            "receiveReadyP99Ms": entry(412.0, 412.0),
            "sendReadyP99Ms": entry(410.0, 410.0),
            "serverCpuPercent": entry(18.8, 18.5),
            "serverPeakRssMiB": entry(95.0, 94.0),
        });
        for (name, value) in overrides.as_object().expect("fixture overrides") {
            metrics[name] = value.clone();
        }
        let source = json!({"passed": true, "comparison": {"conference-10": metrics}}).to_string();
        serde_json::from_str(&source).expect("fixture comparison")
    }

    #[test]
    fn a_comparison_within_budget_has_no_violations() {
        let comparison = comparison(json!({}));
        assert!(evaluate(&comparison, &DEFAULT_THRESHOLDS).is_empty());
        assert!(summarize(&comparison).starts_with("conference-10:"));
    }

    #[test]
    fn failed_workload_is_a_violation_before_metrics_are_read() {
        let comparison = serde_json::from_str(r#"{"passed":false,"comparison":{}}"#).unwrap();
        assert_eq!(
            evaluate(&comparison, &DEFAULT_THRESHOLDS),
            ["comparison did not pass: workload or server shutdown failed"]
        );
    }

    #[test]
    fn regressions_past_budget_are_named_with_their_size() {
        let comparison = comparison(json!({
            "serverCpuPercent": entry(20.0, 26.0), "serverPeakRssMiB": entry(100.0, 120.0),
        }));
        let violations = evaluate(&comparison, &DEFAULT_THRESHOLDS);
        assert_eq!(violations.len(), 2);
        assert!(violations[0].contains("serverCpuPercent regressed 30.0%"));
        assert!(violations[1].contains("serverPeakRssMiB regressed 20.0%"));
    }

    #[test]
    fn improvements_and_missing_baselines_are_explicit() {
        assert!(
            evaluate(
                &comparison(json!({"serverCpuPercent": entry(20.0, 10.0)})),
                &DEFAULT_THRESHOLDS
            )
            .is_empty()
        );
        let mut invalid = entry(0.0, 10.0);
        invalid["deltaPercent"] = json!(0.0);
        let comparison = comparison(json!({"serverCpuPercent": invalid}));
        assert!(evaluate(&comparison, &DEFAULT_THRESHOLDS)[0].contains("no usable baseline"));
    }

    #[test]
    fn thresholds_are_strict_and_missing_metrics_are_named() {
        let comparison = comparison(json!({
            "serverCpuPercent": entry(100.0, 125.0), "serverPeakRssMiB": entry(100.0, 115.0),
        }));
        assert!(evaluate(&comparison, &DEFAULT_THRESHOLDS).is_empty());
        let fractional = self::comparison(json!({
            "serverCpuPercent": entry(0.000001, 0.000001 * 1.25),
        }));
        assert!(evaluate(&fractional, &DEFAULT_THRESHOLDS).is_empty());
        let missing: Comparison =
            serde_json::from_str(r#"{"passed":true,"comparison":{"conference-10":{}}}"#).unwrap();
        let violations = evaluate(&missing, &DEFAULT_THRESHOLDS);
        assert_eq!(violations.len(), 4);
        assert_eq!(
            violations[0],
            "conference-10: metric receiveReadyP99Ms missing from comparison"
        );
    }

    #[test]
    fn source_order_and_javascript_number_rounding_are_preserved() {
        let input = r#"{"passed":true,"comparison":{"z":{
            "z":{"baselineMedian":1e-7,"candidateMedian":1e21,
                 "baselineRange":[null,1e-7],"candidateRange":[1e21,1e21],"deltaPercent":2.55},
            "a":{"baselineMedian":1,"candidateMedian":1,
                 "baselineRange":[1,1],"candidateRange":[1,1],"deltaPercent":1.25}},
            "a":{}}}"#;
        let comparison: Comparison = serde_json::from_str(input).unwrap();
        assert_eq!(
            summarize(&comparison),
            concat!(
                "z:\n",
                "  z: baseline 1e-7 [–1e-7] candidate 1e+21 [1e+21–1e+21] (+2.5%)\n",
                "  a: baseline 1 [1–1] candidate 1 [1–1] (+1.3%)\na:"
            )
        );
        assert_eq!(fixed(-1.25, 1), "-1.3");
        assert_eq!(fixed(-0.001, 1), "-0.0");
        assert_eq!(fixed(-0.0, 1), "0.0");
        assert_eq!(fixed(f64::INFINITY, 1), "Infinity");
        assert_eq!(number(1189437187532196.2), "1189437187532196.2");
    }

    #[test]
    fn malformed_metric_shapes_fail_instead_of_passing_a_budget() {
        assert!(
            serde_json::from_str::<Comparison>(r#"{"passed":true,"comparison":{"a":null}}"#)
                .is_err()
        );
        assert!(
            serde_json::from_str::<Comparison>(
                r#"{"passed":true,"comparison":{"a":{"serverCpuPercent":{"candidateMedian":20}}}}"#
            )
            .is_err()
        );
    }

    #[test]
    fn integer_keys_and_duplicate_entries_follow_object_entries_order() {
        let comparison: Comparison = serde_json::from_str(
            r#"{"passed":true,"comparison":{"z":{},"10":{},"2":{},"a":{},"z":{}}}"#,
        )
        .unwrap();
        assert_eq!(summarize(&comparison), "2:\n10:\nz:\na:");
    }
}
