//! Select coordinates, freeze the target, invoke once and preserve remote uncertainty.

use super::super::namespace::Budget;
use super::{
    failure,
    files::{self, Candidate, Evidence, Original},
    inventory::{self, Target},
    keep,
    options::Options,
    plan::{self, Plan},
    policy::{self, Artifact, ArtifactKind, Revision, Run},
    tools::{Spec, Tools},
    worker::{self, Checkpoint},
};
use crate::{
    ci::{
        command::{Command, Program},
        process::{Executor, Native},
    },
    owned_process::Signals,
    tooling::{Environment, Result},
};
use serde_json::{Value, json};
use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const SMALL: u64 = 4 * 1024 * 1024;
const GH_API: Duration = Duration::from_secs(20);
#[derive(Default)]
struct State {
    evidence: Option<Evidence>,
    tools: Option<Tools>,
    inventory: Option<Original>,
    sources: Vec<Original>,
    git_inputs: Vec<files::GitInput>,
    candidates: Vec<Candidate>,
    commands: usize,
    ansible_exit: Option<super::tools::ExitObservation>,
    signals: Option<Arc<simplestchat_ops::owned_process::CompletionSignals>>,
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
    worker::work(plan, executor, end, move |cp| {
        let mut state = lock(&state)?;
        operation(&mut state, &cp)
    })
    .await
}
fn check(state: &State, budget: &Budget, cp: &Checkpoint) -> Result<()> {
    state
        .inventory
        .as_ref()
        .ok_or_else(|| failure(1))?
        .check(budget, cp)?;
    for source in &state.sources {
        source.check(budget, cp)?;
    }
    for input in &state.git_inputs {
        input.check(budget, cp)?;
    }
    for candidate in &state.candidates {
        candidate.check(budget)?;
        cp.check()?;
    }
    state.evidence.as_ref().ok_or_else(|| failure(1))?.check(cp)
}

struct SourceCustody {
    state: Shared,
    budget: Budget,
    checkpoint: Checkpoint,
}
impl super::tools::Custody for SourceCustody {
    fn check(&self) -> Result<()> {
        let state = lock(&self.state)?;
        if let Some(code) = state.signals.as_ref().and_then(|flags| flags.status()) {
            return Err(failure(code));
        }
        check(&state, &self.budget, &self.checkpoint)?;
        if let Some(code) = state.signals.as_ref().and_then(|flags| flags.status()) {
            return Err(failure(code));
        }
        Ok(())
    }
}
fn resolve(root: &Path, env: &Environment, name: &str, supplied: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = supplied {
        return Ok(path.into());
    }
    let local = root.join("ops/ansible/.venv/bin").join(name);
    if name == "ansible-playbook" && local.is_file() {
        return Ok(local);
    }
    let path = env.get(OsStr::new("PATH")).ok_or_else(|| failure(127))?;
    if path.len() > 128 * 1024 {
        return Err(failure(1));
    }
    std::env::split_paths(path)
        .map(|p| root.join(p).join(name))
        .find(|p| p.is_file() && rustix::fs::access(p, rustix::fs::Access::EXEC_OK).is_ok())
        .ok_or_else(|| failure(127))
}
fn prepare(
    state: &mut State,
    root: &Path,
    options: &Options,
    env: &Environment,
    current: &Path,
    budget: &Budget,
    cp: &Checkpoint,
) -> Result<PathBuf> {
    cp.check()?;
    let ansible = resolve(
        root,
        env,
        "ansible-playbook",
        options.ansible_playbook.as_deref(),
    )?;
    let gh = resolve(root, env, "gh", options.gh.as_deref())?;
    state.tools = Some(Tools::open(root, env, &ansible, &gh, current)?);
    let (original, _) = Original::open(&options.inventory, budget, cp)?;
    state.inventory = Some(original);
    if let Some(path) = &options.application_artifact_dir {
        state.candidates.push(Candidate::open(path, true, budget)?);
    }
    if let Some(path) = &options.ops_artifact_zip {
        state.candidates.push(Candidate::open(path, false, budget)?);
    }
    state.evidence = Some(Evidence::new(root, budget)?);
    let evidence = state.evidence.as_mut().unwrap();
    let tools = state.tools.as_ref().ok_or_else(|| failure(1))?;
    let request = json!({"schemaVersion":1,"operation":"signed-routine-deploy",
        "repository":options.repository,"inventory":options.inventory,"origin":options.origin,
        "limit":options.limit,"waitSeconds":options.wait_seconds,
        "quietSeconds":options.quiet_seconds,"installHelpers":options.install_helpers,
        "applicationArtifactDirectory":options.application_artifact_dir,
        "opsArtifactZip":options.ops_artifact_zip,
        "tools":{"ansible":tools.ansible_path()?,"inventory":tools.inventory_path()?,
            "gh":tools.gh_path()?,"controller":tools.xtask_path()?}});
    evidence.write("request.json", &encode(&request)?, cp)?;
    Ok(evidence.path().into())
}
fn encode(value: &Value) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(value).map_err(|_| failure(1))?;
    bytes.push(b'\n');
    Ok(bytes)
}
async fn call<E: Executor>(
    state: &Shared,
    plan: &Plan,
    budget: &Budget,
    executor: &mut E,
    end: Instant,
    program: Program,
    args: Vec<OsString>,
) -> Result<Vec<u8>> {
    let timeout = match program {
        Program::FrontendAnsible => plan::PLAYBOOK,
        Program::FrontendCurl => Duration::from_secs(15),
        Program::FrontendGh if args.first().is_some_and(|v| v == "api") => GH_API,
        _ => plan::COMMAND,
    };
    let grace = if program == Program::FrontendAnsible {
        plan::PLAYBOOK_GRACE
    } else {
        plan::GRACE
    };
    let maximum = if program == Program::FrontendAnsible {
        64 * 1024 * 1024
    } else {
        SMALL
    };
    let worker_budget = budget.clone();
    stage(state, plan, executor, end, move |state, cp| {
        check(state, &worker_budget, cp)
    })
    .await?;
    let (index, evidence) = {
        let mut state = lock(state)?;
        let index = state.commands;
        state.commands += 1;
        (index, state.evidence.take().ok_or_else(|| failure(1))?)
    };
    let pending = evidence.capture(index, maximum).await;
    lock(state)?.evidence = Some(evidence);
    let mut pending = pending?;
    let mut command = Command::capture(program, &[]);
    command.capture = None;
    command.timeout = timeout;
    command.grace = grace;
    command.arguments = args.clone();
    command.separate_logs = pending.logs.take();
    let prepared = (|| {
        let latest = plan.dispatch(end, timeout, grace)?;
        lock(state)?
            .tools
            .as_ref()
            .ok_or_else(|| failure(1))?
            .invocation(Spec {
                program,
                arguments: args,
                timeout,
                grace,
                maximum,
                end,
                latest,
                custody: Arc::new(SourceCustody {
                    state: state.clone(),
                    budget: budget.clone(),
                    checkpoint: Checkpoint {
                        clock: plan.clock.clone(),
                        origin: plan.origin(),
                        end,
                        abort: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    },
                }),
            })
    })();
    let invocation = match prepared {
        Ok(value) => value,
        Err(error) => {
            let _ = crate::ci::process::settle_unforwarded(command).await;
            return Err(error);
        }
    };
    command.frontend = Some(invocation.clone());
    let result = executor.run(command).await;
    if program == Program::FrontendAnsible {
        lock(state)?.ansible_exit = invocation.exit().ok();
    }
    let seals = invocation.captures();
    drop(invocation);
    let read = match seals {
        Ok(seals) => {
            stage(state, plan, executor, end, move |state, cp| {
                state.evidence.as_mut().ok_or_else(|| failure(1))?.captured(
                    pending,
                    &seals,
                    program != Program::FrontendAnsible,
                    cp,
                )
            })
            .await
        }
        Err(error) => Err(error),
    };
    result.map_err(|mut error| {
        if error.is_child_exit() {
            error.message = "Routine deployment command failed.".into();
        }
        error
    })?;
    let bytes = read?;
    executor.ready().await?;
    if plan.now()? >= end {
        return Err(failure(124));
    }
    Ok(bytes)
}
fn argv(parts: &[&str]) -> Vec<OsString> {
    parts.iter().map(OsString::from).collect()
}
async fn git<E: Executor>(
    state: &Shared,
    plan: &Plan,
    budget: &Budget,
    executor: &mut E,
    end: Instant,
    args: &[&str],
) -> Result<String> {
    let bytes = call(
        state,
        plan,
        budget,
        executor,
        end,
        Program::FrontendGit,
        argv(args),
    )
    .await?;
    String::from_utf8(bytes).map_err(|_| failure(1))
}
async fn checkout<E: Executor>(
    state: &Shared,
    plan: &Plan,
    budget: &Budget,
    executor: &mut E,
    end: Instant,
) -> Result<Revision> {
    let head = git(state, plan, budget, executor, end, &["rev-parse", "HEAD"]).await?;
    let branch = git(
        state,
        plan,
        budget,
        executor,
        end,
        &["branch", "--show-current"],
    )
    .await?;
    let status = git(
        state,
        plan,
        budget,
        executor,
        end,
        &["status", "--porcelain", "--untracked-files=all"],
    )
    .await?;
    let remote = git(
        state,
        plan,
        budget,
        executor,
        end,
        &["remote", "get-url", "origin"],
    )
    .await?;
    policy::admit_revision(&head, &branch, &status, &remote)
}
async fn api<E: Executor>(
    state: &Shared,
    plan: &Plan,
    budget: &Budget,
    executor: &mut E,
    end: Instant,
    endpoint: &str,
) -> Result<Vec<u8>> {
    call(
        state,
        plan,
        budget,
        executor,
        end,
        Program::FrontendGh,
        argv(&[
            "api",
            "--hostname",
            "github.com",
            "--method",
            "GET",
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            "X-GitHub-Api-Version: 2022-11-28",
            endpoint,
        ]),
    )
    .await
}
async fn target<E: Executor>(
    state: &Shared,
    plan: &Plan,
    budget: &Budget,
    executor: &mut E,
    end: Instant,
    options: &Options,
) -> Result<Target> {
    let bytes = call(
        state,
        plan,
        budget,
        executor,
        end,
        Program::FrontendInventory,
        vec![
            "-i".into(),
            options.inventory.clone().into_os_string(),
            "--list".into(),
        ],
    )
    .await?;
    inventory::selected(&bytes, options.limit.as_deref(), &options.origin_host)
}
async fn fresh_main<E: Executor>(
    state: &Shared,
    plan: &Plan,
    budget: &Budget,
    executor: &mut E,
    end: Instant,
    revision: &Revision,
) -> Result<()> {
    policy::main(
        &api(
            state,
            plan,
            budget,
            executor,
            end,
            "repos/swiftraccoon/simplestChat/git/ref/heads/main",
        )
        .await?,
        revision,
    )
}
async fn run_status<E: Executor>(
    state: &Shared,
    plan: &Plan,
    budget: &Budget,
    executor: &mut E,
    end: Instant,
    revision: &Revision,
    run: &Run,
) -> Result<policy::RunProgress> {
    let endpoint = format!("repos/{}/actions/runs/{}", policy::REPOSITORY, run.id.get());
    policy::run(
        &api(state, plan, budget, executor, end, &endpoint).await?,
        revision,
        run,
    )
}
#[derive(Clone, Copy)]
struct SelectedRun<'a> {
    revision: &'a Revision,
    run: &'a Run,
}
async fn artifact<E: Executor>(
    state: &Shared,
    plan: &Plan,
    budget: &Budget,
    executor: &mut E,
    end: Instant,
    selected: SelectedRun<'_>,
    kind: ArtifactKind,
) -> Result<Artifact> {
    let SelectedRun { revision, run } = selected;
    let endpoint = format!(
        "repos/{}/actions/runs/{}/artifacts?name={}&per_page=100",
        policy::REPOSITORY,
        run.id.get(),
        kind.name(revision)
    );
    policy::artifact(
        &api(state, plan, budget, executor, end, &endpoint).await?,
        revision,
        run,
        kind,
    )
}
async fn pause<E: Executor>(plan: &Plan, executor: &mut E, end: Instant) -> Result<()> {
    while plan.now()? < end {
        executor.ready().await?;
        tokio::time::sleep(
            Duration::from_millis(10).min(end.saturating_duration_since(plan.now()?)),
        )
        .await;
    }
    executor.ready().await
}
async fn wait_ci<E: Executor>(
    state: &Shared,
    plan: &Plan,
    budget: &Budget,
    executor: &mut E,
    end: Instant,
    selected: SelectedRun<'_>,
    wait: u64,
) -> Result<()> {
    let SelectedRun { revision, run } = selected;
    if run_status(state, plan, budget, executor, end, revision, run).await?
        == policy::RunProgress::Success
    {
        return Ok(());
    }
    let wait_end = plan.now()? + Duration::from_secs(wait);
    loop {
        let now = plan.now()?;
        if now >= wait_end {
            return Err(failure(124));
        }
        pause(
            plan,
            executor,
            (now + Duration::from_secs(15)).min(wait_end),
        )
        .await?;
        let observation_end = end.min(wait_end);
        if run_status(
            state,
            plan,
            budget,
            executor,
            observation_end,
            revision,
            run,
        )
        .await?
            == policy::RunProgress::Success
        {
            return Ok(());
        }
    }
}
fn same_artifact(left: &Artifact, right: &Artifact) -> Result<()> {
    if left.id != right.id
        || left.zip_bytes != right.zip_bytes
        || left.zip_sha256 != right.zip_sha256
    {
        return Err(failure(1));
    }
    Ok(())
}
fn variables(
    state: &State,
    options: &Options,
    revision: &Revision,
    run: &Run,
    app: &Artifact,
    ops: &Artifact,
) -> Result<Value> {
    let tools = state.tools.as_ref().ok_or_else(|| failure(1))?;
    let mut vars = json!({"scpub_release_repository":options.repository,
        "scpub_release_artifact_id":app.id.get(),
        "scpub_release_expected_revision":revision.as_str(),
        "scpub_release_ci_run":run.id.get(),"scpub_release_deploy":true,
        "scpub_release_native_verification":true,
        "scpub_release_prepared":!options.install_helpers,
        "scpub_release_quiet_seconds":options.quiet_seconds,
        "scpub_ops_xtask":tools.xtask_path()?,"scpub_ops_gh_executable":tools.gh_path()?,
        "scpub_ops_artifact_id":ops.id.get(),"scpub_ops_selection":{
            "repository":options.repository,"revision":revision.as_str(),
            "packageVersion":env!("CARGO_PKG_VERSION"),"protocolVersion":1,
            "target":"x86_64-unknown-linux-musl","buildRunId":run.id.get(),
            "runAttempt":run.attempt.get()}});
    if let Some(path) = &options.application_artifact_dir {
        vars["scpub_release_verified_directory"] = json!(path);
    }
    if let Some(path) = &options.ops_artifact_zip {
        vars["scpub_ops_artifact_zip"] = json!(path);
    }
    Ok(vars)
}
fn public_reply(bytes: &[u8], path: &str, revision: &Revision) -> Result<()> {
    if bytes.len() > 1024 * 1024 + 4 {
        return Err(failure(1));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| failure(1))?;
    let (body, status) = text.rsplit_once('\n').ok_or_else(|| failure(1))?;
    if status != "200" {
        return Err(failure(1));
    }
    if path == "/" {
        if !body
            .chars()
            .take(256)
            .collect::<String>()
            .to_ascii_lowercase()
            .contains("<!doctype html>")
        {
            return Err(failure(1));
        }
    } else {
        #[derive(serde::Deserialize)]
        struct Status {
            status: String,
        }
        #[derive(serde::Deserialize)]
        struct Version {
            revision: String,
        }
        if path == "/app-version.json" {
            let value: Version = serde_json::from_str(body).map_err(|_| failure(1))?;
            if value.revision != revision.as_str() {
                return Err(failure(1));
            }
        } else {
            let value: Status = serde_json::from_str(body).map_err(|_| failure(1))?;
            if value.status != if path == "/health" { "ok" } else { "ready" } {
                return Err(failure(1));
            }
        }
    }
    Ok(())
}
async fn public_checks<E: Executor>(
    state: &Shared,
    plan: &Plan,
    budget: &Budget,
    executor: &mut E,
    end: Instant,
    options: &Options,
    revision: &Revision,
) -> Result<()> {
    for path in ["/health", "/ready", "/", "/app-version.json"] {
        let url = format!("{}{path}", options.origin);
        let bytes = call(
            state,
            plan,
            budget,
            executor,
            end,
            Program::FrontendCurl,
            argv(&[
                "--disable",
                "--silent",
                "--show-error",
                "--fail",
                "--noproxy",
                "*",
                "--proto",
                "=https",
                "--tlsv1.2",
                "--connect-timeout",
                "5",
                "--max-time",
                "10",
                "--max-filesize",
                "1048576",
                "--request",
                "GET",
                "--write-out",
                "\n%{http_code}",
                &url,
            ]),
        )
        .await?;
        public_reply(&bytes, path, revision)?;
    }
    Ok(())
}

async fn body<E: Executor>(
    root: &Path,
    options: &Arc<Options>,
    state: &Shared,
    plan: &Plan,
    budget: &Budget,
    executor: &mut E,
    report: &mut Value,
) -> Result<()> {
    let end = plan.begin(0)?;
    let revision = checkout(state, plan, budget, executor, end).await?;
    report["revision"] = json!(revision.as_str());
    if git(
        state,
        plan,
        budget,
        executor,
        end,
        &["check-ignore", "--", "results/deploy.evidence"],
    )
    .await?
    .trim()
        != "results/deploy.evidence"
    {
        return Err(failure(1));
    }
    let sources = git(
        state,
        plan,
        budget,
        executor,
        end,
        &["ls-files", "-z", "--", "ops/ansible", "build"],
    )
    .await?;
    let paths = files::source_paths(sources.as_bytes())?;
    let worker_root = root.to_owned();
    let worker_budget = budget.clone();
    let worker_revision = revision.clone();
    stage(state, plan, executor, end, move |state, cp| {
        let mut total = 0;
        for path in paths {
            let (source, bytes) = Original::open(&worker_root.join(path), &worker_budget, cp)?;
            total += bytes.len();
            if total > 32 * 1024 * 1024 {
                return Err(failure(1));
            }
            state.sources.push(source);
        }
        state.git_inputs =
            files::git_identity(&worker_root, worker_revision.as_str(), &worker_budget, cp)?;
        Ok(())
    })
    .await?;
    let version = call(
        state,
        plan,
        budget,
        executor,
        end,
        Program::FrontendGh,
        argv(&["--version"]),
    )
    .await?;
    let first = std::str::from_utf8(&version)
        .map_err(|_| failure(1))?
        .lines()
        .next()
        .ok_or_else(|| failure(1))?;
    if first != "gh version 2.102.0" && !first.starts_with("gh version 2.102.0 (") {
        return Err(failure(1));
    }
    let selected = target(state, plan, budget, executor, end, options).await?;
    plan.complete(0, end)?;

    report["phase"] = json!("select");
    let end = plan.begin(1)?;
    fresh_main(state, plan, budget, executor, end, &revision).await?;
    let endpoint = format!(
        "repos/{}/actions/workflows/ci.yml/runs?event=push&branch=main\
        &head_sha={}&per_page=100",
        policy::REPOSITORY,
        revision.as_str()
    );
    let run = policy::runs(
        &api(state, plan, budget, executor, end, &endpoint).await?,
        &revision,
    )?;
    let coordinates = SelectedRun {
        revision: &revision,
        run: &run,
    };
    wait_ci(
        state,
        plan,
        budget,
        executor,
        end,
        coordinates,
        options.wait_seconds,
    )
    .await?;
    let app = artifact(
        state,
        plan,
        budget,
        executor,
        end,
        coordinates,
        ArtifactKind::Application,
    )
    .await?;
    let ops = artifact(
        state,
        plan,
        budget,
        executor,
        end,
        coordinates,
        ArtifactKind::Ops,
    )
    .await?;
    if run_status(state, plan, budget, executor, end, &revision, &run).await?
        != policy::RunProgress::Success
    {
        return Err(failure(1));
    }
    report["ciRunId"] = json!(run.id.get());
    report["runAttempt"] = json!(run.attempt.get());
    report["applicationArtifactId"] = json!(app.id.get());
    report["opsArtifactId"] = json!(ops.id.get());
    plan.complete(1, end)?;

    report["phase"] = json!("recheck");
    let end = plan.begin(2)?;
    if checkout(state, plan, budget, executor, end).await? != revision {
        return Err(failure(1));
    }
    if target(state, plan, budget, executor, end, options).await? != selected {
        return Err(failure(1));
    }
    fresh_main(state, plan, budget, executor, end, &revision).await?;
    if run_status(state, plan, budget, executor, end, &revision, &run).await?
        != policy::RunProgress::Success
    {
        return Err(failure(1));
    }
    same_artifact(
        &app,
        &artifact(
            state,
            plan,
            budget,
            executor,
            end,
            coordinates,
            ArtifactKind::Application,
        )
        .await?,
    )?;
    same_artifact(
        &ops,
        &artifact(
            state,
            plan,
            budget,
            executor,
            end,
            coordinates,
            ArtifactKind::Ops,
        )
        .await?,
    )?;
    let worker_options = options.clone();
    let worker_revision = revision.clone();
    let worker_run = run.clone();
    let worker_budget = budget.clone();
    stage(state, plan, executor, end, move |state, cp| {
        check(state, &worker_budget, cp)?;
        let vars = variables(
            state,
            &worker_options,
            &worker_revision,
            &worker_run,
            &app,
            &ops,
        )?;
        let evidence = state.evidence.as_mut().ok_or_else(|| failure(1))?;
        evidence.write(
            "inventory.json",
            &encode(&json!({"benchmark_hosts":{
            "hosts":{selected.host:selected.variables}}}))?,
            cp,
        )?;
        evidence.write("vars.json", &encode(&vars)?, cp)
    })
    .await?;
    plan.complete(2, end)?;

    report["phase"] = json!("deploy");
    let end = plan.begin(3)?;
    let path = lock(state)?.evidence.as_ref().unwrap().path().to_owned();
    report["remoteOutcome"] = json!("inspect_if_interrupted");
    call(
        state,
        plan,
        budget,
        executor,
        end,
        Program::FrontendAnsible,
        vec![
            "-i".into(),
            path.join("inventory.json").into_os_string(),
            root.join("ops/ansible/release.yml").into_os_string(),
            "--extra-vars".into(),
            format!("@{}", path.join("vars.json").display()).into(),
        ],
    )
    .await?;
    report["deployed"] = json!(true);
    report["remoteOutcome"] = json!("release_succeeded");
    plan.complete(3, end)?;

    report["phase"] = json!("anonymous_verification");
    let end = plan.begin(4)?;
    if run_status(state, plan, budget, executor, end, &revision, &run).await?
        != policy::RunProgress::Success
    {
        return Err(failure(1));
    }
    public_checks(state, plan, budget, executor, end, options, &revision).await?;
    plan.complete(4, end)?;
    report["phase"] = json!("complete");
    Ok(())
}

async fn transaction_owned<E: Executor>(
    root: PathBuf,
    options: Arc<Options>,
    env: Environment,
    current: PathBuf,
    signals: Option<Arc<simplestchat_ops::owned_process::CompletionSignals>>,
    plan: &Plan,
    executor: &mut E,
) -> Result<super::lifecycle::Completed> {
    let budget = Budget::default();
    let state = Arc::new(Mutex::new(State::default()));
    lock(&state)?.signals = signals;
    let mut report = json!({"schemaVersion":1,"operation":"signed-routine-deploy",
        "candidatePassed":false,"deployed":false,"remoteOutcome":"not_started",
        "phase":"prepare","failureClass":null,"repository":options.repository,
        "origin":options.origin,
        "startedAt":chrono::Utc::now().to_rfc3339()});
    let end = plan.begin(0)?;
    let worker_root = root.clone();
    let worker_options = options.clone();
    let worker_env = env.clone();
    let worker_budget = budget.clone();
    let mut primary = match stage(&state, plan, executor, end, move |state, cp| {
        prepare(
            state,
            &worker_root,
            &worker_options,
            &worker_env,
            &current,
            &worker_budget,
            cp,
        )
    })
    .await
    {
        Ok(path) => {
            report["evidence"] = json!(path);
            body(
                &root,
                &options,
                &state,
                plan,
                &budget,
                executor,
                &mut report,
            )
            .await
            .err()
        }
        Err(error) => Some(error),
    };
    if let Err(error) = executor.ready().await {
        keep(&mut primary, error);
    }
    if !executor.cleanup_passed() {
        keep(&mut primary, failure(1));
    }
    if lock(&state)?
        .ansible_exit
        .as_ref()
        .is_some_and(|exit| exit.status == Some(0) && exit.settled && exit.body_completed)
    {
        report["deployed"] = json!(true);
        report["remoteOutcome"] = json!("release_succeeded");
    }
    report["candidatePassed"] = json!(primary.is_none());
    if let Some(error) = &primary {
        report["failureClass"] = json!(error.message);
    }
    if let Some(exit) = &lock(&state)?.ansible_exit {
        report["ansibleExit"] = json!({"status":exit.status,"settled":exit.settled,
            "bodyCompleted":exit.body_completed});
    }
    let closed_state = state.clone();
    let mut closed_report = report.clone();
    let closed_budget = budget.clone();
    let final_end = (plan.clock.now() + plan::FINAL).min(plan.end());
    let final_cp = Checkpoint {
        clock: plan.clock.clone(),
        origin: plan.origin(),
        end: final_end,
        abort: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    let closed = tokio::task::spawn_blocking(move || {
        let mut state = closed_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut failed = None;
        if state.evidence.is_some()
            && let Err(error) = check(&state, &closed_budget, &final_cp)
        {
            keep(&mut failed, error);
        }
        if let Some(tools) = state.tools.take()
            && let Err(error) = tools.finish()
        {
            keep(&mut failed, error);
        }
        if let Err(error) = final_cp.check() {
            keep(&mut failed, error);
        }
        if let Some(error) = &failed {
            closed_report["candidatePassed"] = json!(false);
            if closed_report["failureClass"].is_null() {
                closed_report["failureClass"] = json!(error.message);
            }
        }
        closed_report["finishedAt"] = json!(chrono::Utc::now().to_rfc3339());
        if let Some(mut evidence) = state.evidence.take() {
            match encode(&closed_report) {
                Ok(bytes) => {
                    if let Err(error) = evidence.write("outcome.json", &bytes, &final_cp) {
                        keep(&mut failed, error);
                    }
                }
                Err(error) => keep(&mut failed, error),
            }
            if let Err(error) = evidence.check(&final_cp) {
                keep(&mut failed, error);
            }
            if let Err(error) = evidence.finish() {
                keep(&mut failed, error);
            }
        }
        if let Err(error) = final_cp.check() {
            keep(&mut failed, error);
        }
        (closed_report, failed)
    })
    .await;
    let (mut finished, cleanup_error) = closed.unwrap_or_else(|_| (report, Some(failure(1))));
    if let Some(error) = cleanup_error {
        keep(&mut primary, error);
    }
    if let Err(error) = executor.ready().await {
        keep(&mut primary, error);
    }
    if let Err(error) = plan.now() {
        keep(&mut primary, error);
    }
    if plan.clock.now() >= final_end {
        keep(&mut primary, failure(124));
    }
    let status = primary.as_ref().map_or(0, |error| error.code);
    if let Some(error) = primary {
        finished["candidatePassed"] = json!(false);
        if finished["failureClass"].is_null() {
            finished["failureClass"] = json!(error.message);
        }
    }
    Ok(super::lifecycle::Completed {
        summary: finished,
        end: final_end,
        status,
    })
}

/// A published observation precedes runtime teardown and checked unregister.
/// Its successful candidate still requires the parent's actual exit zero.
pub(super) async fn publish<E: Executor>(
    completed: &mut super::lifecycle::Completed,
    executor: &mut E,
) {
    let mut primary = (completed.status != 0).then(|| failure(completed.status));
    let mut admitted = true;
    if let Err(error) = executor.ready().await {
        keep(&mut primary, error);
        admitted = false;
    }
    if !executor.cleanup_passed() {
        keep(&mut primary, failure(1));
        admitted = false;
    }
    if Instant::now() >= completed.end {
        keep(&mut primary, failure(124));
        admitted = false;
    }
    completed.summary["passed"] = json!(primary.is_none());
    if admitted {
        match encode(&completed.summary) {
            Ok(bytes) => {
                if let Err(error) = executor.replay(&bytes, false, completed.end).await {
                    keep(&mut primary, error);
                }
            }
            Err(error) => keep(&mut primary, error),
        }
    }
    if let Err(error) = executor.ready().await {
        keep(&mut primary, error);
    }
    if !executor.cleanup_passed() {
        keep(&mut primary, failure(1));
    }
    if Instant::now() >= completed.end {
        keep(&mut primary, failure(124));
    }
    completed.status = primary.as_ref().map_or(0, |error| error.code);
    completed.summary["passed"] = json!(primary.is_none());
    if let Some(error) = primary
        && completed.summary["failureClass"].is_null()
    {
        completed.summary["failureClass"] = json!(error.message);
    }
}

pub(super) fn replay_environment() -> Environment {
    [
        ("PATH", "/usr/bin:/bin:/usr/sbin:/sbin"),
        ("LC_ALL", "C"),
        ("LANG", "C"),
        ("TERM", "dumb"),
        ("NO_COLOR", "1"),
    ]
    .into_iter()
    .map(|(key, value)| (key.into(), value.into()))
    .collect()
}

pub(super) async fn native(
    root: PathBuf,
    options: Arc<Options>,
    environment: Environment,
    current: PathBuf,
    flags: Arc<simplestchat_ops::owned_process::CompletionSignals>,
    plan: &Plan,
) -> Result<super::lifecycle::Completed> {
    let replay_environment = replay_environment();
    let mut executor = Native::new(
        &root,
        &replay_environment,
        Signals::from_completion(flags.clone()),
    );
    let mut completed = transaction_owned(
        root.clone(),
        options,
        environment,
        current,
        Some(flags),
        plan,
        &mut executor,
    )
    .await?;
    publish(&mut completed, &mut executor).await;
    Ok(completed)
}

#[cfg(test)]
async fn transaction<E: Executor>(
    root: PathBuf,
    options: Arc<Options>,
    environment: Environment,
    current: PathBuf,
    plan: &Plan,
    executor: &mut E,
) -> Result<super::lifecycle::Completed> {
    transaction_owned(root, options, environment, current, None, plan, executor).await
}

#[cfg(test)]
#[path = "workflow_tests.rs"]
mod tests;
