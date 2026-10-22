//! The assertion transaction and all-attempt retirement of only its two intents.

use super::{
    engine::{self, Clock, Engine, Resource},
    files::{Artifacts, Inputs, MAX_LOG},
    http::{self, Origin},
    options::Context,
    plan::{self, Asset, Role},
};
use crate::{ci::process::Executor, tooling::Result};
use serde::Serialize;
use std::path::PathBuf;

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct Evidence {
    schema_version: u8,
    assertions_passed: bool,
    primary_status: u8,
    expected_migrations: usize,
    applied_migrations: Option<usize>,
    registered: bool,
    restarted: bool,
    engine_cleanup_passed: bool,
    cli_groups_settled: bool,
    source_closed: bool,
    known_container_logs_collected: bool,
}

pub(super) struct Outcome {
    pub path: PathBuf,
    pub result: Result<()>,
}

fn trimmed(bytes: &[u8]) -> &[u8] {
    let mut end = bytes.len();
    while end > 0 && bytes[end - 1] == b'\n' {
        end -= 1;
    }
    &bytes[..end]
}

async fn running<E: Executor, C: Clock>(
    resource: &mut Resource,
    engine: &mut Engine<'_, E, C>,
) -> Result<String> {
    let id = resource.id()?.to_owned();
    if !resource.inspect(engine, &id, false).await?.state.running {
        return Err(engine::failure());
    }
    Ok(id)
}

async fn database_ready<E: Executor, C: Clock>(
    database: &mut Resource,
    engine: &mut Engine<'_, E, C>,
) -> Result<()> {
    for _ in 0..120 {
        let id = running(database, engine).await?;
        let response = engine
            .run(plan::docker(
                engine.endpoint,
                &[
                    "exec",
                    &id,
                    "pg_isready",
                    "-h",
                    "127.0.0.1",
                    "-U",
                    "test_owner",
                    "-d",
                    "simplestchat_test",
                ],
                false,
            ))
            .await;
        match response {
            Ok(_) => return Ok(()),
            Err(error) if matches!(error.code, 130 | 143) || !engine.runner.cleanup_passed() => {
                return Err(error);
            }
            Err(_) => {}
        }
        engine.delay().await?;
    }
    Err(engine::failure())
}

async fn app_ready<E: Executor, C: Clock>(
    app: &mut Resource,
    engine: &mut Engine<'_, E, C>,
    origin: &Origin,
) -> Result<Vec<u8>> {
    for _ in 0..120 {
        running(app, engine).await?;
        match http::fetch(engine, origin, "/api/rooms", 2, 64 * 1024).await {
            Ok(bytes) => return Ok(bytes),
            Err(error) if matches!(error.code, 130 | 143) || !engine.runner.cleanup_passed() => {
                return Err(error);
            }
            Err(_) => {}
        }
        engine.delay().await?;
    }
    Err(engine::failure())
}

async fn create<E: Executor, C: Clock>(
    resource: &mut Resource,
    engine: &mut Engine<'_, E, C>,
    artifacts: &Artifacts,
    command: crate::ci::command::Command,
) -> Result<()> {
    resource.attempted = true;
    let output = engine.run(command).await?;
    resource.creation_completed = true;
    let id = engine::line(&output)?.to_owned();
    resource.inspect(engine, &id, false).await?;
    artifacts.write(
        &format!("{}.id", resource.role.label()),
        format!("{id}\n").as_bytes(),
    )?;
    engine
        .run(plan::docker(
            engine.endpoint,
            &["container", "start", &id],
            false,
        ))
        .await?;
    Ok(())
}

async fn assertions<E: Executor, C: Clock>(
    context: &Context,
    inputs: &Inputs,
    artifacts: &Artifacts,
    engine: &mut Engine<'_, E, C>,
    database: &mut Resource,
    app: &mut Resource,
    evidence: &mut Evidence,
) -> Result<()> {
    let image = engine.image(&context.image, false).await?;
    let database_image = engine.database_image().await?;
    app.image = Some(image.clone());
    database.image = Some(database_image.clone());
    artifacts.write(
        "ownership.json",
        &serde_json::to_vec(&serde_json::json!({
            "schemaVersion":1,
            "state":"creation-planned",
        "engineIdentitySha256":engine.identity_sha256(),
        "transportSha256":engine.endpoint_sha256(),
            "database": {
                "name":database.name,
                "label":format!("{}:database",artifacts.token),
                "image":database_image,
                "sourceReference":plan::POSTGRES_IMAGE,
            },
            "app": {
                "name":app.name,
                "label":format!("{}:app",artifacts.token),
                "image":image,
            },
        }))
        .map_err(|_| engine::failure())?,
    )?;
    let command = plan::create_database(
        engine.endpoint,
        &database.name,
        &artifacts.token,
        &database_image,
    );
    create(database, engine, artifacts, command).await?;
    database_ready(database, engine).await?;
    let id = database.id()?.to_owned();
    let port = engine
        .run(plan::docker(
            engine.endpoint,
            &["port", &id, "3000/tcp"],
            false,
        ))
        .await?;
    let origin = Origin::parse(&port)?;
    let command = plan::create_app(
        engine.endpoint,
        &app.name,
        &artifacts.token,
        &image,
        &id,
        origin.as_str(),
    );
    create(app, engine, artifacts, command).await?;
    let rooms = app_ready(app, engine, &origin).await?;
    artifacts.write("rooms.json", &rooms)?;
    running(app, engine).await?;
    // The reference wraps this curl in `test "$(curl ...)" = ...`, whose
    // ordinary transfer failures become test-status1. Owned cancellation and
    // watchdog/cleanup failures remain authoritative rather than a body mismatch.
    let health = match http::fetch(engine, &origin, "/health", 5, 4096).await {
        Ok(bytes) => bytes,
        Err(error) if matches!(error.code, 124 | 130 | 143) || !engine.runner.cleanup_passed() => {
            return Err(error);
        }
        Err(_) => return Err(engine::failure()),
    };
    if trimmed(&health) != br#"{"status":"ok"}"# {
        return Err(engine::failure());
    }
    running(app, engine).await?;
    let index = http::fetch(engine, &origin, "/", 5, 1024 * 1024).await?;
    if !index
        .windows(b"<!doctype html>".len())
        .any(|value| value.eq_ignore_ascii_case(b"<!doctype html>"))
    {
        return Err(engine::failure());
    }
    artifacts.write("index.html", &index)?;
    for asset in Asset::ALL {
        running(app, engine).await?;
        let bytes = http::asset(engine, &origin, asset, super::files::MAX_ASSET).await?;
        if bytes != inputs.asset(asset) {
            return Err(engine::failure());
        }
        artifacts.write(asset.name(), &bytes)?;
    }
    if trimmed(&rooms) != b"[]" {
        return Err(engine::failure());
    }
    running(database, engine).await?;
    let applied = engine
        .run(plan::psql(engine.endpoint, &id, plan::MIGRATIONS))
        .await?;
    let applied = engine::line(&applied)?
        .parse::<usize>()
        .map_err(|_| engine::failure())?;
    if applied == 0 || applied != inputs.migrations() {
        return Err(engine::failure());
    }
    evidence.applied_migrations = Some(applied);
    running(app, engine).await?;
    http::register(engine, &origin).await?;
    running(database, engine).await?;
    let registered = engine
        .run(plan::psql(engine.endpoint, &id, plan::REGISTERED))
        .await?;
    if trimmed(&registered) != b"1" {
        return Err(engine::failure());
    }
    evidence.registered = true;
    let id = running(app, engine).await?;
    engine
        .run(plan::docker(engine.endpoint, &["restart", &id], false))
        .await?;
    app_ready(app, engine, &origin).await?;
    evidence.restarted = true;
    engine.runner.ready().await
}

async fn logs<E: Executor, C: Clock>(
    resource: &mut Resource,
    engine: &mut Engine<'_, E, C>,
    artifacts: &Artifacts,
) -> Result<()> {
    let Some(id) = resource.locate(engine).await? else {
        return Ok(());
    };
    let mut command = plan::docker(engine.endpoint, &["logs", "--tail", "80", &id], true);
    let (file, stamp) = artifacts.create_log(resource.role)?;
    let log = crate::ci::bounded_log::Log::start(file, MAX_LOG)?;
    command.capture = None;
    command.combined_log = Some(std::sync::Arc::new(log));
    command.timeout = std::time::Duration::from_secs(5);
    command.grace = std::time::Duration::from_secs(1);
    let result = engine.run(command).await.map(|_| ());
    let checked = artifacts.check_log(resource.role, &stamp);
    result.and(checked)
}

pub(super) async fn run<E: Executor, C: Clock>(
    context: &Context,
    endpoint: &str,
    runner: &mut E,
    clock: &mut C,
) -> Result<Outcome> {
    let inputs = Inputs::read(&context.root)?;
    let artifacts = match Artifacts::new(context) {
        Ok(value) => value,
        Err(error) => {
            let _ = inputs.finish();
            return Err(error);
        }
    };
    let path = artifacts.path.clone();
    let mut engine = match Engine::bind(endpoint, runner, clock).await {
        Ok(value) => value,
        Err(error) => {
            let _ = inputs.finish();
            let _ = artifacts.finish();
            return Err(error);
        }
    };
    let mut evidence = Evidence {
        schema_version: 1,
        expected_migrations: inputs.migrations(),
        ..Evidence::default()
    };
    let mut database = Resource::new(Role::Database, &artifacts.token, None);
    let mut app = Resource::new(Role::App, &artifacts.token, None);
    let primary = assertions(
        context,
        &inputs,
        &artifacts,
        &mut engine,
        &mut database,
        &mut app,
        &mut evidence,
    )
    .await;
    evidence.assertions_passed = primary.is_ok();
    evidence.primary_status = primary.as_ref().err().map_or(0, |error| error.code);
    let app_log = logs(&mut app, &mut engine, &artifacts).await;
    let database_log = logs(&mut database, &mut engine, &artifacts).await;
    evidence.known_container_logs_collected = app_log.is_ok() && database_log.is_ok();
    let app_closed = app.retire(&mut engine).await;
    let database_closed = database.retire(&mut engine).await;
    let engine_checked = engine.check(true).await;
    evidence.engine_cleanup_passed =
        app_closed.is_ok() && database_closed.is_ok() && engine_checked.is_ok();
    evidence.source_closed = inputs.finish().is_ok();
    evidence.cli_groups_settled = engine.runner.cleanup_passed();
    let pending = engine.runner.ready().await;
    let recorded = serde_json::to_vec_pretty(&evidence)
        .map_err(|_| engine::failure())
        .and_then(|bytes| artifacts.write("container-smoke.json", &bytes));
    let closed = artifacts.finish();
    let final_pending = engine.runner.ready().await;
    let final_deadline = if engine.clock.elapsed() >= plan::WATCHDOG {
        Err(crate::tooling::Failure::new(
            124,
            "Container smoke watchdog expired.",
        ))
    } else {
        Ok(())
    };
    let cleanup = app_log
        .and(database_log)
        .and(app_closed)
        .and(database_closed)
        .and(engine_checked)
        .and(if evidence.source_closed && evidence.cli_groups_settled {
            Ok(())
        } else {
            Err(engine::failure())
        });
    let result = primary
        .and(cleanup)
        .and(recorded)
        .and(closed)
        .and(pending)
        .and(final_pending)
        .and(final_deadline);
    Ok(Outcome { path, result })
}
