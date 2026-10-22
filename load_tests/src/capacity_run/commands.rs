//! Container argv and workload options; every port remains inside one namespace.

use super::{
    context::Context,
    engine::sandbox,
    types::{Browser, Plan, Workload},
};
use crate::capacity::format::general;

pub(super) fn server(context: &Context<'_>, name: &str, token: &str) -> Vec<String> {
    let mut arguments = vec![
        "run".into(),
        "--detach".into(),
        "--name".into(),
        name.into(),
    ];
    arguments.extend(sandbox(context.engine));
    arguments.extend([
        "--network".into(),
        "none".into(),
        "--label".into(),
        format!("simplestchat.capacity.run={}", context.run_id),
        "--cpus".into(),
        general(context.shape.server_cpus),
        "--memory".into(),
        context.server_memory.clone(),
        "--memory-swap".into(),
        context.server_memory.clone(),
        "--pids-limit".into(),
        "4096".into(),
    ]);
    let mut environment = vec![
        ("BIND_ADDR", "127.0.0.1".into()),
        ("PORT", "3000".into()),
        ("ANNOUNCE_IP", "127.0.0.1".into()),
        ("MEDIA_WORKERS", context.shape.workers.to_string()),
        (
            "CPU_SATURATION_WORKER_UTILIZATION",
            general(context.worker_threshold),
        ),
        ("ALLOW_AD_HOC_ROOMS", "true".into()),
        ("ALLOWED_ORIGINS", "http://127.0.0.1:3000".into()),
        ("METRICS_TOKEN", token.into()),
        ("RUST_LOG", "simplestchat=warn,mediasoup=warn".into()),
    ];
    for (key, value) in context.server_env {
        if let Some((_, previous)) = environment.iter_mut().find(|(name, _)| *name == key) {
            *previous = value.clone();
        } else {
            environment.push((key, value.clone()));
        }
    }
    for (key, value) in environment {
        arguments.extend(["--env".into(), format!("{key}={value}")]);
    }
    arguments.push(context.server_image.into());
    arguments
}

pub(super) fn generator(
    context: &Context<'_>,
    plan: Plan,
    name: &str,
    server: &str,
) -> Vec<String> {
    let mut arguments = vec![
        "run".into(),
        "--detach".into(),
        "--name".into(),
        name.into(),
        "--network".into(),
        format!("container:{server}"),
    ];
    arguments.extend(sandbox(context.engine));
    arguments.extend([
        "--volume".into(),
        "/results".into(),
        "--label".into(),
        format!("simplestchat.capacity.run={}", context.run_id),
        "--cpus".into(),
        general(context.shape.generator_cpus),
        "--memory".into(),
        context.generator_memory.clone(),
        "--memory-swap".into(),
        context.generator_memory.clone(),
        "--pids-limit".into(),
        "8192".into(),
        "--env".into(),
        "RUST_LOG=warn".into(),
        context.generator_image.into(),
    ]);
    arguments.extend(generator_args(
        plan,
        context.browser,
        &format!("capacity-{}-{}", plan.workload, plan.size),
        &context.revisions,
    ));
    arguments
}

pub(super) fn generator_args(
    plan: Plan,
    browser: &Browser,
    label: &str,
    revisions: &(String, String),
) -> Vec<String> {
    let ratio = if plan.workload == Workload::Webinar {
        0.5 / plan.clients() as f64
    } else {
        1.0
    };
    let mut values = vec![
        ("--server", "ws://127.0.0.1:3000/ws".into()),
        ("--clients", plan.clients().to_string()),
        ("--rooms", plan.rooms().to_string()),
        ("--room", format!("capacity-{}", plan.workload)),
        ("--publish-ratio", format!("{ratio:.12}")),
        ("--source-addresses", "250".into()),
        ("--ramp-up", plan.ramp_seconds().to_string()),
        ("--warmup", plan.warmup_seconds.to_string()),
        ("--duration", plan.duration_seconds.to_string()),
        ("--deadline-grace", "60".into()),
        ("--profile", "browser".into()),
        ("--capture", browser.capture.clone()),
        ("--speakers", browser.speakers.to_string()),
        ("--viewport-width", browser.viewport_width.to_string()),
        ("--pixel-ratio", general(browser.pixel_ratio)),
        ("--layout", browser.layout.clone()),
        ("--output-dir", "/results".into()),
        ("--run-label", label.into()),
        ("--server-revision", revisions.0.clone()),
        ("--generator-revision", revisions.1.clone()),
    ];
    if browser.chat_interval_ms != 0 {
        values.push(("--chat-interval-ms", browser.chat_interval_ms.to_string()));
    }
    let mut arguments = values
        .into_iter()
        .flat_map(|(key, value)| [key.into(), value])
        .collect::<Vec<_>>();
    if browser.audio_only {
        arguments.push("--audio-only".into());
    }
    arguments
}
