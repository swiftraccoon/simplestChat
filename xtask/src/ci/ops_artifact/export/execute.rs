//! One signal/process owner retains authenticated inputs until the exact export
//! and original request survive checked close, stdout replay and final fences.

use super::super::{acquire, namespace::Budget};
use super::{
    failure,
    files::{Original, Output, Witness},
    plan::{self, Plan},
    request::Request,
    worker::{self, Checkpoint},
};
use crate::{
    ci::process::{Executor, Native},
    owned_process::Signals,
    tooling::{Environment, Result},
};
use serde_json::json;
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, atomic::AtomicBool},
    time::Instant,
};

#[derive(Default)]
struct State {
    original: Option<Original>,
    request: Option<Request>,
    output: Option<Output>,
    authenticated: Option<acquire::AuthenticatedAcquisition>,
    witness: Option<Witness>,
    receipt: Vec<u8>,
}
type Shared = Arc<Mutex<State>>;
fn lock(state: &Shared) -> Result<std::sync::MutexGuard<'_, State>> {
    state.lock().map_err(|_| failure(1))
}
async fn stage<T: Send + 'static, E: Executor>(
    state: &Shared,
    plan: &Plan,
    executor: &mut E,
    end: Instant,
    operation: impl FnOnce(&mut State, &Checkpoint) -> Result<T> + Send + 'static,
) -> Result<T> {
    let state = state.clone();
    worker::work(plan, executor, end, move |checkpoint| {
        let mut state = lock(&state)?;
        operation(&mut state, &checkpoint)
    })
    .await
}
fn prepare(
    state: &mut State,
    path: &Path,
    selected: &str,
    environment: &Environment,
    budget: &Budget,
    checkpoint: &Checkpoint,
) -> Result<(PathBuf, String)> {
    if !super::request::path(path) {
        return Err(failure(1));
    }
    let (original, bytes) = Original::open(path, budget, checkpoint)?;
    let request = Request::decode(&bytes, selected)?;
    let token = environment
        .get(OsStr::new("GH_TOKEN"))
        .ok_or_else(|| failure(1))?;
    if token.is_empty()
        || token.as_encoded_bytes().len() > 16384
        || token.as_encoded_bytes().contains(&0)
        || path.starts_with(&request.output)
        || request.output.starts_with(path)
    {
        return Err(failure(1));
    }
    state.original = Some(original);
    state.request = Some(request);
    let request = state.request.as_ref().ok_or_else(|| failure(1))?;
    state.output = Some(Output::new(&request.output, budget)?);
    let output = state.output.as_mut().ok_or_else(|| failure(1))?;
    let bytes = request.acquire()?;
    output.write("acquire-request.json", &bytes, checkpoint)?;
    let path = output.path("acquire-request.json");
    // Authentication's mature helper allowance has its own retained-FD bound.
    // Keep the original witness, not this whole export ancestor chain, across it.
    let output = state.output.take().ok_or_else(|| failure(1))?;
    state.witness = Some(output.finish(checkpoint)?);
    Ok((path, super::super::sha(&bytes)))
}
fn build(state: &mut State, budget: &Budget, checkpoint: &Checkpoint) -> Result<()> {
    state
        .original
        .as_ref()
        .ok_or_else(|| failure(1))?
        .check(budget, checkpoint)?;
    let request = state.request.as_ref().ok_or_else(|| failure(1))?;
    let authenticated = state.authenticated.as_ref().ok_or_else(|| failure(1))?;
    authenticated.check(budget, &checkpoint.acquired())?;
    let output = state.witness.as_mut().ok_or_else(|| failure(1))?;
    output.acquired(budget, checkpoint)?;
    let (pair, transfer_sha, transfer_bytes) =
        output.transfer(authenticated, budget, checkpoint)?;
    let executable = authenticated.executable()?;
    if executable.sha256 != pair.binary().sha256() || executable.bytes != pair.binary().bytes() {
        return Err(failure(1));
    }
    let installed = match request.installed.as_ref() {
        Some(pin) if pin.sha256 == pair.binary().sha256() => {
            if pin.bytes != pair.binary().bytes() {
                return Err(failure(1));
            }
            None
        }
        expected => expected,
    };
    let operation = if installed.is_some() {
        "upgrade"
    } else {
        "install"
    };
    let remote = |name: &str, pin: &acquire::PairPins| match name {
        "ops-release.json" => json!({"path":request.remote_input.join(name),
            "sha256":pin.manifest().sha256(),"bytes":pin.manifest().bytes()}),
        "ops-selection.json" => json!({"path":request.remote_input.join(name),
            "sha256":pin.selection().sha256(),"bytes":pin.selection().bytes()}),
        _ => json!({"path":request.remote_input.join("simplestchat-ops"),
            "sha256":pin.binary().sha256(),"bytes":pin.binary().bytes()}),
    };
    let mut install = json!({"schemaVersion":if installed.is_some(){2}else{1},
        "manifest":remote("ops-release.json",&pair),"selection":remote("ops-selection.json",&pair),
        "binary":remote("simplestchat-ops",&pair)});
    if let Some(pin) = installed {
        install["installedBinary"] = json!(pin);
    }
    let bytes = serde_json::to_vec(&install).map_err(|_| failure(1))?;
    output.write("install-request.json", &bytes, budget, checkpoint)?;
    let mut receipt = serde_json::to_vec(&json!({
        "schemaVersion":1,"scope":"AuthenticatedOpsDistributionExport",
        "requestSha256":request.sha256,"selection":request.selection.identity(),
        "artifactId":request.artifact_id,"operation":operation,"remoteInput":request.remote_input,
        "transfer":{"path":output.path("ops-pair.tar"),
            "sha256":transfer_sha,"bytes":transfer_bytes},
        "installerRequest":{"path":output.path("install-request.json"),
            "sha256":super::super::sha(&bytes),"bytes":bytes.len()},
        "binary":{"sha256":pair.binary().sha256(),"bytes":pair.binary().bytes()},
        "installedBinary":request.installed,
    }))
    .map_err(|_| failure(1))?;
    receipt.push(b'\n');
    output.write("export.json", &receipt, budget, checkpoint)?;
    authenticated.check(budget, &checkpoint.acquired())?;
    state
        .original
        .as_ref()
        .ok_or_else(|| failure(1))?
        .check(budget, checkpoint)?;
    state.receipt = receipt;
    checkpoint.check()
}
fn recheck(state: &State, budget: &Budget, checkpoint: &Checkpoint) -> Result<()> {
    state
        .original
        .as_ref()
        .ok_or_else(|| failure(1))?
        .check(budget, checkpoint)?;
    state
        .authenticated
        .as_ref()
        .ok_or_else(|| failure(1))?
        .check(budget, &checkpoint.acquired())?;
    if let Some(witness) = &state.witness {
        witness.check(budget, checkpoint)?;
    }
    checkpoint.check()
}
async fn cleanup(state: &Shared, plan: &Plan) -> Result<()> {
    let state = state.clone();
    let checkpoint = Checkpoint {
        clock: plan.clock.clone(),
        start: plan.origin(),
        end: plan.end(),
        abort: Arc::new(AtomicBool::new(false)),
    };
    tokio::task::spawn_blocking(move || {
        let mut state = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .output
            .take()
            .map(|output| output.finish(&checkpoint).map(|_| ()))
            .unwrap_or(Ok(()))
    })
    .await
    .map_err(|_| failure(1))?
}
pub(super) struct Inputs {
    pub path: PathBuf,
    pub selected: String,
    pub caller: PathBuf,
    pub environment: Environment,
}
pub(super) async fn transaction<E: Executor>(
    inputs: Inputs,
    budget: Budget,
    plan: &Plan,
    executor: &mut E,
) -> Result<()> {
    let state = Arc::new(Mutex::new(State::default()));
    let result = async {
        let end = plan.begin(0)?;
        let worker_budget = budget.clone();
        let worker_path = inputs.path;
        let worker_selected = inputs.selected;
        let worker_environment = inputs.environment.clone();
        let (path, selected) = stage(&state, plan, executor, end, move |state, checkpoint| {
            prepare(
                state,
                &worker_path,
                &worker_selected,
                &worker_environment,
                &worker_budget,
                checkpoint,
            )
        })
        .await?;
        plan.complete(0, end)?;
        let end = plan.begin(1)?;
        let authenticated = Box::pin(acquire::observe_acquired(
            path,
            selected,
            inputs.caller,
            inputs.environment,
            budget.clone(),
            acquire::Window {
                clock: plan.clock.clone(),
                end,
            },
            executor,
        ))
        .await?;
        lock(&state)?.authenticated = Some(authenticated);
        plan.complete(1, end)?;
        let end = plan.begin(2)?;
        let worker_budget = budget.clone();
        stage(&state, plan, executor, end, move |state, checkpoint| {
            build(state, &worker_budget, checkpoint)
        })
        .await?;
        plan.complete(2, end)?;
        let end = plan.begin(3)?;
        let worker_budget = budget.clone();
        let receipt = stage(&state, plan, executor, end, move |state, checkpoint| {
            recheck(state, &worker_budget, checkpoint)?;
            Ok(state.receipt.clone())
        })
        .await?;
        plan.complete(3, end)?;
        let end = plan.begin(4)?;
        let replayed = executor.replay(&receipt, false, end).await;
        let pending = executor.ready().await;
        let timely = plan.complete(4, end);
        let end = plan.begin(5);
        let worker_budget = budget.clone();
        let checked = match end {
            Ok(end) => {
                let result = stage(&state, plan, executor, end, move |state, checkpoint| {
                    recheck(state, &worker_budget, checkpoint)
                })
                .await;
                result.and(plan.complete(5, end))
            }
            Err(error) => Err(error),
        };
        replayed?;
        pending?;
        timely?;
        checked?;
        let end = plan.begin(6)?;
        executor.ready().await?;
        if !executor.cleanup_passed() {
            return Err(failure(1));
        }
        plan.complete(6, end)?;
        Ok(())
    }
    .await;
    let closed = cleanup(&state, plan).await;
    result?;
    closed?;
    executor.ready().await?;
    plan.check()?;
    if !executor.cleanup_passed() {
        return Err(failure(1));
    }
    Ok(())
}
pub(super) async fn native(
    path: &Path,
    selected: &str,
    completion: Arc<simplestchat_ops::owned_process::CompletionSignals>,
    started: Instant,
) -> Result<()> {
    let signals = Signals::from_completion(completion);
    let caller = std::env::current_dir().map_err(|_| failure(1))?;
    let environment: Environment = std::env::vars_os().collect();
    let replay: Environment = [("PATH", "/usr/bin:/bin"), ("LANG", "C"), ("LC_ALL", "C")]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect();
    let plan = Plan::from_start(Arc::new(plan::NativeClock), started);
    let mut executor = Native::new(&caller, &replay, signals);
    Box::pin(transaction(
        Inputs {
            path: path.into(),
            selected: selected.into(),
            caller: caller.clone(),
            environment,
        },
        Budget::default(),
        &plan,
        &mut executor,
    ))
    .await
}
