//! Parse the measured command without starting a runtime or touching an engine.

use super::types::{Browser, Workload};
use anyhow::{Result, bail, ensure};
use std::ffi::OsString;
use std::path::PathBuf;

pub(super) const USAGE: &str = "Usage: load_test capacity run --server-image IMAGE \
--generator-image IMAGE [--engine docker|podman] [--output DIRECTORY] \
[--label LABEL] [--workloads meetings large-meeting webinar] [--quick] \
[--steps N] [--first-size N] [--meeting-size N] [--server-cpus N] \
[--generator-cpus N] [--app-cpus N] [--app-memory-mib N] [--worker-threshold N] \
[--server-env KEY=VALUE] [--capture 720p|1080p] [--speakers N] [--audio-only] \
[--chat-interval-ms N] [--viewport-width N] [--pixel-ratio N] [--layout classic|modern] \
[--monthly-price N] [--egress-price-per-gb N] [--included-egress-tb N] [--port-mbps N]";

#[derive(Clone, Debug)]
pub(super) struct Options {
    pub(super) server_image: String,
    pub(super) generator_image: String,
    pub(super) engine: Option<String>,
    pub(super) output: Option<PathBuf>,
    pub(super) label: String,
    pub(super) workloads: Vec<Workload>,
    pub(super) meeting_size: i64,
    pub(super) server_cpus: Option<f64>,
    pub(super) generator_cpus: Option<f64>,
    pub(super) app_cpus: Option<f64>,
    pub(super) app_memory_mib: Option<i64>,
    pub(super) worker_threshold: f64,
    pub(super) quick: bool,
    pub(super) first_size: Option<i64>,
    pub(super) steps: Option<usize>,
    pub(super) server_env: Vec<(String, String)>,
    pub(super) browser: Browser,
    pub(super) monthly_price: Option<f64>,
    pub(super) egress_price_per_gb: f64,
    pub(super) included_egress_tb: f64,
    pub(super) port_mbps: f64,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            server_image: String::new(),
            generator_image: String::new(),
            engine: None,
            output: None,
            label: String::new(),
            workloads: Workload::ALL.to_vec(),
            meeting_size: 5,
            server_cpus: None,
            generator_cpus: None,
            app_cpus: None,
            app_memory_mib: None,
            worker_threshold: 0.7,
            quick: false,
            first_size: None,
            steps: None,
            server_env: Vec::new(),
            browser: Browser::default(),
            monthly_price: None,
            egress_price_per_gb: 0.0,
            included_egress_tb: 0.0,
            port_mbps: 0.0,
        }
    }
}

pub(super) fn server_env(value: &str) -> Result<(String, String)> {
    let Some((key, setting)) = value.split_once('=') else {
        bail!("--server-env takes KEY=VALUE with an upper-case KEY, not {value:?}");
    };
    ensure!(
        !key.is_empty()
            && key.as_bytes()[0].is_ascii_uppercase()
            && key
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_'),
        "--server-env takes KEY=VALUE with an upper-case KEY, not {value:?}"
    );
    ensure!(
        ![
            "BIND_ADDR",
            "PORT",
            "ANNOUNCE_IP",
            "MEDIA_WORKERS",
            "ALLOW_AD_HOC_ROOMS",
            "ALLOWED_ORIGINS",
            "METRICS_TOKEN",
            "CPU_SATURATION_WORKER_UTILIZATION"
        ]
        .contains(&key),
        "--server-env cannot replace {key}, which the tool sets itself"
    );
    Ok((key.into(), setting.into()))
}

fn workload(value: &str) -> Result<Workload> {
    Workload::ALL
        .into_iter()
        .find(|workload| workload.name() == value)
        .ok_or_else(|| anyhow::anyhow!("unknown workload {value:?}"))
}

pub(super) fn parse(arguments: &[OsString]) -> Result<Options> {
    let mut options = Options::default();
    let mut index = 0;
    while index < arguments.len() {
        let flag = arguments[index]
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("non-UTF8 option"))?;
        index += 1;
        if flag == "--quick" {
            options.quick = true;
            continue;
        }
        if flag == "--audio-only" {
            options.browser.audio_only = true;
            continue;
        }
        if flag == "--workloads" {
            options.workloads.clear();
            while let Some(value) = arguments.get(index).and_then(|value| value.to_str()) {
                if value.starts_with('-') {
                    break;
                }
                options.workloads.push(workload(value)?);
                index += 1;
            }
            ensure!(
                !options.workloads.is_empty(),
                "--workloads needs at least one workload"
            );
            continue;
        }
        let raw = arguments
            .get(index)
            .ok_or_else(|| anyhow::anyhow!("{flag} needs a value"))?;
        index += 1;
        if flag == "--output" {
            options.output = Some(PathBuf::from(raw));
            continue;
        }
        let value = raw
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("{flag} needs UTF8 text"))?;
        match flag {
            "--server-image" => options.server_image = value.into(),
            "--generator-image" => options.generator_image = value.into(),
            "--engine" => {
                ensure!(["docker", "podman"].contains(&value), "unknown engine");
                options.engine = Some(value.into());
            }
            "--label" => options.label = value.into(),
            "--meeting-size" => options.meeting_size = value.parse()?,
            "--server-cpus" => options.server_cpus = Some(value.parse()?),
            "--generator-cpus" => options.generator_cpus = Some(value.parse()?),
            "--app-cpus" => options.app_cpus = Some(value.parse()?),
            "--app-memory-mib" => options.app_memory_mib = Some(value.parse()?),
            "--worker-threshold" => options.worker_threshold = value.parse()?,
            "--first-size" => options.first_size = Some(value.parse()?),
            "--steps" => options.steps = Some(value.parse()?),
            "--server-env" => options.server_env.push(server_env(value)?),
            "--capture" => {
                ensure!(["720p", "1080p"].contains(&value), "unknown capture");
                options.browser.capture = value.into();
            }
            "--speakers" => options.browser.speakers = value.parse()?,
            "--chat-interval-ms" => options.browser.chat_interval_ms = value.parse()?,
            "--viewport-width" => options.browser.viewport_width = value.parse()?,
            "--pixel-ratio" => options.browser.pixel_ratio = value.parse()?,
            "--layout" => {
                ensure!(["classic", "modern"].contains(&value), "unknown layout");
                options.browser.layout = value.into();
            }
            "--monthly-price" => options.monthly_price = Some(value.parse()?),
            "--egress-price-per-gb" => options.egress_price_per_gb = value.parse()?,
            "--included-egress-tb" => options.included_egress_tb = value.parse()?,
            "--port-mbps" => options.port_mbps = value.parse()?,
            _ => bail!("unknown option {flag}"),
        }
    }
    ensure!(
        !options.server_image.is_empty() && !options.generator_image.is_empty(),
        "--server-image and --generator-image are required"
    );
    for image in [&options.server_image, &options.generator_image] {
        ensure!(
            !image.starts_with('-') && !image.bytes().any(|byte| byte.is_ascii_whitespace()),
            "image reference must be one non-option argument"
        );
    }
    ensure!(
        options.meeting_size >= 2 && options.meeting_size <= 10_000,
        "meeting size must be 2..10000"
    );
    for value in [
        options.server_cpus,
        options.generator_cpus,
        options.app_cpus,
    ]
    .into_iter()
    .flatten()
    {
        ensure!(
            value.is_finite() && value > 0.0,
            "CPU quotas must be finite and positive"
        );
    }
    ensure!(
        options.worker_threshold.is_finite()
            && options.worker_threshold > 0.0
            && options.worker_threshold <= 1.0,
        "worker threshold must be in (0, 1]"
    );
    ensure!(
        options.app_memory_mib.is_none_or(|value| value >= 0),
        "app memory cannot be negative"
    );
    ensure!(
        options
            .first_size
            .is_none_or(|value| (0..=10_000_000).contains(&value)),
        "first size is outside the supported range"
    );
    ensure!(
        options.browser.speakers >= 1
            && options.browser.viewport_width > 0
            && options.browser.pixel_ratio.is_finite()
            && options.browser.pixel_ratio > 0.0,
        "browser dimensions, ratio and speakers must be positive"
    );
    ensure!(
        options.browser.chat_interval_ms == 0
            || (1000..=600000).contains(&options.browser.chat_interval_ms),
        "chat interval must be disabled (0) or 1000..600000 ms"
    );
    for value in [
        options.monthly_price,
        Some(options.egress_price_per_gb),
        Some(options.included_egress_tb),
        Some(options.port_mbps),
    ]
    .into_iter()
    .flatten()
    {
        ensure!(
            value.is_finite() && value >= 0.0,
            "prices and network limits must be finite and nonnegative"
        );
    }
    Ok(options)
}
