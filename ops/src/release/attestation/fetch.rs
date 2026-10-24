//! Complete explicit release selection, local authentication and SSH receiver transaction.
//! A private live verification result, never a saved JSON receipt, supplies the envelope.

mod options;
#[cfg(test)]
mod oracle_tests;
mod process;
#[cfg(test)]
mod tests;

use super::{
    acquire,
    control::{self, Cause, Owner, Result, Window},
    files::{self, Dir, Retained},
    verify::{
        self,
        tool::{Environment, Program, Tool},
    },
};
use crate::release::trust;
use options::Options;
use serde::Serialize;
use std::{
    ffi::{OsStr, OsString},
    io::{self, Write},
    os::unix::fs::MetadataExt,
    path::Path,
    process::ExitCode,
    time::{Duration, Instant},
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Report {
    schema_version: u8,
    operation: &'static str,
    passed: bool,
    phase: &'static str,
    remote_settled: bool,
    remote: Option<serde_json::Value>,
    failure_class: Option<&'static str>,
    started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    evidence: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    elapsed_seconds: Option<f64>,
}
impl Report {
    fn new() -> Self {
        Self {
            schema_version: 1,
            operation: "fetch_release_controller",
            passed: false,
            phase: "options",
            remote_settled: false,
            remote: None,
            failure_class: None,
            started_at: chrono::Utc::now().to_rfc3339(),
            evidence: None,
            elapsed_seconds: None,
        }
    }
    fn encode(&self) -> Result<Vec<u8>> {
        let mut bytes = serde_json::to_vec(self).map_err(|_| Cause::Policy)?;
        if bytes.len() >= 131072 {
            return Err(Cause::Bounds);
        }
        bytes.push(b'\n');
        Ok(bytes)
    }
}
struct State {
    local: Option<acquire::State>,
    ssh: Option<Tool>,
    key_parent: Option<Dir>,
    key: Option<Retained>,
    key_hash: Option<String>,
    parent: Option<Dir>,
    attempt: Option<Dir>,
    artifact: Option<Retained>,
    receiver: Option<process::Receiver>,
    environment: Environment,
}
fn ssh_environment(source: &Environment) -> Environment {
    let mut result: Environment = source
        .iter()
        .filter(|(key, _)| {
            ["PATH", "HOME", "USER", "LOGNAME", "SSH_AUTH_SOCK"]
                .iter()
                .any(|name| *key == OsStr::new(name))
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    result.insert("LC_ALL".into(), "C".into());
    result
}
impl State {
    fn new(options: &Options) -> Self {
        let environment: Environment = {
            #[cfg(test)]
            {
                options
                    .environment
                    .clone()
                    .unwrap_or_else(|| std::env::vars_os().collect())
            }
            #[cfg(not(test))]
            {
                std::env::vars_os().collect()
            }
        };
        Self {
            local: Some(acquire::State::with_source(
                &options.local,
                environment.clone(),
            )),
            ssh: None,
            key_parent: None,
            key: None,
            key_hash: None,
            parent: None,
            attempt: None,
            artifact: None,
            receiver: None,
            environment,
        }
    }
    fn key(&mut self, options: &Options, owner: &mut Owner) -> Result<()> {
        let parent = options.identity.parent().ok_or(Cause::Input)?;
        let name = options.identity.file_name().ok_or(Cause::Input)?;
        self.key_parent = Some(Dir::open(parent, owner)?);
        self.key = Some(Retained::open(
            self.key_parent.as_ref().unwrap().clone(),
            name,
            owner,
        )?);
        let key = self.key.as_ref().unwrap();
        let stamp = control::io(owner, key.file().metadata())?;
        if stamp.uid() != rustix::process::getuid().as_raw()
            || !matches!(stamp.mode() & 0o7777, 0o400 | 0o600)
        {
            return control::failed(owner, Cause::Input);
        }
        // OpenSSH still opens its own pathname. This retained witness detects
        // observed replacement; it is neither a descriptor handoff nor a lock.
        self.key_hash = Some(key.hash(owner)?);
        Ok(())
    }
    fn outputs(&self, owner: &mut Owner) -> Result<()> {
        if let Some(key) = &self.key {
            key.check(owner, Window::Body)?;
            if Some(key.hash(owner)?) != self.key_hash {
                return control::failed(owner, Cause::Changed);
            }
        }
        for dir in [&self.parent, &self.attempt].into_iter().flatten() {
            dir.check(owner, Window::Body)?;
        }
        if let Some(artifact) = &self.artifact {
            artifact.check(owner, Window::Body)?;
        }
        if let Some(ssh) = &self.ssh {
            ssh.bytes(owner)?;
        }
        self.local.as_ref().unwrap().original_outputs(owner)
    }
    fn parent(&mut self, path: &Path, owner: &mut Owner) -> Result<()> {
        self.parent = Some(match path.symlink_metadata() {
            Ok(_) => Dir::open(path, owner)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                Dir::open(path.parent().ok_or(Cause::Input)?, owner)?
            }
            Err(error) => return control::io(owner, Err(error)),
        });
        Ok(())
    }
    fn directories(&mut self, path: &Path, owner: &mut Owner) -> Result<()> {
        let parent = self
            .parent
            .take()
            .expect("admitted output parent or ancestor");
        self.parent = Some(if parent.path() == path {
            parent
        } else {
            let made = parent.create_child(path.file_name().ok_or(Cause::Input)?, owner);
            parent.finish(owner);
            made?
        });
        let mut random = [0; 16];
        owner.check(Window::Body)?;
        getrandom::fill(&mut random).map_err(|_| Cause::Io)?;
        owner.check(Window::Body)?;
        let name = format!("release-fetch.{}", files::hex(&random));
        self.attempt = Some(
            self.parent
                .as_ref()
                .unwrap()
                .create_child(OsStr::new(&name), owner)?,
        );
        Ok(())
    }
    async fn perform(
        &mut self,
        options: &Options,
        report: &mut Report,
        owner: &mut Owner,
    ) -> Result<()> {
        self.directories(&options.local.output_parent, owner)?;
        report.evidence = Some(
            self.attempt
                .as_ref()
                .unwrap()
                .path()
                .to_str()
                .ok_or(Cause::Input)?
                .to_owned(),
        );
        report.phase = "github_metadata";
        let metadata = self
            .local
            .as_mut()
            .unwrap()
            .metadata(&options.local, owner)
            .await?;
        report.phase = "attestation";
        let verified = self
            .local
            .as_mut()
            .unwrap()
            .verify_selected(&options.local, metadata, owner, self.attempt.as_ref())
            .await?;
        self.artifact = Some(Retained::create(
            self.attempt.as_ref().unwrap().clone(),
            "artifact.json",
            owner,
        )?);
        self.artifact
            .as_mut()
            .unwrap()
            .write(verified.envelope(), owner)?;
        let envelope = trust::Envelope::decode(verified.envelope()).map_err(|_| Cause::Policy)?;
        let frame = process::frame(verified.envelope())?;
        self.outputs(owner)?;
        report.phase = "receiver_preflight";
        let protocol_end = owner
            .body_end()
            .checked_sub(process::TAIL)
            .ok_or(Cause::Deadline)?;
        if owner.now() >= protocol_end {
            return control::failed(owner, Cause::Deadline);
        }
        let environment = ssh_environment(&self.environment);
        self.ssh = Some(Tool::resolve_program(&environment, Program::Ssh, owner)?);
        self.outputs(owner)?;
        self.receiver = Some(
            process::Receiver::start(
                process::Invocation {
                    program: self.ssh.as_ref().unwrap().path().into(),
                    arguments: options.ssh_arguments(),
                    environment,
                    protocol_end,
                },
                owner,
            )
            .await?,
        );
        owner.begin_receiver_protocol(protocol_end)?;
        #[cfg(test)]
        if PROTOCOL_HASH_FAULT.get() == Some(HashFault::AfterAdmission) {
            PROTOCOL_HASH_FAULT.set(None);
            files::expire_hash_at(protocol_end);
        }
        self.receiver.as_mut().unwrap().send(&frame, owner).await?;
        self.outputs(owner)?;
        let first = self.receiver.as_mut().unwrap().receive(45, owner).await?;
        self.outputs(owner)?;
        let ready = crate::json::decode_limited(&first, 65536).map_err(|_| Cause::Policy)?;
        let exact_ready = ready.object().is_some_and(|value| value.len() == 2)
            && ready
                .get("schemaVersion")
                .and_then(crate::json::Json::integer)
                == Some(1)
            && ready.get("status").and_then(crate::json::Json::string) == Some("ready");
        let final_bytes = if exact_ready {
            report.phase = "download_link";
            // A fresh API grant is requested only after this actual Ready. No
            // grant from local download, cache or saved JSON is reused.
            #[cfg(test)]
            if PROTOCOL_HASH_FAULT.get() == Some(HashFault::AfterReady) {
                PROTOCOL_HASH_FAULT.set(None);
                files::expire_hash_at(protocol_end);
            }
            let grant = self
                .local
                .as_mut()
                .unwrap()
                .new_grant(&options.local.selected, owner, protocol_end)
                .await?;
            self.outputs(owner)?;
            let bytes = serde_json::to_vec(&serde_json::json!({"url":grant.into_url()}))
                .map_err(|_| Cause::Policy)?;
            let frame = process::frame(&bytes)?;
            self.receiver.as_mut().unwrap().send(&frame, owner).await?;
            self.outputs(owner)?;
            report.phase = "remote_fetch";
            self.receiver.as_mut().unwrap().receive(315, owner).await?
        } else {
            first
        };
        self.outputs(owner)?;
        let receipt = trust::ReceiverReceipt::decode(&final_bytes, envelope.value())
            .map_err(|_| Cause::Policy)?;
        let projection = serde_json::to_value(receipt.value()).map_err(|_| Cause::Policy)?;
        let settled = projection
            .get("settled")
            .and_then(serde_json::Value::as_bool)
            .ok_or(Cause::Policy)?;
        report.remote = Some(projection);
        owner.end_receiver_protocol();
        let exit = self.receiver.as_mut().unwrap().finish(owner).await;
        // Receipt data is recorded before transport completion. An exception in
        // finish cannot promote its settled claim into current transport proof.
        owner.check(Window::Body)?;
        report.remote_settled = settled;
        if exit != Some(0) || !receipt.value().completed() || !settled {
            report.failure_class = Some("remote_fetch_failed");
            return control::failed(owner, Cause::Command);
        }
        self.outputs(owner)?;
        report.phase = "complete";
        report.passed = true;
        Ok(())
    }
    async fn finish(mut self, report: &mut Report, started: Instant, owner: &mut Owner) -> Vec<u8> {
        // A failed operation may still own a live SSH child, partial evidence,
        // two pipes and the source tools. Attempt every admitted finalizer.
        owner.end_receiver_protocol();
        if let Some(mut receiver) = self.receiver.take() {
            if !receiver.observation.finish_attempted {
                receiver.finish(owner).await;
            }
            if !receiver.observation.group_retired
                || !receiver.observation.reaped
                || !receiver.observation.input_closed
                || !receiver.observation.output_closed
            {
                report.passed = false;
                report.failure_class = Some("controller_cleanup_uncertain");
            }
            #[cfg(test)]
            LAST_RECEIVER.with(|last| *last.borrow_mut() = Some(receiver.observation.clone()));
        }
        if let Some(local) = self.local.take() {
            local.finish(owner);
        }
        if let Some(ssh) = self.ssh.take() {
            ssh.finish(owner);
        }
        if let Some(artifact) = self.artifact.take() {
            artifact.finish(owner);
        }
        if let Some(key) = self.key.take() {
            key.finish(owner);
        }
        if let Some(parent) = self.key_parent.take() {
            parent.finish(owner);
        }
        if owner.error().is_some() {
            report.passed = false;
            if report.failure_class.is_none() {
                report.failure_class = Some("controller_cleanup_uncertain");
            }
        }
        report.elapsed_seconds = Some(
            (owner.now().saturating_duration_since(started).as_secs_f64() * 1000.0).round()
                / 1000.0,
        );
        let mut bytes = report.encode().unwrap_or_else(|_| failure_bytes());
        let mut write_error = false;
        if let Some(attempt) = &self.attempt {
            write_error |= attempt.final_journal(&bytes, owner).is_err();
            write_error |= attempt.final_sync(owner).is_err();
        }
        if let Some(parent) = &self.parent {
            write_error |= parent.final_sync(owner).is_err();
        }
        if write_error {
            report.passed = false;
            report.failure_class = Some("controller_receipt_write_failed");
        }
        if let Some(attempt) = self.attempt.take() {
            attempt.finish(owner);
        }
        if let Some(parent) = self.parent.take() {
            parent.finish(owner);
        }
        if owner.error().is_some() {
            report.passed = false;
        }
        if !report.passed && report.failure_class.is_none() {
            report.failure_class = Some("controller_operation_failed");
        }
        // The retained journal describes the pre-close observation, as in the
        // reference. The final stdout/exit includes later close uncertainty.
        bytes = report.encode().unwrap_or_else(|_| failure_bytes());
        bytes
    }
}
fn invalid_options() -> Vec<u8> {
    b"{\"passed\":false,\"phase\":\"options\",\"failureClass\":\"invalid_options\"}\n".to_vec()
}
fn failure_bytes() -> Vec<u8> {
    b"{\"passed\":false,\"failureClass\":\"controller_operation_failed\"}\n".to_vec()
}
async fn execute(options: &Options, started: Instant, owner: &mut Owner) -> Vec<u8> {
    let mut report = Report::new();
    let mut state = State::new(options);
    let admitted = state
        .key(options, owner)
        .and_then(|()| state.parent(&options.local.output_parent, owner));
    if let Err(cause) = admitted {
        owner.record(cause);
        if matches!(cause, Cause::Deadline | Cause::Interrupt | Cause::Terminate) {
            report.failure_class = Some(match cause {
                Cause::Deadline => "deadline_exceeded",
                _ => "interrupted",
            });
            return state.finish(&mut report, started, owner).await;
        }
        report.failure_class = Some("invalid_options");
        let _ = state.finish(&mut report, started, owner).await;
        return invalid_options();
    }
    if let Err(cause) = state.perform(options, &mut report, owner).await {
        owner.record(cause);
        report.passed = false;
        if report.failure_class.is_none() {
            report.failure_class = Some(match cause {
                Cause::Deadline => "deadline_exceeded",
                Cause::Interrupt | Cause::Terminate => "interrupted",
                _ => "controller_operation_failed",
            });
        }
    }
    state.finish(&mut report, started, owner).await
}
#[cfg(test)]
#[derive(Clone, Copy, Eq, PartialEq)]
enum HashFault {
    AfterAdmission,
    AfterReady,
}
#[cfg(test)]
thread_local! {
    static PROTOCOL_HASH_FAULT: std::cell::Cell<Option<HashFault>> = const {
        std::cell::Cell::new(None)
    };
    static LAST_RECEIVER: std::cell::RefCell<Option<process::Observation>> = const {
        std::cell::RefCell::new(None)
    };
    static COMPLETED: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}
fn lifecycle(
    started: Instant,
    whole: Duration,
    operation: impl FnOnce() -> (bool, Option<Owner>),
) -> ExitCode {
    let Some(end) = started.checked_add(whole) else {
        return ExitCode::from(2);
    };
    let previous = nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o077));
    let (passed, owner) = operation();
    let _ = nix::sys::stat::umask(previous);
    let finished = owner.is_some_and(|owner| owner.finish().is_ok());
    let now = Instant::now();
    #[cfg(test)]
    let now = COMPLETED.get().unwrap_or(now);
    if passed && finished && now < end {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
/// Verify explicit server bytes, then run the prepared SSH receiver transaction.
///
/// Requires positive explicit whole N and finalization M, with 0 < M < N.
/// Fresh GitHub metadata and four maintained verifiers run locally, including
/// cached candidates. Only an actual receiver Ready permits a new download
/// grant. SSH receives no GitHub token. Current exit0 and the complete report
/// are both required; a late failure invalidates earlier emitted bytes.
///
/// This dedicated Linux/macOS invocation uses trusted OpenSSH configuration and
/// known hosts. The unchanged Python receiver must already be prepared; this
/// does not install or authenticate a target Ops executable or retry unknown
/// remote outcomes. Blocked OS I/O/close/reap remains outside a hard wall bound.
///
/// # Panics
///
/// Valid operations must run outside an entered Tokio runtime; constructing or
/// dropping the owned runtime from another async runtime can panic. Help and
/// license routes return before runtime/environment/file/network effects.
///
/// ```
/// use simplestchat_ops::release::fetch;
/// use std::{ffi::OsString, process::ExitCode};
/// assert_eq!(fetch::run(&[OsString::from("--help")]), ExitCode::SUCCESS);
/// ```
pub fn run(arguments: &[OsString]) -> ExitCode {
    let started = Instant::now();
    if arguments.len() == 1 && arguments[0] == "--licenses" {
        return if acquire::dependency_notices().is_ok() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }
    let options = match Options::parse(arguments) {
        Ok(None) => {
            return if super::prepare::stdout(format!("{}\n", options::USAGE).as_bytes()).is_ok() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            };
        }
        Ok(Some(options)) => options,
        Err(options::ParseError::Invalid) => {
            let _ = super::prepare::stdout(&invalid_options());
            return ExitCode::FAILURE;
        }
        Err(options::ParseError::Usage) => {
            let _ = writeln!(io::stderr().lock(), "{}", options::USAGE);
            return ExitCode::from(2);
        }
    };
    lifecycle(started, options.local.whole, || {
        let mut owner = match Owner::new(started, options.local.whole, options.local.reserve) {
            Ok(owner) => owner,
            Err(_) => return (false, None),
        };
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(_) => {
                owner.record(Cause::Io);
                return (false, Some(owner));
            }
        };
        let passed = runtime.block_on(async {
            let bytes = execute(&options, started, &mut owner).await;
            if let Err(cause) = verify::stdout(&mut owner, &bytes) {
                owner.record(cause);
            }
            owner.check(Window::Final).is_ok()
        });
        drop(runtime);
        (passed, Some(owner))
    })
}
