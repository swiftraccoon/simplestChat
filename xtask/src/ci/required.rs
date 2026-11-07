//! Canonical whole-workflow aggregation, before stored-analysis or receipt checks.
//!
//! GitHub supplies the dependency results once through `GATE_RESULTS`. This
//! boundary accepts bounded JSON with exactly the maintained ten job names and
//! typed terminal conclusions. Job metadata and outputs cannot establish success;
//! neither their content nor parser errors are printed. No subprocesses run here.

use anyhow::Result;
use serde::Deserialize;
use serde::de::{Error, MapAccess, Visitor};
use std::collections::BTreeMap;
use std::fmt;
use std::process::ExitCode;

const USAGE: &str = "usage: cargo run --locked -q -p xtask -- ci required";
const FAILURE: &str = "A required security or correctness gate failed or did not run";
const MAX_RESULTS: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
enum Gate {
    Automation,
    RustLint,
    RustTest,
    NativeDtls,
    Browser,
    Web,
    ImageDatabase,
    Deployment,
    Security,
    Codeql,
}

const GATES: [Gate; 10] = [
    Gate::Automation,
    Gate::RustLint,
    Gate::RustTest,
    Gate::NativeDtls,
    Gate::Browser,
    Gate::Web,
    Gate::ImageDatabase,
    Gate::Deployment,
    Gate::Security,
    Gate::Codeql,
];

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Conclusion {
    Success,
    Failure,
    Cancelled,
    Skipped,
}

/// Unknown GitHub metadata fields remain irrelevant, as in the previous gate.
/// Derive rejects a missing or duplicate `result` rather than choosing a value.
#[derive(Deserialize)]
struct Job {
    result: Conclusion,
}

struct Results(BTreeMap<Gate, Job>);

impl<'de> Deserialize<'de> for Results {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct Jobs;

        impl<'de> Visitor<'de> for Jobs {
            type Value = Results;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("the canonical workflow's dependency results")
            }

            fn visit_map<A: MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Results, A::Error> {
                let mut jobs = BTreeMap::new();
                while let Some((gate, job)) = map.next_entry()? {
                    if jobs.insert(gate, job).is_some() {
                        return Err(A::Error::custom("duplicate required job"));
                    }
                }
                Ok(Results(jobs))
            }
        }

        deserializer.deserialize_map(Jobs)
    }
}

/// Decide solely from an injected environment value, without mutating globals.
/// Absent, non-Unicode, oversized or malformed values all fail closed.
fn successful(input: Option<&str>) -> bool {
    let Some(input) = input.filter(|input| input.len() <= MAX_RESULTS) else {
        return false;
    };
    let Ok(Results(results)) = serde_json::from_str::<Results>(input) else {
        return false;
    };
    results.len() == GATES.len()
        && GATES.iter().all(|gate| {
            results
                .get(gate)
                .is_some_and(|job| job.result == Conclusion::Success)
        })
}

pub(super) fn run(arguments: &[String]) -> Result<ExitCode> {
    if arguments.len() == 1 && matches!(arguments[0].as_str(), "--help" | "-h") {
        println!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    }
    if !arguments.is_empty() {
        eprintln!("{USAGE}");
        return Ok(ExitCode::from(2));
    }
    let input = std::env::var_os("GATE_RESULTS");
    if successful(input.as_deref().and_then(std::ffi::OsStr::to_str)) {
        Ok(ExitCode::SUCCESS)
    } else {
        eprintln!("{FAILURE}");
        Ok(ExitCode::FAILURE)
    }
}

#[cfg(test)]
#[path = "required_tests.rs"]
mod tests;
