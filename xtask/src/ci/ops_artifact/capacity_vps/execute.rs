//! Original source, acquisition and transport ownership through final replay.

use super::super::{acquire, namespace::Budget};
use super::{
    Result, archive, failure,
    files::{Input, Output},
    inventory::Inventory,
    keep,
    options::{self, Options},
    plan::{self, Phase, Plan},
    report::Report,
    sources::{NAMES, Sources},
    tools::Tools,
    transport,
    worker::{self, Checkpoint},
};
use crate::{
    ci::process::{Executor, Native},
    owned_process::Signals,
    tooling::Environment,
};
use simplestchat_ops::capacity_vps::protocol::{
    Action, Members, Mode, Outcome, Pin, Response, ResultData, RetirementPhase, Source, Unit,
    UnitState, Workload,
};
use std::{
    ffi::OsString,
    fs::File,
    io::Read,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub(super) struct State {
    pub options: Options,
    pub run: String,
    pub caller: PathBuf,
    pub home: PathBuf,
    pub environment: Environment,
    pub budget: Budget,
    pub output: Option<Output>,
    pub sources: Option<Arc<Sources>>,
    pub inventory: Option<Arc<Inventory>>,
    pub tools: Option<Tools>,
    pub authenticated: Option<Arc<acquire::AuthenticatedAcquisition>>,
    pub transfer: Option<Input>,
    pub selection: Option<Source>,
    pub members: Option<Members>,
    pub request: Vec<u8>,
    pub extracted: Option<archive::Extraction>,
    pub report: Report,
    pub index: usize,
    pub captures: u64,
    pub cleanup_captures: u64,
    pub polls: usize,
}
pub(super) type Shared = Arc<Mutex<State>>;
pub(super) fn lock(state: &Shared) -> Result<std::sync::MutexGuard<'_, State>> {
    state.lock().map_err(|_| failure(1))
}

fn bound_response(
    state: &Shared,
    observed: &transport::Observed,
    action: Action,
) -> Result<Response> {
    let response = observed.response(action)?;
    let state = lock(state)?;
    response
        .bind_workload(
            observed.header.as_ref().ok_or_else(|| failure(1))?,
            &state.request,
        )
        .map_err(|_| failure(1))?;
    Ok(response)
}

pub(super) async fn stage<T: Send + 'static, E: Executor>(
    state: &Shared,
    plan: &Plan,
    executor: &mut E,
    end: Instant,
    operation: impl FnOnce(&mut State, &Checkpoint) -> Result<T> + Send + 'static,
    settle: impl FnOnce(T, &Checkpoint) -> Result<()> + Send + 'static,
) -> Result<T> {
    let state = state.clone();
    let cleanup_checkpoint = Checkpoint {
        clock: plan.clock.clone(),
        start: plan.start,
        end,
        abort: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    worker::work(
        plan,
        executor,
        end,
        move |checkpoint| {
            let mut state = lock(&state)?;
            operation(&mut state, &checkpoint)
        },
        move |value| settle(value, &cleanup_checkpoint),
    )
    .await
}
fn originals(state: &State, checkpoint: &Checkpoint) -> Result<()> {
    state
        .sources
        .as_ref()
        .ok_or_else(|| failure(1))?
        .check(checkpoint)?;
    state
        .inventory
        .as_ref()
        .ok_or_else(|| failure(1))?
        .check(checkpoint)?;
    state
        .output
        .as_ref()
        .ok_or_else(|| failure(1))?
        .check(checkpoint)?;
    if let Some(acquired) = &state.authenticated {
        acquired.check(&state.budget, &checkpoint.acquired())?;
    }
    if let Some(transfer) = &state.transfer {
        transfer.check(checkpoint)?;
    }
    checkpoint.check()
}
fn git_args(values: &[&str]) -> Vec<OsString> {
    [
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.untrackedCache=false",
        "-c",
        "core.attributesFile=/dev/null",
    ]
    .into_iter()
    .chain(values.iter().copied())
    .map(Into::into)
    .collect()
}
async fn git_identity<E: Executor>(
    state: &Shared,
    plan: &Plan,
    executor: &mut E,
    end: Instant,
    initial: bool,
) -> Result<()> {
    let head = transport::git(state, plan, executor, end, git_args(&["rev-parse", "HEAD"])).await?;
    if head != format!("{}\n", lock(state)?.options.revision).as_bytes() {
        return Err(failure(1));
    }
    let clean = transport::git(
        state,
        plan,
        executor,
        end,
        git_args(&[
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "--ignore-submodules=all",
        ]),
    )
    .await?;
    if !clean.is_empty() {
        return Err(failure(1));
    }
    if initial {
        let root = transport::git(
            state,
            plan,
            executor,
            end,
            git_args(&["rev-parse", "--show-toplevel"]),
        )
        .await?;
        if root != format!("{}\n", lock(state)?.caller.display()).as_bytes() {
            return Err(failure(1));
        }
        for name in NAMES {
            let argument = format!("HEAD:{name}");
            let bytes =
                transport::git(state, plan, executor, end, git_args(&["show", &argument])).await?;
            if bytes != lock(state)?.sources.as_ref().unwrap().body(name)? {
                return Err(failure(1));
            }
        }
    }
    Ok(())
}
struct AcquisitionClock(Arc<dyn plan::Clock>);
impl acquire::AcquisitionClock for AcquisitionClock {
    fn now(&self) -> Instant {
        self.0.now()
    }
}

async fn acquire<E: Executor>(
    state: &Shared,
    plan: &Plan,
    executor: &mut E,
    end: Instant,
) -> Result<()> {
    let (path, sha, caller, environment, budget) = {
        let state = lock(state)?;
        let output = state.output.as_ref().ok_or_else(|| failure(1))?;
        (
            output.path("acquire-request.json"),
            super::super::sha(&output.read(
                "acquire-request.json",
                &Checkpoint {
                    clock: plan.clock.clone(),
                    start: plan.start,
                    end,
                    abort: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                },
            )?),
            state.caller.clone(),
            state.environment.clone(),
            state.budget.clone(),
        )
    };
    let acquired = acquire::observe_acquired(
        path,
        sha,
        caller,
        environment,
        budget,
        acquire::Window {
            clock: Arc::new(AcquisitionClock(plan.clock.clone())),
            end,
        },
        executor,
    )
    .await?;
    lock(state)?.authenticated = Some(Arc::new(acquired));
    stage(
        state,
        plan,
        executor,
        end,
        |state, checkpoint| {
            let acquired = state.authenticated.as_ref().unwrap().clone();
            let (transfer, pins) = state
                .output
                .as_mut()
                .unwrap()
                .transfer(&acquired, checkpoint)?;
            state.members = Some(Members {
                manifest: Pin {
                    sha256: pins.manifest().sha256().into(),
                    bytes: pins.manifest().bytes(),
                },
                selection: Pin {
                    sha256: pins.selection().sha256().into(),
                    bytes: pins.selection().bytes(),
                },
                binary: Pin {
                    sha256: pins.binary().sha256().into(),
                    bytes: pins.binary().bytes(),
                },
            });
            state
                .tools
                .as_mut()
                .ok_or_else(|| failure(1))?
                .bind_artifact(acquired, transfer.original())?;
            state.transfer = Some(transfer);
            originals(state, checkpoint)
        },
        |_, _| Ok(()),
    )
    .await
}
async fn observe<E: Executor>(
    state: &Shared,
    plan: &Plan,
    executor: &mut E,
    end: Instant,
    unit: Unit,
) -> Result<UnitState> {
    loop {
        {
            let mut state = lock(state)?;
            if state.polls >= 6000 {
                return Err(failure(1));
            }
            state.polls += 1;
        }
        let response = transport::rpc(
            state,
            plan,
            executor,
            end,
            Action::Status,
            Mode::Invoke,
            Some(unit),
        )
        .await?;
        let header = response.header.clone().ok_or_else(|| failure(1))?;
        let ResultData::Status(status) = bound_response(state, &response, Action::Status)?.result
        else {
            return Err(failure(1));
        };
        status.validate(&header, unit).map_err(|_| failure(1))?;
        if status.terminal() {
            return Ok(status);
        }
        let until = (plan.now()? + Duration::from_secs(5)).min(end);
        loop {
            executor.ready().await?;
            plan.check()?;
            let now = plan.now()?;
            if now >= until {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50).min(until - now)).await;
        }
    }
}
async fn body<E: Executor>(state: &Shared, plan: &Plan, executor: &mut E) -> Result<()> {
    let end = plan.begin(Phase::Admission)?;
    stage(
        state,
        plan,
        executor,
        end,
        |state, checkpoint| {
            let sources = Arc::new(Sources::open(
                &state.caller,
                state.output.as_ref().unwrap(),
                &state.budget,
                checkpoint,
            )?);
            let inventory = Arc::new(Inventory::open(
                &state.options.inventory,
                &state.options.limit,
                &state.options.revision,
                &state.caller,
                &state.home,
                &state.budget,
                checkpoint,
            )?);
            let selection = Source {
                repository: "swiftraccoon/simplestChat".into(),
                revision: state.options.revision.clone(),
                package_version: sources.package_version()?,
                protocol_version: 1,
                target: "x86_64-unknown-linux-musl".into(),
                build_run_id: state.options.ci_run,
                run_attempt: state.options.attempt,
            };
            selection.validate().map_err(|_| failure(1))?;
            state.output.as_mut().unwrap().write(
                "inventory.snapshot.json",
                &inventory.snapshot,
                checkpoint,
            )?;
            let acquisition = serde_json::to_vec(&serde_json::json!({
                "schemaVersion": 1,
                "selection": selection,
                "artifactId": state.options.artifact,
                "output": state.output.as_ref().unwrap().path("acquired"),
                "authentication": "gh-token",
            }))
            .map_err(|_| failure(1))?;
            state.output.as_mut().unwrap().write(
                "acquire-request.json",
                &acquisition,
                checkpoint,
            )?;
            state.tools = Some(Tools::open(
                &state.caller,
                &state.home,
                &state.environment,
                sources.clone(),
                inventory.clone(),
                state.options.prepare,
                &state.budget,
            )?);
            state.sources = Some(sources);
            state.inventory = Some(inventory);
            state.selection = Some(selection);
            state.output.as_mut().unwrap().write(
                "report.initial.json",
                &state.report.encode()?,
                checkpoint,
            )?;
            originals(state, checkpoint)
        },
        |_, _| Ok(()),
    )
    .await?;
    git_identity(state, plan, executor, end, true).await?;
    plan.complete(Phase::Admission, end)?;
    let end = plan.begin(Phase::Acquisition)?;
    acquire(state, plan, executor, end).await?;
    plan.complete(Phase::Acquisition, end)?;
    let end = plan.begin(Phase::Stage)?;
    let mode = lock(state)?.options.mode;
    if mode == options::Mode::New {
        let response = transport::rpc(
            state,
            plan,
            executor,
            end,
            Action::Preflight,
            Mode::Stage,
            None,
        )
        .await?;
        let ResultData::Preflight(preflight) =
            bound_response(state, &response, Action::Preflight)?.result
        else {
            return Err(failure(1));
        };
        if !preflight.private_host {
            return Err(failure(1));
        }
    } else {
        let mut response = transport::rpc(
            state,
            plan,
            executor,
            end,
            Action::Lookup,
            Mode::Invoke,
            None,
        )
        .await?;
        if response.unavailable {
            response = transport::rpc(
                state,
                plan,
                executor,
                end,
                Action::Lookup,
                Mode::Stage,
                None,
            )
            .await?;
        }
        let header = response.header.clone().ok_or_else(|| failure(1))?;
        let decoded = response.response(Action::Lookup)?;
        let ResultData::Lookup(saved) = &decoded.result else {
            return Err(failure(1));
        };
        saved
            .unit
            .validate(&header, Unit::Capacity)
            .map_err(|_| failure(1))?;
        let bytes = saved.request_json.as_bytes().to_vec();
        if super::super::sha(&bytes) != saved.request_sha256 {
            return Err(failure(1));
        }
        let request = Workload::decode(&bytes).map_err(|_| failure(1))?;
        if request.run_id() != header.run || request.revision() != header.selection.revision {
            return Err(failure(1));
        }
        decoded
            .bind_workload(&header, &bytes)
            .map_err(|_| failure(1))?;
        plan.tighten(request.runtime_seconds())?;
        let mut state = lock(state)?;
        state.request = bytes.clone();
        state.report.adopt(bytes);
    }
    plan.complete(Phase::Stage, end)?;
    let end = plan.begin(Phase::Preflight)?;
    stage(
        state,
        plan,
        executor,
        end,
        |state, checkpoint| originals(state, checkpoint),
        |_, _| Ok(()),
    )
    .await?;
    plan.complete(Phase::Preflight, end)?;
    let prepare = lock(state)?.options.prepare;
    if prepare {
        let end = plan.begin(Phase::Prepare)?;
        transport::preparation(state, plan, executor, end).await?;
        plan.complete(Phase::Prepare, end)?;
    }
    let build_images = lock(state)?.options.build;
    if build_images {
        let end = plan.begin(Phase::Build)?;
        let response = transport::rpc(
            state,
            plan,
            executor,
            end,
            Action::Build,
            Mode::Invoke,
            None,
        )
        .await?;
        let ResultData::Build(build) = bound_response(state, &response, Action::Build)?.result
        else {
            return Err(failure(1));
        };
        if build.unit != "simplestchat-image-build.service" {
            return Err(failure(1));
        }
        lock(state)?.report.set(
            "build",
            serde_json::to_value(build).map_err(|_| failure(1))?,
        );
        let status = observe(state, plan, executor, end, Unit::Build).await?;
        if !status.successful() {
            return Err(failure(1));
        }
        lock(state)?.report.set(
            "buildResult",
            serde_json::to_value(status).map_err(|_| failure(1))?,
        );
        plan.complete(Phase::Build, end)?;
    }
    let end = plan.begin(Phase::Run)?;
    if mode == options::Mode::New {
        // This fsynced private record precedes the first start write/await.
        stage(
            state,
            plan,
            executor,
            end,
            |state, checkpoint| {
                state.report.set("remoteOutcome", "start_requested".into());
                state.output.as_mut().unwrap().write(
                    "start-requested.json",
                    &state.report.encode()?,
                    checkpoint,
                )
            },
            |_, _| Ok(()),
        )
        .await?;
        let response = transport::rpc(
            state,
            plan,
            executor,
            end,
            Action::Start,
            Mode::Invoke,
            None,
        )
        .await?;
        let header = response.header.as_ref().ok_or_else(|| failure(1))?;
        let ResultData::Start(start) = bound_response(state, &response, Action::Start)?.result
        else {
            return Err(failure(1));
        };
        if start.unit != header.capacity_unit()
            || start.evidence != header.evidence_path()
            || start.run != header.run
        {
            return Err(failure(1));
        }
        lock(state)?.report.set(
            "start",
            serde_json::to_value(start).map_err(|_| failure(1))?,
        );
    }
    lock(state)?.report.set("remoteOutcome", "observing".into());
    let status = observe(state, plan, executor, end, Unit::Capacity).await?;
    lock(state)?.report.set(
        "unit",
        serde_json::to_value(status).map_err(|_| failure(1))?,
    );
    plan.complete(Phase::Run, end)?;
    let mut recovered_clean = false;
    if mode == options::Mode::Recover {
        let end = plan.begin(Phase::Recover)?;
        let response = transport::rpc(
            state,
            plan,
            executor,
            end,
            Action::Recover,
            Mode::Invoke,
            None,
        )
        .await?;
        let ResultData::Recover(recovery) =
            bound_response(state, &response, Action::Recover)?.result
        else {
            return Err(failure(1));
        };
        if recovery.run != lock(state)?.run
            || recovery.before.is_none()
            || recovery.after.is_none()
            || (recovery.finalized && !recovery.after.as_ref().is_some_and(|value| value.clean))
        {
            return Err(failure(1));
        }
        recovered_clean = recovery.finalized;
        lock(state)?.report.set(
            "recovery",
            serde_json::to_value(recovery).map_err(|_| failure(1))?,
        );
        plan.complete(Phase::Recover, end)?;
    }
    let end = plan.begin(Phase::Collect)?;
    let response = transport::rpc(
        state,
        plan,
        executor,
        end,
        Action::Collect,
        Mode::Invoke,
        None,
    )
    .await?;
    let header = response.header.as_ref().ok_or_else(|| failure(1))?;
    let ResultData::Collect(collection) = bound_response(state, &response, Action::Collect)?.result
    else {
        return Err(failure(1));
    };
    collection
        .unit
        .validate(header, Unit::Capacity)
        .map_err(|_| failure(1))?;
    let outcome = Outcome::decode(collection.outcome_json.as_bytes()).map_err(|_| failure(1))?;
    let clean_terminal = collection.cleanup.clean
        && collection.unit.terminal()
        && collection.unit.main_pid == "0"
        && (outcome.finalized() || recovered_clean);
    if collection.revision != header.selection.revision
        || collection.evidence != header.evidence_path()
        || (collection.passed
            && !(collection.unit.successful()
                && collection.cleanup.clean
                && outcome.passed()
                && outcome.finalized()))
    {
        return Err(failure(1));
    }
    {
        let mut state = lock(state)?;
        if collection.request_json.as_bytes() != state.request {
            return Err(failure(1));
        }
        let passed = collection.passed;
        let outcome = collection.outcome_json.as_bytes().to_vec();
        state.report.collected(
            serde_json::to_value(collection).map_err(|_| failure(1))?,
            outcome,
        );
        state.report.set("passed", passed.into());
    }
    plan.complete(Phase::Collect, end)?;
    let end = plan.begin(Phase::Archive)?;
    let response = transport::rpc(
        state,
        plan,
        executor,
        end,
        Action::Archive,
        Mode::Invoke,
        None,
    )
    .await?;
    let archive_name = response.stdout;
    plan.complete(Phase::Archive, end)?;
    let end = plan.begin(Phase::Extract)?;
    stage(
        state,
        plan,
        executor,
        end,
        move |state, checkpoint| {
            let input = state
                .output
                .as_ref()
                .unwrap()
                .input(&archive_name, checkpoint)?;
            let extracted = archive::extract(
                &input,
                state.output.as_ref().unwrap(),
                &state.budget,
                checkpoint,
            );
            let sha = input.sha256().to_owned();
            let closed = input.finish(checkpoint);
            state.extracted = Some(extracted?);
            closed?;
            state.report.set("archiveSha256", sha.into());
            state.report.set("remoteOutcome", "collected".into());
            Ok(())
        },
        |_, _| Ok(()),
    )
    .await?;
    plan.complete(Phase::Extract, end)?;
    let end = plan.begin(Phase::Retire)?;
    let response = transport::rpc(
        state,
        plan,
        executor,
        end,
        Action::Retire,
        Mode::Invoke,
        None,
    )
    .await?;
    let ResultData::Retire(retirement) = bound_response(state, &response, Action::Retire)?.result
    else {
        return Err(failure(1));
    };
    if !clean_terminal
        || !retirement.retired
        || retirement.lifecycle != RetirementPhase::Terminal
        || retirement.request_sha256 != super::super::sha(&lock(state)?.request)
    {
        return Err(failure(1));
    }
    plan.complete(Phase::Retire, end)?;
    let end = plan.begin(Phase::Finalize)?;
    git_identity(state, plan, executor, end, false).await?;
    stage(
        state,
        plan,
        executor,
        end,
        |state, checkpoint| {
            originals(state, checkpoint)?;
            if let Some(extracted) = &state.extracted {
                extracted.check(&state.budget, checkpoint)?;
            }
            Ok(())
        },
        |_, _| Ok(()),
    )
    .await?;
    plan.complete(Phase::Finalize, end)
}

async fn cleanup<E: Executor>(
    state: &Shared,
    plan: &Plan,
    executor: &mut E,
    end: Instant,
) -> Result<()> {
    stage(
        state,
        plan,
        executor,
        end,
        |state, checkpoint| {
            let original = originals(state, checkpoint);
            let transfer = state
                .transfer
                .take()
                .map(|transfer| transfer.finish(checkpoint))
                .transpose();
            original?;
            transfer?;
            Ok(())
        },
        |_, _| Ok(()),
    )
    .await
}

pub(super) struct Inputs {
    pub options: Options,
    pub caller: PathBuf,
    pub home: PathBuf,
    pub environment: Environment,
    pub run: String,
    pub output: PathBuf,
}
pub(super) async fn transaction<E: Executor>(
    inputs: Inputs,
    budget: Budget,
    plan: &Plan,
    executor: &mut E,
) -> Result<()> {
    let checkpoint = Checkpoint {
        clock: plan.clock.clone(),
        start: plan.start,
        end: plan.end(),
        abort: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    let request = inputs.options.request(&inputs.run)?;
    let output = Output::new(&inputs.output, &inputs.caller, &budget, &checkpoint)?;
    let report = Report::new(
        &inputs.options.revision,
        &inputs.run,
        &inputs.options.limit,
        &inputs.output,
        request.clone(),
    );
    let state = Arc::new(Mutex::new(State {
        options: inputs.options,
        run: inputs.run,
        caller: inputs.caller,
        home: inputs.home,
        environment: inputs.environment,
        budget: budget.clone(),
        output: Some(output),
        sources: None,
        inventory: None,
        tools: None,
        authenticated: None,
        transfer: None,
        selection: None,
        members: None,
        request,
        report,
        index: 0,
        captures: 0,
        cleanup_captures: 0,
        polls: 0,
        extracted: None,
    }));
    let result = body(&state, plan, executor).await;
    let mut primary = result.err();
    if let Some(error) = &primary {
        lock(&state)?.report.fail(error.code);
    }
    let final_end = plan
        .end()
        .checked_sub(Duration::from_secs(60))
        .ok_or_else(|| failure(124))?;
    if let Err(error) = cleanup(&state, plan, executor, final_end).await {
        keep(&mut primary, error);
    }
    if let Some(error) = &primary {
        lock(&state)?.report.fail(error.code);
    }
    let summary = stage(
        &state,
        plan,
        executor,
        final_end,
        |state, checkpoint| {
            let bytes = state.report.encode()?;
            if let Some(output) = &mut state.output {
                output.write("report.json", &bytes, checkpoint)?;
            }
            Ok(bytes)
        },
        |_, _| Ok(()),
    )
    .await;
    let summary = match summary {
        Ok(bytes) => Some(bytes),
        Err(error) => {
            keep(&mut primary, error);
            None
        }
    };
    if let Some(summary) = summary {
        let replay_end = plan
            .end()
            .checked_sub(Duration::from_secs(30))
            .ok_or_else(|| failure(124))?;
        if let Err(error) = executor.replay(&summary, false, replay_end).await {
            keep(&mut primary, error);
        }
    }
    let can_check_git = lock(&state)?.tools.is_some();
    if can_check_git {
        let clean = transport::final_git(
            &state,
            plan,
            executor,
            plan.end(),
            git_args(&[
                "status",
                "--porcelain=v1",
                "--untracked-files=all",
                "--ignore-submodules=all",
            ]),
        )
        .await;
        match clean {
            Ok(bytes) if bytes.is_empty() => {}
            Ok(_) => keep(&mut primary, failure(1)),
            Err(error) => keep(&mut primary, error),
        }
    }
    let checked = stage(
        &state,
        plan,
        executor,
        plan.end(),
        |state, checkpoint| {
            let original = originals(state, checkpoint);
            let extraction = state
                .extracted
                .as_ref()
                .map(|extracted| extracted.check(&state.budget, checkpoint))
                .transpose();
            // Tool retirement is attempted even if an original or extraction
            // fence failed, or this always-joined worker was already cancelled.
            let closed = state.tools.take().map(Tools::finish).transpose();
            original?;
            extraction?;
            closed?;
            Ok(())
        },
        |_, _| Ok(()),
    )
    .await;
    if let Err(error) = checked {
        keep(&mut primary, error);
    }
    if let Err(error) = executor.ready().await {
        keep(&mut primary, error);
    }
    if let Err(error) = plan.check() {
        keep(&mut primary, error);
    }
    if !executor.cleanup_passed() {
        keep(&mut primary, failure(1));
    }
    if let Some(error) = primary {
        return Err(error);
    }
    if !lock(&state)?.report.passed() {
        return Err(failure(1));
    }
    Ok(())
}
fn random(budget: &Budget) -> Result<String> {
    let _permit = budget.permit()?;
    let mut file = File::open("/dev/urandom").map_err(|_| failure(1))?;
    let mut bytes = [0u8; 16];
    let read = file.read_exact(&mut bytes).map_err(|_| failure(1));
    let closed = nix::unistd::close(file).map_err(|_| failure(1));
    read.and(closed)?;
    Ok(super::super::hex(&bytes))
}
pub(super) async fn native(arguments: &[String]) -> Result<()> {
    let clock: Arc<dyn plan::Clock> = Arc::new(plan::NativeClock);
    let start = clock.now();
    let caller = std::env::current_dir()
        .and_then(|p| p.canonicalize())
        .map_err(|_| failure(1))?;
    let environment = crate::tooling::inherited_env();
    let home = environment
        .get(std::ffi::OsStr::new("HOME"))
        .map(PathBuf::from)
        .ok_or_else(|| failure(2))?;
    if !options::path(&home) || caller.to_str().is_none() || home.to_str().is_none() {
        return Err(failure(2));
    }
    let options = Options::decode(arguments, &caller, &home)?;
    let plan = Plan::new(clock, &options, start);
    let budget = Budget::default();
    let nonce = random(&budget)?;
    let run = options.run.clone().unwrap_or_else(|| nonce.clone());
    let parent = std::env::temp_dir()
        .canonicalize()
        .map_err(|_| failure(1))?;
    let output = parent.join(format!(
        "vps-capacity.{}.{}",
        chrono::Utc::now().format("%Y%m%dT%H%M%SZ"),
        nonce
    ));
    let signals = Signals::new()?;
    let replay: Environment = [("PATH", "/usr/bin:/bin"), ("LANG", "C"), ("LC_ALL", "C")]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect();
    let mut executor = Native::new(&caller, &replay, signals);
    Box::pin(transaction(
        Inputs {
            options,
            caller: caller.clone(),
            home,
            environment,
            run,
            output,
        },
        budget,
        &plan,
        &mut executor,
    ))
    .await
}
