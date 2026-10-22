//! Literal structured argv for loopback-only server, generator and sidecars.

use super::{
    options::{Options, Workload},
    ownership::LABEL,
};
use crate::bench::subscription;
use anyhow::{Result, ensure};
use std::collections::BTreeMap;

const WIRED: [&str; 9] = [
    "BIND_ADDR",
    "PORT",
    "ANNOUNCE_IP",
    "MEDIA_WORKERS",
    "WEBRTC_SERVER_PORT_BASE",
    "ALLOWED_ORIGINS",
    "METRICS_TOKEN",
    "MAX_CONNECTIONS",
    "MAX_CONNECTIONS_PER_IP",
];

pub(super) fn environment(
    options: &Options,
    scenario: &Workload,
    token: &str,
) -> Result<BTreeMap<String, String>> {
    let mut env: BTreeMap<String, String> = [
        ("BIND_ADDR", "0.0.0.0".into()),
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
        ("MAX_CONNECTIONS", options.max_connections.to_string()),
        (
            "MAX_CONNECTIONS_PER_IP",
            options
                .max_connections
                .min(scenario.clients + 16)
                .to_string(),
        ),
        ("WS_HANDSHAKES_PER_MINUTE", "600".into()),
        ("METRICS_TOKEN", token.into()),
        ("RUST_LOG", "simplestchat=info,mediasoup=warn".into()),
    ]
    .into_iter()
    .map(|(key, value)| (key.into(), value))
    .collect();
    for (key, value) in &options.server_env {
        ensure!(
            !WIRED.contains(&key.as_str()),
            "--server-env cannot override {key}"
        );
        env.insert(key.clone(), value.clone());
    }
    Ok(env)
}
fn identity(name: &str, run: &str) -> Vec<String> {
    vec![
        "--pull".into(),
        "never".into(),
        "--name".into(),
        name.into(),
        "--label".into(),
        format!("{LABEL}={run}"),
    ]
}
pub(super) fn server(
    options: &Options,
    name: &str,
    run: &str,
    env: &BTreeMap<String, String>,
) -> Vec<String> {
    let mut args = vec!["run".into(), "--detach".into()];
    args.extend(identity(name, run));
    args.extend([
        "--cpus".into(),
        ryu_js::Buffer::new().format(options.cpus).into(),
        "--memory".into(),
        options.memory.clone(),
        "--pids-limit".into(),
        "512".into(),
        "--publish".into(),
        format!("127.0.0.1:{0}:{0}/tcp", options.port),
    ]);
    for (key, value) in env {
        args.extend(["--env".into(), format!("{key}={value}")]);
    }
    args.push(options.server_image.clone());
    args
}
pub(super) fn generator(
    options: &Options,
    name: &str,
    run: &str,
    server: &str,
    arguments: &[String],
) -> Vec<String> {
    let mut args = vec!["run".into(), "--detach".into()];
    args.extend(identity(name, run));
    args.extend([
        "--network".into(),
        format!("container:{server}"),
        "--cpus".into(),
        ryu_js::Buffer::new().format(options.generator_cpus).into(),
        "--env".into(),
        "RUST_LOG=warn".into(),
        options.generator_image.clone(),
    ]);
    args.extend(arguments.iter().cloned());
    args
}
pub(super) fn generator_args(
    options: &Options,
    scenario: &Workload,
    name: &str,
    room: &str,
    revisions: (&str, &str),
) -> Result<Vec<String>> {
    let mut args = vec![
        "--server".into(),
        format!("ws://127.0.0.1:{}/ws", options.port),
        "--clients".into(),
        scenario.clients.to_string(),
        "--rooms".into(),
        scenario.rooms.to_string(),
        "--room".into(),
        room.into(),
        "--duration".into(),
        options.duration.to_string(),
        "--ramp-up".into(),
        options.ramp_up.to_string(),
        "--warmup".into(),
        options.warmup.to_string(),
        "--mode".into(),
        scenario.mode.clone(),
        "--quality".into(),
        "480p".into(),
        "--fps".into(),
        "30".into(),
        "--max-audio".into(),
        "4".into(),
        "--max-video".into(),
        "4".into(),
        "--output-dir".into(),
        "/results".into(),
        "--run-label".into(),
        name.into(),
        "--server-revision".into(),
        revisions.0.chars().take(16).collect(),
        "--generator-revision".into(),
        revisions.1.chars().take(16).collect(),
    ];
    args.extend(scenario.extra.iter().cloned());
    args.extend(subscription::arguments(&options.graph())?);
    Ok(args)
}
pub(super) fn netem(
    options: &Options,
    name: &str,
    run: &str,
    server: &str,
    script: &str,
) -> Vec<String> {
    let mut args = vec!["run".into(), "--rm".into()];
    args.extend(identity(name, run));
    args.extend([
        "--network".into(),
        format!("container:{server}"),
        "--cap-add".into(),
        "NET_ADMIN".into(),
        "--entrypoint".into(),
        "/bin/sh".into(),
        options.netem_image.clone(),
        "-c".into(),
        script.into(),
    ]);
    args
}
pub(super) fn capture(options: &Options, name: &str, run: &str, server: &str) -> Vec<String> {
    let mut args = vec!["run".into(), "--detach".into()];
    args.extend(identity(name, run));
    args.extend([
        "--network".into(),
        format!("container:{server}"),
        "--cap-add".into(),
        "NET_RAW".into(),
        "--cap-add".into(),
        "NET_ADMIN".into(),
        options.capture_image.clone(),
        "-i".into(),
        "lo".into(),
        "-s".into(),
        "200".into(),
        "-U".into(),
        "-w".into(),
        "/handshakes.pcap".into(),
        "udp and (udp[8] = 0x16 or udp[8] = 0x00 or udp[8] = 0x01)".into(),
    ]);
    args
}
pub(super) fn counters(options: &Options, name: &str, run: &str, server: &str) -> Vec<String> {
    let mut args = vec!["run".into(), "--rm".into()];
    args.extend(identity(name, run));
    args.extend([
        "--network".into(),
        format!("container:{server}"),
        "--entrypoint".into(),
        "sh".into(),
        options.capture_image.clone(),
        "-c".into(),
        "cat /proc/net/snmp; echo @@UDP; cat /proc/net/udp".into(),
    ]);
    args
}

#[cfg(test)]
#[path = "commands_tests.rs"]
mod tests;
