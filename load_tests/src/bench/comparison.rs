//! Descriptive paired medians, with no inferred regression or capacity budget.

use anyhow::{Result, ensure};
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;

fn identity(row: &Value) -> Result<String> {
    let mode = if row["subscriptionPlan"].is_null() {
        "fifo"
    } else {
        row["subscriptionPlan"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing or malformed subscription graph identity"))?
    };
    if mode == "fifo"
        && row["subscriptionSeed"].is_null()
        && row["subscriptionPlanSha256"].is_null()
    {
        return Ok("fifo".to_owned());
    }
    let seed = row["subscriptionSeed"]
        .as_f64()
        .filter(|value| {
            value.is_finite() && value.fract() == 0.0 && *value >= 0.0 && *value <= u32::MAX as f64
        })
        .map(|value| value as u32);
    let hash = row["subscriptionPlanSha256"].as_str().filter(|text| {
        text.len() == 64
            && text
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    });
    ensure!(
        ["ring-v1", "hotspot-v1"].contains(&mode) && seed.is_some() && hash.is_some(),
        "Missing or malformed subscription graph identity"
    );
    Ok(format!("{mode}:{}:{}", seed.unwrap(), hash.unwrap()))
}

/// Each scenario requires equal nonempty baseline/candidate sets, finite fields
/// and one graph identity. Input order and A/B scheduling never select winners.
pub fn compare(rows: &[Value]) -> Result<Value> {
    ensure!(
        !rows.iter().any(|row| row["purpose"] == "diagnostic"),
        "Diagnostic runs are not performance comparisons"
    );
    ensure!(
        super::verdict::performance(rows).passed,
        "Performance comparison requires passing workloads and clean server shutdowns"
    );
    let fields = [
        "joinP99Ms",
        "sendReadyP99Ms",
        "receiveReadyP99Ms",
        "receivedPacketsPerSecond",
        "serverCpuPercent",
        "serverPeakRssMiB",
        "generatorCpuPercent",
        "generatorPeakRssMiB",
    ];
    let scenarios: BTreeSet<_> = rows.iter().map(|row| row["scenario"].as_str()).collect();
    let mut report = Map::new();
    for scenario in scenarios {
        let scenario = scenario.ok_or_else(|| anyhow::anyhow!("Missing comparison scenario"))?;
        let selected: Vec<_> = rows
            .iter()
            .filter(|row| row["scenario"] == scenario)
            .collect();
        let identities = selected
            .iter()
            .map(|row| identity(row))
            .collect::<Result<BTreeSet<_>>>()?;
        ensure!(identities.len() == 1, "Subscription graph mismatch");
        let mut groups = Map::new();
        for field in fields {
            let values = |variant| {
                selected
                    .iter()
                    .filter(|row| row["variant"] == variant)
                    .map(|row| {
                        row[field]
                            .as_f64()
                            .filter(|value| value.is_finite())
                            .ok_or_else(|| anyhow::anyhow!("Incomplete comparison"))
                    })
                    .collect::<Result<Vec<_>>>()
            };
            let baseline = values("baseline")?;
            let candidate = values("candidate")?;
            ensure!(
                !baseline.is_empty() && baseline.len() == candidate.len(),
                "Incomplete comparison"
            );
            let before = super::resources::median(&baseline);
            let after = super::resources::median(&candidate);
            let range = |values: &[f64]| {
                [
                    values.iter().copied().fold(f64::INFINITY, f64::min),
                    values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                ]
            };
            let delta_percent = if before == 0.0 {
                None
            } else {
                Some((after - before) / before * 100.0)
            };
            groups.insert(
                field.to_owned(),
                json!({"baselineMedian":before, "candidateMedian":after,
                "delta":after - before, "deltaPercent":delta_percent,
                "baselineRange":range(&baseline), "candidateRange":range(&candidate)}),
            );
        }
        report.insert(scenario.to_owned(), groups.into());
    }
    Ok(report.into())
}
