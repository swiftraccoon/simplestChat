//! Literal command arguments and the minimal child environment. Credentials
//! belong only to the owned server process and never the retained invocation.

use super::options::{Departure, Detail, Options, Purpose, Scenario};
use anyhow::{Result, ensure};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::Path;

pub fn clean(environment: &BTreeMap<OsString, OsString>) -> BTreeMap<OsString, OsString> {
    ["PATH", "TMPDIR", "LANG", "SYSTEMROOT"]
        .into_iter()
        .filter_map(|key| environment.get_key_value(&OsString::from(key)))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

pub fn departure(value: Departure) -> &'static str {
    match value {
        Departure::Abrupt => "abrupt",
        Departure::ExplicitLeave => "explicit-leave",
    }
}

pub fn diagnostic_environment(options: &Options, directory: &Path) -> BTreeMap<String, String> {
    if !options.full_diagnostics() {
        return BTreeMap::new();
    }
    BTreeMap::from([
        (
            "DIAGNOSTICS_PATH".into(),
            directory
                .join("server-diagnostics.jsonl")
                .to_string_lossy()
                .into_owned(),
        ),
        ("DIAGNOSTICS_MAX_RECORDS".into(), "10000".into()),
        ("MEDIA_DIAGNOSTICS_ENABLED".into(), "true".into()),
        (
            "DIAGNOSTICS_DURATION_SECS".into(),
            300.max(options.ramp_up + options.warmup + options.duration + 200)
                .to_string(),
        ),
    ])
}

pub fn server_environment(
    options: &Options,
    directory: &Path,
    token: &str,
    environment: &BTreeMap<OsString, OsString>,
) -> BTreeMap<OsString, OsString> {
    let mut result = clean(environment);
    for (key, value) in [
        ("BIND_ADDR", "127.0.0.1".into()),
        ("PORT", options.port.to_string()),
        ("ANNOUNCE_IP", "127.0.0.1".into()),
        ("MEDIA_WORKERS", options.workers.to_string()),
        ("WEBRTC_SERVER_PORT_BASE", options.udp_port.to_string()),
        ("ALLOW_AD_HOC_ROOMS", "true".into()),
        (
            "ALLOWED_ORIGINS",
            format!("http://127.0.0.1:{}", options.port),
        ),
        ("REGISTRATION_ENABLED", "false".into()),
        ("MAX_CONNECTIONS_PER_IP", "128".into()),
        ("WS_HANDSHAKES_PER_MINUTE", "600".into()),
        ("METRICS_TOKEN", token.into()),
        ("RUST_LOG", server_log(options).into()),
    ] {
        result.insert(key.into(), value.into());
    }
    for (key, value) in diagnostic_environment(options, directory) {
        result.insert(key.into(), value.into());
    }
    result
}

pub fn server_log(options: &Options) -> &'static str {
    if options.full_diagnostics() {
        "warn,simplestchat::media::transport_manager=info,simplestchat::lifecycle=debug"
    } else {
        "error"
    }
}

pub fn generator_log(options: &Options) -> &'static str {
    if options.full_diagnostics() {
        "warn,load_test=info"
    } else {
        "error"
    }
}

pub fn capture(options: &Options, directory: &Path) -> Result<Vec<String>> {
    let interface = options.capture_interface.as_deref().unwrap_or("");
    ensure!(
        options.purpose == Purpose::Diagnostic && ["lo", "lo0"].contains(&interface),
        "Packet capture requires diagnostic mode and a loopback interface"
    );
    let limit = if options.diagnostic_detail == Detail::CaptureOnly {
        "2000000"
    } else {
        "500000"
    };
    let mut args: Vec<String> = [
        "-i", interface, "-p", "-nn", "-s", "64", "-B", "4096", "-U", "-c", limit, "-w",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    args.push(
        directory
            .join("media-headers.pcap")
            .to_string_lossy()
            .into_owned(),
    );
    args.push(format!(
        "udp and host 127.0.0.1 and portrange {}-{}",
        options.udp_port,
        u32::from(options.udp_port) + options.workers - 1
    ));
    Ok(args)
}

pub struct Invocation<'a> {
    pub scenario: &'a Scenario,
    pub directory: &'a Path,
    pub name: &'a str,
    pub room: &'a str,
    pub server_revision: &'a str,
    pub generator_revision: &'a str,
}

pub fn generator(options: &Options, invocation: Invocation<'_>) -> Result<Vec<String>> {
    let mut args = vec![
        "--server".into(),
        format!("ws://127.0.0.1:{}/ws", options.port),
        "--clients".into(),
        invocation.scenario.clients.to_string(),
        "--rooms".into(),
        invocation.scenario.rooms.to_string(),
        "--room".into(),
        invocation.room.into(),
        "--duration".into(),
        options.duration.to_string(),
        "--ramp-up".into(),
        options.ramp_up.to_string(),
        "--warmup".into(),
        options.warmup.to_string(),
        "--mode".into(),
        invocation.scenario.mode.clone(),
        "--quality".into(),
        "480p".into(),
        "--fps".into(),
        "30".into(),
        "--max-audio".into(),
        "4".into(),
        "--max-video".into(),
        "4".into(),
        "--output-dir".into(),
        invocation.directory.to_string_lossy().into_owned(),
        "--run-label".into(),
        invocation.name.into(),
        "--server-revision".into(),
        invocation.server_revision.into(),
        "--generator-revision".into(),
        invocation.generator_revision.into(),
    ];
    args.extend(invocation.scenario.extra.iter().cloned());
    args.extend(super::subscription::arguments(&options.into())?);
    if options.full_diagnostics() {
        args.extend([
            "--diagnostics".into(),
            "--departure".into(),
            departure(options.departure).into(),
        ]);
    }
    Ok(args)
}

/// Capability-bearing and inherited process entries never enter invocation.json.
pub fn public_configuration(
    environment: &BTreeMap<OsString, OsString>,
) -> BTreeMap<String, String> {
    environment
        .iter()
        .filter_map(|(key, value)| {
            let key = key.to_str()?;
            if ["METRICS_TOKEN", "PATH", "TMPDIR"].contains(&key) {
                return None;
            }
            Some((key.to_owned(), value.to_str()?.to_owned()))
        })
        .collect()
}
