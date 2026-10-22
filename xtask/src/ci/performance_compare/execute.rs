//! Fixed command order and independent retirement of the witnessed worktree.

use super::{
    Clock, Deadline,
    files::{self, Invocation, Role, Scope, Tool},
    plan::{self, Plan},
    worktree::{self, Registration},
};
use crate::{
    ci::{
        bounded_log::Log,
        command::{Command, Program},
        process::Executor,
    },
    tooling::{Environment, Result},
};
use serde::Deserialize;
use std::{sync::Arc, time::Duration};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Receipt {
    schema_version: u8,
    run_id: String,
    completed: bool,
    passed: bool,
    runs: u8,
    group_cleanup_passed: bool,
    streams_closed: bool,
}

pub(super) struct Context<'a, E, C: Clock> {
    executor: &'a mut E,
    deadline: Deadline<'a, C>,
    private: &'a Arc<Scope>,
    metadata: &'a mut usize,
    minimum: Duration,
}
impl<E: Executor, C: Clock> Context<'_, E, C> {
    async fn call(
        &mut self,
        invocation: Arc<Invocation>,
        role: Role,
        args: Vec<String>,
        cleanup: bool,
        log: Option<&str>,
        maximum: Option<Duration>,
    ) -> Result<Vec<u8>> {
        let grace = if role == Role::Benchmark {
            plan::BENCH_GRACE
        } else {
            plan::CLI_GRACE
        };
        let timeout = self.deadline.body(cleanup, grace, maximum)?;
        if role == Role::Benchmark && timeout < self.minimum {
            return Err(plan::failure(124));
        }
        invocation.validate(role)?;
        self.private.check()?;
        if !cleanup {
            self.executor.ready().await?;
        }
        let program = match role {
            Role::Git => Program::ComparisonGit,
            Role::Cargo => Program::ComparisonCargo,
            Role::Benchmark => Program::ComparisonBenchmark,
        };
        let mut command = Command::capture(program, &[]);
        command.arguments = args.into_iter().map(Into::into).collect();
        command.timeout = timeout;
        command.grace = grace;
        command.cleanup = cleanup;
        command.capture = None;
        command.comparison = Some(invocation.clone());
        let metadata = log.is_none();
        let name = log.map(str::to_owned).unwrap_or_else(|| {
            *self.metadata += 1;
            format!("metadata-{}.log", self.metadata)
        });
        let limit = if metadata {
            4 * 1024 * 1024
        } else {
            plan::LOG_LIMIT
        };
        command.combined_log = Some(Arc::new(Log::start(self.private.create(&name)?, limit)?));
        let result = self.executor.run(command).await;
        let logged = if metadata {
            self.private.read(&name, limit)
        } else {
            Ok(Vec::new())
        };
        let checked = invocation
            .validate(role)
            .map(|_| ())
            .and(self.private.check())
            .and(self.deadline.check(cleanup));
        result?;
        checked?;
        if !cleanup {
            self.executor.ready().await?;
        }
        self.deadline.check(cleanup)?;
        if !cleanup && !self.executor.cleanup_passed() {
            return Err(plan::failure(1));
        }
        logged
    }
    async fn git(
        &mut self,
        tool: &Arc<Tool>,
        root: &Arc<Scope>,
        environment: &Environment,
        args: Vec<String>,
        cleanup: bool,
    ) -> Result<Vec<u8>> {
        self.call(
            Invocation::new(tool.clone(), root.clone(), environment.clone()),
            Role::Git,
            args,
            cleanup,
            None,
            Some(Duration::from_secs(30)),
        )
        .await
    }
    async fn registration(
        &mut self,
        plan: &Plan,
        tool: &Arc<Tool>,
        root: &Arc<Scope>,
        revision: &str,
        cleanup: bool,
    ) -> Result<Registration> {
        let bytes = self
            .git(
                tool,
                root,
                &plan.environment,
                ["worktree", "list", "--porcelain", "-z"]
                    .map(str::to_owned)
                    .into(),
                cleanup,
            )
            .await?;
        worktree::listing(&bytes, &plan.baseline, revision)
    }
}

fn cargo_arguments(generator: bool) -> Vec<String> {
    if generator {
        vec![
            "build",
            "--locked",
            "--release",
            "-p",
            "simplestchat-load-test",
            "--bin",
            "load_test",
        ]
    } else {
        vec!["build", "--locked", "--release", "--bin", "simplestChat"]
    }
    .into_iter()
    .map(str::to_owned)
    .collect()
}

fn receipt(scope: &Scope, run_id: &str) -> Result<bool> {
    let bytes = scope.read("comparison-owner.json", 16 * 1024)?;
    let value: Receipt = serde_json::from_slice(&bytes).map_err(|_| plan::failure(1))?;
    if value.schema_version != 1 || value.run_id != run_id || value.runs > 6 {
        return Err(plan::failure(1));
    }
    Ok(value.completed
        && value.passed
        && value.runs == 6
        && value.group_cleanup_passed
        && value.streams_closed)
}

/// Every acquired tool/directory is closed after the command and worktree
/// finalizers. The first known command status survives cleanup uncertainty.
pub(super) async fn execute(
    plan: &Plan,
    executor: &mut impl Executor,
    clock: &impl Clock,
) -> Result<()> {
    execute_with(plan, executor, clock, |_| {}).await
}
pub(super) async fn execute_with(
    plan: &Plan,
    executor: &mut impl Executor,
    clock: &impl Clock,
    acquired: impl Fn(&Arc<Scope>),
) -> Result<()> {
    executor.ready().await?;
    let root = Scope::existing(&plan.root)?;
    let temporary = match Scope::existing(&plan.temporary) {
        Ok(v) => v,
        Err(e) => {
            let _ = files::finish(root, false);
            return Err(e);
        }
    };
    let run_id = match files::nonce() {
        Ok(v) => v,
        Err(e) => {
            let _ = files::finish(root, false);
            let _ = files::finish(temporary, false);
            return Err(e);
        }
    };
    let private = match Scope::fresh(&temporary, &format!("performance-compare.{run_id}")) {
        Ok(v) => v,
        Err(e) => {
            let _ = files::finish(root, false);
            let _ = files::finish(temporary, false);
            return Err(e);
        }
    };
    let mut baseline = None;
    let mut git = None;
    let mut cargo = None;
    let mut baseline_cargo = None;
    let mut benchmark = None;
    let mut revision = None;
    let mut baseline_digest = None;
    let mut attempted = false;
    let mut metadata = 0;
    let mut inner_completion = None;
    let result: Result<()> = async {
        if !temporary.absent("baseline")? || !temporary.absent("comparison")? {
            return Err(plan::failure(1));
        }
        git = Some(Tool::select(plan, Role::Git)?);
        cargo = Some(Tool::select(plan, Role::Cargo)?);
        let mut context = Context {
            executor,
            deadline: Deadline { clock },
            private: &private,
            metadata: &mut metadata,
            minimum: plan.profile.minimum(),
        };
        let selected = worktree::revision(
            &context
                .git(
                    git.as_ref().unwrap(),
                    &root,
                    &plan.environment,
                    vec![
                        "rev-parse".into(),
                        "--verify".into(),
                        "--end-of-options".into(),
                        format!("{}^{{commit}}", plan.reference),
                    ],
                    false,
                )
                .await?,
        )?;
        revision = Some(selected.clone());
        private.json(
            "ownership.json",
            &serde_json::json!({"schemaVersion":1,"runId":run_id,
            "baseline":plan.baseline,"revision":selected,"state":"creation-planned"}),
        )?;
        baseline = Some(Scope::fresh(&temporary, "baseline")?);
        acquired(baseline.as_ref().expect("owned baseline"));
        attempted = true;
        context
            .git(
                git.as_ref().unwrap(),
                &root,
                &plan.environment,
                vec![
                    "worktree".into(),
                    "add".into(),
                    "--detach".into(),
                    "--".into(),
                    plan.baseline.to_string_lossy().into_owned(),
                    selected.clone(),
                ],
                false,
            )
            .await?;
        if context
            .registration(plan, git.as_ref().unwrap(), &root, &selected, false)
            .await?
            != Registration::Owned
        {
            return Err(plan::failure(1));
        }
        let baseline = baseline.as_ref().unwrap();
        worktree::vendor_times(baseline)?;
        baseline_cargo = Some(Tool::select_at(plan, Role::Cargo, &baseline.path)?);
        let mut environment = plan.environment.clone();
        environment.insert(
            "CARGO_TARGET_DIR".into(),
            plan.target.clone().into_os_string(),
        );
        context
            .call(
                Invocation::new(
                    baseline_cargo.as_ref().unwrap().clone(),
                    baseline.clone(),
                    environment,
                ),
                Role::Cargo,
                cargo_arguments(false),
                false,
                Some("baseline-build.log"),
                None,
            )
            .await?;
        baseline_digest = Some(files::copy_baseline(plan, baseline)?);
        for (generator, log) in [
            (false, "candidate-build.log"),
            (true, "generator-build.log"),
        ] {
            context
                .call(
                    Invocation::new(
                        cargo.as_ref().unwrap().clone(),
                        root.clone(),
                        plan.environment.clone(),
                    ),
                    Role::Cargo,
                    cargo_arguments(generator),
                    false,
                    Some(log),
                    None,
                )
                .await?;
        }
        benchmark = Some(Tool::benchmark(plan)?);
        let mut environment = plan.environment.clone();
        environment.insert("BENCH_CI_RUN_ID".into(), run_id.clone().into());
        let compared = context
            .call(
                Invocation::new(
                    benchmark.as_ref().unwrap().clone(),
                    root.clone(),
                    environment,
                ),
                Role::Benchmark,
                plan.comparison_arguments(),
                false,
                Some("comparison.log"),
                None,
            )
            .await;
        let observed = Scope::existing(&plan.comparison).and_then(|comparison| {
            let checked = receipt(&comparison, &run_id);
            let closed = files::finish(comparison, false);
            let value = checked?;
            closed?;
            Ok(value)
        });
        inner_completion = observed.as_ref().ok().copied();
        compared?;
        if !observed? {
            return Err(plan::failure(1));
        }
        Ok(())
    }
    .await;
    let cleanup: Result<()> = async {
        if !attempted {
            return Ok(());
        }
        let git = git.as_ref().ok_or_else(|| plan::failure(1))?;
        let selected = revision.as_ref().ok_or_else(|| plan::failure(1))?;
        let baseline = baseline.as_ref().ok_or_else(|| plan::failure(1))?;
        let mut context = Context {
            executor,
            deadline: Deadline { clock },
            private: &private,
            metadata: &mut metadata,
            minimum: plan.profile.minimum(),
        };
        let before = context
            .registration(plan, git, &root, selected, true)
            .await?;
        let removed = if before == Registration::Owned {
            baseline.check()?;
            context
                .git(
                    git,
                    &root,
                    &plan.environment,
                    vec![
                        "worktree".into(),
                        "remove".into(),
                        "--force".into(),
                        "--".into(),
                        plan.baseline.to_string_lossy().into_owned(),
                    ],
                    true,
                )
                .await
                .map(|_| ())
        } else {
            Ok(())
        };
        let absent = context.registration(plan, git, &root, selected, true).await;
        let unlinked = if matches!(absent, Ok(Registration::Absent)) {
            baseline.remove_leaf()
        } else {
            Err(plan::failure(1))
        };
        removed?;
        absent?;
        unlinked
    }
    .await;
    let mut closed = true;
    for tool in [
        benchmark.take(),
        baseline_cargo.take(),
        cargo.take(),
        git.take(),
    ]
    .into_iter()
    .flatten()
    {
        closed &= files::close_tool(tool).is_ok();
    }
    if let Some(scope) = baseline.take() {
        closed &= files::finish(scope, cleanup.is_ok()).is_ok();
    }
    let recorded = private.json(
        "result.json",
        &serde_json::json!({"schemaVersion":1,
        "runId":run_id,"completed":result.is_ok(),"prepared":result.is_ok()&&cleanup.is_ok()
            &&closed&&executor.cleanup_passed(),"worktreeCleanupPassed":cleanup.is_ok(),
            "cliCleanupPassed":executor.cleanup_passed(),"baselineBinarySha256":baseline_digest,
            "innerCompletion":inner_completion,
        "claims":"local observations before publisher close; no authenticated build or \
            separate-session closure"}),
    );
    closed &= files::finish(private, false).is_ok();
    closed &= files::finish(temporary, false).is_ok();
    closed &= files::finish(root, false).is_ok();
    result?;
    cleanup?;
    recorded?;
    if !closed || !executor.cleanup_passed() {
        return Err(plan::failure(1));
    }
    Deadline { clock }.check(true)?;
    executor.ready().await?;
    Deadline { clock }.check(true)
}
