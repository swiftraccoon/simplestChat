//! Four fixed gh invocations with one retained, caller-trusted executable.

use super::super::namespace::{Budget, Permit};
use super::{failure, request::Request};
use crate::{
    ci::{command::Program, release_checks::files::Executable},
    tooling::{Environment, Result},
};
use simplestchat_ops::distribution::manifest::Selection;
use std::{
    ffi::OsString,
    fs::Metadata,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

const REPOSITORY: &str = "swiftraccoon/simplestChat";
const SUBJECT: &str = "ops-preparation.json";
const PREDICATE: &str = "https://github.com/swiftraccoon/simplestChat/attestations/ops-artifact/v1";
const SAN: &str =
    "https://github.com/swiftraccoon/simplestChat/.github/workflows/ci.yml@refs/heads/main";

#[derive(Clone)]
pub(in crate::ci) struct Seal {
    pub sha256: String,
    pub bytes: u64,
    pub metadata: Metadata,
}
#[derive(Clone)]
pub(in crate::ci) struct Captures {
    pub stdout: Seal,
    pub stderr: Seal,
}

struct Context {
    directory: PathBuf,
    environment: Environment,
    arguments: [Vec<OsString>; 4],
}
struct Dispatch {
    clock: Arc<dyn super::plan::Clock>,
    start: std::time::Instant,
    latest: std::time::Instant,
}
pub(in crate::ci) struct Invocation {
    executable: Arc<Executable>,
    context: Arc<Context>,
    index: usize,
    admitted: AtomicBool,
    invalid: AtomicBool,
    captures: Mutex<Option<Captures>>,
    dispatch: Mutex<Option<Dispatch>>,
}
impl std::fmt::Debug for Invocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RetainedOpsVerifierInvocation")
    }
}
impl Invocation {
    fn dispatch_check(&self) -> Result<()> {
        let dispatch = self.dispatch.lock().map_err(|_| failure(1))?;
        let dispatch = dispatch.as_ref().ok_or_else(|| failure(1))?;
        let now = dispatch.clock.now();
        if now < dispatch.start {
            return Err(failure(1));
        }
        if now > dispatch.latest {
            return Err(failure(124));
        }
        Ok(())
    }
    pub(in crate::ci) fn begin(&self) -> Result<()> {
        if self.admitted.swap(true, Ordering::SeqCst) {
            self.invalid.store(true, Ordering::SeqCst);
            return Err(failure(1));
        }
        Ok(())
    }
    pub(in crate::ci) fn validate(&self, program: Program, args: &[OsString]) -> Result<&Path> {
        if program != Program::OpsGh
            || args != self.context.arguments[self.index]
            || self.invalid.load(Ordering::SeqCst)
        {
            return Err(failure(1));
        }
        self.dispatch_check()?;
        let path = self.executable.selected_path().map_err(|_| failure(1))?;
        self.dispatch_check()?;
        Ok(path)
    }
    pub(in crate::ci) fn context(
        &self,
        program: Program,
        args: &[OsString],
    ) -> Result<(&Path, &Environment)> {
        self.validate(program, args)?;
        Ok((&self.context.directory, &self.context.environment))
    }
    pub(in crate::ci) fn observed_io(&self, value: Option<Captures>) {
        if let Ok(mut capture) = self.captures.lock() {
            *capture = value;
        } else {
            self.invalid.store(true, Ordering::SeqCst);
        }
    }
    pub(super) fn captures(&self) -> Result<Captures> {
        if self.invalid.load(Ordering::SeqCst) || !self.admitted.load(Ordering::SeqCst) {
            return Err(failure(1));
        }
        self.captures
            .lock()
            .map_err(|_| failure(1))?
            .clone()
            .ok_or_else(|| failure(1))
    }
    pub(super) fn schedule(&self, context: &super::plan::Context) -> Result<()> {
        let (start, latest) = context.dispatch_window()?;
        let mut dispatch = self.dispatch.lock().map_err(|_| failure(1))?;
        if dispatch.is_some() {
            return Err(failure(1));
        }
        *dispatch = Some(Dispatch {
            clock: context.clock.clone(),
            start,
            latest,
        });
        Ok(())
    }
}

pub(super) struct Tools {
    executable: Arc<Executable>,
    context: Arc<Context>,
    _permits: Vec<Permit>,
}
fn strings(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}
fn api(endpoint: &str) -> Vec<OsString> {
    let mut arguments = strings(&[
        "api",
        "--hostname",
        "github.com",
        "--method",
        "GET",
        "-H",
        "Accept: application/vnd.github+json",
        "-H",
        "X-GitHub-Api-Version: 2022-11-28",
    ]);
    arguments.push(endpoint.into());
    arguments
}
fn arguments(directory: &Path, selection: &Selection) -> Result<[Vec<OsString>; 4]> {
    let identity = serde_json::to_value(selection.identity()).map_err(|_| failure(1))?;
    let revision = identity["revision"].as_str().ok_or_else(|| failure(1))?;
    let run = identity["buildRunId"].as_u64().ok_or_else(|| failure(1))?;
    let attempt = identity["runAttempt"].as_u64().ok_or_else(|| failure(1))?;
    let mut verify = strings(&["attestation", "verify"]);
    verify.push(directory.join(SUBJECT).into_os_string());
    verify.push("--bundle".into());
    verify.push(directory.join(super::BUNDLE).into_os_string());
    verify.extend(strings(&[
        "--repo",
        REPOSITORY,
        "--hostname",
        "github.com",
        "--digest-alg",
        "sha256",
        "--predicate-type",
        PREDICATE,
        "--cert-identity",
        SAN,
        "--cert-oidc-issuer",
        "https://token.actions.githubusercontent.com",
        "--signer-digest",
        revision,
        "--source-digest",
        revision,
        "--source-ref",
        "refs/heads/main",
        "--deny-self-hosted-runners",
        "--format",
        "json",
    ]));
    Ok([
        strings(&["--version"]),
        api(&format!("repos/{REPOSITORY}")),
        api(&format!(
            "repos/{REPOSITORY}/actions/runs/{run}/attempts/{attempt}"
        )),
        verify,
    ])
}
pub(super) fn base_environment() -> Environment {
    [
        ("PATH", "/usr/bin:/bin"),
        ("LC_ALL", "C"),
        ("LANG", "C"),
        ("TERM", "dumb"),
        ("NO_COLOR", "1"),
    ]
    .into_iter()
    .map(|(key, value)| (key.into(), value.into()))
    .collect()
}
pub(super) fn environment(request: &Request, captured: &Environment) -> Result<Environment> {
    let mut environment = base_environment();
    let work = request.output.join("work");
    for (key, directory) in [
        ("HOME", "home"),
        ("GH_CONFIG_DIR", "gh-config"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_CACHE_HOME", "cache"),
        ("TMPDIR", "tmp"),
    ] {
        environment.insert(key.into(), work.join(directory).into_os_string());
    }
    for key in [
        "GH_PROMPT_DISABLED",
        "GH_NO_UPDATE_NOTIFIER",
        "GH_NO_EXTENSION_UPDATE_NOTIFIER",
    ] {
        environment.insert(key.into(), "1".into());
    }
    environment.insert("GH_TELEMETRY".into(), "disabled".into());
    environment.insert("GH_TOKEN".into(), request.token(captured)?);
    Ok(environment)
}
impl Tools {
    pub fn open(
        caller: &Path,
        captured: &Environment,
        request: &Request,
        budget: &Budget,
    ) -> Result<Self> {
        let context = Context {
            directory: request.output.clone(),
            environment: environment(request, captured)?,
            arguments: arguments(&request.output, &request.selection)?,
        };
        let permits = budget.reserve(2)?;
        let executable =
            crate::ci::release_checks::files::select(caller, captured, std::ffi::OsStr::new("gh"))
                .map_err(|_| failure(1))?;
        Ok(Self {
            executable: Arc::new(executable),
            context: Arc::new(context),
            _permits: permits,
        })
    }
    pub fn invocation(&self, index: usize) -> Result<Arc<Invocation>> {
        if index >= 4 {
            return Err(failure(1));
        }
        self.executable.validate().map_err(|_| failure(1))?;
        Ok(Arc::new(Invocation {
            executable: self.executable.clone(),
            context: self.context.clone(),
            index,
            admitted: AtomicBool::new(false),
            invalid: AtomicBool::new(false),
            captures: Mutex::new(None),
            dispatch: Mutex::new(None),
        }))
    }
    pub fn arguments(&self, index: usize) -> Result<Vec<OsString>> {
        self.context
            .arguments
            .get(index)
            .cloned()
            .ok_or_else(|| failure(1))
    }
    pub fn finish(self) -> Result<()> {
        let checked = self
            .executable
            .validate()
            .map(|_| ())
            .map_err(|_| failure(1));
        let closed = Arc::try_unwrap(self.executable)
            .map_err(|_| failure(1))?
            .close()
            .map_err(|_| failure(1));
        checked.and(closed)
    }
}
