//! Complete controller ownership: selected checkout, authenticated bytes,
//! immutable transfer, one trusted keeper and original witnesses through replay.

use super::super::{acquire, namespace::Budget};
use super::{
    failure,
    files::{Input, Original, Output},
    plan::{self, Plan},
    receipt::{self, Pins},
    request::Request,
    sources::Sources,
    tools::Tools,
    worker::{self, Checkpoint},
};
use crate::{
    ci::{
        command::{Command, Program},
        process::{self, Executor, Native},
    },
    owned_process::Signals,
    tooling::{Environment, Result},
};
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Default)]
struct State {
    request: Option<Request>,
    original: Option<Original>,
    output: Option<Output>,
    tools: Option<Tools>,
    sources: Option<Arc<Sources>>,
    authenticated: Option<acquire::AuthenticatedAcquisition>,
    input: Option<Input>,
    pins: Option<Pins>,
    io_settled: bool,
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
    settle: impl FnOnce(T) -> Result<()> + Send + 'static,
) -> Result<T> {
    let state = state.clone();
    worker::work(
        plan,
        executor,
        end,
        move |checkpoint| {
            let mut state = lock(&state)?;
            operation(&mut state, &checkpoint)
        },
        settle,
    )
    .await
}
fn prepare(
    state: &mut State,
    path: &Path,
    selected: &str,
    caller: &Path,
    environment: &Environment,
    budget: &Budget,
    checkpoint: &Checkpoint,
) -> Result<()> {
    if !super::request::path(path) || !super::request::path(caller) {
        return Err(failure(1));
    }
    let (original, bytes) = Original::open(path, super::request::MAXIMUM, budget, checkpoint)?;
    original.require_private()?;
    let request = Request::decode(&bytes, selected)?;
    let token = environment
        .get(OsStr::new("GH_TOKEN"))
        .ok_or_else(|| failure(1))?;
    if token.is_empty()
        || token.as_encoded_bytes().len() > 16384
        || token.as_encoded_bytes().contains(&0)
        || request.output.starts_with(caller)
        || caller.starts_with(&request.output)
        || request.output.starts_with(path)
        || path.starts_with(&request.output)
    {
        return Err(failure(1));
    }
    state.original = Some(original);
    state.output = Some(Output::new(&request.output, caller, budget, checkpoint)?);
    state.tools = Some(Tools::open(
        caller,
        state.output.as_ref().unwrap(),
        environment,
        budget,
        checkpoint,
    )?);
    state.sources = Some(state.tools.as_ref().unwrap().sources());
    state.request = Some(request);
    state.io_settled = true;
    checkpoint.check()
}
async fn call<E: Executor>(
    state: &Shared,
    plan: &Plan,
    executor: &mut E,
    budget: &Budget,
    index: usize,
    args: Vec<OsString>,
    end: Instant,
) -> Result<Vec<u8>> {
    executor.ready().await?;
    plan.check()?;
    let output = lock(state)?.output.take().ok_or_else(|| failure(1))?;
    let pending = output.capture(index).await;
    lock(state)?.output = Some(output);
    let mut pending = pending?;
    let mut command = Command::capture(
        if index == 3 {
            Program::OpsRuntimePython
        } else {
            Program::OpsRuntimeGit
        },
        &[],
    );
    command.capture = None;
    command.arguments = args;
    command.separate_logs = pending.logs.take();
    command.timeout = if index == 3 {
        plan::KEEPER
    } else {
        Duration::from_secs(5)
    };
    command.grace = if index == 3 {
        plan::GRACE
    } else {
        Duration::from_secs(5)
    };
    let reserved = budget.reserve(16);
    let allowance = match reserved {
        Ok(value) => value,
        Err(error) => {
            let _ = process::settle_unforwarded(command).await;
            return Err(error);
        }
    };
    if index == 3 {
        let cloned = stage(
            state,
            plan,
            executor,
            end,
            |state, checkpoint| {
                state
                    .input
                    .as_ref()
                    .ok_or_else(|| failure(1))?
                    .clone_input(checkpoint)
            },
            |file| nix::unistd::close(file).map_err(|_| failure(1)),
        )
        .await;
        match cloned {
            Ok(file) => command.stdin = Some(Arc::new(file)),
            Err(error) => {
                let _ = process::settle_unforwarded(command).await;
                return Err(error);
            }
        }
    }
    let proof = (|| {
        let state = lock(state)?;
        let tools = state.tools.as_ref().ok_or_else(|| failure(1))?;
        if index == 3 {
            tools.keeper_invocation(
                state.pins.as_ref().ok_or_else(|| failure(1))?,
                state
                    .input
                    .as_ref()
                    .ok_or_else(|| failure(1))?
                    .stamp()
                    .clone(),
                plan,
                end,
            )
        } else {
            tools.git_invocation(index, plan, end)
        }
    })();
    let proof = match proof {
        Ok(value) => value,
        Err(error) => {
            let _ = process::settle_unforwarded(command).await;
            return Err(error);
        }
    };
    command.ops_runtime = Some(proof.clone());
    let result = executor.run(command).await;
    let settled = proof.settled();
    let captures = proof.captures();
    drop(proof);
    drop(allowance);
    let read = match captures {
        Ok(captures) => {
            stage(
                state,
                plan,
                executor,
                end,
                move |state, checkpoint| {
                    state
                        .output
                        .as_mut()
                        .ok_or_else(|| failure(1))?
                        .capture_read(pending, &captures, checkpoint)
                },
                |_| Ok(()),
            )
            .await
        }
        Err(error) => Err(error),
    };
    if read.is_err() {
        lock(state)?.io_settled = false;
    }
    let checked = if index == 3 {
        stage(
            state,
            plan,
            executor,
            end,
            |state, checkpoint| {
                state
                    .input
                    .as_ref()
                    .ok_or_else(|| failure(1))?
                    .check(checkpoint)
            },
            |_| Ok(()),
        )
        .await
    } else {
        Ok(())
    };
    result?;
    settled?;
    let bytes = read?;
    checked?;
    executor.ready().await?;
    plan.check()?;
    Ok(bytes)
}
fn original_checks(state: &State, budget: &Budget, checkpoint: &Checkpoint) -> Result<()> {
    if state.original.as_ref().ok_or_else(|| failure(1))?.sha256()
        != state.request.as_ref().ok_or_else(|| failure(1))?.sha256
    {
        return Err(failure(1));
    }
    state
        .original
        .as_ref()
        .ok_or_else(|| failure(1))?
        .check(budget, checkpoint)?;
    state
        .output
        .as_ref()
        .ok_or_else(|| failure(1))?
        .check(checkpoint)?;
    state
        .sources
        .as_ref()
        .ok_or_else(|| failure(1))?
        .check(checkpoint)?;
    state
        .authenticated
        .as_ref()
        .ok_or_else(|| failure(1))?
        .check(budget, &checkpoint.acquired())?;
    if let Some(input) = &state.input {
        input.check(checkpoint)?;
    }
    checkpoint.check()
}
async fn cleanup(state: &Shared, plan: &Plan) -> Result<()> {
    let state = state.clone();
    let checkpoint = Checkpoint {
        clock: plan.clock.clone(),
        start: plan.clock.now(),
        end: plan.end(),
        abort: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    tokio::task::spawn_blocking(move || {
        let mut state = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let input = state
            .input
            .take()
            .map(|input| input.finish(&checkpoint))
            .transpose();
        let tools = state.tools.take().map(Tools::finish).transpose();
        input.map(|_| ()).and(tools.map(|_| ()))
    })
    .await
    .map_err(|_| failure(1))?
}
struct AcquisitionClock(Arc<dyn plan::Clock>);
impl acquire::AcquisitionClock for AcquisitionClock {
    fn now(&self) -> Instant {
        self.0.now()
    }
}

pub(super) struct Inputs {
    pub path: PathBuf,
    pub selected: String,
    pub caller: PathBuf,
    pub environment: Environment,
    pub run_id: String,
}
pub(super) async fn transaction<E: Executor>(
    inputs: Inputs,
    budget: Budget,
    plan: &Plan,
    executor: &mut E,
) -> Result<()> {
    let Inputs {
        path,
        selected,
        caller,
        environment,
        run_id,
    } = inputs;
    let state = Arc::new(Mutex::new(State::default()));
    let mut publication_end = None;
    let result = async {
        let end = plan.begin(0)?;
        let worker_path = path.clone();
        let worker_selected = selected.clone();
        let worker_caller = caller.clone();
        let captured = environment.clone();
        let worker_budget = budget.clone();
        stage(
            &state,
            plan,
            executor,
            end,
            move |state, checkpoint| {
                prepare(
                    state,
                    &worker_path,
                    &worker_selected,
                    &worker_caller,
                    &captured,
                    &worker_budget,
                    checkpoint,
                )
            },
            |_| Ok(()),
        )
        .await?;
        for index in 0..3 {
            let bytes = call(
                &state,
                plan,
                executor,
                &budget,
                index,
                Tools::git_arguments(index)?,
                end,
            )
            .await?;
            if index == 0 {
                let expected = serde_json::to_value(
                    lock(&state)?.request.as_ref().unwrap().selection.identity(),
                )
                .map_err(|_| failure(1))?;
                let revision = expected["revision"].as_str().ok_or_else(|| failure(1))?;
                if bytes != format!("{revision}\n").as_bytes() {
                    return Err(failure(1));
                }
            } else if index == 1 {
                if !bytes.is_empty() {
                    return Err(failure(1));
                }
            } else {
                let state = lock(&state)?;
                let selected =
                    serde_json::to_value(state.request.as_ref().unwrap().selection.identity())
                        .map_err(|_| failure(1))?;
                state.sources.as_ref().unwrap().archive(
                    &bytes,
                    selected["revision"].as_str().ok_or_else(|| failure(1))?,
                )?;
            }
        }
        plan.complete(0, end)?;
        let end = plan.begin(1)?;
        let (acquisition_path, acquisition_sha) = stage(
            &state,
            plan,
            executor,
            end,
            |state, checkpoint| {
                let bytes = state.request.as_ref().unwrap().acquire_request()?;
                let output = state.output.as_mut().unwrap();
                output.write("acquire-request.json", &bytes, checkpoint)?;
                Ok((
                    output.path.join("acquire-request.json"),
                    super::super::sha(&bytes),
                ))
            },
            |_| Ok(()),
        )
        .await?;
        // Keep the embedded verifier's large Native future out of this outer
        // state machine while borrowing the very same executor and owner.
        let acquired = Box::pin(acquire::observe_acquired(
            acquisition_path,
            acquisition_sha,
            caller.clone(),
            environment,
            budget.clone(),
            acquire::Window {
                clock: Arc::new(AcquisitionClock(plan.clock.clone())),
                end,
            },
            executor,
        ))
        .await?;
        lock(&state)?.authenticated = Some(acquired);
        plan.complete(1, end)?;
        let end = plan.begin(2)?;
        stage(
            &state,
            plan,
            executor,
            end,
            move |state, checkpoint| {
                let authenticated = state.authenticated.as_ref().ok_or_else(|| failure(1))?;
                let executable = authenticated.executable()?;
                let (input, pair) = state
                    .output
                    .as_mut()
                    .unwrap()
                    .transfer(authenticated, checkpoint)?;
                let transfer_sha = input.sha256().to_owned();
                let transfer_bytes = input.bytes();
                state.input = Some(input);
                state.pins = Some(Pins::new(
                    state.request.as_ref().unwrap(),
                    pair,
                    &executable,
                    run_id,
                    transfer_sha,
                    transfer_bytes,
                    state.sources.as_ref().unwrap().cloud_sha512.clone(),
                )?);
                checkpoint.check()
            },
            |_| Ok(()),
        )
        .await?;
        plan.complete(2, end)?;
        let end = plan.begin(3)?;
        let args = {
            let state = lock(&state)?;
            state
                .tools
                .as_ref()
                .unwrap()
                .keeper_arguments(state.pins.as_ref().unwrap())
        };
        let keeper = call(&state, plan, executor, &budget, 3, args, end).await?;
        let observation = {
            let state = lock(&state)?;
            receipt::admit(&keeper, state.pins.as_ref().unwrap())?
        };
        plan.complete(3, end)?;
        let end = plan.begin(4)?;
        publication_end = Some(end);
        let worker_budget = budget.clone();
        stage(
            &state,
            plan,
            executor,
            end,
            move |state, checkpoint| {
                original_checks(state, &worker_budget, checkpoint)?;
                if !state.io_settled {
                    return Err(failure(1));
                }
                receipt::summary(state.pins.as_ref().unwrap(), &observation)
            },
            |_| Ok(()),
        )
        .await
    }
    .await;
    let closed = cleanup(&state, plan).await;
    let summary = result?;
    closed?;
    executor.ready().await?;
    plan.check()?;
    if !executor.cleanup_passed() {
        return Err(failure(1));
    }
    let end = publication_end.ok_or_else(|| failure(1))?;
    let worker_budget = budget.clone();
    let original = summary.clone();
    stage(
        &state,
        plan,
        executor,
        end,
        move |state, checkpoint| {
            original_checks(state, &worker_budget, checkpoint)?;
            state
                .output
                .as_mut()
                .unwrap()
                .write("summary.json", &original, checkpoint)
        },
        |_| Ok(()),
    )
    .await?;
    plan.complete(4, end)?;
    let end = plan.begin(5)?;
    let replay = executor.replay(&summary, false, end).await;
    let pending = executor.ready().await;
    let replayed = plan.complete(5, end);
    let end = plan.begin(6);
    let files = match end {
        Ok(end) => {
            let worker_budget = budget.clone();
            stage(
                &state,
                plan,
                executor,
                end,
                move |state, checkpoint| original_checks(state, &worker_budget, checkpoint),
                |_| Ok(()),
            )
            .await
            .and_then(|()| plan.complete(6, end))
        }
        Err(error) => Err(error),
    };
    replay?;
    pending?;
    replayed?;
    files?;
    executor.ready().await?;
    plan.check()?;
    if !executor.cleanup_passed() {
        return Err(failure(1));
    }
    Ok(())
}
fn random(budget: &Budget) -> Result<String> {
    use std::io::Read;
    let _permit = budget.permit()?;
    let mut file = File::open("/dev/urandom").map_err(|_| failure(1))?;
    let mut bytes = [0; 16];
    let read = file.read_exact(&mut bytes).map_err(|_| failure(1));
    let closed = nix::unistd::close(file).map_err(|_| failure(1));
    read.and(closed)?;
    Ok(super::super::hex(&bytes))
}
pub(super) async fn native(path: &Path, selected: &str) -> Result<()> {
    let platform = rustix::system::uname();
    if platform.sysname().to_bytes() != b"Linux"
        || platform.machine().to_bytes() != b"x86_64"
        || rustix::process::geteuid().as_raw() == 0
    {
        return Err(failure(1));
    }
    let signals = Signals::new()?;
    let caller = std::env::current_dir().map_err(|_| failure(1))?;
    let environment = crate::tooling::inherited_env();
    let replay: Environment = [("PATH", "/usr/bin:/bin"), ("LANG", "C"), ("LC_ALL", "C")]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect();
    let plan = Plan::new(Arc::new(plan::NativeClock));
    let budget = Budget::default();
    let run_id = random(&budget)?;
    let mut executor = Native::new(&caller, &replay, signals);
    Box::pin(transaction(
        Inputs {
            path: path.into(),
            selected: selected.into(),
            caller: caller.clone(),
            environment,
            run_id,
        },
        budget,
        &plan,
        &mut executor,
    ))
    .await
}
