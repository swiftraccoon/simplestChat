//! Resource accounting uses measured process deltas, never nominal wall time.

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessSample {
    pub rss_ki_b: f64,
    pub cpu_seconds: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sample {
    pub elapsed_ms: f64,
    pub server: Option<ProcessSample>,
    pub generator: Option<ProcessSample>,
}

#[derive(Debug, Clone, Copy)]
pub enum Role {
    Server,
    Generator,
}

impl Role {
    fn name(self) -> &'static str {
        match self {
            Self::Server => "server",
            Self::Generator => "generator",
        }
    }
    fn reading(self, sample: &Sample) -> Option<ProcessSample> {
        match self {
            Self::Server => sample.server,
            Self::Generator => sample.generator,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub samples: usize,
    pub failed_samples: usize,
    pub sampled_duration_seconds: f64,
    pub requested_duration_seconds: f64,
    pub cpu_seconds: f64,
    pub cpu_percent_of_one_core: f64,
    pub peak_rss_mi_b: f64,
    pub median_rss_mi_b: f64,
}

/// The caller establishes finite inputs. Preserve numeric ascending order and
/// midpoint arithmetic used by retained JavaScript comparison artifacts.
pub(super) fn median(values: &[f64]) -> f64 {
    let mut values = values.to_vec();
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len() % 2 == 1 {
        values[middle]
    } else {
        (values[middle - 1] + values[middle]) / 2.0
    }
}

/// Require two observations and coverage within1.5seconds of the requested
/// window, with no more than10% failed samples. Sampling remains descriptive;
/// this does not establish production capacity or a regression budget.
pub fn summarize(samples: &[Sample], role: Role, start: f64, end: f64) -> Result<Summary> {
    let in_window: Vec<_> = samples
        .iter()
        .filter(|sample| sample.elapsed_ms >= start && sample.elapsed_ms <= end)
        .collect();
    let readings: Vec<_> = in_window
        .iter()
        .filter_map(|sample| role.reading(sample).map(|reading| (*sample, reading)))
        .collect();
    let failures = in_window.len() - readings.len();
    ensure!(
        readings.len() >= 2,
        "Insufficient {} resource samples in measurement window",
        role.name()
    );
    let (first_sample, first) = readings[0];
    let (last_sample, last) = readings[readings.len() - 1];
    let cpu_seconds = last.cpu_seconds - first.cpu_seconds;
    let sampled_duration_seconds = (last_sample.elapsed_ms - first_sample.elapsed_ms) / 1000.0;
    let requested_duration_seconds = (end - start) / 1000.0;
    ensure!(
        requested_duration_seconds - sampled_duration_seconds <= 1.5,
        "{} resource samples under-cover the measurement window",
        role.name()
    );
    ensure!(
        failures as f64 <= 0.1 * in_window.len() as f64,
        "Too many failed {} resource samples",
        role.name()
    );
    let memory: Vec<_> = readings
        .iter()
        .map(|(_, reading)| reading.rss_ki_b)
        .collect();
    Ok(Summary {
        samples: readings.len(),
        failed_samples: failures,
        sampled_duration_seconds,
        requested_duration_seconds,
        cpu_seconds,
        cpu_percent_of_one_core: cpu_seconds / sampled_duration_seconds * 100.0,
        peak_rss_mi_b: memory.iter().copied().fold(f64::NEG_INFINITY, f64::max) / 1024.0,
        median_rss_mi_b: median(&memory) / 1024.0,
    })
}

/// Parse C-locale `ps` CPU text, including days and fractional seconds. The
/// live macOS sampler supplies the exact `rss=,time=` shape.
pub fn cpu_seconds(text: &str) -> Result<f64> {
    let mut days = text.split('-');
    let (day, clock) = if text.contains('-') {
        (
            super::options::number(days.next().unwrap()).unwrap_or(f64::NAN),
            days.next().unwrap(),
        )
    } else {
        (0.0, text)
    };
    let parts: Vec<_> = clock.split(':').map(super::options::number).collect();
    ensure!(
        (2..=3).contains(&parts.len()) && parts.iter().all(Option::is_some),
        "Invalid ps CPU time"
    );
    Ok(day * 86400.0
        + parts
            .iter()
            .rev()
            .enumerate()
            .map(|(index, part)| part.unwrap() * 60_f64.powi(index as i32))
            .sum::<f64>())
}

/// Linux stat names can contain spaces or parentheses. Locate the last close
/// parenthesis before reading utime/stime ticks and RSS pages.
pub fn linux_sample(stat: &str, clock_ticks: f64, page_size: f64) -> Result<ProcessSample> {
    let close = stat
        .rfind(')')
        .ok_or_else(|| anyhow::anyhow!("Invalid /proc stat"))?;
    let fields: Vec<_> = stat
        .get(close + 2..)
        .unwrap_or("")
        .split_whitespace()
        .collect();
    let field = |index: usize| {
        fields
            .get(index)
            .and_then(|value| super::options::number(value))
    };
    let (utime, stime, rss) = (field(11), field(12), field(21));
    ensure!(
        utime.is_some() && stime.is_some() && rss.is_some() && clock_ticks > 0.0 && page_size > 0.0,
        "Invalid /proc stat"
    );
    Ok(ProcessSample {
        rss_ki_b: rss.unwrap() * page_size / 1024.0,
        cpu_seconds: (utime.unwrap() + stime.unwrap()) / clock_ticks,
    })
}
