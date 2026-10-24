//! One systemd-owned capacity child and its retained failure/cleanup evidence.

use super::{
    Failure, Result,
    control::{Owner, now},
    files::{self, Access, Fs},
    host::{self, CURRENT, Identity, Session, Stage},
    process::{self, Runner},
    protocol::{self, Header, Mode, Unit, Workload},
};
use serde_json::{Value, json};
use simplestchat_load_test::capacity::vps;
use std::{ffi::OsStr, path::Path, process::ExitCode};

fn arguments<'a>(run: &'a OsStr, sha: &'a OsStr) -> Result<(&'a str, &'a str)> {
    let run = run.to_str().ok_or(Failure::Arguments)?;
    let sha = sha.to_str().ok_or(Failure::Arguments)?;
    if !protocol::hex(run, 32) || !protocol::hex(sha, 64) {
        return Err(Failure::Arguments);
    }
    Ok((run, sha))
}

fn execute(
    header: &Header,
    workload: &Workload,
    stage: &Stage,
    session: &mut Session<'_>,
) -> Result<()> {
    let directory = header.evidence_path();
    let mut before = Vec::new();
    let mut assessment = None;
    let mut exit_status = None;
    let mut current_claimed = false;
    let body = (|| {
        let state = session.unit(Unit::Capacity, false)?;
        if state.main_pid != rustix::process::getpid().as_raw_pid().to_string()
            || !matches!(state.active_state.as_str(), "active" | "activating")
        {
            return Err(Failure::Unit);
        }
        // The no-fork flock launcher retains the canonical inherited lock.
        // It is external kernel custody, not a Rust-owned raw-FD/close claim.
        session.preflight(Some(header.capacity_unit().as_str()))?;
        session.source(workload)?;
        let images = session.images(workload)?;
        if session
            .fs
            .exists(&format!("{directory}/containers-before.json"))?
            || session.fs.exists(&format!("{directory}/outcome.json"))?
        {
            return Err(Failure::Journal);
        }
        before = session.containers(false, None, false)?;
        session.save(
            &format!("{directory}/containers-before.json"),
            &json!({"ids":before}),
        )?;
        session.save(
            CURRENT,
            &json!({"schemaVersion":1,"operation":"capacity","finalized":false,
            "run":header.run,"evidence":directory}),
        )?;
        current_claimed = true;
        session
            .fs
            .create(&format!("{directory}/images.json"), &images, session.owner)?;
        let mut args = vec!["capacity".into()];
        args.extend(vps::capacity_arguments(workload, &images).map_err(|_| Failure::Request)?);
        let mut command = vec![stage.binary.clone()];
        command.extend(args.iter().cloned());
        session.save(
            &format!("{directory}/command.json"),
            &json!({"argv":command}),
        )?;
        let docker_config = format!("{directory}/docker-config");
        session.fs.mkdir(&docker_config)?;
        let mut environment = process::environment();
        environment.insert("DOCKER_HOST".into(), "unix:///var/run/docker.sock".into());
        environment.insert("DOCKER_CONFIG".into(), docker_config);
        stage.check(session.fs, session.owner)?;
        let output = session.runner.capacity(
            Path::new(&stage.binary),
            &args,
            &environment,
            workload
                .runtime_seconds()
                .checked_add(600)
                .ok_or(Failure::Bounds)?,
            session.owner,
        );
        let log = match output {
            Ok(output) => {
                exit_status = output.status;
                output.combined
            }
            Err(error) => {
                session.owner.latch(error);
                session.owner.begin_cleanup();
                if let Some(capture) = session.runner.take_capture() {
                    exit_status = capture.status;
                    let _saved = session.fs.replace(
                        &format!("{directory}/capacity.log"),
                        &capture.combined,
                        session.owner,
                    );
                }
                return Err(error);
            }
        };
        session
            .fs
            .create(&format!("{directory}/capacity.log"), &log, session.owner)?;
        stage.check(session.fs, session.owner)?;
        let report = session.read(
            &format!("{directory}/workload/calibration.json"),
            protocol::MAX_RESPONSE,
        )?;
        assessment = Some(vps::assess(&report, workload).map_err(|_| Failure::Collection)?);
        if exit_status != Some(0) || !assessment.as_ref().is_some_and(|v| v.passed) {
            return Err(Failure::Collection);
        }
        Ok(())
    })();
    if let Err(error) = &body {
        session.owner.latch(*error);
    }
    // Normal cleanup and interrupted cleanup share the same original end. This
    // only enables the fixed cleanup/evidence tail, never ordinary work again.
    session.owner.begin_cleanup();
    let cleanup = session.residuals(before, true);
    if let Err(error) = &cleanup {
        session.owner.latch(*error);
    }
    let finalized = current_claimed && cleanup.as_ref().is_ok_and(|v| v.clean);
    let passed = body.is_ok() && finalized && session.owner.failure().is_none();
    let failure = session
        .owner
        .failure()
        .or_else(|| body.as_ref().err().copied())
        .or_else(|| cleanup.as_ref().err().copied());
    let mut control = json!({"schemaVersion":1,"revision":workload.revision(),
        "passed":passed,"finalized":finalized});
    if let Some(status) = exit_status {
        control["exitStatus"] = json!(status);
    }
    if let Some(error) = failure {
        control["failureClass"] = json!(error.code());
    }
    if cleanup.is_err() {
        control["cleanupFailure"] = json!(true);
    }
    let cleanup_bytes = cleanup
        .as_ref()
        .ok()
        .map(serde_json::to_vec)
        .transpose()
        .map_err(|_| Failure::Journal);
    let outcome = (|| {
        let cleanup_bytes = cleanup_bytes?;
        vps::project_worker_outcome(
            &serde_json::to_vec(&control).map_err(|_| Failure::Journal)?,
            assessment.as_ref(),
            cleanup_bytes.as_deref(),
        )
        .map_err(|_| Failure::Journal)
    })();
    if let Err(error) = &outcome {
        session.owner.latch(*error);
    }
    let outcome_saved = match &outcome {
        Ok(bytes) => session
            .fs
            .replace(&format!("{directory}/outcome.json"), bytes, session.owner)
            .map(|_| ()),
        Err(error) => Err(*error),
    };
    if let Err(error) = &outcome_saved {
        session.owner.latch(*error);
    }
    let journal_saved = if current_claimed {
        (|| {
            let journal = session.json(CURRENT)?;
            if journal.get("run").and_then(Value::as_str) != Some(header.run.as_str()) {
                return Err(Failure::Journal);
            }
            session.save(
                CURRENT,
                &json!({"schemaVersion":1,"operation":"capacity",
                "run":header.run,"evidence":directory,"finalized":finalized,"passed":passed}),
            )
        })()
    } else {
        Ok(())
    };
    if let Err(error) = &journal_saved {
        session.owner.latch(*error);
    }
    let stage_checked = stage.check(session.fs, session.owner);
    body?;
    cleanup?;
    outcome_saved?;
    journal_saved?;
    stage_checked?;
    if !passed {
        return Err(Failure::Collection);
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn fixture_execute(
    header: &Header,
    workload: &Workload,
    stage: &Stage,
    session: &mut Session<'_>,
) -> Result<()> {
    session.owner.worker(workload.runtime_seconds())?;
    execute(header, workload, stage, session)
}

pub(super) fn run(run: &OsStr, sha: &OsStr) -> ExitCode {
    let mut failure = None;
    let result = (|| {
        let (run, sha) = arguments(run, sha)?;
        let origin = now()?;
        let mut owner = Owner::new(origin, Mode::Invoke)?;
        let mut fs = match Fs::new(Access::default()) {
            Ok(fs) => fs,
            Err(error) => {
                let _ = owner.finish();
                return Err(error);
            }
        };
        let mut runner = Runner::new();
        let body = (|| {
            let directory = format!("{}/results/capacity-controller.{run}", host::ROOT);
            fs.directory(&directory, true)?;
            let id = fs.open(
                &format!("{directory}/request.json"),
                protocol::MAX_REQUEST as u64,
                Some(0o600),
            )?;
            let bytes = fs.read(id, protocol::MAX_REQUEST, &mut owner)?;
            if files::sha(&bytes) != sha {
                return Err(Failure::Identity);
            }
            let id = fs.open(
                &format!("{directory}/ops-identity.json"),
                64 * 1024,
                Some(0o600),
            )?;
            let identity = fs.read(id, 64 * 1024, &mut owner)?;
            crate::json::decode_limited(&identity, 64 * 1024).map_err(|_| Failure::Identity)?;
            let identity: Identity =
                serde_json::from_slice(&identity).map_err(|_| Failure::Identity)?;
            let header = identity.header();
            header.validate()?;
            identity.bind(&header, &bytes)?;
            if header.run != run {
                return Err(Failure::Identity);
            }
            let workload = Workload::decode(&bytes).map_err(|_| Failure::Request)?;
            owner.worker(workload.runtime_seconds())?;
            let stage = Stage::open(&header, &mut fs, &mut owner)?;
            let mut session = Session {
                header: &header,
                fs: &mut fs,
                owner: &mut owner,
                runner: &mut runner,
            };
            execute(&header, &workload, &stage, &mut session)
        })();
        if let Err(error) = &body {
            owner.latch(*error);
        }
        let primary = owner.failure().or_else(|| body.as_ref().err().copied());
        let files = fs.finish();
        let control = owner.finish();
        if let Some(error) = primary {
            return Err(error);
        }
        body?;
        files?;
        control?;
        Ok(())
    })();
    if let Err(error) = result {
        failure = Some(error);
    }
    if let Some(error) = failure {
        process::failure(error);
        ExitCode::from(error.exit_code())
    } else {
        ExitCode::SUCCESS
    }
}
