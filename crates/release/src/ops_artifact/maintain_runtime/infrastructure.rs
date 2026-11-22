//! The scheduled infrastructure job: one owned Debian guest, the production
//! provisioning playbooks applied twice each, the signed release staged and
//! deployed through the production commands, and the guest's own checks.
//!
//! Full scope: a clean checkout of the workflow's revision, that revision's
//! one successful `ci.yml` push run and its two signed artifacts (the
//! production selection), both authenticated by the production verifiers;
//! then boot, `site.yml` twice, the production delivery of the signed Ops
//! executable, `security/vm/stage.yml` (the production `release stage`),
//! `public.yml` twice with a guest snapshot after each, the guest's database
//! action, `backup.yml` twice and the guest's backup and isolated restore.
//! Boot scope: the clean checkout, the boot and cloud-init only.
//!
//! The summary keeps fixed labels, exit statuses, recap counts, the guest's
//! aggregate receipts and the guest's lifecycle evidence; command output,
//! keys and secrets stay in the private output directory and the guest.

use super::{
    Error, Result,
    ansible::Recap,
    auth, catalog,
    control::Control,
    execute::{self, Delivery, Owners},
    files::{self, Original},
    options::{self, Options, Purpose, REPOSITORY},
    process::Runner,
    vm::{self, Vm},
};
use crate::ops_artifact::deploy_frontend::policy::{
    self as selection, ArtifactKind, Revision, RunProgress,
};
use serde_json::{Value, json};
use simplestchat_ops::release::infrastructure::{self as guest, Action, Receipt};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub(super) const USAGE: &str = "usage: simplestchat-release ops-artifact infrastructure-runtime \
[--boot-only] --output ABS";
/// The installed signed executable whose guest role checks the deployment.
const GUEST: &str = "/usr/local/libexec/simplestchat-public/simplestchat-ops";
/// The exit statuses a playbook may end with whose recap is still kept.
const PLAYBOOK_EXITS: [i32; 8] = [0, 1, 2, 4, 8, 99, 250, 255];
/// How long CI of the revision may still run, and how often it is asked.
const CI_WAIT: Duration = Duration::from_secs(900);
const CI_POLL: Duration = Duration::from_secs(30);

/// What the workflow asked for: the scope, a fresh output directory and the
/// workflow's own revision.
pub(super) struct Request {
    pub purpose: Purpose,
    pub output: PathBuf,
    pub revision: String,
}

impl Request {
    /// `--output ABS`, or `--boot-only` and `--output ABS` in either order;
    /// `--help` alone is no request. The revision is `GITHUB_SHA`.
    pub fn parse(arguments: &[String]) -> Result<Option<Self>> {
        Self::parse_for(arguments, std::env::var("GITHUB_SHA").ok())
    }

    /// `parse` with the workflow's revision `revision`: 40 lowercase
    /// hexadecimal digits, or no request.
    pub(super) fn parse_for(
        arguments: &[String],
        revision: Option<String>,
    ) -> Result<Option<Self>> {
        let words: Vec<&str> = arguments.iter().map(String::as_str).collect();
        let (purpose, output) = match words.as_slice() {
            ["--help"] => return Ok(None),
            ["--output", output] => (Purpose::Infrastructure, *output),
            ["--boot-only", "--output", output] | ["--output", output, "--boot-only"] => {
                (Purpose::Boot, *output)
            }
            _ => return Err(Error::Input),
        };
        let output = PathBuf::from(output);
        if !files::safe_path(&output) {
            return Err(Error::Input);
        }
        let revision = revision.ok_or(Error::Input)?;
        if !options::digest(&revision, 40) {
            return Err(Error::Input);
        }
        Ok(Some(Self {
            purpose,
            output,
            revision,
        }))
    }
}

/// One playbook the job applies.
#[derive(Clone, Copy)]
enum Playbook {
    Site,
    Stage,
    Public,
    Backup,
}

impl Playbook {
    fn name(self) -> &'static str {
        match self {
            Self::Site => "site",
            Self::Stage => "stage",
            Self::Public => "public",
            Self::Backup => "backup",
        }
    }
    fn path(self) -> &'static str {
        match self {
            Self::Site => "ops/ansible/site.yml",
            Self::Stage => "security/vm/stage.yml",
            Self::Public => "ops/ansible/public.yml",
            Self::Backup => "ops/ansible/backup.yml",
        }
    }
}

/// The fixed arguments of one playbook run on the generated guest; the host
/// role runs only its host, Docker and benchmark tags, never a source clone.
fn playbook_command(root: &Path, inventory: &Path, playbook: Playbook) -> Vec<OsString> {
    let mut argv: Vec<OsString> = vec![
        "-i".into(),
        inventory.as_os_str().into(),
        "--limit".into(),
        "vmfixture".into(),
        "--forks".into(),
        "1".into(),
        root.join(playbook.path()).into_os_string(),
    ];
    if matches!(playbook, Playbook::Site) {
        argv.extend(["--tags".into(), "host,docker,benchmark".into()]);
    }
    argv
}

/// The guest action's command: the installed signed executable's guest role.
fn guest_command(action: Action, run: &str) -> Vec<String> {
    [
        "/usr/bin/sudo",
        "-n",
        GUEST,
        "release",
        "infrastructure-guest",
        action.name(),
        "--run-id",
        run,
    ]
    .map(str::to_owned)
    .to_vec()
}

/// The single generated host and the fixture's deployment shape: loopback
/// only, the fixture domain, the smallest sizing, backups on and TURN off,
/// bound to this run, its revision and the authenticated release directory.
pub(super) fn inventory_value(
    work: &Path,
    port: u16,
    run: &str,
    artifacts: &Path,
    revision: &str,
) -> Result<Value> {
    if port < 1024 || !options::digest(run, 32) || !options::digest(revision, 40) {
        return Err(Error::Input);
    }
    let ssh = execute::ssh_options(work)
        .iter()
        .map(|part| execute::quote(part))
        .collect::<Vec<_>>()
        .join(" ");
    Ok(json!({"benchmark_hosts":{"hosts":{"vmfixture":{
        "ansible_host":"127.0.0.1","ansible_port":port,"ansible_user":"fixture",
        "ansible_python_interpreter":"/usr/bin/python3",
        "ansible_ssh_private_key_file":work.join("client"),"ansible_ssh_args":ssh,
        "ansible_become":true,"scbench_revision":revision,"scbench_upgrade_packages":false,
        "scbench_reboot":false,"scpub_enabled":true,"scpub_release_revision":revision,
        "scpub_domain":"vm-fixture.test","scpub_announce_ip":"127.0.0.1",
        "scpub_announce_ipv6":"","scpub_media_workers":1,"scpub_app_cpus":1,
        "scpub_app_memory_mib":1024,"scpub_postgres_memory_mib":1024,
        "scpub_postgres_shared_buffers_mib":256,"scpub_backup_enabled":true,
        "scpub_turn_enabled":false,"vm_fixture_run_id":run,
        "vm_fixture_revision":revision,"vm_fixture_artifact_dir":artifacts}}}}))
}

/// The revision's signed coordinates: its CI run and attempt, and the
/// application and Ops artifacts of that run.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Signed {
    pub run: u64,
    pub attempt: u64,
    pub application: u64,
    pub ops: u64,
}

/// The commands one job step issues, so the step can be run against fakes.
pub(super) trait Steps {
    /// Run `tool` with `args` as role `role`, admitting the exit statuses
    /// in `accepted`; returns the status and standard output.
    async fn run(
        &mut self,
        role: &str,
        tool: &str,
        args: Vec<OsString>,
        cap: Duration,
        accepted: &[i32],
    ) -> Result<(i32, Vec<u8>)>;
    /// The same, with the job's `GH_TOKEN` and the pinned `gh` first on `PATH`.
    async fn credential(
        &mut self,
        role: &str,
        tool: &str,
        args: Vec<OsString>,
        cap: Duration,
    ) -> Result<Vec<u8>>;
    /// The SSH arguments that run `command` in the owned guest.
    fn ssh(&mut self, command: &[String]) -> Result<Vec<OsString>>;
    /// Require the guest still running, healthy and its inputs unchanged.
    fn check(&mut self) -> Result<()>;
    /// Wait `pause`, within the control's own deadline.
    async fn pause(&mut self, pause: Duration) -> Result<()>;
    /// Seconds since the step's start.
    fn elapsed(&self) -> Duration;
    /// The number of commands issued so far.
    fn issued(&self) -> usize;
}

/// The live job: the retained runner, the owned guest and the control.
struct Live<'a> {
    runner: &'a mut Runner,
    vm: Option<&'a mut Vm>,
    control: &'a mut Control,
    started: Instant,
}

impl Steps for Live<'_> {
    async fn run(
        &mut self,
        role: &str,
        tool: &str,
        args: Vec<OsString>,
        cap: Duration,
        accepted: &[i32],
    ) -> Result<(i32, Vec<u8>)> {
        let observed = self
            .runner
            .run(self.control, role, tool, args, cap, accepted)
            .await?;
        Ok((observed.status, observed.stdout))
    }
    async fn credential(
        &mut self,
        role: &str,
        tool: &str,
        args: Vec<OsString>,
        cap: Duration,
    ) -> Result<Vec<u8>> {
        let observed = self
            .runner
            .credential(self.control, role, tool, args, cap)
            .await?;
        Ok(observed.stdout)
    }
    fn ssh(&mut self, command: &[String]) -> Result<Vec<OsString>> {
        let vm = self.vm.as_deref().ok_or(Error::Refused)?;
        vm.ssh(self.runner, self.control, command)
    }
    fn check(&mut self) -> Result<()> {
        if let Some(vm) = self.vm.as_deref_mut() {
            vm.check(self.control)?;
        }
        self.runner.integrity(self.control)
    }
    async fn pause(&mut self, pause: Duration) -> Result<()> {
        let pause = self.control.remaining(pause)?;
        tokio::time::sleep(pause).await;
        self.control.check()
    }
    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }
    fn issued(&self) -> usize {
        self.runner.calls().len()
    }
}

/// What the summary keeps of each command beyond its status: a playbook's
/// recap counts and a guest's failure labels, by command number.
#[derive(Default)]
pub(super) struct Notes {
    pub recaps: BTreeMap<usize, Value>,
    pub failures: BTreeMap<usize, Value>,
}

fn checkpoint(report: &mut Value, name: &str) {
    report["checkpoint"] = name.into();
    report["failure"] = name.into();
}

fn refuse(report: &mut Value, code: impl Into<Value>) -> Error {
    report["failure"] = code.into();
    Error::Refused
}

fn api(endpoint: &str) -> Vec<OsString> {
    let words = [
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
    ];
    words.iter().map(OsString::from).collect()
}

/// The checkout is the workflow's own revision and holds no change, tracked
/// or untracked.
pub(super) async fn source(
    steps: &mut impl Steps,
    report: &mut Value,
    revision: &str,
) -> Result<()> {
    let cap = Duration::from_secs(120);
    let head = ["rev-parse", "HEAD"].map(OsString::from);
    let (_, head) = steps
        .run("source-revision", "git", head.to_vec(), cap, &[0])
        .await?;
    if head.trim_ascii() != revision.as_bytes() {
        return Err(refuse(report, "vm_requires_workflow_revision"));
    }
    let status = ["status", "--porcelain", "--untracked-files=normal"].map(OsString::from);
    let (_, changes) = steps
        .run("source-clean", "git", status.to_vec(), cap, &[0])
        .await?;
    if !changes.trim_ascii().is_empty() {
        return Err(refuse(report, "vm_requires_clean_checkout"));
    }
    Ok(())
}

/// Select the revision's one `ci.yml` push run, wait for it to succeed and
/// select its application and Ops artifacts, as a routine release does.
pub(super) async fn select(
    steps: &mut impl Steps,
    report: &mut Value,
    revision: &str,
) -> Result<Signed> {
    let named = Revision::named(revision).map_err(|_| Error::Input)?;
    let code = |report: &mut Value, failure: simplestchat_toolkit::tooling::Failure| {
        refuse(report, failure.message)
    };
    let cap = Duration::from_secs(20);
    let listing = format!(
        "repos/{REPOSITORY}/actions/workflows/ci.yml/runs?event=push&branch=main\
         &head_sha={revision}&per_page=100"
    );
    let found = steps
        .credential("select-run", "gh", api(&listing), cap)
        .await?;
    let run = selection::runs(&found, &named).map_err(|failure| code(report, failure))?;
    let status = format!("repos/{REPOSITORY}/actions/runs/{}", run.id.get());
    let waited = steps.elapsed();
    loop {
        let answer = steps
            .credential("ci-status", "gh", api(&status), cap)
            .await?;
        let progress =
            selection::run(&answer, &named, &run).map_err(|failure| code(report, failure))?;
        if progress == RunProgress::Success {
            break;
        }
        if steps.elapsed().saturating_sub(waited) >= CI_WAIT {
            return Err(refuse(report, "ci_wait_timeout"));
        }
        steps.pause(CI_POLL).await?;
    }
    let mut artifacts = Vec::new();
    for (role, kind) in [
        ("select-application", ArtifactKind::Application),
        ("select-ops", ArtifactKind::Ops),
    ] {
        let listing = format!(
            "repos/{REPOSITORY}/actions/runs/{}/artifacts?name={}&per_page=100",
            run.id.get(),
            kind.name(&named)
        );
        let answer = steps.credential(role, "gh", api(&listing), cap).await?;
        let artifact = selection::artifact(&answer, &named, &run, kind)
            .map_err(|failure| code(report, failure))?;
        artifacts.push(artifact.id.get());
    }
    let answer = steps
        .credential("ci-final", "gh", api(&status), cap)
        .await?;
    let progress =
        selection::run(&answer, &named, &run).map_err(|failure| code(report, failure))?;
    if progress != RunProgress::Success {
        return Err(refuse(report, "ci_status_changed"));
    }
    let [application, ops] = artifacts.as_slice() else {
        return Err(Error::Refused);
    };
    Ok(Signed {
        run: run.id.get(),
        attempt: run.attempt.get(),
        application: *application,
        ops: *ops,
    })
}

/// Apply `playbook` once; the second run (`second`) must change nothing.
pub(super) async fn apply(
    steps: &mut impl Steps,
    report: &mut Value,
    notes: &mut Notes,
    command: Vec<OsString>,
    playbook: &'static str,
    second: bool,
) -> Result<()> {
    steps.check()?;
    let name = format!("{playbook}-{}", if second { "second" } else { "first" });
    checkpoint(report, &name);
    let cap = Duration::from_secs(600);
    let (status, output) = steps
        .run(&name, "ansible-playbook", command, cap, &PLAYBOOK_EXITS)
        .await?;
    let index = steps.issued().saturating_sub(1);
    let recap = Recap::parse(&output).map_err(|code| refuse(report, code))?;
    notes.recaps.insert(index, recap.to_json());
    if status != 0 {
        return Err(refuse(report, format!("vm_command_failed_{name}")));
    }
    recap.verdict(second).map_err(|code| refuse(report, code))?;
    // Staging runs once; its phase is named for the playbook alone.
    let key = match (playbook, second) {
        ("stage", _) => playbook.to_owned(),
        (_, false) => format!("{playbook}First"),
        (_, true) => format!("{playbook}Second"),
    };
    report["phases"][key] = recap.to_json();
    Ok(())
}

/// Run guest `action` and admit its receipt; a failed check keeps its labels.
pub(super) async fn act(
    steps: &mut impl Steps,
    report: &mut Value,
    notes: &mut Notes,
    run: &str,
    action: Action,
) -> Result<Value> {
    steps.check()?;
    checkpoint(report, action.name());
    let argv = steps.ssh(&guest_command(action, run))?;
    let cap = Duration::from_secs(400);
    let (status, output) = steps.run(action.name(), "ssh", argv, cap, &[0, 1]).await?;
    let index = steps.issued().saturating_sub(1);
    match guest::admit(status, &output, run, action) {
        Ok(Receipt::Passed(projection)) => Ok(projection),
        Ok(Receipt::Failed(diagnostic)) => {
            notes.failures.insert(index, diagnostic.to_json());
            Err(refuse(report, "vm_guest_assertion_failed"))
        }
        Err(malformed) => Err(refuse(report, malformed.code())),
    }
}

/// Everything after the guest booted: the playbooks, the delivery and the
/// guest's three actions, in the job's fixed order.
pub(super) async fn exercise(
    steps: &mut impl Steps,
    report: &mut Value,
    notes: &mut Notes,
    root: &Path,
    inventory: &Path,
    delivery: Vec<OsString>,
    run: &str,
) -> Result<()> {
    for second in [false, true] {
        let command = playbook_command(root, inventory, Playbook::Site);
        apply(steps, report, notes, command, "site", second).await?;
    }
    steps.check()?;
    checkpoint(report, "ops-install");
    let cap = Duration::from_secs(600);
    let installed = steps
        .credential("ops-install", "ansible-playbook", delivery, cap)
        .await?;
    let recap = Recap::parse(&installed).map_err(|code| refuse(report, code))?;
    notes
        .recaps
        .insert(steps.issued().saturating_sub(1), recap.to_json());
    recap.verdict(false).map_err(|code| refuse(report, code))?;
    report["phases"]["opsInstall"] = recap.to_json();
    let command = playbook_command(root, inventory, Playbook::Stage);
    apply(steps, report, notes, command, Playbook::Stage.name(), false).await?;
    for second in [false, true] {
        let command = playbook_command(root, inventory, Playbook::Public);
        apply(steps, report, notes, command, "public", second).await?;
        let snapshot = act(steps, report, notes, run, Action::Snapshot).await?;
        let key = if second {
            "snapshotSecond"
        } else {
            "snapshotFirst"
        };
        report["phases"][key] = snapshot;
    }
    report["phases"]["database"] = act(steps, report, notes, run, Action::Database).await?;
    for second in [false, true] {
        let command = playbook_command(root, inventory, Playbook::Backup);
        apply(steps, report, notes, command, "backup", second).await?;
    }
    report["phases"]["restore"] = act(steps, report, notes, run, Action::BackupRestore).await?;
    steps.check()?;
    Ok(())
}

async fn body(
    request: &Request,
    control: &mut Control,
    owners: &mut Owners,
    report: &mut Value,
    notes: &mut Notes,
) -> Result<()> {
    options::guard(request.purpose, &request.output)?;
    control.check()?;
    let parent = request.output.parent().ok_or(Error::Input)?;
    vm::preflight(parent, control)?;
    fs::create_dir(&request.output).map_err(|_| Error::Io)?;
    fs::set_permissions(&request.output, fs::Permissions::from_mode(0o700))
        .map_err(|_| Error::Io)?;
    let root = crate::root().map_err(|_| Error::Input)?;
    owners.runner = Some(Runner::new(root, request.output.clone(), control)?);
    let runner = owners.runner.as_mut().ok_or(Error::Refused)?;
    for name in catalog::sources_for(request.purpose) {
        let original = Original::open(&runner.root.join(&name), 1024 * 1024, control)?;
        runner.sources.insert(name, original);
    }
    let (_, cloud) = vm::pin(&runner.source("security/vm/debian-cloud.json", control)?)?;
    report["cloudImageSha512"] = cloud.into();
    checkpoint(report, "checkout");
    let started = Instant::now();
    let mut steps = Live {
        runner,
        vm: None,
        control,
        started,
    };
    source(&mut steps, report, &request.revision).await?;
    let signed = if request.purpose == Purpose::Infrastructure {
        checkpoint(report, "selection");
        Some(select(&mut steps, report, &request.revision).await?)
    } else {
        None
    };
    let runner = owners.runner.as_mut().ok_or(Error::Refused)?;
    let mut selected = None;
    if let Some(signed) = &signed {
        report["ciRunId"] = signed.run.into();
        report["runAttempt"] = signed.attempt.into();
        report["serverArtifactId"] = signed.application.into();
        report["opsArtifactId"] = signed.ops.into();
        let options = Options {
            purpose: request.purpose,
            run: signed.run,
            attempt: signed.attempt,
            artifact: signed.ops,
            scenario: "deployment-restore".into(),
            output: request.output.clone(),
            revision: request.revision.clone(),
        };
        checkpoint(report, "authentication");
        owners.ops = Some(auth::ops(runner, control, &options, report).await?);
        report["authenticatedOpsDistribution"] = true.into();
        checkpoint(report, "server-authentication");
        let server =
            auth::server_explicit(runner, control, &options, signed.application, false, report)
                .await?;
        report["imageArchiveSha256"] = server.pin("image.tar")?.sha256.clone().into();
        report["authenticatedServerDistribution"] = true.into();
        owners.server = Some(server);
        execute::timing(report, "authentication", control)?;
        control.advance()?;
        selected = Some(options);
    }
    let run = execute::identity(control)?;
    report["runId"] = run.clone().into();
    checkpoint(report, "boot");
    let work = runner.work.clone();
    let created = Vm::create(&mut owners.vm, runner, control, &work, &run).await;
    report["guestExecuted"] = owners.vm.as_ref().is_some_and(Vm::launched).into();
    created?;
    let vm = owners.vm.as_mut().ok_or(Error::Refused)?;
    runner.set_guest_health(vm.health(), vm.failure());
    execute::timing(report, "boot", control)?;
    let Some(options) = selected else {
        report["failure"] = Value::Null;
        report["checkpoint"] = "complete".into();
        return Ok(());
    };
    checkpoint(report, "inventory");
    let server = owners.server.as_ref().ok_or(Error::Refused)?;
    let ops = owners.ops.as_ref().ok_or(Error::Refused)?;
    let value = inventory_value(
        &runner.work,
        vm.port(),
        &run,
        server.path(),
        &request.revision,
    )?;
    let inventory = runner.work.join("inventory.json");
    files::write(&inventory, &auth::encoded(&value)?, control)?;
    let held = Original::open(&inventory, 16384, control)?;
    runner
        .sources
        .insert("generated/inventory.json".into(), held);
    let controller = PathBuf::from(runner.tool("controller", control)?);
    let gh = PathBuf::from(runner.tool("gh", control)?);
    let delivery = Delivery {
        revision: &request.revision,
        run: options.run,
        artifact: options.artifact,
        selection: &ops.selection,
        controller: &controller,
        gh: &gh,
        server: server.path(),
        binary: ops.pin("simplestchat-ops")?,
        purpose: options.purpose,
    };
    let values = runner.output.join("ops-install-vars.json");
    files::write(&values, &auth::encoded(&delivery.variables())?, control)?;
    let held = Original::open(&values, 16384, control)?;
    runner
        .sources
        .insert("generated/ops-install-vars.json".into(), held);
    let delivery = execute::delivery_command(&runner.root, &inventory, &values);
    let root = runner.root.clone();
    let mut steps = Live {
        runner,
        vm: Some(vm),
        control,
        started,
    };
    exercise(&mut steps, report, notes, &root, &inventory, delivery, &run).await?;
    ops.check(control)?;
    server.check(control)?;
    execute::timing(report, "exercise", control)?;
    report["failure"] = Value::Null;
    report["checkpoint"] = "complete".into();
    Ok(())
}

/// The summary's command list: every command's role and exit status, with a
/// playbook's recap counts and a guest's failure labels where they were kept.
fn checks(runner: Option<&Runner>, notes: &Notes) -> Value {
    let calls = runner.map(Runner::calls).unwrap_or_default();
    let rows = calls.iter().enumerate().map(|(index, (name, status))| {
        let mut row = json!({"name":name,"exitStatus":status});
        if let Some(recap) = notes.recaps.get(&index) {
            row["ansibleRecap"] = recap.clone();
        }
        if let Some(failure) = notes.failures.get(&index) {
            row["guestFailure"] = failure.clone();
        }
        row
    });
    Value::Array(rows.collect())
}

/// How a run ended, as its summary concludes it.
#[derive(Clone, Copy, Debug)]
pub(super) struct Ending {
    /// Whether the owned cleanup finished without an error of its own.
    pub cleaned: bool,
    /// Whether any cleanup step was recorded as failed.
    pub cleanup_failed: bool,
    pub first: Option<Error>,
    pub status: u8,
    pub boot: bool,
}

/// Conclude `report`: cleanup passed only when it finished and no step of it
/// failed; the run passed only with no recorded failure and status 0, and
/// validated a full deployment only when it passed outside the boot scope.
pub(super) fn conclude(report: &mut Value, ending: Ending) {
    report["cleanupPassed"] = (ending.cleaned && !ending.cleanup_failed).into();
    if let Some(error) = ending.first {
        report["failureKind"] = error.kind().into();
        if report["failure"].is_null() {
            report["failure"] = "finalization".into();
        }
    }
    let passed = ending.first.is_none() && ending.status == 0;
    report["passed"] = passed.into();
    report["fullDeploymentValidated"] = (passed && !ending.boot).into();
}

/// Run the job for `request`; the summary is complete whatever failed, and
/// passes only when every step, every check and the owned cleanup passed.
pub(super) async fn run(request: &Request, control: &mut Control) -> Value {
    let boot = request.purpose == Purpose::Boot;
    let mut report = json!({"schemaVersion":1,"scope":request.purpose.summary_scope(),
        "bootOnly":boot,"passed":false,"cleanupPassed":false,"guestExecuted":false,
        "fullDeploymentValidated":false,"failure":"guard","checkpoint":"guard",
        "failureKind":null,"runId":null,"revision":request.revision,"elapsedSeconds":{},
        "controllerImplementation":"rust-outer-controller","checks":[]});
    if !boot {
        report["phases"] = json!({});
    }
    let mut owners = Owners::new();
    let mut notes = Notes::default();
    if let Err(error) = body(request, control, &mut owners, &mut report, &mut notes).await {
        control.record(error);
    }
    if let Err(error) = execute::timing(&mut report, "body-end", control) {
        control.record(error);
    }
    control.settle();
    let cleanup = owners.finish(control, &mut report).await;
    let cleaned = cleanup.is_ok();
    if let Err(error) = cleanup {
        control.record(error);
    }
    report["checks"] = checks(owners.runner.as_ref(), &notes);
    let ending = Ending {
        cleaned,
        cleanup_failed: control.cleanup_failed,
        first: control.first,
        status: control.status,
        boot,
    };
    conclude(&mut report, ending);
    report
}
