//! Complete offline server signing-input preparation.
//!
//! This Linux/macOS CLI creates four metadata copies and one unsigned predicate.
//! It never signs, verifies a signature, fetches, runs Git/Docker or deploys.
//! Run it in a dedicated CLI process with explicit caller-selected whole and
//! finalization budgets. The existing Python command remains the reference.

#[path = "options.rs"]
mod options;
#[cfg(test)]
#[path = "oracle_tests.rs"]
mod oracle_tests;
#[path = "policy.rs"]
mod policy;
#[cfg(test)]
#[path = "signal_tests.rs"]
mod signal_tests;
#[cfg(test)]
#[path = "tests.rs"]
mod tests;

use super::{control, files};
use control::{Cause, Owner, Result, Window};
use files::{Dir, Retained};
use options::Options;
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    io::{self, Write},
    os::fd::{AsFd, BorrowedFd},
    process::ExitCode,
    time::Instant,
};

const INPUTS: [(usize, &str); 13] = [
    (0, "image.tar"),
    (0, "release.json"),
    (0, "outcome.json"),
    (0, "source.json"),
    (1, "outcome.json"),
    (2, "sbom.spdx.json"),
    (1, "native.json"),
    (1, "elf.json"),
    (1, "secret-paths.json"),
    (1, "database-status.json"),
    (1, "runtime-proof.json"),
    (1, "vex.openvex.json"),
    (1, "runtime-license-evidence.json"),
];
const EVIDENCE_INPUTS: [usize; 8] = [5, 6, 7, 8, 9, 10, 11, 12];
const COPIES: [(usize, &str); 4] = [
    (5, "sbom.spdx.json"),
    (4, "image-security.json"),
    (10, "runtime-proof.json"),
    (11, "vex.openvex.json"),
];

struct State {
    directories: [Option<Dir>; 3],
    inputs: [Option<Retained>; 13],
    outputs: Vec<Retained>,
}
impl State {
    fn new() -> Self {
        Self {
            directories: std::array::from_fn(|_| None),
            inputs: std::array::from_fn(|_| None),
            outputs: Vec::new(),
        }
    }
    fn directory(&mut self, at: usize, options: &Options, owner: &mut Owner) -> Result<Dir> {
        if self.directories[at].is_none() {
            let path = match at {
                0 => options.artifact.clone(),
                1 => options.security.clone(),
                _ => options.security.join("spdx"),
            };
            self.directories[at] = Some(Dir::open(&path, owner)?);
        }
        Ok(self.directories[at].as_ref().unwrap().clone())
    }
    fn input(&mut self, at: usize, options: &Options, owner: &mut Owner) -> Result<&mut Retained> {
        if self.inputs[at].is_none() {
            let (directory, name) = INPUTS[at];
            let directory = self.directory(directory, options, owner)?;
            self.inputs[at] = Some(Retained::open(directory, name, owner)?);
        }
        Ok(self.inputs[at].as_mut().unwrap())
    }
    fn bound(&mut self, name: &str, options: &Options, owner: &mut Owner) -> Result<&mut Retained> {
        if let Some(at) = INPUTS[..4].iter().position(|(_, input)| *input == name) {
            return self.input(at, options, owner);
        }
        self.outputs
            .iter_mut()
            .find(|file| file.name() == OsStr::new(name))
            .ok_or(Cause::Policy)
    }
    fn read(
        &mut self,
        name: &str,
        options: &Options,
        owner: &mut Owner,
    ) -> Result<crate::json::Json> {
        let bytes = self
            .bound(name, options, owner)?
            .bytes(policy::METADATA, owner)?;
        policy::decode(&bytes, owner)
    }
    fn perform(&mut self, options: &Options, owner: &mut Owner) -> Result<()> {
        let bytes = self
            .input(1, options, owner)?
            .bytes(crate::release::manifest::MAX_BYTES, owner)?;
        let manifest =
            crate::release::manifest::Manifest::decode(&bytes).map_err(|_| Cause::Policy)?;
        let archive = self.input(0, options, owner)?.archive(&manifest, owner)?;
        let archive_sha = archive.identity().archive_sha256.clone();
        let config = format!("sha256:{}", archive.identity().config_sha256);
        let bytes = self
            .input(4, options, owner)?
            .bytes(policy::METADATA, owner)?;
        let security = policy::decode(&bytes, owner)?;
        policy::security(&security, &options.selection, &archive_sha, &config, owner)?;
        for ((field, _), at) in policy::EVIDENCE.iter().zip(EVIDENCE_INPUTS) {
            let actual = self.input(at, options, owner)?.hash(owner)?;
            policy::evidence(&security, field, &actual, owner)?;
        }
        for (at, name) in COPIES {
            self.input(at, options, owner)?
                .bound(policy::METADATA as u64, owner)?;
            let directory = self.directory(0, options, owner)?;
            self.outputs.push(Retained::create(directory, name, owner)?);
            let source = self.inputs[at].as_ref().unwrap();
            source.copy(self.outputs.last_mut().unwrap(), owner)?;
        }
        let mut digests = BTreeMap::new();
        for name in crate::release::trust::BOUND_FILES {
            digests.insert(
                name.to_owned(),
                self.bound(name, options, owner)?.hash(owner)?,
            );
        }
        let claim = policy::claim(&options.selection, &config, &digests, owner)?;
        // Preserve the reference's late bounds and second fresh binding pass.
        for name in crate::release::trust::BOUND_FILES {
            let maximum = if name == "image.tar" {
                policy::IMAGE
            } else {
                policy::METADATA as u64
            };
            let file = self.bound(name, options, owner)?;
            file.bound(maximum, owner)?;
            let actual = file.hash(owner)?;
            policy::require(owner, actual == digests[name])?;
        }
        let security = self.read("image-security.json", options, owner)?;
        let selected = options.selection.claim_selection()?;
        policy::bind_security(&security, &selected, &config, &digests, owner)?;
        self.bind_runtime(&security, options, &config, &digests, owner)?;
        let outcome = self.read("outcome.json", options, owner)?;
        policy::bind_outcome(&outcome, &selected, &config, owner)?;
        let manifest = self.read("release.json", options, owner)?;
        policy::bind_manifest(&manifest, &selected, &digests, owner)?;
        let directory = self.directory(0, options, owner)?;
        self.outputs.push(Retained::create(
            directory,
            "release-predicate.json",
            owner,
        )?);
        self.outputs.last_mut().unwrap().write(&claim, owner)?;
        owner.check(Window::Body)
    }
    fn bind_runtime(
        &mut self,
        security: &crate::json::Json,
        options: &Options,
        image: &str,
        digests: &BTreeMap<String, String>,
        owner: &mut Owner,
    ) -> Result<()> {
        let required = policy::runtime_security(security, digests, owner)?;
        let runtime = self.read("runtime-proof.json", options, owner)?;
        let selected = options.selection.claim_selection()?;
        policy::runtime_proof(&runtime, required, &selected, image, digests, owner)?;
        let vex = self.read("vex.openvex.json", options, owner)?;
        policy::runtime_vex(&vex, required, owner)?;
        policy::runtime_review(security, &runtime, required, owner)
    }
    fn finish(mut self, owner: &mut Owner) {
        for file in self.inputs.iter_mut().filter_map(Option::take) {
            file.finish(owner);
        }
        for output in self.outputs.drain(..) {
            output.finish(owner);
        }
        for dir in self.directories.iter_mut().rev().filter_map(Option::take) {
            dir.finish(owner);
        }
        let _ = owner.check(Window::Final);
    }
}
fn execute(options: &Options, owner: &mut Owner) -> Result<()> {
    let mut state = State::new();
    if let Err(error) = state.perform(options, owner) {
        owner.record(error);
    }
    state.finish(owner);
    owner.error().map_or(Ok(()), Err)
}
struct Descriptor<'a>(BorrowedFd<'a>);
impl Write for Descriptor<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        rustix::io::write(self.0, bytes).map_err(io::Error::from)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
pub(super) fn stdout(bytes: &[u8]) -> io::Result<()> {
    let output = io::stdout();
    let mut lock = output.lock();
    lock.flush()?;
    emit(Descriptor(lock.as_fd()), bytes)
}
fn emit(mut writer: impl Write, bytes: &[u8]) -> io::Result<()> {
    writer.write_all(bytes)?;
    writer.flush()
}
#[cfg(test)]
thread_local! {
    static COMPLETION_OFFSET: std::cell::Cell<Option<std::time::Duration>> = const {
        std::cell::Cell::new(None)
    };
    static RUNTIME_DROPPED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
fn operation(started: Instant, end: Instant, options: &Options) -> bool {
    // The original registrations must precede our runtime's worker threads.
    let mut owner = match Owner::new(started, options.whole, options.reserve) {
        Ok(owner) => owner,
        Err(_) => return false,
    };
    let passed = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => {
            let passed = runtime.block_on(async { execute(options, &mut owner).is_ok() });
            drop(runtime);
            #[cfg(test)]
            RUNTIME_DROPPED.set(true);
            passed
        }
        Err(_) => {
            owner.record(Cause::Io);
            false
        }
    };
    let finished = owner.finish().is_ok();
    let completed = Instant::now();
    #[cfg(test)]
    let completed = COMPLETION_OFFSET.with(|offset| {
        offset
            .get()
            .and_then(|offset| end.checked_add(offset))
            .unwrap_or(completed)
    });
    passed && finished && completed < end
}
/// Prepare unsigned server metadata in the existing artifact directory.
///
/// Full option names occur once; IDs/budgets use ASCII decimal syntax, and
/// filesystem paths are canonical absolute paths with no symlink components.
/// Run IDs are below 2^53 and attempts below 2^31. `0 < reserve < whole`; there is
/// no implicit budget, extension or arbitrary maximum. Body work leaves the
/// caller's reserve for all-peer finalization inside the original deadline.
/// Exit 0 requires every body check and explicit close; failures are 1, usage 2.
/// Blocked kernel I/O cannot be bounded by cooperative deadline checks.
/// This creates no signature, verification proof or deployment authority.
///
/// # Panics
///
/// Call a valid operation from a synchronous CLI context outside an entered
/// Tokio runtime. Entering or dropping this operation's runtime from an existing
/// asynchronous runtime context can panic. Help and invalid arguments return
/// before creating a runtime.
///
/// ```
/// use simplestchat_ops::release::attestation::prepare;
/// use std::{ffi::OsString, process::ExitCode};
/// assert_eq!(prepare::run(&[OsString::from("--help")]), ExitCode::SUCCESS);
/// ```
pub fn run(arguments: &[OsString]) -> ExitCode {
    let started = Instant::now();
    let options = match Options::parse(arguments) {
        Ok(None) => {
            return if stdout(format!("{}\n", options::USAGE).as_bytes()).is_ok() {
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
    let Some(end) = started.checked_add(options.whole) else {
        let _ = writeln!(io::stderr().lock(), "{}", options::USAGE);
        return ExitCode::from(2);
    };
    if operation(started, end, &options) {
        ExitCode::SUCCESS
    } else {
        let _ = stdout(b"{\"passed\":false,\"failureClass\":\"release_attestation_failed\"}\n");
        ExitCode::FAILURE
    }
}
