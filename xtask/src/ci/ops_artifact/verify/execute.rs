//! A complete retained-file transaction under the existing Signals/CLI owner.

use super::super::namespace::Budget;
use super::{
    BUNDLE, failure,
    files::{self, Original, Output, Source},
    hash,
    plan::{self, Context},
    request::Request,
    tools::Tools,
};
use crate::{
    ci::{
        command::{Command, Program},
        process::{Executor, Native},
    },
    owned_process::Signals,
    tooling::{Environment, Failure, Result},
};
use simplestchat_ops::distribution::manifest::{
    verification::{MatchedVerifierPolicy, RetainedMetadata},
    verify_pair,
};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

#[derive(Default)]
struct State {
    request: Option<Request>,
    original: Option<Original>,
    source: Option<Source>,
    metadata: Option<RetainedMetadata>,
    output: Option<Output>,
    tools: Option<Tools>,
    executable: Option<simplestchat_ops::distribution::ExecutableIdentity>,
    expected: BTreeSet<String>,
}
type Shared = Arc<Mutex<State>>;
fn command_failure(mut error: Failure, index: usize) -> Failure {
    if error.is_child_exit() {
        error.message = [
            "Ops artifact verification gh version command failed.",
            "Ops artifact verification gh repository command failed.",
            "Ops artifact verification gh run command failed.",
            "Ops artifact verification gh attestation command failed.",
        ]
        .get(index)
        .copied()
        .unwrap_or("Ops artifact verification command failed.")
        .into();
    }
    error
}
fn lock(state: &Shared) -> Result<std::sync::MutexGuard<'_, State>> {
    state.lock().map_err(|_| failure(1))
}
async fn stage<T: Send + 'static, E: Executor>(
    state: &Shared,
    context: &Context,
    executor: &mut E,
    operation: impl FnOnce(&mut State, &hash::Checkpoint) -> Result<T> + Send + 'static,
) -> Result<T> {
    let state = state.clone();
    hash::work(context, executor, move |checkpoint| {
        let mut state = lock(&state)?;
        operation(&mut state, &checkpoint)
    })
    .await
}
fn prepare(
    state: &mut State,
    path: &Path,
    selected: &str,
    caller: &Path,
    environment: &Environment,
    budget: &Budget,
    checkpoint: &hash::Checkpoint,
) -> Result<()> {
    if !super::request::path(path) {
        return Err(failure(1));
    }
    let (original, bytes) = files::original(path, Request::maximum_bytes(), budget, checkpoint)?;
    let request = Request::decode(&bytes, selected)?;
    request.token(environment)?;
    if request.output.starts_with(path) || path.starts_with(&request.output) {
        return Err(failure(1));
    }
    state.original = Some(original);
    let (source, mut data) = Source::small(&request, budget, checkpoint)?;
    let metadata = RetainedMetadata::decode(
        &request.selection,
        &data["ops-release.json"],
        &data["ops-evidence.json"],
        &data["ops-predicate.json"],
        &data["ops-preparation.json"],
    )
    .map_err(|_| failure(1))?;
    let mut total = source.records[BUNDLE].bytes;
    let mut expected = BTreeSet::from([BUNDLE.into()]);
    for payload in metadata.payloads() {
        total = total
            .checked_add(payload.bytes())
            .ok_or_else(|| failure(1))?;
        if !expected.insert(payload.name().into()) {
            return Err(failure(1));
        }
    }
    if total > super::MAX_TOTAL || expected.len() > 22 {
        return Err(failure(1));
    }
    source.names(budget, &expected)?;
    let output_parent = source.output_parent(&request.output, budget)?;
    state.source = Some(source);
    state.metadata = Some(metadata);
    state.expected = expected;
    state.request = Some(request);
    let request = state.request.as_ref().unwrap();
    state.output = Some(Output::bound(&request.output, budget, &output_parent)?);
    let output = state.output.as_mut().unwrap();
    for (name, bytes) in std::mem::take(&mut data) {
        checkpoint.check()?;
        output.write(&name, &bytes, checkpoint)?;
    }
    state.tools = Some(Tools::open(caller, environment, request, budget)?);
    checkpoint.check()
}
fn version(bytes: &[u8]) -> Result<()> {
    let text = std::str::from_utf8(bytes).map_err(|_| failure(1))?;
    let mut lines = text.lines();
    let first = lines.next().ok_or_else(|| failure(1))?;
    if first != "gh version 2.102.0" && !first.starts_with("gh version 2.102.0 (") {
        return Err(failure(1));
    }
    if text.len() > 4096 || text.contains('\0') {
        return Err(failure(1));
    }
    Ok(())
}
async fn call<E: Executor>(
    state: &Shared,
    context: &Context,
    executor: &mut E,
    budget: &Budget,
    index: usize,
) -> Result<Vec<u8>> {
    executor.ready().await?;
    let timeout = context.command(index)?;
    // Take the output across asynchronous sink construction; no Mutex guard is
    // held across an await. This future is retained through every setup close.
    let output = lock(state)?.output.take().ok_or_else(|| failure(1))?;
    let maximum = [4096, super::SMALL, super::SMALL, super::MAX_BUNDLE][index];
    let captures = output.capture(index, maximum).await;
    lock(state)?.output = Some(output);
    let (logs, names, log_permits) = captures?;
    let prepared = (|| {
        let state = lock(state)?;
        let tools = state.tools.as_ref().ok_or_else(|| failure(1))?;
        let invocation = tools.invocation(index)?;
        let mut command = Command::capture(Program::OpsGh, &[]);
        command.arguments = tools.arguments(index)?;
        command.capture = None;
        command.timeout = timeout;
        command.grace = plan::GRACE;
        command.retained_ops = Some(invocation.clone());
        Ok((command, invocation))
    })();
    let (mut command, invocation) = match prepared {
        Ok(value) => value,
        Err(error) => {
            let mut command = Command::capture(Program::OpsGh, &[]);
            command.capture = None;
            command.separate_logs = Some(logs);
            let _ = crate::ci::process::settle_unforwarded(command).await;
            return Err(error);
        }
    };
    command.separate_logs = Some(logs);
    let allowance = budget.reserve_with_limit(16, 48);
    let allowance = match allowance {
        Ok(value) => value,
        Err(error) => {
            let _ = crate::ci::process::settle_unforwarded(command).await;
            return Err(error);
        }
    };
    let admission = async {
        executor.ready().await?;
        lock(state)?
            .output
            .as_ref()
            .ok_or_else(|| failure(1))?
            .check()?;
        context.command(index)?;
        invocation.schedule(context)
    }
    .await;
    if let Err(error) = admission {
        let _ = crate::ci::process::settle_unforwarded(command).await;
        return Err(error);
    }
    let executed = executor.run(command).await;
    drop(allowance);
    drop(log_permits);
    // A known process status is primary even if subsequent seal/clock checks fail.
    executed.map_err(|error| command_failure(error, index))?;
    if !executor.cleanup_passed() {
        return Err(failure(1));
    }
    let seals = invocation.captures()?;
    drop(invocation);
    context.complete_command(index)?;
    stage(state, context, executor, move |state, checkpoint| {
        state
            .output
            .as_mut()
            .ok_or_else(|| failure(1))?
            .capture_read(&names, &seals, checkpoint)
    })
    .await
}
fn payloads(state: &mut State, budget: &Budget, checkpoint: &hash::Checkpoint) -> Result<()> {
    let metadata = state.metadata.as_ref().ok_or_else(|| failure(1))?;
    let source = state.source.as_mut().ok_or_else(|| failure(1))?;
    let output = state.output.as_mut().ok_or_else(|| failure(1))?;
    for requirement in metadata.payloads() {
        if !output.records.contains_key(requirement.name()) {
            source.copy(
                output,
                requirement.name(),
                requirement.sha256(),
                requirement.bytes(),
                checkpoint,
            )?;
        }
    }
    source.names(budget, &state.expected)?;
    checkpoint.check()?;
    // Output has <=23 retained namespace/work FDs; selected tools/source owners
    // are closed. This reservation covers mature Snapshot's hidden ancestors,
    // metadata probes and inspector cursor while preserving its existing policy.
    let _library = budget.reserve_with_limit(64, 24)?;
    let request = state.request.as_ref().ok_or_else(|| failure(1))?;
    let pair = verify_pair(
        metadata.manifest(),
        &output.path("simplestchat-ops"),
        &request.selection,
    )
    .map_err(|_| failure(1))?;
    metadata.check_pair(&pair).map_err(|_| failure(1))?;
    state.executable = Some(pair.executable().clone());
    drop(_library);
    checkpoint.check()
}
fn final_hashes(
    state: &mut State,
    budget: &Budget,
    checkpoint: &hash::Checkpoint,
) -> Result<String> {
    let source = state.source.as_ref().ok_or_else(|| failure(1))?;
    let output = state.output.as_ref().ok_or_else(|| failure(1))?;
    state
        .original
        .as_ref()
        .ok_or_else(|| failure(1))?
        .read(budget, checkpoint)?;
    source.names(budget, &state.expected)?;
    let original = source.frame(budget, checkpoint)?;
    let stored = output.frame(&state.expected, checkpoint)?;
    if original != stored {
        return Err(failure(1));
    }
    output.verify_all(checkpoint)?;
    Ok(original)
}
async fn cleanup(state: &Shared) -> Result<()> {
    let state = state.clone();
    // Always join, even after the ordinary hash budget is exhausted. No signal
    // or timeout branch can drop this final file-close owner.
    tokio::task::spawn_blocking(move || {
        let mut state = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let tools = state.tools.take().map(Tools::finish).transpose();
        let output = state.output.take().map(Output::finish).transpose();
        tools.map(|_| ()).and(output.map(|_| ()))
    })
    .await
    .map_err(|_| failure(1))?
}
pub(super) async fn observation<E: Executor>(
    path: PathBuf,
    selected: String,
    caller: PathBuf,
    environment: Environment,
    budget: Budget,
    context: &Context,
    executor: &mut E,
) -> Result<super::RetainedObservation> {
    let state = Arc::new(Mutex::new(State::default()));
    let result = async {
        let captured = environment;
        let worker_budget = budget.clone();
        stage(&state, context, executor, move |state, checkpoint| {
            prepare(
                state,
                &path,
                &selected,
                &caller,
                &captured,
                &worker_budget,
                checkpoint,
            )
        })
        .await?;
        let version_bytes = call(&state, context, executor, &budget, 0).await?;
        version(&version_bytes)?;
        let repository = call(&state, context, executor, &budget, 1).await?;
        let attempt = call(&state, context, executor, &budget, 2).await?;
        let verification = call(&state, context, executor, &budget, 3).await?;
        let matched: MatchedVerifierPolicy = lock(&state)?
            .metadata
            .as_ref()
            .ok_or_else(|| failure(1))?
            .interpret_successful_verifier_output(&repository, &attempt, &verification)
            .map_err(|_| failure(1))?;
        stage(&state, context, executor, move |state, checkpoint| {
            checkpoint.check()?;
            state
                .tools
                .take()
                .map(Tools::finish)
                .transpose()
                .map(|_| ())
        })
        .await?;
        let worker_budget = budget.clone();
        stage(&state, context, executor, move |state, checkpoint| {
            payloads(state, &worker_budget, checkpoint)
        })
        .await?;
        let worker_budget = budget.clone();
        let frame = stage(&state, context, executor, move |state, checkpoint| {
            final_hashes(state, &worker_budget, checkpoint)
        })
        .await?;
        executor.ready().await?;
        context.admit()?;
        let observation = {
            let state = lock(&state)?;
            let request = state.request.as_ref().ok_or_else(|| failure(1))?;
            serde_json::to_vec(&serde_json::json!({
                "schemaVersion":1,"kind":"ops-retained-verification-observation",
                "requestSha256":request.sha256,"requestBytes":request.bytes,
                "files":state.expected.len(),"retainedFrameSha256":frame,
                "matchedVerifierPolicy":matched,
                "claims":"observedOnlineVerifierAndRetainedPayloadRelationship",
                "scope":"NoTransportBuildRuntimeOrInstallProof"
            }))
            .map_err(|_| failure(1))?
        };
        let saved = observation.clone();
        stage(&state, context, executor, move |state, checkpoint| {
            checkpoint.check()?;
            state.output.as_mut().ok_or_else(|| failure(1))?.write(
                "verification-observation.json",
                &saved,
                checkpoint,
            )
        })
        .await?;
        let (witness, executable) = stage(&state, context, executor, move |state, checkpoint| {
            let output = state.output.as_ref().ok_or_else(|| failure(1))?;
            output.verify_all(checkpoint)?;
            let witness = output.witness()?;
            Ok((witness, state.executable.take().ok_or_else(|| failure(1))?))
        })
        .await?;
        Ok(super::RetainedObservation {
            bytes: observation,
            witness,
            executable,
        })
    }
    .await;
    let closed = cleanup(&state).await;
    let observation = result?;
    closed?;
    executor.ready().await?;
    if !executor.cleanup_passed() {
        return Err(failure(1));
    }
    context.remaining()?;
    Ok(observation)
}

pub(super) async fn transaction<E: Executor>(
    path: PathBuf,
    selected: String,
    caller: PathBuf,
    environment: Environment,
    budget: Budget,
    context: &Context,
    executor: &mut E,
) -> Result<()> {
    let observation = observation(
        path,
        selected,
        caller,
        environment,
        budget,
        context,
        executor,
    )
    .await?;
    executor
        .replay(&observation.bytes, false, context.replay_deadline()?)
        .await?;
    executor.ready().await?;
    context.remaining()?;
    if !executor.cleanup_passed() {
        return Err(failure(1));
    }
    Ok(())
}
pub(super) async fn native(path: &Path, selected: &str) -> Result<()> {
    let signals = Signals::new()?;
    let caller = std::env::current_dir().map_err(|_| failure(1))?;
    let environment: Environment = std::env::vars_os().collect();
    let replay_environment = super::tools::base_environment();
    let context = Context::new(Arc::new(plan::NativeClock));
    let mut executor = Native::new(&caller, &replay_environment, signals);
    transaction(
        path.into(),
        selected.into(),
        caller.clone(),
        environment,
        Budget::default(),
        &context,
        &mut executor,
    )
    .await
}
