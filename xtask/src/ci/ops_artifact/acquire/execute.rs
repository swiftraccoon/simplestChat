//! One Signals/Native owner from original request through authenticated retention.

use super::super::{namespace::Budget, verify};
use super::{
    failure,
    files::{self, Original, Output, Witness},
    hash,
    metadata::{self, Envelope},
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
    tooling::{Environment, Result},
};
use simplestchat_ops::distribution::manifest::verification::RetainedMetadata;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

#[derive(Default)]
struct State {
    request: Option<Request>,
    original: Option<Original>,
    output: Option<Output>,
    witness: Option<Witness>,
    retained: Option<verify::RetainedWitness>,
    tools: Option<Tools>,
    envelope: Option<Envelope>,
    files: usize,
    expanded: u64,
}
type Shared = Arc<Mutex<State>>;
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
    let (original, bytes) = files::original(path, budget, checkpoint)?;
    let request = Request::decode(&bytes, selected)?;
    Request::token(environment)?;
    if path.starts_with(&request.output) || request.output.starts_with(path) {
        return Err(failure(1));
    }
    state.original = Some(original);
    state.request = Some(request);
    let request = state.request.as_ref().unwrap();
    state.output = Some(Output::new(&request.output, budget)?);
    state.tools = Some(Tools::open(caller, environment, request, budget)?);
    checkpoint.check()
}
fn version(bytes: &[u8]) -> Result<()> {
    let text = std::str::from_utf8(bytes).map_err(|_| failure(1))?;
    let first = text.lines().next().ok_or_else(|| failure(1))?;
    if bytes.len() > 4096
        || text.contains('\0')
        || (first != "gh version 2.102.0" && !first.starts_with("gh version 2.102.0 ("))
    {
        Err(failure(1))
    } else {
        Ok(())
    }
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
    let output = lock(state)?.output.take().ok_or_else(|| failure(1))?;
    let maximum = [4096, super::SMALL, super::SMALL, super::SMALL, super::ZIP][index];
    let pending = output.capture(index, maximum).await;
    lock(state)?.output = Some(output);
    let mut pending = pending?;
    let prepared = (|| {
        let state = lock(state)?;
        let tools = state.tools.as_ref().ok_or_else(|| failure(1))?;
        let invocation = tools.invocation(index)?;
        let mut command = Command::capture(Program::OpsGh, &[]);
        command.arguments = tools.arguments(index)?;
        command.capture = None;
        command.timeout = timeout;
        command.grace = plan::GRACE;
        command.ops_acquire = Some(invocation.clone());
        Ok((command, invocation))
    })();
    let (mut command, invocation) = match prepared {
        Ok(value) => value,
        Err(error) => {
            let mut command = Command::capture(Program::OpsGh, &[]);
            command.capture = None;
            command.separate_logs = pending.logs.take();
            let _ = crate::ci::process::settle_unforwarded(command).await;
            return Err(error);
        }
    };
    command.separate_logs = pending.logs.take();
    let allowance = match budget.reserve_with_limit(16, 48) {
        Ok(allowance) => allowance,
        Err(error) => {
            let _ = crate::ci::process::settle_unforwarded(command).await;
            return Err(error);
        }
    };
    let admitted = async {
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
    if let Err(error) = admitted {
        let _ = crate::ci::process::settle_unforwarded(command).await;
        return Err(error);
    }
    let result = executor.run(command).await;
    let seals = invocation.captures();
    drop(invocation);
    drop(allowance);
    let advanced = context.complete_command(index);
    let read = match seals {
        Ok(seals) => {
            stage(state, context, executor, move |state, checkpoint| {
                state
                    .output
                    .as_mut()
                    .ok_or_else(|| failure(1))?
                    .capture_read(pending, &seals, checkpoint, index != 4)
            })
            .await
        }
        Err(error) => Err(error),
    };
    result?;
    let bytes = read?;
    advanced?;
    executor.ready().await?;
    context.admit()?;
    Ok(bytes)
}
fn extracted(
    state: &mut State,
    budget: &Budget,
    checkpoint: &hash::Checkpoint,
) -> Result<(PathBuf, String)> {
    let request = state.request.as_ref().ok_or_else(|| failure(1))?;
    let envelope = state.envelope.as_ref().ok_or_else(|| failure(1))?;
    let output = state.output.as_mut().ok_or_else(|| failure(1))?;
    let inventory = output.extract(&envelope.zip_sha256, envelope.zip_bytes, checkpoint)?;
    let metadata = RetainedMetadata::decode(
        &request.selection,
        &output.small_payload("ops-release.json", checkpoint)?,
        &output.small_payload("ops-evidence.json", checkpoint)?,
        &output.small_payload("ops-predicate.json", checkpoint)?,
        &output.small_payload("ops-preparation.json", checkpoint)?,
    )
    .map_err(|_| failure(1))?;
    let mut expected = metadata
        .payloads()
        .iter()
        .map(|entry| (entry.name(), (entry.sha256(), entry.bytes())))
        .collect::<BTreeMap<_, _>>();
    let bundle = inventory
        .records()
        .iter()
        .find(|entry| entry.name() == "ops-attestation.jsonl")
        .ok_or_else(|| failure(1))?;
    if expected
        .insert(bundle.name(), (bundle.sha256(), bundle.bytes()))
        .is_some()
        || expected.len() != inventory.records().len()
        || inventory.records().iter().any(|entry| {
            expected.get(entry.name()).copied() != Some((entry.sha256(), entry.bytes()))
        })
        || inventory.bytes() > super::EXPANDED
    {
        return Err(failure(1));
    }
    state.files = inventory.records().len();
    state.expanded = inventory.bytes();
    let bytes = serde_json::to_vec(&serde_json::json!({
        "schemaVersion":1, "selection":request.selection.identity(),
        "directory":output.path("input"), "output":output.path("verified"),
        "authentication":"gh-token"
    }))
    .map_err(|_| failure(1))?;
    output.write("verify-request.json", &bytes, checkpoint)?;
    let path = output.path("verify-request.json");
    let sha = super::super::sha(&bytes);
    let tools = state.tools.take().map(Tools::finish).transpose();
    let output = state.output.take().ok_or_else(|| failure(1))?.finish();
    tools?;
    state.witness = Some(output?);
    state
        .original
        .as_ref()
        .ok_or_else(|| failure(1))?
        .read(budget, checkpoint)?;
    state
        .witness
        .as_ref()
        .unwrap()
        .check(budget, checkpoint, false)?;
    Ok((path, sha))
}
async fn cleanup(state: &Shared) -> Result<()> {
    let state = state.clone();
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
struct VerificationClock(Arc<dyn plan::Clock>);
impl verify::RetainedClock for VerificationClock {
    fn now(&self) -> std::time::Instant {
        self.0.now()
    }
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
    let state = Arc::new(Mutex::new(State::default()));
    let result = async {
        let worker_path = path.clone();
        let worker_caller = caller.clone();
        let worker_environment = environment.clone();
        let worker_budget = budget.clone();
        let worker_selected = selected.clone();
        stage(&state, context, executor, move |state, checkpoint| {
            prepare(
                state,
                &worker_path,
                &worker_selected,
                &worker_caller,
                &worker_environment,
                &worker_budget,
                checkpoint,
            )
        })
        .await?;
        version(&call(&state, context, executor, &budget, 0).await?)?;
        let repository = call(&state, context, executor, &budget, 1).await?;
        let attempt = call(&state, context, executor, &budget, 2).await?;
        let artifact = call(&state, context, executor, &budget, 3).await?;
        {
            let mut state = lock(&state)?;
            state.envelope = Some(metadata::admit(
                &repository,
                &attempt,
                &artifact,
                state.request.as_ref().ok_or_else(|| failure(1))?,
            )?);
        }
        call(&state, context, executor, &budget, 4).await?;
        let worker_budget = budget.clone();
        let (verification_path, verification_sha) =
            stage(&state, context, executor, move |state, checkpoint| {
                extracted(state, &worker_budget, checkpoint)
            })
            .await?;
        executor.ready().await?;
        context.verification_window()?;
        let verified = verify::observe_retained(
            verification_path,
            verification_sha,
            caller,
            environment,
            budget.clone(),
            verify::Window {
                clock: Arc::new(VerificationClock(context.clock.clone())),
                end: context.end(),
            },
            executor,
        )
        .await?;
        context.complete_verification()?;
        executor.ready().await?;
        let worker_budget = budget.clone();
        let observation = stage(&state, context, executor, move |state, checkpoint| {
            verified
                .witness
                .check(&worker_budget, &checkpoint.retained())?;
            state
                .original
                .as_ref()
                .ok_or_else(|| failure(1))?
                .read(&worker_budget, checkpoint)?;
            let witness = state.witness.as_mut().ok_or_else(|| failure(1))?;
            witness.check(&worker_budget, checkpoint, true)?;
            let request = state.request.as_ref().ok_or_else(|| failure(1))?;
            let verification: serde_json::Value =
                serde_json::from_slice(&verified.bytes).map_err(|_| failure(1))?;
            let observation = serde_json::to_vec(&serde_json::json!({
                "schemaVersion":1,"kind":"ops-artifact-acquisition-observation",
                "requestSha256":request.sha256,"requestBytes":request.bytes,
                "selection":request.selection.identity(),"artifact":state.envelope,
                "retainedFiles":state.files,"retainedBytes":state.expanded,
                "verification":verification,
                "scope":"AuthenticatedRetainedBytesWithoutExecutionOrInstallation"
            }))
            .map_err(|_| failure(1))?;
            witness.write(
                "acquisition-observation.json",
                &observation,
                &worker_budget,
                checkpoint,
            )?;
            verified
                .witness
                .check(&worker_budget, &checkpoint.retained())?;
            state.retained = Some(verified.witness);
            Ok(observation)
        })
        .await?;
        Ok(observation)
    }
    .await;
    let closed = cleanup(&state).await;
    let observation = result?;
    closed?;
    executor.ready().await?;
    context.remaining()?;
    if !executor.cleanup_passed() {
        return Err(failure(1));
    }
    let replayed = executor
        .replay(&observation, false, context.replay_deadline()?)
        .await;
    let pending = executor.ready().await;
    let advanced = context.complete_replay();
    let worker_budget = budget.clone();
    let final_files = stage(&state, context, executor, move |state, checkpoint| {
        state
            .original
            .as_ref()
            .ok_or_else(|| failure(1))?
            .read(&worker_budget, checkpoint)?;
        state.witness.as_ref().ok_or_else(|| failure(1))?.check(
            &worker_budget,
            checkpoint,
            true,
        )?;
        state
            .retained
            .as_ref()
            .ok_or_else(|| failure(1))?
            .check(&worker_budget, &checkpoint.retained())
    })
    .await;
    replayed?;
    pending?;
    advanced?;
    final_files?;
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
    let replay: Environment = [("PATH", "/usr/bin:/bin"), ("LANG", "C"), ("LC_ALL", "C")]
        .into_iter()
        .map(|(key, value)| (key.into(), value.into()))
        .collect();
    let context = Context::new(Arc::new(plan::NativeClock));
    let mut executor = Native::new(&caller, &replay, signals);
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
