//! The machine the image gate runs on for real: the checkout it was built
//! from, the engine it was told to use, the caller's environment and clock,
//! and the signals that cancel it.

use crate::{
    Fault, Result, context,
    document::Document,
    elf, engine, files,
    image::{
        archive,
        gate::{Host, LOCK},
        policy::Moment,
        runner::{Bounds, Runner},
        runtime,
        sandbox::{Engine, Platform, TOOLS},
    },
    policy::Date,
    require, tools,
};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

/// How long a command that the selection of the engine asks may take.
const ASKED: Duration = Duration::from_secs(30);
/// The most bytes it may print on either of its streams.
const ASKED_BYTES: u64 = 2 << 20;
/// How long git may take to answer the review of the runtime proof.
const GIT: Duration = Duration::from_secs(15);
/// The most bytes git may complain in.
const GIT_COMPLAINTS: u64 = 65_536;
/// Where the scanners' default installations are, below the checkout.
const TOOL_CACHE: &str = ".cache/security-tools";

/// What a command printed, once it ended with the status 0 within its
/// bounds: `arguments` begin with the program.
fn asked(arguments: &[OsString]) -> Result<Vec<u8>> {
    const FAILED: Fault = Fault("command_failed");
    let (program, rest) = arguments.split_first().ok_or(FAILED)?;
    let mut command = Command::new(program);
    command.args(rest);
    let answered = context::asked(&mut command, &[], ASKED, (ASKED_BYTES, ASKED_BYTES));
    let (status, printed) = answered.map_err(|_| FAILED)?;
    require(status == 0, FAILED.0)?;
    Ok(printed)
}

/// This machine, for the checkout at `root`.
pub struct Local<'a> {
    /// The checkout, which holds the lock and the installations of the
    /// scanners.
    pub root: &'a Path,
    /// The engine the scanners run in.
    pub engine: engine::Name,
    /// The caller's environment.
    pub ambient: &'a dyn Fn(&str) -> Option<OsString>,
    /// The day the review of the runtime proof is read on.
    pub today: Date,
    /// The cancellation that the signals set.
    pub cancelled: Arc<AtomicBool>,
}

impl Local<'_> {
    /// What `work` answers for the site of the scanners of the lock's
    /// platform `platform`.
    fn sited(
        &self,
        platform: &str,
        work: impl FnOnce(&tools::Site<'_>) -> Result<PathBuf>,
    ) -> Result<PathBuf> {
        let (lock, cache) = (self.root.join(LOCK), self.root.join(TOOL_CACHE));
        work(&tools::Site {
            lock: &lock,
            cache: &cache,
            platform,
            limits: tools::Limits::REVIEWED,
        })
    }

    /// What git prints for `arguments` in the checkout, in at most `limit`
    /// bytes, once it ends with the status 0.
    fn git(&self, arguments: &[String], limit: u64) -> Result<Vec<u8>> {
        const FAILED: Fault = Fault("runtime_source_git");
        let search = (self.ambient)("PATH");
        let program = context::executable("git", search.as_deref()).map_err(|_| FAILED)?;
        let mut command = Command::new(program);
        command.args(arguments).current_dir(self.root);
        let answered = context::asked(&mut command, &[], GIT, (limit, GIT_COMPLAINTS));
        let (status, printed) = answered.map_err(|_| FAILED)?;
        require(status == 0, FAILED.0)?;
        Ok(printed)
    }
}

impl Host for Local<'_> {
    fn extract(&self, archive: &Path, manifest: &Path, tree: &Path) -> Result<()> {
        archive::extract(archive, manifest, tree, &archive::Limits::REVIEWED).map(drop)
    }

    fn audit(&self, rootfs: &Path, platform: Platform, report: &Path) -> Result<()> {
        let (audited, passed) = elf::report(elf::audit(rootfs, platform.name()));
        files::write_private(report, audited.pretty().as_bytes(), 0o600)?;
        require(passed, "image_elf_audit_failed")
    }

    fn prefix(&self) -> Result<Vec<OsString>> {
        let search = (self.ambient)("PATH");
        let located = |name: &'static str| context::executable(name, search.as_deref()).ok();
        let present = |candidate: &Path| candidate.exists();
        let mut asked = asked;
        let mut host = engine::Host {
            located: &located,
            home: (self.ambient)("HOME").map(PathBuf::from),
            present: &present,
            mac: cfg!(target_os = "macos"),
            asked: &mut asked,
        };
        engine::prefix(self.engine, &mut host)
    }

    fn engine(
        &self,
        prefix: &[OsString],
        environment: Vec<(&'static str, OsString)>,
        logs: PathBuf,
    ) -> Box<dyn Engine> {
        let environment = environment.into_iter();
        let environment = environment.map(|(name, value)| (name.to_owned(), value));
        let runner = Runner::new(
            prefix.to_vec(),
            environment.collect(),
            self.root,
            &logs,
            Bounds::REVIEWED,
        );
        Box::new(runner)
    }

    fn install(&self, platform: &'static str, directory: &Path) -> Result<PathBuf> {
        let names: Vec<String> = TOOLS.iter().map(|name| (*name).to_owned()).collect();
        self.sited(platform, |site| {
            tools::install(site, &names, Some(directory), &mut tools::Curl::default())
        })
    }

    fn locate(
        &self,
        name: &'static str,
        platform: &'static str,
        directory: &Path,
    ) -> Result<PathBuf> {
        self.sited(platform, |site| tools::path(site, name, Some(directory)))
    }

    fn prove(&self, evidence: &runtime::Evidence<'_>) -> Result<Document> {
        let limits = runtime::Limits::REVIEWED;
        let mut git = |arguments: &[String], limit: u64| self.git(arguments, limit);
        let mut review = |revision: &str| {
            runtime::source_review(self.root, revision, self.today, &limits, &mut git)
        };
        runtime::proof(evidence, &limits, &mut review)
    }

    fn ambient(&self, name: &str) -> Option<OsString> {
        (self.ambient)(name)
    }

    fn now(&self) -> Moment {
        Moment::now()
    }

    fn cancelled(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancelled)
    }
}

/// SIGINT and SIGTERM, which cancel the gate while this lasts instead of
/// ending it: every scanner it starts is then stopped and removed by its
/// identity, and its evidence is still written.
pub struct Signals(Vec<signal_hook::SigId>);

impl Signals {
    /// Set `cancelled` on SIGINT and on SIGTERM.
    pub fn cancelling(cancelled: &Arc<AtomicBool>) -> Result<Self> {
        let mut signals = Self(Vec::new());
        for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
            let registered = signal_hook::flag::register(signal, Arc::clone(cancelled));
            signals
                .0
                .push(registered.map_err(|_| Fault("image_signal_unavailable"))?);
        }
        Ok(signals)
    }
}

impl Drop for Signals {
    fn drop(&mut self) {
        for registered in self.0.drain(..) {
            signal_hook::low_level::unregister(registered);
        }
    }
}

#[cfg(test)]
#[path = "host_tests.rs"]
mod tests;
