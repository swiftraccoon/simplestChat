//! Complete cached server verification with four fresh maintained verifier calls.
//! The nine-field caller selection and returned envelope are passive data.

#[cfg(test)]
use super::verification_fixture as fixture;
#[path = "verify/licenses.rs"]
pub(super) mod licenses;
#[path = "verify/options.rs"]
pub(super) mod options;
#[cfg(test)]
#[path = "verify/oracle_tests.rs"]
mod oracle_tests;
#[path = "verify/policy.rs"]
mod policy;
#[path = "verify/process.rs"]
pub(super) mod process;
#[cfg(test)]
#[path = "verify/tests.rs"]
mod tests;
#[path = "verify/tool.rs"]
pub(super) mod tool;
#[path = "verify/zip.rs"]
pub(super) mod zip;

use super::{
    bindings,
    control::{self, Cause, Owner, Result, Window},
    files::{self, Dir, Retained},
};
use crate::{json::Json, release::trust};
use options::Options;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::{self, Write},
    os::fd::AsFd,
    process::ExitCode,
    time::{Duration, Instant},
};

trait Executor {
    async fn run(&mut self, owner: &mut Owner, invocation: process::Invocation) -> Result<Vec<u8>>;
}
#[derive(Default)]
struct Native {
    observations: Vec<process::Observation>,
}
impl Executor for Native {
    async fn run(&mut self, owner: &mut Owner, invocation: process::Invocation) -> Result<Vec<u8>> {
        let entered = owner.now();
        let cap = invocation.settlement_end.saturating_duration_since(entered);
        let completed = process::run(owner, invocation).await;
        let observed = &completed.observation;
        if let Ok(output) = &completed.result {
            let complete = observed.pid.is_some()
                && observed.exit_status == Some(0)
                && observed.stdin_close_attempted
                && observed.stdin_closed
                && observed.stdout_close_attempted
                && observed.stdout_closed
                && observed.stderr_close_attempted
                && observed.stderr_closed
                && observed.stdout_eof
                && observed.stderr_eof
                && observed.group_retired
                && observed.reaped
                && !observed.stdout_limited
                && !observed.stderr_limited
                && !observed.timed_out
                && observed.cleanup_errors.is_empty()
                && observed.elapsed < cap
                && usize::try_from(observed.stdout_bytes).ok() == Some(output.len())
                && observed.stderr_bytes <= process::ERR_LIMIT
                && observed.stdout_sha256 == files::hex(&Sha256::digest(output))
                && options::hex(&observed.stderr_sha256, 64);
            if !complete {
                owner.record(Cause::Close);
            }
        }
        self.observations.push(completed.observation);
        owner.check(Window::Body)?;
        completed.result
    }
}
struct State {
    directory: Option<Dir>,
    zip: Option<Retained>,
    members: [Option<Retained>; 10],
    tool: Option<tool::Tool>,
}
impl State {
    fn new() -> Self {
        Self {
            directory: None,
            zip: None,
            members: std::array::from_fn(|_| None),
            tool: None,
        }
    }
    fn file(&self, name: &str) -> &Retained {
        self.members[zip::SELECTED
            .iter()
            .position(|value| *value == name)
            .unwrap()]
        .as_ref()
        .unwrap()
    }
    fn read(&self, name: &str, owner: &mut Owner) -> Result<Json> {
        bindings::decode(&self.file(name).bytes(bindings::METADATA, owner)?, owner)
    }
    fn originals(&self, owner: &mut Owner) -> Result<()> {
        self.directory
            .as_ref()
            .unwrap()
            .check(owner, Window::Body)?;
        self.zip.as_ref().unwrap().check(owner, Window::Body)?;
        for file in self.members.iter().flatten() {
            file.check(owner, Window::Body)?;
        }
        if let Some(tool) = &self.tool {
            tool.check(owner)?;
        }
        Ok(())
    }
    fn bind_files(
        &self,
        options: &Options,
        claim: &policy::Claim,
        owner: &mut Owner,
    ) -> Result<()> {
        let digests = claim.digests();
        for name in trust::BOUND_FILES {
            let file = self.file(name);
            file.bound(
                if name == "image.tar" {
                    bindings::IMAGE
                } else {
                    bindings::METADATA as u64
                },
                owner,
            )?;
            let actual = file.hash(owner)?;
            policy::require(owner, actual == digests[name])?;
        }
        let selected = options.metadata.selection()?;
        let security = self.read("image-security.json", owner)?;
        bindings::bind_security(&security, &selected, claim.image(), &digests, owner)?;
        let required = bindings::runtime_security(&security, &digests, owner)?;
        let proof = self.read("runtime-proof.json", owner)?;
        bindings::runtime_proof(&proof, required, &selected, claim.image(), &digests, owner)?;
        let vex = self.read("vex.openvex.json", owner)?;
        bindings::runtime_vex(&vex, required, owner)?;
        bindings::runtime_review(&security, &proof, required, owner)?;
        let outcome = self.read("outcome.json", owner)?;
        bindings::bind_outcome(&outcome, &selected, claim.image(), owner)?;
        let manifest = self.read("release.json", owner)?;
        bindings::bind_manifest(&manifest, &selected, &digests, owner)
    }
    async fn perform(
        &mut self,
        options: &Options,
        owner: &mut Owner,
        executor: &mut impl Executor,
    ) -> Result<Vec<u8>> {
        self.directory = Some(Dir::open(&options.directory, owner)?);
        self.zip = Some(Retained::open(
            self.directory.as_ref().unwrap().clone(),
            "artifact.zip",
            owner,
        )?);
        zip::verify(
            self.zip.as_ref().unwrap(),
            self.directory.as_ref().unwrap(),
            &mut self.members,
            &options.metadata,
            owner,
        )?;
        let bytes = self
            .file("release-predicate.json")
            .bytes(bindings::METADATA, owner)?;
        let claim = policy::claim(&bytes, &options.metadata, owner)?;
        self.bind_files(options, &claim, owner)?;
        let bundle = self.file("release-attestation.jsonl");
        bundle.bound(process::OUT_LIMIT, owner)?;
        let environment = options.environment.as_ref().map_or_else(
            || tool::restricted(std::env::vars_os()),
            |environment| tool::restricted(environment.clone()),
        );
        let bundle_digest = bundle.hash(owner)?;
        self.tool = Some(tool::Tool::resolve(&environment, owner)?);
        let mut verification = BTreeMap::new();
        for name in trust::SUBJECTS {
            self.originals(owner)?;
            let entry = owner.now();
            let body_end = owner
                .body_end()
                .checked_sub(Duration::from_secs(10))
                .ok_or(Cause::Deadline)?
                .min(
                    entry
                        .checked_add(Duration::from_secs(180))
                        .ok_or(Cause::Deadline)?,
                );
            if entry >= body_end {
                return control::failed(owner, Cause::Deadline);
            }
            let settlement_end = body_end
                .checked_add(Duration::from_secs(10))
                .ok_or(Cause::Deadline)?
                .min(owner.body_end());
            let output = executor
                .run(
                    owner,
                    process::Invocation {
                        program: self.tool.as_ref().unwrap().path().to_owned(),
                        arguments: arguments(&options.directory, name, &options.metadata),
                        environment: environment.clone(),
                        body_end,
                        settlement_end,
                    },
                )
                .await?;
            self.originals(owner)?;
            policy::statement(&output, &options.metadata, &claim, owner)?;
            verification.insert(name.to_owned(), files::hex(&Sha256::digest(&output)));
        }
        #[cfg(test)]
        owner.after_verifiers();
        let actual = self.file("release-attestation.jsonl").hash(owner)?;
        policy::require(owner, actual == bundle_digest)?;
        let envelope = policy::envelope(
            &options.metadata,
            &claim,
            &bundle_digest,
            &verification,
            owner,
        )?;
        // These stricter build/source/manifest guards are late in the reference.
        policy::outcome(&self.read("outcome.json", owner)?, &options.metadata, owner)?;
        policy::source(&self.read("source.json", owner)?, &options.metadata, owner)?;
        let bytes = self
            .file("release.json")
            .bytes(crate::release::manifest::MAX_BYTES, owner)?;
        let manifest =
            crate::release::manifest::Manifest::decode(&bytes).map_err(|_| Cause::Archive)?;
        policy::require(
            owner,
            manifest.fields().revision == options.metadata.revision(),
        )?;
        let image = self.members[0].as_mut().unwrap();
        let _ = image.archive(&manifest, owner)?;
        let current = self.read("release-predicate.json", owner)?;
        policy::require(owner, current.semantic_eq(&claim.original))?;
        let actual = self.file("release-attestation.jsonl").hash(owner)?;
        policy::require(owner, actual == bundle_digest)?;
        self.bind_files(options, &claim, owner)?;
        self.originals(owner)?;
        self.tool.as_ref().unwrap().bytes(owner)?;
        owner.check(Window::Body)?;
        Ok(envelope)
    }
    fn finish(mut self, owner: &mut Owner) {
        if let Some(tool) = self.tool.take() {
            tool.finish(owner);
        }
        for file in self.members.iter_mut().filter_map(Option::take) {
            file.finish(owner);
        }
        if let Some(file) = self.zip.take() {
            file.finish(owner);
        }
        if let Some(directory) = self.directory.take() {
            directory.finish(owner);
        }
        let _ = owner.check(Window::Final);
    }
}
async fn execute(
    options: &Options,
    owner: &mut Owner,
    executor: &mut impl Executor,
) -> Result<Vec<u8>> {
    let mut state = State::new();
    let result = state.perform(options, owner, executor).await;
    if let Err(cause) = &result {
        owner.record(*cause);
    }
    state.finish(owner);
    if let Some(cause) = owner.error() {
        Err(cause)
    } else {
        result
    }
}
pub(super) async fn acquired(
    directory: std::path::PathBuf,
    metadata: options::Metadata,
    environment: tool::Environment,
    owner: &mut Owner,
) -> Result<Vec<u8>> {
    let options = Options {
        directory,
        metadata,
        whole: Duration::ZERO,
        reserve: Duration::ZERO,
        environment: Some(environment),
    };
    execute(&options, owner, &mut Native::default()).await
}
fn arguments(
    directory: &std::path::Path,
    name: &str,
    metadata: &options::Metadata,
) -> Vec<OsString> {
    vec![
        "attestation".into(),
        "verify".into(),
        directory.join(name).into_os_string(),
        "--bundle".into(),
        directory.join("release-attestation.jsonl").into_os_string(),
        "--repo".into(),
        metadata.repository().into(),
        "--hostname".into(),
        "github.com".into(),
        "--predicate-type".into(),
        trust::PREDICATE_TYPE.into(),
        "--cert-identity".into(),
        format!(
            "https://github.com/{}/{}@{}",
            metadata.repository(),
            trust::WORKFLOW,
            trust::REF
        )
        .into(),
        "--cert-oidc-issuer".into(),
        trust::ISSUER.into(),
        "--signer-digest".into(),
        metadata.revision().into(),
        "--source-digest".into(),
        metadata.revision().into(),
        "--source-ref".into(),
        trust::REF.into(),
        "--deny-self-hosted-runners".into(),
        "--format".into(),
        "json".into(),
    ]
}
pub(super) fn stdout(owner: &mut Owner, bytes: &[u8]) -> Result<()> {
    let output = io::stdout();
    let mut lock = output.lock();
    lock.flush().map_err(|_| Cause::Io)?;
    let descriptor = lock.as_fd();
    let previous = rustix::fs::fcntl_getfl(descriptor).map_err(|_| Cause::Io)?;
    rustix::fs::fcntl_setfl(descriptor, previous | rustix::fs::OFlags::NONBLOCK)
        .map_err(|_| Cause::Io)?;
    let written = (|| {
        let mut offset = 0;
        while offset < bytes.len() {
            if owner.error().is_none() {
                owner.check(Window::Final)?;
            } else {
                owner.cleanup_before(owner.end())?;
            }
            match rustix::io::write(descriptor, &bytes[offset..]) {
                Ok(0) => return Err(Cause::Io),
                Ok(count) => offset += count,
                Err(rustix::io::Errno::AGAIN) => std::thread::sleep(Duration::from_millis(10)),
                Err(rustix::io::Errno::INTR) => {}
                Err(_) => return Err(Cause::Io),
            }
        }
        Ok(())
    })();
    let restored = rustix::fs::fcntl_setfl(descriptor, previous).map_err(|_| Cause::Close);
    written.and(restored)
}
#[cfg(test)]
thread_local! {
    static COMPLETION_PROBE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static COMPLETION_OFFSET: std::cell::Cell<Option<Duration>> = const {
        std::cell::Cell::new(None)
    };
    static FRAME_CLOSED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static COMPLETION_AFTER_FRAME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
fn lifecycle(started: Instant, whole: Duration, operation: impl FnOnce() -> bool) -> ExitCode {
    let Some(end) = started.checked_add(whole) else {
        return ExitCode::from(2);
    };
    let passed = operation();
    let completed = Instant::now();
    #[cfg(test)]
    let completed = {
        if COMPLETION_PROBE.get() {
            COMPLETION_AFTER_FRAME.set(FRAME_CLOSED.get());
        }
        COMPLETION_OFFSET
            .get()
            .and_then(|offset| end.checked_add(offset))
            .unwrap_or(completed)
    };
    if passed && completed < end {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
/// Verify cached server bytes through four fresh maintained `gh` invocations.
///
/// Use one dedicated synchronous CLI invocation with explicit whole/finalization
/// budgets. The caller supplies fresh API-selected nine-field metadata; decoding
/// that data does not establish API freshness or signature/deployment authority.
/// Exit0 requires complete local/cryptographic relationships and checked peers,
/// runtime/output finalization within the original whole budget. Failures are1,
/// invalid usage2. Any nonzero exit invalidates all earlier output, including a
/// complete passive envelope emitted before a late finalization refusal.
/// Candidate bytes remain in place. Blocked kernel I/O/reap is
/// outside a hard wall bound. See the Ops guide for the accepted native profile.
///
/// # Panics
///
/// A valid operation must run outside an entered Tokio runtime. Starting or
/// dropping its runtime from an existing asynchronous runtime context can panic.
/// Help/notices/invalid options return before runtime, environment or file work.
///
/// ```
/// use simplestchat_ops::release::attestation::verify;
/// use std::{ffi::OsString,process::ExitCode};
/// assert_eq!(verify::run(&[OsString::from("--help")]),ExitCode::SUCCESS);
/// ```
pub fn run(arguments: &[OsString]) -> ExitCode {
    let started = Instant::now();
    if arguments.len() == 1 && arguments[0] == "--licenses" {
        return if licenses::stdout().is_ok() {
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
        Err(_) => {
            let _ = writeln!(io::stderr().lock(), "{}", options::USAGE);
            return ExitCode::from(2);
        }
    };
    lifecycle(started, options.whole, || {
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(_) => return false,
        };
        let passed = runtime.block_on(async {
            let mut owner = match Owner::new(started, options.whole, options.reserve) {
                Ok(owner) => owner,
                Err(_) => return false,
            };
            let mut native = Native::default();
            let result = execute(&options, &mut owner, &mut native).await;
            let mut passed = result.is_ok();
            let bytes = result.unwrap_or_else(|_| {
                b"{\"passed\":false,\"failureClass\":\"release_attestation_failed\"}\n".to_vec()
            });
            if let Err(cause) = stdout(&mut owner, &bytes) {
                owner.record(cause);
                passed = false;
            }
            passed && owner.check(Window::Final).is_ok()
        });
        drop(runtime);
        passed
    })
}
