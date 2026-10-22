//! One invocation owns setup, both process groups and every credential sink.
//! Assessment reads only follow positively settled generator streams/groups.

use super::{
    config::{Config, FINAL_MS},
    database::{self, Database},
    files::{Directory, Record},
    http::Http,
    measure, report,
    sampling::Observations,
};
use crate::bench::{
    clock::Clock,
    process::{self, Child, Command, Host, Sink},
    sampling::Ports,
};
use anyhow::{Result, ensure};
use base64::Engine;
use serde_json::{Value, json};
use std::{path::Path, time::Duration};

pub(super) struct Services<'a> {
    pub host: &'a dyn Host,
    pub clock: &'a dyn Clock,
    pub http: &'a dyn Http,
    pub observations: &'a dyn Observations,
    pub ports: &'a dyn Ports,
}

pub(super) struct Outcome {
    pub code: u8,
    pub report: Value,
}

#[derive(Debug)]
pub(super) struct Interrupted(pub u8);
impl std::fmt::Display for Interrupted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("account_room_interrupted")
    }
}
impl std::error::Error for Interrupted {}

pub(super) fn fence(config: &Config, services: &Services<'_>) -> Result<()> {
    config.bound(services.clock.elapsed_ms(), Duration::from_secs(1))?;
    Ok(())
}

pub(super) async fn check(config: &Config, services: &Services<'_>) -> Result<()> {
    tokio::select! {
        biased;
        code = services.host.cancelled() => return Err(Interrupted(code).into()),
        () = std::future::ready(()) => {},
    }
    fence(config, services)
}

pub(super) async fn bounded<T>(
    config: &Config,
    services: &Services<'_>,
    maximum: Duration,
    future: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    check(config, services).await?;
    let bound = config.bound(services.clock.elapsed_ms(), maximum)?;
    let value = tokio::select! {
        biased;
        code = services.host.cancelled() => return Err(Interrupted(code).into()),
        result = tokio::time::timeout(bound, future) => result
            .map_err(|_| anyhow::anyhow!("account_room_deadline"))??,
    };
    check(config, services).await?;
    Ok(value)
}

fn command(
    config: &Config,
    executable: &Path,
    environment: super::config::Environment,
    stdout: std::fs::File,
    stderr: std::fs::File,
    timeout: Duration,
) -> Command {
    Command {
        executable: executable.to_owned(),
        arguments: Vec::new(),
        directory: config.directory.clone(),
        environment,
        timeout,
        stdout_limit: 32 * 1024 * 1024,
        stderr_limit: 32 * 1024 * 1024,
        input: Vec::new(),
        stdout: Sink::File(stdout),
        stderr: Sink::File(stderr),
        cleanup: false,
    }
}

#[derive(Debug)]
pub(super) struct ObservedExit(pub process::Exit);
impl std::fmt::Display for ObservedExit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("owned_server_exited")
    }
}
impl std::error::Error for ObservedExit {}

pub(super) fn exit_status(exit: &process::Exit) -> u8 {
    match (exit.code, exit.signal.as_deref()) {
        (Some(code), None) => u8::try_from(code).unwrap_or(1),
        (_, Some("SIGINT")) => 130,
        (_, Some("SIGTERM")) => 143,
        _ => 1,
    }
}
pub(super) fn running(exit: Option<process::Exit>) -> Result<()> {
    if let Some(exit) = exit {
        return Err(ObservedExit(exit).into());
    }
    Ok(())
}
pub(super) fn known_status(error: &anyhow::Error) -> Option<u8> {
    error
        .downcast_ref::<Interrupted>()
        .map(|v| v.0)
        .or_else(|| {
            error
                .downcast_ref::<ObservedExit>()
                .map(|v| nonzero(exit_status(&v.0)))
        })
        .or_else(|| process::failure_status(error))
        .or_else(|| crate::bench::environment::failure_status(error))
        .or_else(|| database::failure_status(error))
        .or_else(|| {
            matches!(
                error.to_string().as_str(),
                "account_room_deadline" | "generator_deadline"
            )
            .then_some(124)
        })
}
pub(super) fn status(error: &anyhow::Error) -> u8 {
    known_status(error).unwrap_or(1)
}

fn failure_code(error: &anyhow::Error) -> String {
    if let Some(code) = database::failure_code(error) {
        return code;
    }
    match error.to_string().as_str() {
        "pinned_postgresql_version_required" => "pinned_postgresql_version_required",
        "invalid_registered_account" => "invalid_registered_account",
        "server_readiness_failed" => "server_readiness_failed",
        "account_room_deadline" => "account_room_deadline",
        "generator_deadline" => "generator_deadline",
        "account_room_interrupted" => "account_room_interrupted",
        "command_timed_out" => "owned_command_timeout",
        "Benchmark interrupted" => "account_room_interrupted",
        "owned_server_exited" => "owned_server_exited",
        _ => "orchestration_failed",
    }
    .into()
}

fn fail(report: &mut Value, code: &str) {
    report["passed"] = false.into();
    if report["failure"].is_null() {
        report["failure"] = code.into();
    }
}

fn settled(child: &dyn Child) -> bool {
    child
        .retirement()
        .is_some_and(|v| v.group_cleanup_passed && v.streams_closed)
}

pub(super) fn observed_exit(facts: &process::Retirement) -> Option<process::Exit> {
    (facts.wait_code.is_some() || facts.wait_signal.is_some()).then(|| process::Exit {
        code: facts.wait_code,
        signal: facts.wait_signal.map(|signal| {
            nix::sys::signal::Signal::try_from(signal)
                .map_or_else(|_| format!("SIG{signal}"), |v| v.to_string())
        }),
    })
}

async fn retire(
    child: &mut Option<Box<dyn Child>>,
    services: &Services<'_>,
) -> (bool, Option<process::Exit>, Option<u8>) {
    let Some(child) = child.as_mut() else {
        return (true, None, None);
    };
    let remaining = (FINAL_MS - services.clock.elapsed_ms()).max(0.0);
    let grace = Duration::from_secs_f64((remaining / 1000.0).min(5.0));
    let result = child.stop(grace).await;
    let observed = child.retirement().as_ref().and_then(observed_exit);
    let exit = result.as_ref().ok().cloned().or(observed);
    let failure = match &result {
        Err(error) => process::failure_status(error)
            .or_else(|| exit.as_ref().map(exit_status).filter(|v| *v != 0)),
        Ok(exit) if !exit.success() => Some(nonzero(exit_status(exit))),
        Ok(_) => None,
    };
    (result.is_ok() && settled(child.as_ref()), exit, failure)
}

/// Once output is acquired, every workload/setup failure proceeds through both
/// retirements, all sink closes and exact private-file cleanup before reporting.
pub(super) async fn execute(config: &Config, services: &Services<'_>) -> Result<Outcome> {
    check(config, services).await?;
    services
        .ports
        .available(config.port, config.media_port, 1)?;
    let output = Directory::new(&config.output)?;
    let generator_output = Directory::new(&config.output.join("generator"))?;
    let private = Directory::new(&config.temporary)?;
    let mut value = report::initial(config, services.clock);
    let mut server: Option<Box<dyn Child>> = None;
    let mut generator: Option<Box<dyn Child>> = None;
    let mut logs: Vec<Record> = Vec::new();
    let mut samples: Option<Record> = None;
    let mut window: Option<measure::Window> = None;
    let mut accounts = Vec::new();
    let room = format!("account-room-{}", uuid::Uuid::new_v4());
    let mut server_identity = None;
    let mut generator_identity = None;
    let result: Result<process::Exit> = async {
        let psql = database::find(&config.environment, &config.directory, "psql")?;
        let database = Database {
            config,
            host: services.host,
            clock: services.clock,
            executable: psql,
            environment: database::environment(config)?,
        };
        value["controllerIdentity"] = report::implementation();
        value["cargoLockSha256"] =
            crate::bench::provenance::hash_file(&config.directory.join("Cargo.lock"))?
                .0
                .into();
        let host = crate::bench::environment::observe(
            services.host,
            &config.directory,
            &config.environment,
        )
        .await?;
        value["host"] = json!({"platform":host["platform"],"release":host["release"],
            "arch":host["arch"],"cpuModel":host["cpu"],"logicalCpus":host["logicalCpus"],
            "totalMemoryBytes":host["memoryBytes"]});
        server_identity = Some(crate::bench::provenance::hash_file(&config.server)?);
        generator_identity = Some(crate::bench::provenance::hash_file(&config.generator)?);
        value["serverBinarySha256"] = server_identity.as_ref().unwrap().0.clone().into();
        value["generatorBinarySha256"] = generator_identity.as_ref().unwrap().0.clone().into();
        value["databaseServerVersion"] = database.query(database::VERSION, false).await?;
        ensure!(
            value["databaseServerVersion"] == 180006,
            "pinned_postgresql_version_required"
        );
        logs.push(private.create("server.stdout.log", 32 * 1024 * 1024)?);
        logs.push(private.create("server.stderr.log", 32 * 1024 * 1024)?);
        let request = command(
            config,
            &config.server,
            config.server_environment(),
            logs[0].duplicate()?,
            logs[1].duplicate()?,
            config.bound(services.clock.elapsed_ms(), Duration::from_secs(33 * 60))?,
        );
        check(config, services).await?;
        server = Some(services.host.spawn(request)?);
        let readiness = services.clock.elapsed_ms() + 20_000.0;
        loop {
            check(config, services).await?;
            ensure!(
                services.clock.elapsed_ms() < readiness,
                "server_readiness_failed"
            );
            running(server.as_mut().unwrap().poll().await?)?;
            if bounded(
                config,
                services,
                Duration::from_millis(500),
                services.http.ready(),
            )
            .await
            .unwrap_or(false)
            {
                break;
            }
            services.clock.sleep(Duration::from_millis(100)).await;
        }
        let mut credentials = Vec::new();
        let mut owner_token = None;
        value["phase"] = "registration".into();
        for index in 0..super::config::CLIENTS {
            let email = format!("account-room-{}@example.test", uuid::Uuid::new_v4());
            let mut random = [0_u8; 32];
            rand::RngExt::fill(&mut rand::rng(), &mut random);
            let password = format!(
                "{}Aa1!",
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random)
            );
            let body = json!({"email":email,"password":password,
                "display_name":format!("Account {}",index+1)});
            let response = bounded(
                config,
                services,
                Duration::from_secs(15),
                services.http.post(index, "/api/auth/register", &body, None),
            )
            .await?;
            let id = response["user"]["id"]
                .as_str()
                .filter(|id| {
                    uuid::Uuid::parse_str(id).is_ok_and(|parsed| {
                        id.eq_ignore_ascii_case(&parsed.hyphenated().to_string())
                    })
                })
                .ok_or_else(|| anyhow::anyhow!("invalid_registered_account"))?;
            let token = response["token"]
                .as_str()
                .filter(|v| v.len() <= 16 * 1024)
                .ok_or_else(|| anyhow::anyhow!("invalid_registered_account"))?;
            credentials.push(json!({"email":email,"password":password,"accountId":id}));
            accounts.push(id.to_owned());
            if index == 0 {
                owner_token = Some(token.to_owned());
            }
        }
        value["phase"] = "room_creation".into();
        let body = json!({"id":room,"display_name":"Thirty-account measurement",
            "require_registration":true,"guests_allowed":false,"secret":true,
            "max_participants":30,"max_broadcasters":30});
        bounded(
            config,
            services,
            Duration::from_secs(15),
            services
                .http
                .post(0, "/api/rooms", &body, owner_token.as_deref()),
        )
        .await?;
        drop(owner_token);
        private.json("accounts.json", &credentials)?;
        drop(credentials);
        value["roomId"] = room.clone().into();
        value["accountsRegistered"] = accounts.len().into();
        for name in [
            "load_test_summary.json",
            "load_test_results.json",
            "chat_inventory.json",
        ] {
            generator_output
                .create(name, 32 * 1024 * 1024)?
                .finish(&generator_output)?;
        }
        services
            .observations
            .initialize(server.as_ref().unwrap().pid())
            .await?;
        logs.push(private.create("generator.stdout.log", 32 * 1024 * 1024)?);
        logs.push(private.create("generator.stderr.log", 32 * 1024 * 1024)?);
        samples = Some(output.create("resource-samples.ndjson", 32 * 1024 * 1024)?);
        let credentials_path = private.path().join("accounts.json");
        let arguments = config.generator_arguments(&credentials_path, &room);
        value["arguments"] = arguments
            .iter()
            .map(|argument| {
                if argument == &credentials_path.to_string_lossy() {
                    "<private credential cohort>".to_owned()
                } else {
                    argument.clone()
                }
            })
            .collect::<Vec<_>>()
            .into();
        let mut environment = config.path_environment();
        environment.insert("RUST_LOG".into(), "error".into());
        let mut request = command(
            config,
            &config.generator,
            environment,
            logs[2].duplicate()?,
            logs[3].duplicate()?,
            config.bound(
                services.clock.elapsed_ms(),
                Duration::from_millis(1_470_000),
            )?,
        );
        request.arguments = arguments;
        check(config, services).await?;
        window = Some(measure::Window::new(services)?);
        ensure!(
            super::config::ACTIVE_MS - services.clock.elapsed_ms() >= super::config::GENERATOR_MS,
            "account_room_deadline"
        );
        generator = Some(services.host.spawn(request)?);
        value["phase"] = "generator".into();
        measure::run(
            config,
            services,
            &database,
            server.as_mut().unwrap().as_mut(),
            generator.as_mut().unwrap().as_mut(),
            samples.as_mut().unwrap(),
            window.as_mut().unwrap(),
        )
        .await
    }
    .await;
    let mut fallback = result
        .as_ref()
        .err()
        .is_some_and(|error| known_status(error).is_none());
    let mut code = match &result {
        Ok(exit) if exit.success() => 0,
        Ok(exit) => nonzero(exit_status(exit)),
        Err(error) => status(error),
    };
    if let Err(error) = &result {
        fail(&mut value, &failure_code(error));
    }
    let (generator_stopped, generator_exit, generator_failure) =
        retire(&mut generator, services).await;
    if (code == 0 || fallback)
        && let Some(failure) = generator_failure
    {
        code = failure;
        fallback = false;
    }
    value["generatorExit"] = result
        .as_ref()
        .ok()
        .or(generator_exit.as_ref())
        .map_or(Value::Null, |v| {
            serde_json::to_value(v).unwrap_or(Value::Null)
        });
    let generator_facts = generator.as_ref().and_then(|v| v.retirement());
    value["generatorCleanup"] = report::cleanup(
        generator_facts.as_ref(),
        generator_stopped,
        generator.is_some(),
    );
    if !generator_stopped {
        drop(generator.take());
    }
    if !generator_stopped {
        fail(&mut value, "generator_cleanup_failed");
        code = nonzero(code);
    }
    if let (Ok(exit), Some(window)) = (&result, &window)
        && generator_stopped
    {
        let assessment = async {
            let database = Database {
                config,
                host: services.host,
                clock: services.clock,
                executable: database::find(&config.environment, &config.directory, "psql")?,
                environment: database::environment(config)?,
            };
            measure::assess(
                services,
                &database,
                &generator_output,
                window,
                measure::Cohort {
                    accounts: &accounts,
                    room: &room,
                },
                exit,
                &mut value,
            )
            .await
        }
        .await;
        match assessment {
            Ok(Some(observed)) if code == 0 => code = observed,
            Err(error) => {
                fail(&mut value, "stopped_artifact_assessment_failed");
                if code == 0 {
                    code = status(&error);
                }
            }
            _ => {}
        }
    }
    if generator_facts
        .as_ref()
        .is_some_and(|v| v.leader_grace_expired == Some(true))
    {
        fail(&mut value, "generator_forced_shutdown");
        code = nonzero(code);
    }
    if let Some(record) = samples.take()
        && record.finish(&output).is_err()
    {
        fail(&mut value, "sample_close_failed");
        code = nonzero(code);
    }
    if value["passed"] != true {
        code = nonzero(code);
    }
    if let Some(child) = server.as_mut() {
        match child.poll().await {
            Ok(Some(exit)) => {
                if code == 0 {
                    code = nonzero(exit_status(&exit));
                }
                fail(&mut value, "server_exited_before_cleanup");
            }
            Err(error) => {
                if code == 0 {
                    code = status(&error);
                }
                fail(&mut value, "server_observation_failed");
            }
            Ok(None) => {}
        }
    }
    let status_before_server_cleanup = code;
    let (server_stopped, server_exit, server_failure) = retire(&mut server, services).await;
    if (code == 0 || fallback)
        && let Some(failure) = server_failure
    {
        code = failure;
    }
    let server_facts = server.as_ref().and_then(|v| v.retirement());
    let server_pid = server.as_ref().map(|v| v.pid());
    if !server_stopped {
        drop(server.take());
    }
    let graceful = server_stopped
        && server_facts
            .as_ref()
            .is_some_and(|v| v.leader_grace_expired == Some(false))
        && server_exit
            .as_ref()
            .is_some_and(|v| v.code == Some(0) && v.signal.is_none());
    if !graceful {
        fail(&mut value, "server_cleanup_failed");
        code = nonzero(code);
    }
    let mut logs_closed = true;
    for record in logs {
        logs_closed &= record.finish(&private).is_ok();
    }
    if !logs_closed {
        fail(&mut value, "private_log_close_failed");
        code = nonzero(code);
    }
    for (path, expected) in [
        (&config.server, server_identity),
        (&config.generator, generator_identity),
    ] {
        if expected.is_some_and(|expected| {
            crate::bench::provenance::hash_file(path).ok().as_ref() != Some(&expected)
        }) {
            fail(&mut value, "selected_binary_changed");
            code = nonzero(code);
        }
    }
    let removed = private.remove(&[
        "accounts.json",
        "server.stdout.log",
        "server.stderr.log",
        "generator.stdout.log",
        "generator.stderr.log",
        ".creator",
    ]);
    let private_closed = private.remove_directory();
    let cleaned = removed.is_ok() && private_closed.is_ok();
    value["privateCredentialsRemoved"] = cleaned.into();
    if !cleaned {
        fail(&mut value, "private_credential_cleanup_failed");
        code = nonzero(code);
    }
    let generator_closed = generator_output.finish();
    if generator_closed.is_err() {
        fail(&mut value, "generator_namespace_close_failed");
        code = nonzero(code);
    }
    let shutdown = report::shutdown(
        server_pid,
        status_before_server_cleanup,
        server_exit.as_ref(),
        server_facts.as_ref(),
        graceful,
        services.clock,
    );
    if output.json("server-shutdown.json", &shutdown).is_err() {
        fail(&mut value, "shutdown_report_failed");
        code = nonzero(code);
    }
    if services.clock.elapsed_ms() >= FINAL_MS {
        if code == 0 {
            code = 124;
        }
        fail(&mut value, "account_room_deadline");
    }
    tokio::select! { biased;
        signal = services.host.cancelled() => {
            if code==0 { code=signal; } fail(&mut value, "account_room_interrupted");
        }, () = std::future::ready(()) => {},
    }
    value["finishedAt"] = services.clock.utc().into();
    if value["passed"] != true {
        code = nonzero(code);
    }
    let written = output.json("account-room-report.json", &value);
    let closed = output.finish();
    if written.is_err() || closed.is_err() {
        fail(&mut value, "final_evidence_close_failed");
        code = nonzero(code);
    }
    if services.clock.elapsed_ms() >= FINAL_MS && code == 0 {
        code = 124;
    }
    tokio::select! { biased;
        signal = services.host.cancelled() => { if code==0 {code=signal;} },
        () = std::future::ready(()) => {},
    }
    Ok(Outcome {
        code,
        report: value,
    })
}
fn nonzero(code: u8) -> u8 {
    if code == 0 { 1 } else { code }
}
