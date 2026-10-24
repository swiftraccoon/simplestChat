//! One application replacement, original held lock and one known recovery attempt.

#[cfg(test)]
#[path = "bulk_effect_tests.rs"]
mod bulk_effect_tests;
#[cfg(test)]
#[path = "effect_tests.rs"]
mod effect_tests;
#[path = "replacement.rs"]
mod replacement;
#[path = "retention.rs"]
mod retention;
#[path = "steps.rs"]
mod steps;

use super::super::{
    control::{self, Cause, Owner, Result, Window},
    files::Dir,
    stage,
};
use super::{
    Access, Bulk, CommandOutput, Executor, Invocation, Options, Outcome, Role, budget,
    policy::{self, *},
    records::{self, Record, Records},
};
use crate::{
    json::Json,
    release::{assets, manifest::Manifest},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    time::{Duration, Instant},
};

const CONTAINER: &str = concat!(
    "{\"id\":{{json .Id}},\"image\":{{json .Image}},\"state\":{{json .State}},",
    "\"restarts\":{{.RestartCount}},",
    "\"configHash\":{{json (index .Config.Labels \"com.docker.compose.config-hash\")}}}"
);
const LEDGER: &str = concat!(
    "SELECT version, success, encode(checksum, 'hex') ",
    "FROM public._sqlx_migrations ORDER BY version"
);
const SELECTION: [&str; 3] = ["compose.public.yml", "app.env", "images.json"];
const GIB: u64 = 1024 * 1024 * 1024;

fn encoded(value: &impl serde::Serialize) -> Result<Vec<u8>> {
    let mut data = serde_json::to_vec_pretty(value).map_err(|_| Cause::Io)?;
    data.push(b'\n');
    Ok(data)
}
fn timestamp() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}
fn report_bytes(
    fields: &Value,
    quiet: Option<&Json>,
    replacement: Option<&Json>,
) -> Result<Vec<u8>> {
    struct Report<'a> {
        fields: &'a Value,
        quiet: Option<&'a Json>,
        replacement: Option<&'a Json>,
    }
    impl serde::Serialize for Report<'_> {
        fn serialize<S: serde::Serializer>(
            &self,
            serializer: S,
        ) -> std::result::Result<S::Ok, S::Error> {
            use serde::ser::SerializeMap;
            let fields = self
                .fields
                .as_object()
                .ok_or_else(|| serde::ser::Error::custom("fixed deployment report object"))?;
            let mut output = serializer.serialize_map(Some(fields.len()))?;
            for (name, value) in fields {
                match name.as_str() {
                    "roomsActiveAtQuietCheck" => output.serialize_entry(name, &self.quiet)?,
                    "roomsActiveBeforeStop" => output.serialize_entry(name, &self.replacement)?,
                    _ => output.serialize_entry(name, value)?,
                }
            }
            output.end()
        }
    }
    encoded(&Report {
        fields,
        quiet,
        replacement,
    })
}
fn raw_text(data: &[u8]) -> policy::Result<&str> {
    std::str::from_utf8(data).map_err(|_| "UnicodeDecodeError")
}
fn argv(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}
fn stage_access(access: &Access) -> stage::Access {
    let value = stage::Access::for_host(
        access.root.clone(),
        access.config.clone(),
        access.work.clone(),
        access.docker.clone(),
        access.socket.clone(),
        access.uid,
    );
    #[cfg(test)]
    let value = {
        let mut value = value;
        value.free_bytes = access.free_bytes;
        value
    };
    value
}
struct StageExecutor<'a, E>(&'a mut E);
impl<E: Executor> stage::Executor for StageExecutor<'_, E> {
    async fn run(&mut self, owner: &mut Owner, request: stage::Invocation) -> stage::CommandOutput {
        let role = match request.role {
            stage::Role::Load => Role::Load,
            stage::Role::Inspect => Role::Inspect,
            stage::Role::Create => Role::Create,
            stage::Role::Start => Role::Start,
            stage::Role::Wait => Role::Wait,
            stage::Role::Logs => Role::Logs,
            stage::Role::Remove => Role::Remove,
        };
        let output = self
            .0
            .run(
                owner,
                Invocation {
                    role,
                    program: request.program,
                    arguments: request.arguments,
                    body_end: request.body_end,
                    settlement_end: request.settlement_end,
                    cleanup: request.cleanup,
                    input: None,
                    output: None,
                },
            )
            .await;
        stage::CommandOutput {
            stdout: output.stdout,
            stderr: output.stderr,
            status: output.status,
            error: output.error,
            settled: output.settled,
            stdout_complete: output.stdout_complete,
        }
    }
}

#[derive(Clone, Copy)]
struct CommandIo {
    input: Option<usize>,
    output: Option<(usize, u64)>,
    recovery: bool,
}
impl CommandIo {
    fn ordinary(recovery: bool) -> Self {
        Self {
            input: None,
            output: None,
            recovery,
        }
    }
    fn bulk(index: usize, maximum: u64) -> Self {
        Self {
            input: None,
            output: Some((index, maximum)),
            recovery: false,
        }
    }
    fn inherited(index: usize) -> Self {
        Self {
            input: Some(index),
            output: None,
            recovery: false,
        }
    }
}
struct PreparedSelection {
    old: Json,
    database: Json,
    proxy: Json,
    origin: String,
    compose: Vec<u8>,
    environment: Vec<u8>,
}

struct State {
    stage: stage::State,
    config: Option<Dir>,
    records: Records,
    inputs: BTreeMap<String, usize>,
    curl_directory: Option<Dir>,
    assets: Option<assets::host::HostStore>,
    commands: usize,
    failure: Option<String>,
    candidate_end: Option<Instant>,
    asset_id: Option<String>,
    asset_remove_attempted: bool,
    asset_remove_admitted: bool,
    asset_remove_dispatched: bool,
    asset_remove_effected: bool,
    pending_logs: Vec<(usize, Vec<u8>)>,
    quiet_rooms: Option<Json>,
    replacement_rooms: Option<Json>,
    #[cfg(test)]
    fault: Option<super::files::Fault>,
}
impl State {
    fn new(access: &Access) -> Self {
        #[cfg(not(test))]
        let _ = access;
        Self {
            stage: stage::State::for_host(),
            config: None,
            records: Records::default(),
            inputs: BTreeMap::new(),
            curl_directory: None,
            assets: None,
            commands: 0,
            failure: None,
            candidate_end: None,
            asset_id: None,
            asset_remove_attempted: false,
            asset_remove_admitted: false,
            asset_remove_dispatched: false,
            asset_remove_effected: false,
            pending_logs: Vec::new(),
            quiet_rooms: None,
            replacement_rooms: None,
            #[cfg(test)]
            fault: access.fault,
        }
    }
    fn policy<T, E: Into<String>>(
        &mut self,
        owner: &mut Owner,
        result: std::result::Result<T, E>,
    ) -> Result<T> {
        result.map_err(|message| {
            if self.failure.is_none() {
                self.failure = Some(message.into());
            }
            owner.record(Cause::Policy);
            Cause::Policy
        })
    }
    fn refused<T>(&mut self, owner: &mut Owner, message: &'static str) -> Result<T> {
        self.policy(owner, Err(message))
    }
    fn command_failure(&mut self, owner: &mut Owner) {
        owner.record(Cause::Command);
        if owner.error() != Some(Cause::Command) {
            self.latch_failure(owner);
            return;
        }
        if self.failure.is_none() {
            self.failure = Some(format!(
                "Command {:03} failed; inspect private output",
                self.commands
            ));
        }
    }
    fn latch_failure(&mut self, owner: &Owner) {
        if self.failure.is_none() {
            self.failure = self
                .stage
                .failure()
                .map(str::to_owned)
                .or_else(|| owner.error().map(|cause| cause_name(cause).into()));
        }
    }
    fn config(&self) -> &Dir {
        self.config.as_ref().expect("configuration admitted")
    }
    fn attempt(&self) -> Dir {
        self.stage.attempt().expect("attempt admitted")
    }
    fn body_end(&self, owner: &Owner, recovery: bool) -> Instant {
        if recovery {
            owner.body_end()
        } else {
            self.candidate_end.unwrap_or(owner.body_end())
        }
    }
    fn originals(&self, access: &Access, owner: &mut Owner, recovery: bool) -> Result<()> {
        records::check(owner, recovery)?;
        if recovery {
            self.stage.cleanup_witness(&stage_access(access), owner)?;
        } else {
            self.stage.originals(owner)?;
        }
        self.records.verify(owner, recovery)?;
        records::check(owner, recovery)
    }
    fn input(
        &mut self,
        name: &str,
        maximum: u64,
        access: &Access,
        owner: &mut Owner,
    ) -> Result<usize> {
        if let Some(at) = self.inputs.get(name) {
            return Ok(*at);
        }
        let at = if name == "Caddyfile" {
            self.records
                .caddy(self.config().clone(), access.uid, owner)?
        } else {
            self.records
                .open(self.config().clone(), name, access.uid, maximum, owner)?
        };
        self.inputs.insert(name.into(), at);
        Ok(at)
    }
    fn bytes(&self, name: &str, maximum: usize, owner: &mut Owner) -> Result<Vec<u8>> {
        self.records.files[*self.inputs.get(name).ok_or(Cause::Input)?].bytes(maximum, owner, false)
    }
    fn free(&self, access: &Access, owner: &mut Owner) -> Result<u64> {
        #[cfg(test)]
        if let Some(value) = access.free_bytes {
            return Ok(value);
        }
        #[cfg(not(test))]
        let _ = access;
        self.attempt().free_bytes(owner)
    }
    #[cfg(test)]
    fn fault(&mut self, selected: super::files::Fault, owner: &mut Owner) -> Result<()> {
        if self.fault == Some(selected) {
            self.fault = None;
            return control::failed(owner, Cause::Io);
        }
        Ok(())
    }
    async fn command(
        &mut self,
        access: &Access,
        role: Role,
        arguments: Vec<OsString>,
        request: CommandIo,
        owner: &mut Owner,
        executor: &mut impl Executor,
    ) -> Result<CommandOutput> {
        if role == Role::AssetRemove && !request.recovery {
            owner.begin_deploy_recovery()?;
            let result = self
                .command_inner(access, role, arguments, request, owner, executor)
                .await;
            owner.end_deploy_recovery();
            result
        } else {
            self.command_inner(access, role, arguments, request, owner, executor)
                .await
        }
    }
    fn retirement_witness(&self, access: &Access, owner: &mut Owner) -> Result<()> {
        owner.final_work()?;
        self.stage.cleanup_witness(&stage_access(access), owner)?;
        let lease = self.stage.borrowed_lease()?;
        lease.check().map_err(|error| {
            let cause = match error {
                crate::release::workload::Failure::Io => Cause::Io,
                crate::release::workload::Failure::Close => Cause::Close,
                crate::release::workload::Failure::Changed => Cause::Changed,
                crate::release::workload::Failure::Busy => Cause::Policy,
            };
            owner.record(cause);
            cause
        })?;
        owner.final_work()
    }
    async fn command_inner(
        &mut self,
        access: &Access,
        role: Role,
        arguments: Vec<OsString>,
        request: CommandIo,
        owner: &mut Owner,
        executor: &mut impl Executor,
    ) -> Result<CommandOutput> {
        let recovery = request.recovery;
        let io = (request.input, request.output);
        let retirement = role == Role::AssetRemove;
        if retirement {
            let exact = self
                .asset_id
                .as_ref()
                .is_some_and(|id| arguments == [OsString::from("rm"), OsString::from(id)]);
            if !exact {
                return control::failed(owner, Cause::Input);
            }
            self.retirement_witness(access, owner)?;
        } else {
            self.originals(access, owner, recovery)?;
        }
        let now = owner.now();
        let end = self.body_end(owner, recovery);
        let body_end = now
            .checked_add(Duration::from_secs(role.seconds()))
            .ok_or(Cause::Deadline)?
            .min(end.checked_sub(budget::SETTLE).ok_or(Cause::Deadline)?);
        if body_end <= now {
            return control::failed(owner, Cause::Deadline);
        }
        let settlement_end = body_end
            .checked_add(budget::SETTLE)
            .ok_or(Cause::Deadline)?
            .min(end);
        self.commands += 1;
        if retirement {
            self.asset_remove_admitted = true;
        }
        let attempt = self.attempt();
        let stdout = if io.1.is_none() {
            Some(self.records.prepare_log(
                &attempt,
                &format!("{:03}.stdout", self.commands),
                owner,
                recovery,
            )?)
        } else {
            None
        };
        let stderr = match self.records.prepare_log(
            &attempt,
            &format!("{:03}.stderr", self.commands),
            owner,
            recovery,
        ) {
            Ok(value) => value,
            Err(cause) => {
                if let Some(at) = stdout {
                    if recovery {
                        let _ = self.records.finish_log(at, &[], owner, true);
                    } else {
                        self.pending_logs.push((at, Vec::new()));
                    }
                }
                return Err(cause);
            }
        };
        let curl = matches!(role, Role::Ready | Role::Metrics);
        let arguments = if curl {
            arguments
        } else {
            let mut fixed = argv(&["--host", "unix:///var/run/docker.sock"]);
            fixed.extend(arguments);
            fixed
        };
        if retirement {
            self.asset_remove_dispatched = true;
        }
        let output = executor
            .run(
                owner,
                Invocation {
                    role,
                    program: if curl {
                        access.curl.clone()
                    } else {
                        access.docker.clone()
                    },
                    arguments,
                    body_end,
                    settlement_end,
                    cleanup: recovery,
                    input: io.0.map(|at| self.records.files[at].file()),
                    output: io.1.map(|(at, maximum)| Bulk {
                        file: self.records.files[at].file(),
                        maximum,
                    }),
                },
            )
            .await;
        // Effect observations precede every fallible evidence write and later
        // witness. A completed nonzero is never ownership of a created object.
        if output.status == Some(0) {
            if role == Role::AssetCreate
                && output.stdout_complete
                && let Ok(value) = std::str::from_utf8(&output.stdout)
                && exact_hex(stage::python_strip(value), 64)
            {
                self.asset_id = Some(stage::python_strip(value).into());
            } else if role == Role::AssetRemove {
                self.asset_id = None;
                self.asset_remove_effected = true;
            }
        }
        if let Some(cause) = output.error {
            owner.record(cause);
        }
        if !output.settled {
            owner.record(Cause::Close);
        }
        if output.status != Some(0) && !curl {
            self.command_failure(owner);
        }
        if let Some((at, _)) = io.1 {
            let failed_normal = !recovery && owner.error().is_some();
            let grown = if failed_normal {
                match owner.begin_deploy_recovery() {
                    Ok(()) => {
                        let result = self.records.files[at].observe_growth(owner, true);
                        owner.end_deploy_recovery();
                        result
                    }
                    Err(cause) => Err(cause),
                }
            } else {
                self.records.files[at].observe_growth(owner, recovery)
            };
            if grown.is_ok()
                && (self.records.files[at].len() != output.bulk_bytes
                    || self.records.files[at].sha256() != Some(output.bulk_sha256.as_str()))
            {
                owner.record(Cause::Changed);
            }
        }
        if !recovery && let Some(cause) = owner.error() {
            self.latch_failure(owner);
            self.pending_logs.push((stderr, output.stderr));
            if let Some(at) = stdout {
                self.pending_logs.push((at, output.stdout));
            }
            return Err(cause);
        }
        let error_log = self
            .records
            .finish_log(stderr, &output.stderr, owner, recovery);
        let output_log =
            stdout.map(|at| self.records.finish_log(at, &output.stdout, owner, recovery));
        self.latch_failure(owner);
        error_log?;
        if let Some(result) = output_log {
            result?;
        }
        if let Some(cause) = output.error {
            return Err(cause);
        }
        if !output.settled {
            return Err(Cause::Close);
        }
        if retirement {
            self.retirement_witness(access, owner)?;
        } else {
            self.originals(access, owner, recovery)?;
        }
        Ok(output)
    }
    async fn docker(
        &mut self,
        access: &Access,
        role: Role,
        arguments: Vec<OsString>,
        recovery: bool,
        owner: &mut Owner,
        executor: &mut impl Executor,
    ) -> Result<Vec<u8>> {
        let output = self
            .command(
                access,
                role,
                arguments,
                CommandIo::ordinary(recovery),
                owner,
                executor,
            )
            .await?;
        if output.status != Some(0) {
            self.command_failure(owner);
            return Err(Cause::Command);
        }
        Ok(output.stdout)
    }
    fn compose_args(
        &self,
        access: &Access,
        arguments: Vec<OsString>,
        selection: Option<(&str, &str)>,
    ) -> Vec<OsString> {
        let (compose, environment) = selection.map_or_else(
            || {
                (
                    access.config.join("compose.public.yml"),
                    access.config.join("app.env"),
                )
            },
            |(compose, environment)| {
                (
                    self.attempt().path().join(compose),
                    self.attempt().path().join(environment),
                )
            },
        );
        let mut fixed = argv(&[
            "compose",
            "--project-name",
            "simplestchat-public",
            "--project-directory",
        ]);
        fixed.push(access.config.clone().into_os_string());
        fixed.push("--env-file".into());
        fixed.push(environment.into_os_string());
        fixed.push("-f".into());
        fixed.push(compose.into_os_string());
        fixed.extend(arguments);
        fixed
    }
    async fn compose(
        &mut self,
        access: &Access,
        arguments: Vec<OsString>,
        selection: Option<(&str, &str)>,
        recovery: bool,
        owner: &mut Owner,
        executor: &mut impl Executor,
    ) -> Result<Vec<u8>> {
        let role = match arguments.first().and_then(|value| value.to_str()) {
            Some("stop") => Role::Stop,
            Some("up") => Role::Up,
            _ => Role::Compose,
        };
        let arguments = self.compose_args(access, arguments, selection);
        self.docker(access, role, arguments, recovery, owner, executor)
            .await
    }
    async fn container(
        &mut self,
        access: &Access,
        service: &str,
        recovery: bool,
        owner: &mut Owner,
        executor: &mut impl Executor,
    ) -> Result<Json> {
        let output = self
            .compose(
                access,
                argv(&["ps", "--status", "running", "--quiet", service]),
                None,
                recovery,
                owner,
                executor,
            )
            .await?;
        let id = self.policy(owner, raw_text(&output))?;
        let id = stage::python_strip(id);
        if !exact_hex(id, 64) {
            return self.refused(
                owner,
                match service {
                    "simplestchat" => "Exactly one running simplestchat container is required",
                    "postgres" => "Exactly one running postgres container is required",
                    _ => "Exactly one running caddy container is required",
                },
            );
        }
        let data = self
            .docker(
                access,
                Role::Inspect,
                argv(&["inspect", "--format", CONTAINER, id]),
                recovery,
                owner,
                executor,
            )
            .await?;
        let value = self.policy(owner, decode(&data))?;
        let state = self.policy(owner, member(&value, "state"))?;
        let running = self.policy(owner, member(state, "Running"))?;
        let killed = self.policy(owner, member(state, "OOMKilled"))?;
        if !running.truthiness() || killed.truthiness() {
            return self.refused(
                owner,
                match service {
                    "simplestchat" => "simplestchat is not running normally",
                    "postgres" => "postgres is not running normally",
                    _ => "caddy is not running normally",
                },
            );
        }
        Ok(value)
    }
    fn journal(
        &mut self,
        phase: &str,
        finalized: bool,
        owner: &mut Owner,
        recovery: bool,
    ) -> Result<()> {
        self.stage.journal_in(phase, finalized, owner, recovery)
    }
    fn atomic(
        &mut self,
        parent: &Dir,
        name: &str,
        data: &[u8],
        owner: &mut Owner,
        recovery: bool,
    ) -> Result<()> {
        self.records.atomic(parent, name, data, owner, recovery)
    }
}

pub(super) async fn execute(
    options: &Options,
    access: &Access,
    owner: &mut Owner,
    executor: &mut impl Executor,
    _started: Instant,
) -> Outcome {
    let mut state = State::new(access);
    let mut report = json!({"action":"deploy","revision":options.revision,
        "startedAt":timestamp(),"passed":false,"phase":"stage"});
    let operation: Result<()> = async {
        let manifest = state
            .stage
            .setup(&stage_access(access), &options.revision, owner)?;
        state.config = Some(Dir::open(&access.config, owner)?);
        let curl_parent = Dir::open(access.curl.parent().ok_or(Cause::Input)?, owner)?;
        state.curl_directory = Some(curl_parent.clone());
        let curl = Record::tool(
            curl_parent,
            access
                .curl
                .file_name()
                .ok_or(Cause::Input)?
                .to_str()
                .ok_or(Cause::Input)?,
            access.uid,
            owner,
        )?;
        state.records.files.push(curl);
        state
            .stage
            .stage(
                &stage_access(access),
                &manifest,
                owner,
                &mut StageExecutor(executor),
            )
            .await?;
        state.commands = state.stage.command_count();
        report["phase"] = json!("preflight");
        let selection = state
            .preflight(options, access, &manifest, &mut report, owner, executor)
            .await?;
        state
            .replace(options, access, &selection, &mut report, owner, executor)
            .await
    }
    .await;
    if let Err(cause) = operation {
        owner.record(cause);
    }
    state.latch_failure(owner);
    if state.asset_remove_dispatched && !state.asset_remove_admitted
        || state.asset_remove_effected
            && (!state.asset_remove_dispatched || state.asset_id.is_some())
    {
        owner.record(Cause::Changed);
    }
    if let Some(id) = state.asset_id.clone()
        && !state.asset_remove_attempted
    {
        state.asset_remove_attempted = true;
        if owner.begin_deploy_recovery().is_ok() {
            let _ = state
                .docker(
                    access,
                    Role::AssetRemove,
                    argv(&["rm", &id]),
                    true,
                    owner,
                    executor,
                )
                .await;
            owner.end_deploy_recovery();
        }
    }
    state.stage.continue_commands(state.commands);
    state
        .stage
        .settle_known(&stage_access(access), owner, &mut StageExecutor(executor))
        .await;
    // Failed normal-command evidence cannot borrow reserved rollback time.
    // Files remain owned until known recovery/retirement has settled, then
    // their bounded prefixes may use the original local finalization window.
    state
        .pending_logs
        .sort_unstable_by_key(|(index, _)| std::cmp::Reverse(*index));
    for (index, data) in state.pending_logs.drain(..) {
        let _ = state.records.finish_log(index, &data, owner, true);
    }
    if let Some(assets) = state.assets.take()
        && assets.finish(owner).is_err()
    {
        owner.record(Cause::Close);
    }
    // Borrowers have explicitly settled before the sole owner releases its
    // actual flock. No failure here starts another application rollback.
    state.stage.unlock(owner);
    let attempt = state.stage.attempt();
    let mut emit = false;
    if let Some(attempt) = &attempt {
        report["finishedAt"] = json!(timestamp());
        report["passed"] = json!(operation.is_ok() && owner.error().is_none());
        if owner.error().is_some() {
            report["failure"] = json!(
                state
                    .failure
                    .clone()
                    .or_else(|| state.stage.failure().map(str::to_owned))
                    .unwrap_or_else(|| cause_name(owner.error().unwrap()).into())
            );
        }
        let written = report_bytes(
            &report,
            state.quiet_rooms.as_ref(),
            state.replacement_rooms.as_ref(),
        )
        .and_then(|data| {
            #[cfg(test)]
            let fault = match state.fault {
                Some(super::files::Fault::OutcomeWrite) => Some(0),
                Some(super::files::Fault::OutcomeFileSync) => Some(1),
                Some(super::files::Fault::OutcomeDirectorySync) => Some(2),
                _ => None,
            };
            #[cfg(not(test))]
            let fault = None;
            state
                .records
                .atomic_fault(attempt, "outcome.json", &data, owner, true, fault)
        });
        if let Err(cause) = written {
            owner.record(cause);
        } else {
            emit = true;
        }
    }
    state.records.finish(owner);
    if let Some(directory) = state.curl_directory.take() {
        directory.finish(owner);
    }
    if let Some(directory) = state.config.take() {
        directory.finish(owner);
    }
    let evidence = attempt
        .as_ref()
        .and_then(|value| value.path().to_str().map(str::to_owned));
    drop(attempt);
    state.stage.finish(owner);
    let passed = operation.is_ok() && owner.check(Window::Final).is_ok();
    report["passed"] = json!(passed);
    if !passed
        && report.get("failure").is_none()
        && let Some(cause) = owner.error()
    {
        report["failure"] = json!(cause_name(cause));
    }
    if let Some(evidence) = evidence.filter(|_| emit) {
        report["evidence"] = json!(evidence);
        Outcome {
            bytes: report_bytes(
                &report,
                state.quiet_rooms.as_ref(),
                state.replacement_rooms.as_ref(),
            )
            .unwrap_or_default(),
            passed,
        }
    } else {
        Outcome {
            bytes: Vec::new(),
            passed: false,
        }
    }
}

fn cause_name(cause: Cause) -> &'static str {
    match cause {
        Cause::Input => "native_input_refused",
        Cause::Policy => "native_policy_refused",
        Cause::Archive | Cause::Zip => "native_archive_refused",
        Cause::Changed => "native_input_changed",
        Cause::Io => "OSError",
        Cause::Close => "native_close_refused",
        Cause::Deadline => "TimeoutExpired",
        Cause::Interrupt | Cause::Terminate => "Release interrupted",
        Cause::Bounds => "command_output_limit",
        Cause::Command => "ReleaseError",
    }
}

#[cfg(test)]
mod report_tests {
    use super::*;

    #[test]
    fn metrics_counts_keep_python_nd_separators_and_large_integer_report_lexemes() {
        let positive = metrics_integer("+١_٢");
        let negative = metrics_integer("-000_123");
        let large_text = "9".repeat(4300);
        let large = metrics_integer(&large_text);
        let refused = metrics_integer(&(large_text.clone() + "9"));
        let report = json!({"passed":false,"roomsActiveAtQuietCheck":null,
            "roomsActiveBeforeStop":null});
        let bytes = report_bytes(&report, positive.as_ref(), large.as_ref());
        let decoded = bytes.as_ref().ok().and_then(|value| decode(value).ok());
        assert!(matches!(positive, Some(Json::Integer(ref value)) if value == "12"));
        assert!(matches!(negative, Some(Json::Integer(ref value)) if value == "-123"));
        assert!(refused.is_none());
        assert!(
            decoded
                .as_ref()
                .and_then(|value| value.get("roomsActiveBeforeStop"))
                .is_some_and(|value| matches!(value, Json::Integer(value) if value == &large_text))
        );
    }
}
