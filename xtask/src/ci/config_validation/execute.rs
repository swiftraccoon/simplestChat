//! One complete validator transaction and independent exact-resource retirement.

use super::{
    capture::Capture,
    engine::{self, Clock, Core, OwnedContainer, Policy, Scope},
    files::{AdapterFailure, Artifacts, CommandObservation, Inputs, LOG_LIMIT},
    plan::{self, Context},
};
use crate::{
    ci::process::Executor,
    tooling::{Failure, Result},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct Evidence {
    schema_version: u8,
    kind: &'static str,
    inputs_sha256: String,
    engine_sha256: String,
    transport_sha256: String,
    creation_response_completed: bool,
    guest_exit_code: Option<u8>,
    guest_terminal_agreement: bool,
    logs_collected: bool,
    resource_absence_observed: bool,
    cli_groups_settled: bool,
    source_closed: bool,
    status_before_publication: u8,
    last_command: Option<CommandObservation>,
    first_adapter_failure: Option<AdapterFailure>,
}

pub(super) fn early(inputs: Inputs, artifacts: Artifacts, error: Failure) -> Result<()> {
    let _ = inputs.finish();
    let _ = artifacts.finish();
    Err(error)
}

const CATALOG_FORMAT: &str = concat!(
    "{\"repository\":{{json .Repository}},\"digest\":{{json .Digest}},",
    "\"id\":{{json .ID}},\"tag\":{{json .Tag}}}",
);
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogRow {
    repository: String,
    digest: String,
    id: String,
    tag: String,
}

/// Docker's documented repository filter, four placeholders and --no-trunc:
/// <https://docs.docker.com/reference/cli/docker/image/ls/#format-the-output---format>
/// v28.5.1 formatter/image.go emits every tag/digest pair. Distinct tags may
/// therefore repeat one immutable ID; duplicate tag/digest rows may not.
fn catalog(bytes: &[u8], kind: plan::Kind) -> Result<Option<String>> {
    if bytes.len() > 64 * 1024 || (!bytes.is_empty() && !bytes.ends_with(b"\n")) {
        return Err(plan::failure());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| plan::failure())?;
    let digest = kind.image().split_once('@').expect("fixed image digest").1;
    let mut matched = None;
    let mut rows = BTreeSet::new();
    for (index, line) in text.lines().enumerate() {
        if index >= 512 {
            return Err(plan::failure());
        }
        let row: CatalogRow = serde_json::from_str(line).map_err(|_| plan::failure())?;
        if !engine::image_id(&row.id)
            || row.repository != kind.repository()
            || !(engine::image_id(&row.digest) || row.digest == "<none>")
            || !tag(&row.tag)
            || !rows.insert((row.digest.clone(), row.tag))
        {
            return Err(plan::failure());
        }
        if row.repository == kind.repository() && row.digest == digest {
            if matched.as_ref().is_some_and(|id| id != &row.id) {
                return Err(plan::failure());
            }
            matched = Some(row.id);
        }
    }
    Ok(matched)
}

/// distribution/reference v0.6.0 regexp.go: `[\w][\w.-]{0,127}` (ASCII Go regexp).
fn tag(value: &str) -> bool {
    value == "<none>"
        || (!value.is_empty()
            && value.len() <= 128
            && value.bytes().enumerate().all(|(index, byte)| {
                byte.is_ascii_alphanumeric()
                    || byte == b'_'
                    || (index > 0 && matches!(byte, b'.' | b'-'))
            }))
}

async fn image<E: Executor, C: Clock>(
    core: &mut Core<'_, E, C>,
    kind: plan::Kind,
) -> Result<String> {
    let selector = kind.image();
    let listing = core
        .run(core.command(
            &[
                "image",
                "ls",
                "--all",
                "--no-trunc",
                "--digests",
                "--format",
                CATALOG_FORMAT,
                kind.repository(),
            ],
            false,
        ))
        .await?;
    if let Some(expected) = catalog(&listing, kind)? {
        let selected = core.image(selector, false).await?;
        if selected != expected {
            return Err(plan::failure());
        }
        return Ok(selected);
    }
    // Only a successful, closed, bounded catalog can authorize this one fixed
    // selector's cold pull. No failed command or log completion is a cache miss.
    let mut pull = core.command(&["pull", "--quiet", selector], false);
    pull.timeout = core.policy().watchdog();
    core.run(pull).await?;
    core.image(selector, false).await
}

fn guest_code(bytes: &[u8]) -> Result<u8> {
    let line = engine::line(bytes, Scope::Validation)?;
    if line.len() > 3
        || !line.bytes().all(|b| b.is_ascii_digit())
        || (line.len() > 1 && line.starts_with('0'))
    {
        return Err(plan::failure());
    }
    line.parse().map_err(|_| plan::failure())
}

async fn validate<E: Executor, C: Clock>(
    context: &Context,
    artifacts: &Artifacts,
    core: &mut Core<'_, E, C>,
    resource: &mut OwnedContainer,
    evidence: &mut Evidence,
) -> Result<()> {
    let selected = image(core, context.kind).await?;
    resource.image = Some(selected.clone());
    let intent = serde_json::to_vec(&serde_json::json!({
        "schemaVersion":1,"kind":context.kind.label(),"state":"creation-planned",
        "name":resource.name,"labelKey":plan::OWNER,
        "labelValue":format!("{}:{}",artifacts.token,context.kind.label()),
        "image":selected,"sourceReference":context.kind.image(),
        "engineSha256":core.identity_sha256(),"transportSha256":core.endpoint_sha256(),
    }))
    .map_err(|_| plan::failure())?;
    artifacts.write("ownership.json", &intent)?;
    resource.attempted = true;
    let output = core
        .run(context.create(core.endpoint, &resource.name, &artifacts.token, &selected))
        .await?;
    resource.creation_completed = true;
    evidence.creation_response_completed = true;
    let id = engine::line(&output, Scope::Validation)?.to_owned();
    resource.inspect(core, &id, false).await?;
    artifacts.write("container.id", format!("{id}\n").as_bytes())?;
    let started = core
        .run(core.command(&["container", "start", &id], false))
        .await?;
    if engine::line(&started, Scope::Validation)? != id {
        return Err(plan::failure());
    }
    let mut wait = core.command(&["container", "wait", &id], false);
    wait.timeout = core.policy().watchdog();
    let output = core.run(wait).await?;
    let code = guest_code(&output)?;
    evidence.guest_exit_code = Some(code);
    let primary = if code == 0 {
        Ok(())
    } else {
        Err(Failure::new(
            code,
            "Configuration validator reported failure.",
        ))
    };
    // Even a known nonzero guest result still gets the independent terminal
    // observation, but that later failure cannot erase its observed status.
    let terminal = resource.inspect(core, &id, false).await.and_then(|value| {
        if value.state.running
            || value.state.status.as_deref() != Some("exited")
            || value.state.exit_code != Some(i64::from(code))
        {
            return Err(plan::failure());
        }
        evidence.guest_terminal_agreement = true;
        Ok(())
    });
    primary.and(terminal)
}

async fn logs<E: Executor, C: Clock>(
    core: &mut Core<'_, E, C>,
    resource: &mut OwnedContainer,
) -> Result<()> {
    if !resource.attempted {
        return Ok(());
    }
    let id = resource.locate(core).await?.ok_or_else(plan::failure)?;
    let mut command = core.command(&["logs", &id], true);
    command.capture = Some(LOG_LIMIT);
    core.run(command).await.map(|_| ())
}

async fn body<E: Executor, C: Clock>(
    context: &Context,
    endpoint: &str,
    artifacts: &Artifacts,
    runner: &mut E,
    clock: &mut C,
    evidence: &mut Evidence,
) -> Result<()> {
    let mut capture = Capture {
        inner: runner,
        artifacts,
        started: context.started,
    };
    let mut core = Core::bind(endpoint, &mut capture, clock, Policy::validation()).await?;
    evidence.engine_sha256 = core.identity_sha256();
    evidence.transport_sha256 = core.endpoint_sha256();
    let mut resource = OwnedContainer::new(
        Scope::Validation,
        format!(
            "simplestchat-validate-{}-{}",
            context.kind.label(),
            artifacts.token
        ),
        plan::OWNER.into(),
        format!("{}:{}", artifacts.token, context.kind.label()),
        None,
    );
    let primary = validate(context, artifacts, &mut core, &mut resource, evidence).await;
    // Sample a known cancellation before cleanup errors can become primary.
    let pending = core.runner.ready().await;
    let collected = logs(&mut core, &mut resource).await;
    evidence.logs_collected = resource.attempted && collected.is_ok();
    let retired = resource.retire(&mut core).await;
    let checked = core.check(true).await;
    evidence.resource_absence_observed = resource.attempted && retired.is_ok() && checked.is_ok();
    primary
        .and(pending)
        .and(collected)
        .and(retired)
        .and(checked)
}

pub(super) async fn run<E: Executor, C: Clock>(
    context: &Context,
    endpoint: &str,
    runner: &mut E,
    clock: &mut C,
    inputs: Inputs,
    artifacts: Artifacts,
) -> Result<()> {
    let mut evidence = Evidence {
        schema_version: 1,
        kind: context.kind.label(),
        inputs_sha256: inputs.sha256.clone(),
        ..Evidence::default()
    };
    let primary = body(context, endpoint, &artifacts, runner, clock, &mut evidence).await;
    evidence.cli_groups_settled = runner.cleanup_passed();
    let pending = runner.ready().await;
    let source = inputs.finish();
    evidence.source_closed = source.is_ok();
    let settled = if evidence.cli_groups_settled {
        Ok(())
    } else {
        Err(plan::failure())
    };
    let primary = primary.and(pending).and(source).and(settled);
    evidence.status_before_publication = primary.as_ref().err().map_or(0, |error| error.code);
    evidence.last_command = artifacts.last_command();
    evidence.first_adapter_failure = artifacts.first_adapter_failure();
    // Facts observed before publication do not certify the later directory
    // closes, output replay, final cancellation fence or process exit.
    let record = serde_json::to_vec_pretty(&evidence)
        .map_err(|_| plan::failure())
        .and_then(|bytes| artifacts.write("config-validation.json", &bytes));
    let projection = format!(
        "Configuration {}: guest status {}; final CLI status is authoritative.\n",
        context.kind.label(),
        evidence
            .guest_exit_code
            .map_or_else(|| "unavailable".into(), |code| code.to_string())
    );
    let replay = if Policy::validation()
        .watchdog()
        .checked_sub(clock.elapsed())
        .is_some_and(|left| left > Duration::from_secs(40))
    {
        runner
            .replay(
                projection.as_bytes(),
                false,
                Instant::now() + Duration::from_secs(25),
            )
            .await
    } else {
        Err(Scope::Validation.expired())
    };
    let closed = artifacts.finish();
    let final_pending = runner.ready().await;
    let deadline = if clock.elapsed() < Policy::validation().watchdog() {
        Ok(())
    } else {
        Err(Scope::Validation.expired())
    };
    primary
        .and(record)
        .and(replay)
        .and(closed)
        .and(final_pending)
        .and(deadline)
}
