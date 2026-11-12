//! The move itself: prove the tools and both hosts, select one signed
//! release, freeze both targets in private inventories, then apply the
//! maintained playbooks in order, with a durable checkpoint before every
//! host operation and recovery only while the destination never started.

use super::super::super::namespace::Budget;
use super::super::{
    execute::{
        self, Selector, Shared, Signed, State, api, argv, call, call_within, check, encode, git,
        lock, selection, stage,
    },
    failure,
    files::{Candidate, Evidence, Original},
    inventory::Target,
    keep,
    lifecycle::Completed,
    plan::{self, Plan},
    policy::{self, Revision},
    tools::Tools,
    worker::Checkpoint,
};
use super::{
    activation,
    checks::{self, Services, Transport, refusal},
    options::Options,
};
use serde_json::{Map, Value, json};
use simplestchat_ci::{
    command::Program,
    process::{Executor, Native},
};
use simplestchat_toolkit::{
    owned_process::Signals,
    tooling::{Environment, Failure, Result},
};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// The ceiling of a preparation or service playbook.
const PREPARE: Duration = Duration::from_secs(2400);
/// The ceiling of a playbook that moves the data or stages the release.
const DATA: Duration = Duration::from_secs(5400);
/// One window for all anonymous requests while certificates are obtained.
const WINDOW: Duration = Duration::from_secs(90);
/// The paths the destination must answer, the served revision last.
const PATHS: [&str; 5] = [
    "/health",
    "/ready",
    "/",
    "/api/capabilities",
    "/app-version.json",
];
const INSPECTION: usize = 64 * 1024;

/// Every bounded step of a move, in order. A step a move does not need is
/// passed over; the last two run only to recover from a failed transfer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Preflight,
    Select,
    Recheck,
    Verify,
    Inspect,
    Site,
    Bootstrap,
    Stage,
    Public,
    Transfer,
    Launch,
    Https,
    RelayPrepare,
    RelayActivate,
    Backup,
    Monitoring,
    Finalize,
    Cutover,
    PublicHttps,
    Activate,
    Canary,
    StopTarget,
    ResumeSource,
}

impl Step {
    const ALL: [Self; 23] = [
        Self::Preflight,
        Self::Select,
        Self::Recheck,
        Self::Verify,
        Self::Inspect,
        Self::Site,
        Self::Bootstrap,
        Self::Stage,
        Self::Public,
        Self::Transfer,
        Self::Launch,
        Self::Https,
        Self::RelayPrepare,
        Self::RelayActivate,
        Self::Backup,
        Self::Monitoring,
        Self::Finalize,
        Self::Cutover,
        Self::PublicHttps,
        Self::Activate,
        Self::Canary,
        Self::StopTarget,
        Self::ResumeSource,
    ];

    fn index(self) -> usize {
        self as usize
    }

    /// The playbook a step applies and the action it selects in it.
    fn play(self) -> Option<(&'static str, Option<&'static str>)> {
        Some(match self {
            Self::Verify => ("migration-prepare.yml", Some("verify")),
            Self::Inspect => ("migration-prepare.yml", Some("inspect")),
            Self::Site => ("site.yml", None),
            Self::Bootstrap => ("migration-prepare.yml", Some("bootstrap")),
            Self::Stage => ("migration-prepare.yml", Some("stage")),
            Self::Public => ("public.yml", None),
            Self::Transfer => ("migration-data.yml", Some("transfer")),
            Self::Launch => ("migration-data.yml", Some("launch")),
            Self::RelayPrepare | Self::RelayActivate => ("turn.yml", None),
            Self::Backup => ("backup.yml", None),
            Self::Monitoring => ("monitoring.yml", None),
            Self::Finalize => ("migration-data.yml", Some("finalize-source")),
            Self::StopTarget => ("migration-data.yml", Some("stop-target")),
            Self::ResumeSource => ("migration-data.yml", Some("resume-source")),
            _ => return None,
        })
    }

    /// How long the step's one playbook run may take.
    fn ceiling(self) -> Duration {
        match self.play() {
            Some(("migration-data.yml", _)) => DATA,
            _ if self == Self::Stage => DATA,
            _ => PREPARE,
        }
    }

    /// The step's share of the whole operation's envelope.
    fn share(self, options: &Options) -> Duration {
        let seconds = match self {
            Self::Preflight | Self::Recheck => 540,
            Self::Select => 360 + options.wait_seconds,
            Self::Https | Self::PublicHttps | Self::Canary => 150,
            Self::Cutover => options.dns_wait_seconds.max(10) + 60,
            Self::Activate => 60,
            _ => {
                let settled = plan::PLAYBOOK_GRACE + plan::CAPTURE + Duration::from_secs(30);
                return self.ceiling() + settled;
            }
        };
        Duration::from_secs(seconds)
    }
}

/// The ordered phases of one move's envelope.
pub(in super::super) fn phases(options: &Options) -> Vec<Duration> {
    Step::ALL.iter().map(|step| step.share(options)).collect()
}

/// Make `step` the next phase and begin it.
fn enter(plan: &Plan, step: Step) -> Result<Instant> {
    plan.advance(step.index())?;
    plan.begin(step.index())
}

#[derive(Clone, Copy)]
struct Context<'a> {
    root: &'a Path,
    options: &'a Options,
    state: &'a Shared,
    plan: &'a Plan,
    budget: &'a Budget,
}

/// What a move has done so far: its report, the count of its checkpoints,
/// the two boundaries that decide recovery, and the frozen selection.
struct Run {
    report: Value,
    sequence: usize,
    freeze_attempted: bool,
    startup_attempted: bool,
    frozen: Option<Frozen>,
    /// The source inventory as it was selected from, kept to prove it is
    /// unchanged if this move is asked to replace it.
    original: Option<Original>,
}

/// The private evidence directory and the variables every play receives.
#[derive(Clone)]
struct Frozen {
    evidence: PathBuf,
    variables: Map<String, Value>,
}

fn utf8(path: &Path) -> Result<&str> {
    path.to_str().ok_or_else(|| failure(1))
}

/// Open the tools and both inventories and create the private evidence.
fn prepare(
    state: &mut State,
    root: &Path,
    options: &Options,
    environment: &Environment,
    current: &Path,
    budget: &Budget,
    cp: &Checkpoint,
) -> Result<PathBuf> {
    cp.check()?;
    let supplied = options.ansible_playbook.as_deref();
    let ansible = execute::resolve(root, environment, "ansible-playbook", supplied)?;
    let gh = execute::resolve(root, environment, "gh", options.gh.as_deref())?;
    let dns = options.keeps_hostname();
    if dns && execute::resolve(root, environment, "dig", None).is_err() {
        return Err(refusal("migration_dns_tool_missing"));
    }
    state.tools = Some(Tools::open_with(
        root,
        environment,
        &ansible,
        &gh,
        current,
        dns,
    )?);
    let (target, _) = Original::open(&options.target.inventory, budget, cp)?;
    state.inventory = Some(target);
    let (source, original) = Original::open(&options.source.inventory, budget, cp)?;
    state.companion = Some(source);
    if let Some(path) = &options.application_artifact_dir {
        state.candidates.push(Candidate::open(path, true, budget)?);
    }
    if let Some(path) = &options.ops_artifact_zip {
        state.candidates.push(Candidate::open(path, false, budget)?);
    }
    if let Some(selected) = &options.activate_inventory {
        activation::admit(root, &options.source.inventory, selected)?;
    }
    state.evidence = Some(Evidence::named(root, budget, "migration")?);
    let evidence = state.evidence.as_mut().ok_or_else(|| failure(1))?;
    let tools = state.tools.as_ref().ok_or_else(|| failure(1))?;
    let request = json!({"schemaVersion": 1, "operation": "migrate",
        "repository": options.repository,
        "sourceInventory": options.source.inventory, "sourceOrigin": options.source.origin,
        "sourceLimit": options.source.limit,
        "inventory": options.target.inventory, "origin": options.target.origin,
        "limit": options.target.limit,
        "waitSeconds": options.wait_seconds, "dnsWaitSeconds": options.dns_wait_seconds,
        "revision": options.revision, "activateInventory": options.activate_inventory,
        "updateCanary": options.update_canary,
        "applicationArtifactDirectory": options.application_artifact_dir,
        "opsArtifactZip": options.ops_artifact_zip,
        "tools": {"ansible": tools.ansible_path()?, "inventory": tools.inventory_path()?,
            "gh": tools.gh_path()?, "controller": tools.controller_path()?}});
    evidence.write("request.json", &encode(&request)?, cp)?;
    if options.activate_inventory.is_some() {
        evidence.write("original-inventory", &original, cp)?;
    }
    Ok(evidence.path().into())
}

/// Retain one private record of this move.
async fn retain<E: Executor>(
    cx: &Context<'_>,
    executor: &mut E,
    end: Instant,
    name: String,
    bytes: Vec<u8>,
) -> Result<()> {
    stage(cx.state, cx.plan, executor, end, move |state, cp| {
        let evidence = state.evidence.as_mut().ok_or_else(|| failure(1))?;
        evidence.write(&name, &bytes, cp)
    })
    .await
}

/// Read what a play left in the private evidence: one ordinary small file.
async fn left<E: Executor>(
    cx: &Context<'_>,
    executor: &mut E,
    end: Instant,
    path: PathBuf,
) -> Result<Vec<u8>> {
    let budget = cx.budget.clone();
    stage(cx.state, cx.plan, executor, end, move |_, cp| {
        Original::open(&path, &budget, cp).map(|(_, bytes)| bytes)
    })
    .await
}

/// Record the intent of the next operation before it starts, so that even an
/// ambiguous failure leaves the phase it happened in.
async fn checkpoint<E: Executor>(
    cx: &Context<'_>,
    executor: &mut E,
    run: &mut Run,
    end: Instant,
    phase: &str,
) -> Result<()> {
    run.sequence += 1;
    run.report["phase"] = json!(phase);
    run.report["sourceFreezeAttempted"] = json!(run.freeze_attempted);
    run.report["destinationStartupAttempted"] = json!(run.startup_attempted);
    let name = format!("phase-{:02}.json", run.sequence);
    retain(cx, executor, end, name, encode(&run.report)?).await
}

/// Apply one step's playbook once: to both frozen hosts for the migration
/// playbooks, to the destination alone for every other.
async fn play<E: Executor>(
    cx: &Context<'_>,
    executor: &mut E,
    run: &mut Run,
    step: Step,
    extra: &[(&str, Value)],
) -> Result<()> {
    let (playbook, action) = step.play().ok_or_else(|| failure(1))?;
    let end = enter(cx.plan, step)?;
    let Frozen {
        evidence,
        mut variables,
    } = run.frozen.clone().ok_or_else(|| failure(1))?;
    variables.extend(
        extra
            .iter()
            .map(|(name, value)| ((*name).to_owned(), value.clone())),
    );
    if let Some(action) = action {
        variables.insert("scmig_action".into(), json!(action));
    }
    let phase = action.unwrap_or_else(|| playbook.trim_end_matches(".yml"));
    checkpoint(cx, executor, run, end, phase).await?;
    let name = format!("play-{:02}.json", run.sequence);
    let bytes = encode(&Value::Object(variables))?;
    retain(cx, executor, end, name.clone(), bytes).await?;
    let (inventory, hosts) = if playbook.starts_with("migration-") {
        (
            "migration-inventory.json",
            "migration_source:migration_target",
        )
    } else {
        ("target-inventory.json", "migration_target")
    };
    let arguments = vec![
        "-i".into(),
        evidence.join(inventory).into_os_string(),
        cx.root.join("ops/ansible").join(playbook).into_os_string(),
        "--limit".into(),
        hosts.into(),
        "--extra-vars".into(),
        format!("@{}", utf8(&evidence.join(name))?).into(),
    ];
    let Context {
        state,
        plan,
        budget,
        ..
    } = *cx;
    let program = Program::FrontendAnsible;
    let ceiling = Some(step.ceiling());
    call_within(
        state, plan, budget, executor, end, program, arguments, ceiling,
    )
    .await?;
    plan.complete(step.index(), end)
}

/// Whether git ignores `path`; anything else is the named refusal.
async fn ignored<E: Executor>(
    cx: &Context<'_>,
    executor: &mut E,
    end: Instant,
    path: &str,
    refused: &'static str,
) -> Result<()> {
    let arguments = ["check-ignore", "--", path];
    match git(cx.state, cx.plan, cx.budget, executor, end, &arguments).await {
        Ok(listed) if listed.trim_end_matches('\n') == path => Ok(()),
        Ok(_) => Err(refusal(refused)),
        Err(error) if error.is_child_exit() => Err(refusal(refused)),
        Err(error) => Err(error),
    }
}

/// Require the repository's canary variable to name `origin` now.
async fn canary<E: Executor>(
    cx: &Context<'_>,
    executor: &mut E,
    end: Instant,
    origin: &str,
) -> Result<()> {
    let endpoint = format!(
        "repos/{}/actions/variables/CANARY_ORIGIN",
        policy::REPOSITORY
    );
    let record = api(cx.state, cx.plan, cx.budget, executor, end, &endpoint).await?;
    checks::canary(&record, origin)
}

/// Ask the destination anonymously for its health, readiness, home page,
/// capabilities and served revision, retrying inside one shared window
/// while its certificate is issued. `pinned` reaches it by address while
/// its name still resolves to the source.
async fn https<E: Executor>(
    cx: &Context<'_>,
    executor: &mut E,
    end: Instant,
    revision: &str,
    pinned: Option<&Transport>,
) -> Result<()> {
    let Context {
        options,
        state,
        plan,
        budget,
        ..
    } = *cx;
    let deadline = (plan.now()? + WINDOW).min(end);
    let resolution = pinned.map(|transport| transport.pin(&options.target.origin_host));
    for path in PATHS {
        let remaining = deadline.saturating_duration_since(plan.now()?).as_secs();
        if remaining == 0 {
            return Err(refusal("migration_https_timeout"));
        }
        let request = remaining.min(15).to_string();
        let retries = (remaining - remaining.min(15)).max(1).to_string();
        let largest = checks::MAX_REPLY.to_string();
        let mut arguments = argv(&[
            "--disable",
            "--silent",
            "--show-error",
            "--fail",
            "--proto",
            "=https",
            "--tlsv1.2",
            "--noproxy",
            "*",
            "--connect-timeout",
            "5",
            "--max-time",
            &request,
            "--retry",
            "30",
            "--retry-delay",
            "1",
            "--retry-max-time",
            &retries,
            "--retry-all-errors",
            "--max-filesize",
            &largest,
            "--request",
            "GET",
            "--header",
            "Accept-Encoding: identity",
            "--write-out",
            "\n%{http_code}",
        ]);
        if let Some(resolution) = &resolution {
            arguments.extend(argv(&["--resolve", resolution]));
        }
        arguments.push(format!("{}{path}", options.target.origin).into());
        let program = Program::FrontendCurl;
        let ceiling = Some(Duration::from_secs(remaining));
        let bytes = call_within(
            state, plan, budget, executor, end, program, arguments, ceiling,
        )
        .await?;
        checks::reply(&bytes, path, revision)?;
    }
    Ok(())
}

/// The addresses the public name resolves to now, both families.
async fn resolved<E: Executor>(
    cx: &Context<'_>,
    executor: &mut E,
    end: Instant,
    deadline: Instant,
) -> Result<BTreeSet<String>> {
    let Context {
        options,
        state,
        plan,
        budget,
        ..
    } = *cx;
    let mut found = BTreeSet::new();
    for (family, wide) in [("A", false), ("AAAA", true)] {
        let remaining = deadline.saturating_duration_since(plan.now()?).as_secs();
        let ceiling = Some(Duration::from_secs(remaining.clamp(1, 5)));
        let query = [
            "+short",
            "+timeout=2",
            "+tries=1",
            &options.target.origin_host,
            family,
        ];
        let program = Program::FrontendDig;
        let arguments = argv(&query);
        let output = call_within(
            state, plan, budget, executor, end, program, arguments, ceiling,
        )
        .await?;
        found.extend(checks::answers(&output, wide)?);
    }
    Ok(found)
}

/// Tell the operator the destination is ready for DNS, then wait until the
/// public name resolves to exactly the destination's addresses. DNS is never
/// changed here, and a name that does not follow leaves the source sealed.
async fn cutover<E: Executor>(
    cx: &Context<'_>,
    executor: &mut E,
    end: Instant,
    transport: &Transport,
) -> Result<()> {
    let Context { options, plan, .. } = *cx;
    let expected = transport.addresses();
    let wait = options.dns_wait_seconds;
    let notice = json!({"phase": "awaiting_dns_cutover", "domain": options.target.origin_host,
        "addresses": expected, "waitSeconds": wait});
    executor.replay(&encode(&notice)?, false, end).await?;
    let window = Duration::from_secs(if wait == 0 { 10 } else { wait });
    let deadline = (plan.now()? + window).min(end);
    loop {
        match resolved(cx, executor, end, deadline).await {
            Ok(found) if found == expected => return Ok(()),
            Ok(_) => {}
            // A query that failed or ran out of time is simply asked again.
            Err(error) if error.is_child_exit() || error.code == 124 => {}
            Err(error) => return Err(error),
        }
        let now = plan.now()?;
        if wait == 0 || now >= deadline {
            return Err(refusal(
                "migration_dns_cutover_timeout_source_remains_sealed",
            ));
        }
        execute::pause(plan, executor, (now + Duration::from_secs(2)).min(deadline)).await?;
    }
}

/// The two selected hosts and what the destination's inventory asks for.
struct Selected {
    source: Target,
    target: Target,
    transport: Option<Transport>,
    services: Services,
}

fn selectors(options: &Options) -> [Selector<'_>; 2] {
    [&options.source, &options.target].map(|side| Selector {
        inventory: &side.inventory,
        limit: side.limit.as_deref(),
        origin_host: &side.origin_host,
    })
}

/// Prove clean published tools and an ancestor application revision, hold
/// every controller input, and select two different hosts.
async fn preflight<E: Executor>(
    cx: &Context<'_>,
    executor: &mut E,
    run: &mut Run,
) -> Result<(Revision, Revision, Selected)> {
    let Context {
        root,
        options,
        state,
        plan,
        budget,
    } = *cx;
    let end = plan.begin(Step::Preflight.index())?;
    let tools = execute::checkout(state, plan, budget, executor, end).await?;
    let revision = match options.revision.as_deref() {
        Some(selected) if selected != tools.as_str() => {
            let ancestry = ["merge-base", "--is-ancestor", selected, tools.as_str()];
            match git(state, plan, budget, executor, end, &ancestry).await {
                Ok(_) => Revision::named(selected)?,
                Err(error) if error.is_child_exit() => {
                    return Err(refusal("migration_release_not_tool_ancestor"));
                }
                Err(error) => return Err(error),
            }
        }
        _ => tools.clone(),
    };
    run.report["toolRevision"] = json!(tools.as_str());
    run.report["revision"] = json!(revision.as_str());
    let evidence = "results/migration.evidence";
    ignored(cx, executor, end, evidence, "results_must_be_git_ignored").await?;
    if let Some(path) = &options.activate_inventory {
        let refused = "migration_activation_inventory_must_be_ignored";
        ignored(cx, executor, end, utf8(path)?, refused).await?;
    }
    execute::custody(root, state, plan, budget, executor, end, &tools).await?;
    let [old, new] = selectors(options);
    let source = selection(state, plan, budget, executor, end, old).await?;
    let target = selection(state, plan, budget, executor, end, new).await?;
    checks::distinct(&source.variables, &target.variables)?;
    let transport = match options.keeps_hostname() {
        true => Some(checks::transport(&source.variables, &target.variables)?),
        false => None,
    };
    let services = checks::services(&target.variables)?;
    if options.update_canary {
        canary(cx, executor, end, &options.source.origin).await?;
    }
    plan.complete(Step::Preflight.index(), end)?;
    let selected = Selected {
        source,
        target,
        transport,
        services,
    };
    Ok((tools, revision, selected))
}

/// The variables every play receives: the operation, the revision and the
/// typed signed selections the staging step verifies.
fn variables(
    cx: &Context<'_>,
    revision: &Revision,
    chosen: &Signed,
    transport: Option<&Transport>,
    operation: &str,
    evidence: &Path,
) -> Result<Map<String, Value>> {
    let options = cx.options;
    let state = lock(cx.state)?;
    let tools = state.tools.as_ref().ok_or_else(|| failure(1))?;
    let revision = revision.as_str();
    let run = chosen.run.id.get();
    let mut variables = json!({
        "scmig_run_id": operation, "scmig_evidence": evidence,
        "scmig_source_origin": options.source.origin,
        "scmig_destination_origin": options.target.origin,
        "scmig_revision": revision, "scbench_revision": revision,
        "scbench_repository": format!("https://github.com/{}.git", options.repository),
        "scpub_release_revision": revision, "scpub_release_expected_revision": revision,
        "scpub_release_repository": options.repository,
        "scpub_release_artifact_id": chosen.application.id.get(),
        "scpub_release_ci_run": run,
        "scpub_ops_release_controller": tools.controller_path()?,
        "scpub_ops_gh_executable": tools.gh_path()?,
        "scpub_ops_artifact_id": chosen.ops.id.get(),
        "scpub_ops_selection": {"repository": options.repository, "revision": revision,
            "packageVersion": env!("CARGO_PKG_VERSION"), "protocolVersion": 1,
            "target": "x86_64-unknown-linux-musl", "buildRunId": run,
            "runAttempt": chosen.run.attempt.get()}});
    if let Some(path) = &options.application_artifact_dir {
        variables["scpub_release_verified_directory"] = json!(path);
    }
    if let Some(path) = &options.ops_artifact_zip {
        variables["scpub_ops_artifact_zip"] = json!(path);
    }
    if let Some(transport) = transport {
        variables["scpub_turn_verify_address"] = json!(transport.address);
    }
    variables.as_object().cloned().ok_or_else(|| failure(1))
}

/// Select the signed release, prove nothing moved while waiting for it, and
/// freeze both hosts in private inventories only this move uses.
async fn select<E: Executor>(
    cx: &Context<'_>,
    executor: &mut E,
    run: &mut Run,
    tools: &Revision,
    revision: &Revision,
    selected: &Selected,
) -> Result<()> {
    let Context {
        options,
        state,
        plan,
        budget,
        ..
    } = *cx;
    run.report["phase"] = json!("ci");
    let end = enter(plan, Step::Select)?;
    execute::fresh_main(state, plan, budget, executor, end, tools).await?;
    let wait = options.wait_seconds;
    let chosen = execute::signed(state, plan, budget, executor, end, revision, wait).await?;
    run.report["ciRunId"] = json!(chosen.run.id.get());
    run.report["runAttempt"] = json!(chosen.run.attempt.get());
    run.report["applicationArtifactId"] = json!(chosen.application.id.get());
    run.report["opsArtifactId"] = json!(chosen.ops.id.get());
    plan.complete(Step::Select.index(), end)?;

    run.report["phase"] = json!("recheck");
    let end = enter(plan, Step::Recheck)?;
    if execute::checkout(state, plan, budget, executor, end).await? != *tools {
        return Err(refusal("checkout_changed"));
    }
    let [old, new] = selectors(options);
    if selection(state, plan, budget, executor, end, old).await? != selected.source
        || selection(state, plan, budget, executor, end, new).await? != selected.target
    {
        return Err(refusal("inventory_target_changed"));
    }
    execute::fresh_main(state, plan, budget, executor, end, tools).await?;
    execute::unchanged(state, plan, budget, executor, end, revision, &chosen).await?;
    let operation = activation::entropy(16)?;
    let evidence = {
        let state = lock(state)?;
        let evidence = state.evidence.as_ref().ok_or_else(|| failure(1))?;
        evidence.path().to_owned()
    };
    let transport = selected.transport.as_ref();
    let variables = variables(cx, revision, &chosen, transport, &operation, &evidence)?;
    let hosts = |hosts: Value| encode(&json!({"benchmark_hosts": {"hosts": hosts}}));
    let (source, target) = (&selected.source.variables, &selected.target.variables);
    let inventories = [
        (
            "source-inventory.json",
            hosts(json!({"migration_source": source}))?,
        ),
        (
            "target-inventory.json",
            hosts(json!({"migration_target": target}))?,
        ),
        (
            "migration-inventory.json",
            hosts(json!({"migration_source": source, "migration_target": target}))?,
        ),
    ];
    let worker = (budget.clone(), evidence.clone());
    // From here every play reads the frozen copies, so custody moves to them:
    // a later edit of an operator's own inventory cannot disturb the move.
    run.original = stage(state, plan, executor, end, move |state, cp| {
        let (budget, directory) = &worker;
        check(state, budget, cp)?;
        let evidence = state.evidence.as_mut().ok_or_else(|| failure(1))?;
        for (name, bytes) in &inventories {
            evidence.write(name, bytes, cp)?;
        }
        let held = |name: &str| Original::open(&directory.join(name), budget, cp);
        state.inventory = Some(held("target-inventory.json")?.0);
        Ok(state.companion.replace(held("migration-inventory.json")?.0))
    })
    .await?;
    run.report["runId"] = json!(operation);
    run.frozen = Some(Frozen {
        evidence,
        variables,
    });
    plan.complete(Step::Recheck.index(), end)
}

/// Verify on the controller, prepare while the source stays live, transfer
/// while it is frozen, then start the destination and its services.
async fn perform<E: Executor>(
    cx: &Context<'_>,
    executor: &mut E,
    run: &mut Run,
    revision: &Revision,
    selected: &Selected,
) -> Result<()> {
    let Context { options, plan, .. } = *cx;
    let evidence = run
        .frozen
        .as_ref()
        .ok_or_else(|| failure(1))?
        .evidence
        .clone();
    let set = |run: &mut Run, name: &str, value: Value| {
        let frozen = run.frozen.as_mut().ok_or_else(|| failure(1))?;
        frozen.variables.insert(name.into(), value);
        Ok::<(), Failure>(())
    };
    // Nothing has contacted either host yet: the signed application and the
    // Ops executable are authenticated first. Staging rechecks the same
    // application originals, and the inspection runs that executable.
    play(cx, executor, run, Step::Verify, &[]).await?;
    let end = enter(plan, Step::Inspect)?;
    let record = left(cx, executor, end, evidence.join("application.json")).await?;
    let directory = checks::application(&record, revision.as_str())?;
    set(run, "scpub_release_verified_directory", json!(directory))?;
    let record = left(cx, executor, end, evidence.join("ops.json")).await?;
    let (inspector, pin) = checks::executable(&record, &evidence)?;
    set(run, "scmig_ops_executable", json!(inspector))?;
    set(run, "scmig_ops_sha256", json!(pin))?;
    run.report["githubAttested"] = json!(true);
    play(cx, executor, run, Step::Inspect, &[]).await?;
    let end = enter(plan, Step::Site)?;
    let inspected = left(cx, executor, end, evidence.join("inspection.json")).await?;
    if inspected.len() > INSPECTION {
        return Err(refusal("migration_inspection_file"));
    }
    let inspected: Value = serde_json::from_slice(&inspected).map_err(|_| failure(1))?;
    let (old, new) = (&options.source.origin_host, &options.target.origin_host);
    let identity = checks::relying_party(&inspected, old, new)?;
    set(run, "scpub_webauthn_rp_id", json!(identity))?;
    let frozen = run.frozen.clone().ok_or_else(|| failure(1))?;
    let bytes = encode(&Value::Object(frozen.variables))?;
    retain(cx, executor, end, "variables.json".into(), bytes).await?;
    play(cx, executor, run, Step::Site, &[]).await?;
    play(cx, executor, run, Step::Bootstrap, &[]).await?;
    play(cx, executor, run, Step::Stage, &[]).await?;
    play(
        cx,
        executor,
        run,
        Step::Public,
        &[("scpub_turn_enabled", json!(false))],
    )
    .await?;
    run.freeze_attempted = true;
    play(cx, executor, run, Step::Transfer, &[]).await?;
    // The launch can start the destination before its transport returns, so
    // from here no failure or interrupt ever resumes the source.
    run.startup_attempted = true;
    play(cx, executor, run, Step::Launch, &[]).await?;
    run.report["destinationStarted"] = json!(true);
    let end = enter(plan, Step::Https)?;
    checkpoint(cx, executor, run, end, "https").await?;
    let pinned = selected.transport.as_ref();
    https(cx, executor, end, revision.as_str(), pinned).await?;
    plan.complete(Step::Https.index(), end)?;
    if selected.services.relay {
        let activate = |enabled: bool| [("scpub_turn_activate", json!(enabled))];
        play(cx, executor, run, Step::RelayPrepare, &activate(false)).await?;
        play(cx, executor, run, Step::RelayActivate, &activate(true)).await?;
    }
    if selected.services.backup {
        play(cx, executor, run, Step::Backup, &[]).await?;
    }
    if selected.services.monitoring {
        play(cx, executor, run, Step::Monitoring, &[]).await?;
    }
    play(cx, executor, run, Step::Finalize, &[]).await?;
    if let Some(transport) = &selected.transport {
        let end = enter(plan, Step::Cutover)?;
        checkpoint(cx, executor, run, end, "awaiting_dns_cutover").await?;
        cutover(cx, executor, end, transport).await?;
        plan.complete(Step::Cutover.index(), end)?;
        let end = enter(plan, Step::PublicHttps)?;
        checkpoint(cx, executor, run, end, "public_https").await?;
        https(cx, executor, end, revision.as_str(), None).await?;
        plan.complete(Step::PublicHttps.index(), end)?;
        run.report["dnsCutoverVerified"] = json!(true);
    }
    run.report["recovery"] = json!("source_retained_stopped");
    run.report["phase"] = json!("destination_ready");
    Ok(())
}

/// Move the operator's own selectors, each only when it was asked for: the
/// private source inventory, then the repository's canary origin.
async fn switch<E: Executor>(
    cx: &Context<'_>,
    executor: &mut E,
    run: &mut Run,
    target: &Target,
) -> Result<()> {
    let Context {
        options,
        state,
        plan,
        budget,
        ..
    } = *cx;
    if let Some(path) = options.activate_inventory.clone() {
        let end = enter(plan, Step::Activate)?;
        checkpoint(cx, executor, run, end, "activate_inventory").await?;
        let moved = &run.frozen.as_ref().ok_or_else(|| failure(1))?.variables;
        let bytes = activation::selected(&target.host, &target.variables, moved)?;
        let original = run.original.clone().ok_or_else(|| failure(1))?;
        let worker_budget = budget.clone();
        let (replaced, outcome) = stage(state, plan, executor, end, move |_, cp| {
            let unchanged = || {
                let held = original.check(&worker_budget, cp);
                held.map_err(|_| refusal("migration_activation_inventory_changed"))
            };
            Ok(activation::replace(&path, &bytes, unchanged))
        })
        .await?;
        run.report["inventoryActivated"] = json!(replaced);
        outcome?;
        plan.complete(Step::Activate.index(), end)?;
    }
    if options.update_canary {
        let end = enter(plan, Step::Canary)?;
        checkpoint(cx, executor, run, end, "update_canary").await?;
        canary(cx, executor, end, &options.source.origin).await?;
        run.report["canaryUpdateAttempted"] = json!(true);
        checkpoint(cx, executor, run, end, "update_canary").await?;
        let origin = options.target.origin.as_str();
        let repository = policy::REPOSITORY;
        let set = [
            "variable",
            "set",
            "CANARY_ORIGIN",
            "--repo",
            repository,
            "--body",
            origin,
        ];
        call(
            state,
            plan,
            budget,
            executor,
            end,
            Program::FrontendGh,
            argv(&set),
        )
        .await?;
        canary(cx, executor, end, origin).await?;
        run.report["canaryUpdated"] = json!(true);
        plan.complete(Step::Canary.index(), end)?;
    }
    run.report["phase"] = json!("complete");
    Ok(())
}

async fn body<E: Executor>(cx: &Context<'_>, executor: &mut E, run: &mut Run) -> Result<()> {
    let (tools, revision, selected) = preflight(cx, executor, run).await?;
    select(cx, executor, run, &tools, &revision, &selected).await?;
    perform(cx, executor, run, &revision, &selected).await?;
    switch(cx, executor, run, &selected.target).await
}

/// After a failure, resume the source only when it was frozen and the
/// destination provably never started, and only after stopping the
/// destination. Any doubt leaves both to the operator.
async fn recover<E: Executor>(cx: &Context<'_>, executor: &mut E, run: &mut Run) {
    if !run.freeze_attempted {
        run.report["recovery"] = json!("source_untouched");
        return;
    }
    if run.startup_attempted {
        run.report["recovery"] = json!("manual_destination_recovery_source_must_remain_frozen");
        return;
    }
    run.report["recovery"] = json!("target_stop_required_before_source_resume");
    if play(cx, executor, run, Step::StopTarget, &[]).await.is_ok() {
        run.report["recovery"] = json!("source_resume_pending");
        if play(cx, executor, run, Step::ResumeSource, &[])
            .await
            .is_ok()
        {
            run.report["recovery"] = json!("source_resumed_target_stopped");
            return;
        }
    }
    // A destination that may still accept writes never lets the source resume.
    run.report["recovery"] = json!("manual_recovery_required");
}

/// One whole move over any executor; the caller publishes its report.
pub(in super::super) async fn transaction<E: Executor>(
    root: PathBuf,
    options: Arc<Options>,
    environment: Environment,
    current: PathBuf,
    signals: Option<Arc<simplestchat_ops::owned_process::CompletionSignals>>,
    plan: &Plan,
    executor: &mut E,
) -> Result<Completed> {
    let budget = Budget::default();
    let state: Shared = Arc::new(Mutex::new(State::default()));
    lock(&state)?.signals = signals;
    let mut run = Run {
        report: json!({"schemaVersion": 1, "operation": "migrate", "passed": false,
            "phase": "preflight", "repository": options.repository,
            "sourceOrigin": options.source.origin, "destinationOrigin": options.target.origin,
            "sourceFreezeAttempted": false, "destinationStartupAttempted": false,
            "destinationStarted": false, "githubAttested": false,
            "inventoryActivated": false,
            "inventoryActivationRequested": options.activate_inventory.is_some(),
            "canaryUpdateAttempted": false, "canaryUpdated": false,
            "canaryUpdateRequested": options.update_canary,
            "recovery": "source_untouched", "failureClass": null,
            "startedAt": chrono::Utc::now().to_rfc3339()}),
        sequence: 0,
        freeze_attempted: false,
        startup_attempted: false,
        frozen: None,
        original: None,
    };
    let cx = Context {
        root: &root,
        options: &options,
        state: &state,
        plan,
        budget: &budget,
    };
    let end = plan.begin(Step::Preflight.index())?;
    let worker = (root.clone(), options.clone(), budget.clone());
    let prepared = stage(&state, plan, executor, end, move |state, cp| {
        let (root, options, budget) = &worker;
        prepare(state, root, options, &environment, &current, budget, cp)
    })
    .await;
    let mut primary = match prepared {
        Ok(evidence) => {
            run.report["evidence"] = json!(evidence);
            if let Some(path) = &options.activate_inventory {
                run.report["activationInventory"] = json!(path);
            }
            body(&cx, executor, &mut run).await.err()
        }
        Err(error) => Some(error),
    };
    if primary.is_some() {
        run.report["failedPhase"] = run.report["phase"].clone();
        recover(&cx, executor, &mut run).await;
    }
    if let Err(error) = executor.ready().await {
        keep(&mut primary, error);
    }
    if !executor.cleanup_passed() {
        keep(&mut primary, failure(1));
    }
    if let Some(error) = &primary {
        run.report["failureClass"] = json!(checks::class(error));
        run.report["phase"] = json!("failed");
    }
    run.report["passed"] = json!(primary.is_none());
    execute::close(
        &state, plan, &budget, executor, run.report, primary, "passed",
    )
    .await
}

/// The move with native children and the caller's original signals.
pub(in super::super) async fn native(
    root: PathBuf,
    options: Arc<Options>,
    environment: Environment,
    current: PathBuf,
    flags: Arc<simplestchat_ops::owned_process::CompletionSignals>,
    plan: &Plan,
) -> Result<Completed> {
    let replay = execute::replay_environment();
    let signals = Signals::from_completion(flags.clone());
    let mut executor = Native::new(&root, &replay, signals);
    let mut completed = transaction(
        root.clone(),
        options,
        environment,
        current,
        Some(flags),
        plan,
        &mut executor,
    )
    .await?;
    execute::publish(&mut completed, &mut executor).await;
    Ok(completed)
}

#[cfg(test)]
#[path = "workflow_tests.rs"]
mod tests;
