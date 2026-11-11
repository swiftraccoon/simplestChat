//! The interface's traffic per billing period beside the provider's
//! allowance. Kernel counters restart with the interface, so the period's
//! totals live in the state directory and grow by each sample's delta.

use super::failure::{Failure, Result, require};
use super::state;
use super::text::{Sample, gauge};
use crate::json::Json;
use chrono::{Datelike, NaiveDate};
use std::path::Path;

/// Providers sell transfer in decimal terabytes.
const TERABYTE: f64 = 1_000_000_000_000.0;
const MAX_RESET_DAY: i128 = 28;

/// The interface, allowance and reset day the collector was configured with.
#[derive(Debug, PartialEq)]
pub(super) struct Settings {
    pub interface: String,
    pub allowance_tb: f64,
    pub reset_day: u32,
}

/// A whole number spelled as a JSON integer; booleans and floats are not one.
fn whole(value: Option<&Json>) -> Option<i128> {
    match value {
        Some(Json::Integer(text)) => text.parse().ok(),
        _ => None,
    }
}

/// Read the three settings, each with its default, refusing any other shape.
pub(super) fn settings(configuration: &Json) -> Result<Settings> {
    let interface = match configuration.get("transfer_interface") {
        None => Some("eth0"),
        Some(value) => value.string(),
    };
    let named = |name: &&str| {
        (1..=15).contains(&name.len())
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    };
    let interface = interface.filter(named);
    require(interface.is_some(), "Invalid transfer interface")?;
    let allowance = match configuration.get("transfer_allowance_tb") {
        None => Some(0.0),
        Some(value @ (Json::Integer(_) | Json::Float(_))) => value.number(),
        Some(_) => None,
    };
    let allowance = allowance.filter(|allowance| *allowance >= 0.0);
    require(allowance.is_some(), "Invalid transfer allowance")?;
    let reset_day = match configuration.get("transfer_reset_day") {
        None => Some(1),
        value => whole(value),
    };
    let reset_day = reset_day.filter(|day| (1..=MAX_RESET_DAY).contains(day));
    require(reset_day.is_some(), "Invalid transfer reset day")?;
    Ok(Settings {
        interface: interface.unwrap_or_default().to_owned(),
        allowance_tb: allowance.unwrap_or_default(),
        reset_day: u32::try_from(reset_day.unwrap_or(1)).map_err(|_| Failure::Value)?,
    })
}

/// The first day of the billing period `today` falls in.
pub(super) fn period_start(today: NaiveDate, reset_day: u32) -> Option<NaiveDate> {
    if today.day() >= reset_day {
        return today.with_day(reset_day);
    }
    today.with_day(1)?.pred_opt()?.with_day(reset_day)
}

/// The period's record: totals so far and the counters last seen.
#[derive(serde::Serialize)]
struct Record {
    period: String,
    tx_bytes: i128,
    rx_bytes: i128,
    last_tx: u64,
    last_rx: u64,
}

/// Sample both counters, grow the period's totals and report them. A counter
/// below its last reading restarted and contributes what it holds. The first
/// sample of a period counts nothing: the counter's earlier history is unknown.
pub(super) fn transfer(
    lines: &mut Vec<String>,
    configuration: &Json,
    network: &Path,
    state_directory: &Path,
    today: NaiveDate,
) -> Result<()> {
    let settings = settings(configuration)?;
    let counter = |direction: &str| -> Result<u64> {
        let statistics = network.join(&settings.interface).join("statistics");
        let text = std::fs::read_to_string(statistics.join(format!("{direction}_bytes")))?;
        text.trim().parse().map_err(|_| Failure::Value)
    };
    let counters = [("tx", counter("tx")?), ("rx", counter("rx")?)];
    let period = period_start(today, settings.reset_day).ok_or(Failure::Value)?;
    let path = state_directory.join("transfer.json");
    let previous = state::object_or_empty(&path, 1024 * 1024)?;
    let label = period.format("%Y-%m-%d").to_string();
    let same = previous.get("period").and_then(Json::string) == Some(label.as_str());
    let mut totals = [0_i128; 2];
    for (at, (direction, counter)) in counters.iter().enumerate() {
        let field = |name: String| whole(previous.get(&name)).filter(|_| same);
        let mut running = field(format!("{direction}_bytes")).unwrap_or(0);
        if let Some(last) = field(format!("last_{direction}")) {
            let counter = i128::from(*counter);
            running += if counter >= last {
                counter - last
            } else {
                counter
            };
        }
        totals[at] = running;
    }
    let record = Record {
        period: label,
        tx_bytes: totals[0],
        rx_bytes: totals[1],
        last_tx: counters[0].1,
        last_rx: counters[1].1,
    };
    state::atomic_json(&path, &record)?;
    gauge(
        lines,
        "transfer_period_tx_bytes",
        Sample::Whole(totals[0]),
        None,
    );
    gauge(
        lines,
        "transfer_period_rx_bytes",
        Sample::Whole(totals[1]),
        None,
    );
    let allowance = Sample::Float(settings.allowance_tb * TERABYTE);
    gauge(lines, "transfer_allowance_bytes", allowance, None);
    let midnight = period.and_hms_opt(0, 0, 0).ok_or(Failure::Value)?;
    #[expect(
        clippy::cast_precision_loss,
        reason = "epoch seconds are far below 2^53"
    )]
    let start = Sample::Float(midnight.and_utc().timestamp() as f64);
    gauge(
        lines,
        "transfer_period_start_timestamp_seconds",
        start,
        None,
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::recorded::reference;
    use super::super::state::tests::Scratch;
    use super::*;

    fn date(text: &serde_json::Value) -> NaiveDate {
        NaiveDate::parse_from_str(text.as_str().unwrap(), "%Y-%m-%d").unwrap()
    }

    /// Reset days before, on and after today, month ends and a leap year.
    #[test]
    fn billing_periods_start_on_the_recorded_days() {
        let recorded = reference();
        let cases = recorded["periodStart"].as_array().unwrap();
        assert_eq!(cases.len(), 10);
        for case in cases {
            let reset = u32::try_from(case["resetDay"].as_u64().unwrap()).unwrap();
            let start = period_start(date(&case["today"]), reset).unwrap();
            assert_eq!(start, date(&case["value"]), "{case}");
        }
    }

    /// Defaults, each accepted shape and each recorded refusal.
    #[test]
    fn settings_are_read_or_refused_as_recorded() {
        let recorded = reference();
        let cases = recorded["transferSettings"].as_array().unwrap();
        assert_eq!(cases.len(), 16);
        for case in cases {
            let text = case["configuration"].to_string();
            let answer = settings(&state::decode(text.as_bytes()).unwrap());
            match (answer, case.get("refused")) {
                (Err(Failure::Refused(message)), Some(expected)) => {
                    assert_eq!(message, expected.as_str().unwrap(), "{case}");
                }
                (Ok(settings), None) => {
                    let expected = case["value"].as_array().unwrap();
                    assert_eq!(settings.interface, expected[0].as_str().unwrap());
                    assert_eq!(settings.allowance_tb, expected[1].as_f64().unwrap());
                    assert_eq!(u64::from(settings.reset_day), expected[2].as_u64().unwrap());
                }
                other => panic!("{case}: {other:?}"),
            }
        }
    }

    /// Seven samples: a first reading, growth, a restarted counter, a new
    /// period and a quiet day give the recorded lines and the recorded file.
    #[test]
    fn totals_follow_the_recorded_samples_restarts_and_period_change() {
        let recorded = reference();
        let scratch = Scratch::new();
        let statistics = scratch.path().join("net/eth0/statistics");
        std::fs::create_dir_all(&statistics).unwrap();
        let state_directory = scratch.path().join("state");
        std::fs::create_dir(&state_directory).unwrap();
        let text = recorded["transfer"]["configuration"].to_string();
        let configuration = state::decode(text.as_bytes()).unwrap();
        let samples = recorded["transfer"]["samples"].as_array().unwrap();
        assert_eq!(samples.len(), 7);
        for sample in samples {
            for direction in ["tx", "rx"] {
                let value = format!("{}\n", sample[direction]);
                std::fs::write(statistics.join(format!("{direction}_bytes")), value).unwrap();
            }
            let mut lines = Vec::new();
            let network = scratch.path().join("net");
            let today = date(&sample["today"]);
            transfer(
                &mut lines,
                &configuration,
                &network,
                &state_directory,
                today,
            )
            .unwrap();
            let expected = sample["lines"].as_array().unwrap();
            let expected = expected.iter().map(|line| line.as_str().unwrap());
            assert_eq!(lines, expected.collect::<Vec<_>>(), "{}", sample["today"]);
            let written = std::fs::read_to_string(state_directory.join("transfer.json"));
            assert_eq!(written.unwrap(), sample["stateText"].as_str().unwrap());
        }
    }

    /// A missing counter, a counter that is not a number and a record that is
    /// not an object are faults, and a hand-edited record cannot poison totals.
    #[test]
    fn unreadable_counters_and_foreign_records_are_faults_not_totals() {
        let scratch = Scratch::new();
        let statistics = scratch.path().join("net/eth0/statistics");
        std::fs::create_dir_all(&statistics).unwrap();
        let empty = state::decode(b"{}").unwrap();
        let today = NaiveDate::from_ymd_opt(2026, 11, 11).unwrap();
        let network = scratch.path().join("net");
        let run =
            |lines: &mut Vec<String>| transfer(lines, &empty, &network, scratch.path(), today);
        assert_eq!(run(&mut Vec::new()), Err(Failure::Os));
        std::fs::write(statistics.join("tx_bytes"), "many\n").unwrap();
        std::fs::write(statistics.join("rx_bytes"), "5\n").unwrap();
        assert_eq!(run(&mut Vec::new()), Err(Failure::Value));
        std::fs::write(statistics.join("tx_bytes"), "7\n").unwrap();
        std::fs::write(scratch.path().join("transfer.json"), "[1]").unwrap();
        assert_eq!(run(&mut Vec::new()), Err(Failure::Value));
        let edited =
            r#"{"period":"2026-11-01","tx_bytes":true,"last_tx":1.5,"rx_bytes":"9","last_rx":2}"#;
        std::fs::write(scratch.path().join("transfer.json"), edited).unwrap();
        let mut lines = Vec::new();
        run(&mut lines).unwrap();
        assert_eq!(lines[0], "simplestchat_ops_transfer_period_tx_bytes 0");
        assert_eq!(lines[1], "simplestchat_ops_transfer_period_rx_bytes 3");
    }
}
