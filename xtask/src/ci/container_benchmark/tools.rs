//! Caller-trusted root tools and closed invocations, not executable authentication.
use super::{
    failure,
    io::{Budget, Endpoint, Permit},
    namespace::{self, Lease},
    plan,
};
use crate::{
    ci::{
        command::{Command, Program},
        release_checks::files::{self, Executable},
        release_controller::files::Directory,
    },
    tooling::{Environment, Result},
};
use std::{
    ffi::{OsStr, OsString},
    fs::{File, Metadata},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

fn same(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.mode() == b.mode()
        && a.uid() == b.uid()
        && a.gid() == b.gid()
        && a.nlink() == b.nlink()
        && a.len() == b.len()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
        && a.ctime() == b.ctime()
        && a.ctime_nsec() == b.ctime_nsec()
}
struct Tool {
    proof: Executable,
    file: File,
    stamp: Metadata,
}
impl Tool {
    fn select(caller: &Path, env: &Environment, name: &str, uid: u32) -> Result<Self> {
        let proof = files::select(caller, env, OsStr::new(name)).map_err(|_| failure(1))?;
        let admitted = (|| {
            let path = proof.validate().map_err(|_| failure(1))?;
            let file = rustix::fs::open(
                path,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::NONBLOCK
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .map(File::from)
            .map_err(|_| failure(1))?;
            let stamp = match file.metadata() {
                Ok(value) => value,
                Err(_) => {
                    let _ = nix::unistd::close(file);
                    return Err(failure(1));
                }
            };
            let valid = stamp.is_file()
                && stamp.uid() == uid
                && stamp.mode() & 0o7022 == 0
                && stamp.mode() & 0o111 != 0
                && stamp.len() > 0
                && stamp.len() <= 128 * 1024 * 1024;
            if !valid || proof.validate().is_err() {
                let _ = nix::unistd::close(file);
                return Err(failure(1));
            }
            Ok((file, stamp))
        })();
        match admitted {
            Ok((file, stamp)) => Ok(Self { proof, file, stamp }),
            Err(error) => {
                let _ = proof.close();
                Err(error)
            }
        }
    }
    fn validate(&self) -> Result<&Path> {
        let path = self.proof.validate().map_err(|_| failure(1))?;
        let visible = rustix::fs::open(
            path,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map(File::from)
        .map_err(|_| failure(1))?;
        let observed = visible.metadata().map_err(|_| failure(1));
        let closed = nix::unistd::close(visible).map_err(|_| failure(1));
        if !same(&observed?, &self.stamp) {
            return Err(failure(1));
        }
        closed?;
        if !same(&self.file.metadata().map_err(|_| failure(1))?, &self.stamp) {
            return Err(failure(1));
        }
        self.proof.validate().map_err(|_| failure(1))?;
        Ok(path)
    }
    fn finish(self) -> Result<()> {
        let checked = self.validate().map(|_| ());
        let file = nix::unistd::close(self.file).map_err(|_| failure(1));
        let proof = self.proof.close().map_err(|_| failure(1));
        checked.and(file).and(proof)
    }
}
struct Context {
    caller: PathBuf,
    discovery: Environment,
    frozen: Environment,
    server: Environment,
    config: Directory,
    _permit: Permit,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Docker,
    Curl,
}
#[derive(Clone)]
pub(in crate::ci) struct IoSeals {
    pub paired: Option<crate::ci::ops_artifact::verify::tools::Captures>,
    pub combined: Option<crate::ci::ops_artifact::verify::tools::Seal>,
}
enum Received {
    Fresh,
    Claimed,
    Complete(Box<IoSeals>),
    Invalid,
}
/// A single command's actual completed log evidence. This owns no descriptors
/// and cannot make missing, repeated or failed settlement positive.
pub(in crate::ci) struct CaptureReceipt(Mutex<Received>);
impl Default for CaptureReceipt {
    fn default() -> Self {
        Self(Mutex::new(Received::Fresh))
    }
}
impl std::fmt::Debug for CaptureReceipt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ContainerBenchmarkCaptureReceipt")
    }
}
impl CaptureReceipt {
    pub fn new() -> Self {
        Self::default()
    }
    fn claim(&self) -> Result<()> {
        let mut state = self.0.lock().map_err(|_| failure(1))?;
        let previous = std::mem::replace(&mut *state, Received::Invalid);
        if matches!(previous, Received::Fresh) {
            *state = Received::Claimed;
            Ok(())
        } else {
            Err(failure(1))
        }
    }
    pub fn observed(&self, value: Option<IoSeals>) {
        if let Ok(mut state) = self.0.lock() {
            let previous = std::mem::replace(&mut *state, Received::Invalid);
            if matches!(previous, Received::Claimed)
                && let Some(value) = value
                && (value.paired.is_some() ^ value.combined.is_some())
            {
                *state = Received::Complete(Box::new(value));
            }
        }
    }
    pub fn get(&self) -> Result<IoSeals> {
        let mut state = self.0.lock().map_err(|_| failure(1))?;
        match std::mem::replace(&mut *state, Received::Invalid) {
            Received::Complete(value) => Ok(*value),
            _ => Err(failure(1)),
        }
    }
    #[cfg(test)]
    pub fn observed_test(&self, value: Option<IoSeals>) -> Result<()> {
        self.claim()?;
        self.observed(value);
        Ok(())
    }
}
pub(in crate::ci) struct Invocation {
    role: Role,
    selected: Arc<Tool>,
    curl: Option<Arc<Tool>>,
    namespace: Option<Arc<Lease>>,
    context: Arc<Context>,
    arguments: Vec<OsString>,
    time: plan::Time,
    dispatch: Mutex<Option<Dispatch>>,
    receipt: Mutex<Option<Arc<CaptureReceipt>>>,
    outcome: Mutex<Option<Outcome>>,
    io_observed: AtomicBool,
    io_closed: AtomicBool,
    begun: AtomicBool,
    invalid: AtomicBool,
}
struct Dispatch {
    start: Instant,
    latest: Instant,
    last: Instant,
    cleanup: bool,
}
struct Outcome {
    completed: bool,
    code: Option<u8>,
    timed_out: bool,
    interrupted: Option<u8>,
    group_settled: bool,
}
impl Outcome {
    fn retryable(&self, error: u8) -> bool {
        self.group_settled
            && self.interrupted.is_none()
            && ((self.timed_out && error == 124)
                || (!self.timed_out && self.completed && self.code == Some(error)))
    }
}
impl std::fmt::Debug for Invocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ContainerBenchmarkInvocation")
    }
}
impl Invocation {
    fn fence(&self, settled: bool) -> Result<()> {
        let now = self.time.now();
        let mut dispatch = self.dispatch.lock().map_err(|_| failure(1))?;
        let dispatch = dispatch.as_mut().ok_or_else(|| failure(1))?;
        if now < dispatch.start || now < dispatch.last {
            return Err(failure(1));
        }
        dispatch.last = now;
        if settled {
            return self.time.fence(dispatch.cleanup);
        }
        if now > dispatch.latest {
            return Err(failure(124));
        }
        Ok(())
    }
    pub(in crate::ci) fn begin(&self, command: &Command) -> Result<()> {
        if self.begun.swap(true, Ordering::SeqCst) {
            self.invalid.store(true, Ordering::SeqCst);
            self.observed_io(None);
            return Err(failure(1));
        }
        if command.arguments != self.arguments
            || !matches!(
                (self.role, command.program),
                (Role::Docker, Program::Docker) | (Role::Curl, Program::BenchmarkCurl)
            )
            || (self.role == Role::Docker && !plan::profile(command))
            || (self.role == Role::Curl && !curl_profile(command))
        {
            self.invalid.store(true, Ordering::SeqCst);
            return Err(failure(1));
        }
        let output = command
            .benchmark_output
            .as_ref()
            .ok_or_else(|| failure(1))?;
        output.claim()?;
        *self.receipt.lock().map_err(|_| failure(1))? = Some(output.clone());
        let latest = self
            .time
            .latest(command.timeout, command.grace, command.cleanup)?;
        let start = self.time.now();
        *self.dispatch.lock().map_err(|_| failure(1))? = Some(Dispatch {
            start,
            latest,
            last: start,
            cleanup: command.cleanup,
        });
        self.fence(false)
    }
    pub(in crate::ci) fn observed_io(&self, value: Option<IoSeals>) {
        if self.io_observed.swap(true, Ordering::SeqCst) {
            self.invalid.store(true, Ordering::SeqCst);
        }
        let closed = value
            .as_ref()
            .is_some_and(|s| s.paired.is_some() ^ s.combined.is_some())
            && !self.invalid.load(Ordering::SeqCst);
        self.io_closed.store(closed, Ordering::SeqCst);
        if let Ok(receipt) = self.receipt.lock()
            && let Some(receipt) = receipt.as_ref()
        {
            receipt.observed(if closed { value } else { None });
        }
    }
    pub(in crate::ci) fn outcome(
        &self,
        completed: bool,
        code: Option<u8>,
        timed_out: bool,
        interrupted: Option<u8>,
        group_settled: bool,
    ) {
        if let Ok(mut outcome) = self.outcome.lock() {
            if outcome.is_some() || !self.begun.load(Ordering::SeqCst) {
                self.invalid.store(true, Ordering::SeqCst);
                return;
            }
            *outcome = Some(Outcome {
                completed,
                code,
                timed_out,
                interrupted,
                group_settled,
            });
        }
    }
    /// Only ordinary observed Curl failures may be retried. Source/capture,
    /// namespace, group, input and reader settlement have separate mandatory
    /// gates; an error code alone cannot turn missing process facts positive.
    pub(super) fn retryable(&self, error: u8) -> bool {
        if self.role != Role::Curl
            || error == 0
            || matches!(error, 130 | 143)
            || self.invalid.load(Ordering::SeqCst)
            || !self.io_closed.load(Ordering::SeqCst)
            || self
                .validate_settled(Program::BenchmarkCurl, &self.arguments)
                .is_err()
        {
            return false;
        }
        self.outcome
            .lock()
            .ok()
            .is_some_and(|v| v.as_ref().is_some_and(|v| v.retryable(error)))
    }
    pub(in crate::ci) fn validate(&self, program: Program, args: &[OsString]) -> Result<&Path> {
        self.check(program, args, false)
    }
    pub(super) fn validate_settled(&self, program: Program, args: &[OsString]) -> Result<()> {
        let result = self.check(program, args, true).map(|_| ());
        if result.is_err() {
            self.invalid.store(true, Ordering::SeqCst);
        }
        result
    }
    fn check(&self, program: Program, args: &[OsString], settled: bool) -> Result<&Path> {
        if !self.begun.load(Ordering::SeqCst)
            || self.invalid.load(Ordering::SeqCst)
            || args != self.arguments
            || !matches!(
                (self.role, program),
                (Role::Docker, Program::Docker) | (Role::Curl, Program::BenchmarkCurl)
            )
        {
            return Err(failure(1));
        }
        self.fence(settled)?;
        self.context.config.verify().map_err(|_| failure(1))?;
        if let Some(curl) = &self.curl {
            curl.validate()?;
        }
        if let Some(namespace) = &self.namespace {
            namespace.check_original()?;
        }
        let path = self.selected.validate()?;
        self.fence(settled)?;
        Ok(path)
    }
    pub(in crate::ci) fn context(
        &self,
        program: Program,
        args: &[OsString],
    ) -> Result<(&Path, &Environment)> {
        self.validate(program, args)?;
        let discovery = args
            == [
                "context",
                "inspect",
                "--format",
                "{{.Endpoints.docker.Host}}",
            ]
            .map(OsString::from);
        let server = args
            .windows(2)
            .any(|v| v == [OsString::from("--env"), "METRICS_TOKEN".into()]);
        let env = if discovery {
            &self.context.discovery
        } else if self.role == Role::Docker && server {
            &self.context.server
        } else {
            &self.context.frozen
        };
        Ok((&self.context.caller, env))
    }
    pub(super) fn belongs(&self, tools: &Tools) -> bool {
        Arc::ptr_eq(&self.context, &tools.context)
    }
}
pub(super) struct Tools {
    docker: Arc<Tool>,
    nsenter: Arc<Tool>,
    curl: Arc<Tool>,
    context: Arc<Context>,
    time: plan::Time,
    budget: Budget,
}
impl Tools {
    pub fn new(
        caller: &Path,
        env: &Environment,
        time: plan::Time,
        token: &str,
        docker_config: &Path,
        budget: Budget,
    ) -> Result<Self> {
        Self::with_uid(caller, env, time, token, docker_config, budget, 0)
    }
    fn with_uid(
        caller: &Path,
        env: &Environment,
        time: plan::Time,
        token: &str,
        docker_config: &Path,
        budget: Budget,
        uid: u32,
    ) -> Result<Self> {
        if token.len() != 64
            || !token
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(failure(1));
        }
        let caller = caller.canonicalize().map_err(|_| failure(1))?;
        let permit = budget.reserve(namespace::count(docker_config)? + 10)?;
        let config = Directory::open(docker_config).map_err(|_| failure(1))?;
        let setup = (|| {
            let m = config.file().metadata().map_err(|_| failure(1))?;
            if m.uid() != uid || m.mode() & 0o7777 != 0o700 {
                return Err(failure(1));
            }
            let mut frozen: Environment = [
                ("PATH", "/usr/sbin:/usr/bin:/sbin:/bin"),
                ("LANG", "C"),
                ("LC_ALL", "C"),
            ]
            .map(|(k, v)| (k.into(), v.into()))
            .into();
            frozen.insert("DOCKER_CONFIG".into(), docker_config.as_os_str().into());
            let mut discovery: Environment = [
                ("PATH", "/usr/sbin:/usr/bin:/sbin:/bin"),
                ("LANG", "C"),
                ("LC_ALL", "C"),
            ]
            .map(|(k, v)| (k.into(), v.into()))
            .into();
            for key in ["HOME", "DOCKER_CONFIG", "DOCKER_CONTEXT"] {
                if let Some(value) = env.get(OsStr::new(key)) {
                    discovery.insert(key.into(), value.clone());
                }
            }
            let mut server = frozen.clone();
            server.insert("METRICS_TOKEN".into(), token.into());
            Ok((frozen, discovery, server))
        })();
        let (frozen, discovery, server) = match setup {
            Ok(value) => value,
            Err(error) => {
                let _ = config.finish();
                return Err(error);
            }
        };
        let mut selected = Vec::new();
        for name in ["docker", "nsenter", "curl"] {
            match Tool::select(&caller, env, name, uid) {
                Ok(tool) => selected.push(tool),
                Err(error) => {
                    for tool in selected {
                        let _ = tool.finish();
                    }
                    let _ = config.finish();
                    return Err(error);
                }
            }
        }
        let mut selected = selected.into_iter();
        Ok(Self {
            docker: Arc::new(selected.next().unwrap()),
            nsenter: Arc::new(selected.next().unwrap()),
            curl: Arc::new(selected.next().unwrap()),
            context: Arc::new(Context {
                caller,
                discovery,
                frozen,
                server,
                config,
                _permit: permit,
            }),
            time,
            budget,
        })
    }
    fn make(&self, command: &Command, namespace: Option<Arc<Lease>>) -> Result<Arc<Invocation>> {
        let role = if command.program == Program::Docker
            && plan::allowed(command.program, &command.arguments)
        {
            Role::Docker
        } else if command.program == Program::BenchmarkCurl && namespace.is_some() {
            Role::Curl
        } else {
            return Err(failure(1));
        };
        Ok(Arc::new(Invocation {
            role,
            selected: if role == Role::Docker {
                self.docker.clone()
            } else {
                self.nsenter.clone()
            },
            curl: (role == Role::Curl).then(|| self.curl.clone()),
            namespace,
            context: self.context.clone(),
            arguments: command.arguments.clone(),
            time: self.time.clone(),
            dispatch: Mutex::new(None),
            receipt: Mutex::new(None),
            outcome: Mutex::new(None),
            io_observed: AtomicBool::new(false),
            io_closed: AtomicBool::new(false),
            begun: AtomicBool::new(false),
            invalid: AtomicBool::new(false),
        }))
    }
    pub fn invocation(&self, command: &Command) -> Result<Arc<Invocation>> {
        self.make(command, None)
    }
    pub fn curl_path(&self) -> Result<&Path> {
        self.curl.validate()
    }
    pub fn budget(&self) -> Budget {
        self.budget.clone()
    }
    pub fn curl_arguments(&self, ns: &Arc<Lease>, endpoint: Endpoint) -> Result<Vec<OsString>> {
        let mut args = vec![
            OsString::from(format!(
                "--net={}",
                ns.descriptor_path()?.to_str().ok_or_else(|| failure(1))?
            )),
            "--".into(),
            self.curl_path()?.as_os_str().to_owned(),
        ];
        args.extend(
            [
                "--disable",
                "--config",
                "-",
                "--noproxy",
                "*",
                "--proto",
                "=http",
                "--fail",
                "--silent",
                "--show-error",
                "--connect-timeout",
                "1",
                "--max-time",
                "2",
                "--max-filesize",
                "65536",
            ]
            .map(OsString::from),
        );
        args.push(format!("http://127.0.0.1:3000{}", endpoint.path()).into());
        Ok(args)
    }
    pub fn curl_invocation(&self, command: &Command, ns: Arc<Lease>) -> Result<Arc<Invocation>> {
        if command.arguments != self.curl_arguments(&ns, Endpoint::Ready)?
            && command.arguments != self.curl_arguments(&ns, Endpoint::Metrics)?
        {
            return Err(failure(1));
        }
        self.make(command, Some(ns))
    }
    pub fn finish(self) -> Result<()> {
        let mut result = Ok(());
        for tool in [self.docker, self.nsenter, self.curl] {
            let closed = match Arc::try_unwrap(tool) {
                Ok(tool) => tool.finish(),
                Err(_) => Err(failure(1)),
            };
            result = result.and(closed);
        }
        let context = match Arc::try_unwrap(self.context) {
            Ok(value) => value.config.finish().map_err(|_| failure(1)),
            Err(_) => Err(failure(1)),
        };
        result.and(context)
    }
}
fn curl_profile(c: &Command) -> bool {
    c.program == Program::BenchmarkCurl
        && !c.timeout.is_zero()
        && c.timeout <= std::time::Duration::from_secs(4)
        && c.grace == std::time::Duration::from_secs(1)
        && c.capture.is_none()
        && c.stdout.is_none()
        && c.combined_log.is_none()
        && c.stdin.is_some()
        && c.separate_logs.as_ref().is_some_and(|l| {
            l.stdout.maximum() == plan::CAPTURE && l.stderr.maximum() == plan::CAPTURE
        })
}

#[cfg(test)]
mod tests {
    use super::super::{namespace::fixture::Tree, native::Adapter};
    use super::*;
    use crate::ci::process::{self, Executor, Native};
    use crate::{
        ci::{bounded_log::Log, command::SeparateLogs},
        owned_process::Signals,
    };
    use std::{
        fs,
        os::unix::fs::{DirBuilderExt, PermissionsExt},
        time::Duration,
    };

    fn setup(script: &[u8]) -> (Tree, Tools, Budget) {
        let tree = Tree::new();
        let bin = tree.path.join("bin");
        fs::create_dir(&bin).unwrap();
        for name in ["docker", "nsenter", "curl"] {
            let path = bin.join(name);
            fs::write(&path, script).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let config = tree.path.join("config");
        fs::DirBuilder::new().mode(0o700).create(&config).unwrap();
        let env: Environment = [
            ("PATH".into(), bin.into_os_string()),
            ("GH_TOKEN".into(), "PRIVATE".into()),
            ("HTTP_PROXY".into(), "PRIVATE".into()),
            (
                "DOCKER_CONFIG".into(),
                tree.path.join("original-config").into_os_string(),
            ),
        ]
        .into();
        let budget = Budget::default();
        let tools = Tools::with_uid(
            &tree.path,
            &env,
            plan::Time::new(Instant::now()),
            &"c".repeat(64),
            &config,
            budget.clone(),
            tree.path.metadata().unwrap().uid(),
        )
        .unwrap();
        (tree, tools, budget)
    }
    fn command(tree: &Tree, discovery: bool, name: &str) -> Command {
        let mut command = if discovery {
            Command::capture(
                Program::Docker,
                &[
                    "context",
                    "inspect",
                    "--format",
                    "{{.Endpoints.docker.Host}}",
                ],
            )
        } else {
            plan::command(
                "unix:///fixed.sock",
                &["info", "--format", "{{.ID}}"],
                false,
            )
        };
        command.capture = None;
        command.timeout = Duration::from_secs(5);
        command.grace = Duration::from_secs(1);
        command.separate_logs = Some(Arc::new(SeparateLogs {
            stdout: Log::start(
                tree.private_file(&format!("{name}.out"), b""),
                if discovery { 4096 } else { plan::CAPTURE },
            )
            .unwrap(),
            stderr: Log::start(
                tree.private_file(&format!("{name}.err"), b""),
                plan::CAPTURE,
            )
            .unwrap(),
        }));
        command.benchmark_output = Some(Arc::new(CaptureReceipt::new()));
        command
    }
    #[tokio::test]
    async fn final_profile_environment_and_sticky_reuse_are_closed_before_dispatch() {
        let (tree, tools, budget) = setup(b"#!/bin/sh\nexit 0\n");
        for discovery in [true, false] {
            let request = command(
                &tree,
                discovery,
                if discovery { "discovery" } else { "frozen" },
            );
            let proof = tools.invocation(&request).unwrap();
            proof.begin(&request).unwrap();
            let (cwd, env) = proof.context(request.program, &request.arguments).unwrap();
            assert_eq!(cwd, tree.path);
            assert!(!env.contains_key(OsStr::new("GH_TOKEN")));
            assert!(!env.contains_key(OsStr::new("HTTP_PROXY")));
            assert!(!env.contains_key(OsStr::new("METRICS_TOKEN")));
            assert_eq!(
                env.get(OsStr::new("DOCKER_CONFIG")).unwrap(),
                &tree
                    .path
                    .join(if discovery {
                        "original-config"
                    } else {
                        "config"
                    })
                    .into_os_string()
            );
            assert!(
                proof
                    .validate(Program::BenchmarkCurl, &request.arguments)
                    .is_err()
            );
            assert!(proof.begin(&request).is_err());
            assert!(proof.validate(request.program, &request.arguments).is_err());
            process::settle_unforwarded(request).await.unwrap();
            drop(proof);
        }
        let mut request = command(&tree, false, "changed");
        let proof = tools.invocation(&request).unwrap();
        request.arguments.push("foreign".into());
        assert!(proof.begin(&request).is_err());
        request.arguments.pop();
        assert!(proof.begin(&request).is_err());
        process::settle_unforwarded(request).await.unwrap();
        drop(proof);
        tools.finish().unwrap();
        assert_eq!(budget.count(), 0);
    }
    #[tokio::test]
    async fn selected_tool_replacement_and_late_admission_refuse_without_adopting_paths() {
        let (tree, tools, budget) = setup(b"#!/bin/sh\nexit 0\n");
        let request = command(&tree, false, "replace");
        let proof = tools.invocation(&request).unwrap();
        proof.begin(&request).unwrap();
        let path = tree.path.join("bin/docker");
        fs::rename(&path, path.with_extension("original")).unwrap();
        fs::write(&path, b"FOREIGN").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(proof.validate(request.program, &request.arguments).is_err());
        process::settle_unforwarded(request).await.unwrap();
        drop(proof);
        assert!(tools.finish().is_err());
        assert_eq!(budget.count(), 0);
        assert_eq!(fs::read(path).unwrap(), b"FOREIGN");
        let (tree, tools, budget) = setup(b"#!/bin/sh\nexit 0\n");
        let request = command(&tree, false, "late");
        let proof = tools.invocation(&request).unwrap();
        tools.time.set(Duration::from_secs(1115));
        assert_eq!(proof.begin(&request).unwrap_err().code, 124);
        process::settle_unforwarded(request).await.unwrap();
        drop(proof);
        tools.finish().unwrap();
        assert_eq!(budget.count(), 0);
    }
    #[tokio::test]
    async fn actual_inert_selected_docker_ignores_outer_path_and_preserves_exit_and_both_logs() {
        let (tree, tools, budget) =
            setup(b"#!/bin/sh\nprintf '%s\\n' \"$PWD\"\nprintf 'private stderr\\n' >&2\nexit 42\n");
        let wrong = tree.path.join("wrong");
        fs::create_dir(&wrong).unwrap();
        let poison = tree.path.join("poison");
        fs::create_dir(&poison).unwrap();
        fs::write(poison.join("docker"), b"#!/bin/sh\nexit 99\n").unwrap();
        fs::set_permissions(poison.join("docker"), fs::Permissions::from_mode(0o755)).unwrap();
        let environment: Environment = [("PATH".into(), poison.into_os_string())].into();
        let mut native = Native::new(&wrong, &environment, Signals::new().unwrap());
        let failure = {
            let mut adapter = Adapter::new(&mut native, &tools);
            let failure = adapter
                .run(command(&tree, false, "native"))
                .await
                .unwrap_err();
            assert!(adapter.cleanup_passed());
            failure
        };
        assert_eq!(failure.code, 42);
        drop(native);
        assert_eq!(
            fs::read_to_string(tree.path.join("native.out")).unwrap(),
            format!("{}\n", tree.path.display())
        );
        assert_eq!(
            fs::read(tree.path.join("native.err")).unwrap(),
            b"private stderr\n"
        );
        tools.finish().unwrap();
        assert_eq!(budget.count(), 0);
    }

    #[tokio::test]
    async fn actual_inert_curl_keeps_the_namespace_lease_and_regular_input_through_settlement() {
        use super::super::io::Namespace as _;
        use std::os::unix::fs::OpenOptionsExt;

        // Both selected scripts are inert: the first validates only the
        // namespace argument's spelling, then execs the second without opening
        // that path. No setns, socket, network, curl or namespace tool runs.
        const SCRIPT: &[u8] = br##"#!/bin/sh
printf 'entered\n' >> inert-executed
case "$1" in
  --net=/proc/*/fd/*)
    [ "$2" = -- ] || exit 90
    shift 2
    exec "$@"
    ;;
  --disable)
    IFS= read -r config || exit 91
    [ "$config" = 'header = "Authorization: Bearer PRIVATE"' ] || exit 92
    printf '{"status":"ready"}\n'
    printf 'private curl stderr\n' >&2
    ;;
  *) exit 93 ;;
esac
"##;
        for replace in [false, true] {
            let (tree, tools, budget) = setup(SCRIPT);
            let lease = Arc::new(Lease::open(&tree.roots, &tree.processes[0], &budget).unwrap());
            let alias = lease.clone();
            let input_path = tree.path.join("input");
            nix::unistd::close(
                tree.private_file("input", b"header = \"Authorization: Bearer PRIVATE\"\n"),
            )
            .unwrap();
            let input = fs::OpenOptions::new()
                .read(true)
                .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
                .open(&input_path)
                .unwrap();
            fs::remove_file(input_path).unwrap();
            let mut request = command(&tree, false, "curl");
            request.program = Program::BenchmarkCurl;
            request.timeout = Duration::from_secs(4);
            request.arguments = tools.curl_arguments(&lease, Endpoint::Ready).unwrap();
            request.stdin = Some(Arc::new(input));
            request.benchmark = Some(tools.curl_invocation(&request, lease.clone()).unwrap());
            if replace {
                fs::write(
                    tree.roots.proc.join("321/stat"),
                    super::super::namespace::fixture::stat(321, 100),
                )
                .unwrap();
            }
            let environment = Environment::new();
            let mut native = Native::new(&tree.path, &environment, Signals::new().unwrap());
            let result = Adapter::new(&mut native, &tools).run(request).await;
            assert!(native.cleanup_passed());
            drop(native);
            if replace {
                assert_eq!(result.unwrap_err().code, 1);
                assert!(!tree.path.join("inert-executed").exists());
                assert!(fs::read(tree.path.join("curl.out")).unwrap().is_empty());
                assert!(lease.finish().is_err());
            } else {
                result.unwrap();
                assert_eq!(
                    fs::read(tree.path.join("curl.out")).unwrap(),
                    b"{\"status\":\"ready\"}\n"
                );
                assert_eq!(
                    fs::read(tree.path.join("curl.err")).unwrap(),
                    b"private curl stderr\n"
                );
                assert_eq!(
                    fs::read(tree.path.join("inert-executed")).unwrap(),
                    b"entered\nentered\n"
                );
                lease.check(&tree.processes[0]).unwrap();
                lease.finish().unwrap();
            }
            assert!(alias.check_original().is_err());
            drop(alias);
            drop(lease);
            tools.finish().unwrap();
            assert_eq!(budget.count(), 0);
        }
    }

    #[tokio::test]
    async fn reusing_a_receipt_cannot_import_a_later_completion_from_the_first_invocation() {
        let (tree, tools, budget) = setup(b"#!/bin/sh\nexit 0\n");
        let mut first = command(&tree, false, "first-receipt");
        let receipt = first.benchmark_output.as_ref().unwrap().clone();
        let first_proof = tools.invocation(&first).unwrap();
        first_proof.begin(&first).unwrap();
        let mut second = command(&tree, false, "second-receipt");
        second.benchmark_output = Some(receipt.clone());
        let second_proof = tools.invocation(&second).unwrap();
        assert!(second_proof.begin(&second).is_err());
        let logs = Arc::try_unwrap(first.separate_logs.take().unwrap()).unwrap();
        let stdout = logs.stdout.finish().await.unwrap();
        let stderr = logs.stderr.finish().await.unwrap();
        let seal =
            |v: crate::ci::bounded_log::Finished| crate::ci::ops_artifact::verify::tools::Seal {
                sha256: v.sha256,
                bytes: v.bytes,
                metadata: v.metadata,
            };
        first_proof.observed_io(Some(IoSeals {
            paired: Some(crate::ci::ops_artifact::verify::tools::Captures {
                stdout: seal(stdout),
                stderr: seal(stderr),
            }),
            combined: None,
        }));
        assert!(receipt.get().is_err());
        process::settle_unforwarded(first).await.unwrap();
        process::settle_unforwarded(second).await.unwrap();
        drop(first_proof);
        drop(second_proof);
        tools.finish().unwrap();
        assert_eq!(budget.count(), 0);
    }

    #[tokio::test]
    async fn actual_native_seals_refuse_postfinish_same_length_rewrites_and_preserve_known_status()
    {
        use std::io::{Seek, SeekFrom, Write};

        for (status, mutate) in [
            (0, false),
            (0, true),
            (42, true),
            (124, true),
            (130, true),
            (143, true),
        ] {
            let script = format!(
                concat!(
                    "#!/bin/sh\nprintf 'owned-metadata\\n'\n",
                    "printf 'private stderr\\n' >&2\nexit {}\n"
                ),
                status
            );
            let (tree, tools, budget) = setup(script.as_bytes());
            let logs_permit = budget.reserve(14).unwrap();
            let request = command(&tree, false, "sealed");
            let receipt = request.benchmark_output.as_ref().unwrap().clone();
            let path = tree.path.join("sealed.out");
            let read_permit = budget.reserve(1).unwrap();
            let reader = File::open(&path).unwrap();
            let write_permit = budget.reserve(1).unwrap();
            let mut writer = fs::OpenOptions::new().write(true).open(&path).unwrap();
            fs::remove_file(path).unwrap();
            fs::remove_file(tree.path.join("sealed.err")).unwrap();
            let initial = reader.metadata().unwrap();
            let environment = Environment::new();
            let mut native = Native::new(&tree.path, &environment, Signals::new().unwrap());
            let result = Adapter::new(&mut native, &tools).run(request).await;
            assert!(native.cleanup_passed());
            drop(native);
            let seals = receipt.get().unwrap();
            assert!(receipt.get().is_err());
            assert!(seals.combined.is_none());
            let seals = seals.paired.unwrap();
            assert_eq!(seals.stdout.bytes, b"owned-metadata\n".len() as u64);
            assert_eq!(seals.stderr.bytes, b"private stderr\n".len() as u64);
            if mutate {
                // The Native-owned original output FD has already finished and
                // closed. This deliberately retained test alias changes that
                // same unlinked inode, preserving its length and private mode.
                writer.seek(SeekFrom::Start(0)).unwrap();
                writer.write_all(b"OWNED-METADATA\n").unwrap();
                writer.sync_all().unwrap();
                assert_eq!(writer.metadata().unwrap().len(), seals.stdout.bytes);
            }
            nix::unistd::close(writer).unwrap();
            drop(write_permit);
            let readback = super::super::files::read_sealed_test(
                reader,
                initial,
                read_permit,
                plan::CAPTURE,
                &seals.stdout,
            );
            if mutate {
                assert_eq!(readback.unwrap_err().code, 1);
            } else {
                assert_eq!(readback.unwrap(), b"owned-metadata\n");
            }
            if status == 0 {
                result.unwrap();
            } else {
                assert_eq!(result.unwrap_err().code, status);
            }
            drop(logs_permit);
            tools.finish().unwrap();
            assert_eq!(budget.count(), 0);
        }
    }

    #[tokio::test]
    async fn settled_cli_uses_the_entry_fence_without_reapplying_its_dispatch_cutoff() {
        struct Advance<'a, E> {
            inner: &'a mut E,
            time: plan::Time,
            after: Duration,
        }
        impl<E: Executor> Executor for Advance<'_, E> {
            async fn run(&mut self, request: Command) -> Result<Vec<u8>> {
                let result = self.inner.run(request).await;
                // Advance only after the actual inert child's group, pipes,
                // both logs and all Native-owned input/output have settled.
                self.time.set(self.after);
                result
            }
            async fn ready(&mut self) -> Result<()> {
                self.inner.ready().await
            }
            fn cleanup_passed(&self) -> bool {
                self.inner.cleanup_passed()
            }
        }
        for (cleanup, status, start, after, expected) in [
            (true, 0, 90, 116, 0),
            (true, 0, 90, 1260, 124),
            (true, 42, 90, 1260, 42),
            (false, 0, 1100, 1130, 0),
            (false, 0, 1100, 1140, 124),
        ] {
            let script = format!("#!/bin/sh\nprintf 'settled\\n'\nexit {status}\n");
            let (tree, mut tools, budget) = setup(script.as_bytes());
            tools.time = if cleanup {
                plan::Time::recovery(Instant::now())
            } else {
                plan::Time::new(Instant::now())
            };
            tools.time.set(Duration::from_secs(start));
            if cleanup {
                tools.time.begin_retirement().unwrap();
            }
            let mut request = command(&tree, false, "settled");
            request.cleanup = cleanup;
            // 1260 - 1160 gives latest dispatch100, while the admitted
            // info slot's26s settlement leaves1134s after a finish at116.
            assert_eq!(
                tools
                    .time
                    .latest(request.timeout, request.grace, cleanup)
                    .unwrap(),
                tools.time.now() + Duration::from_secs(if cleanup { 10 } else { 14 })
            );
            let environment = Environment::new();
            let mut native = Native::new(&tree.path, &environment, Signals::new().unwrap());
            let result = {
                let mut advance = Advance {
                    inner: &mut native,
                    time: tools.time.clone(),
                    after: Duration::from_secs(after),
                };
                Adapter::new(&mut advance, &tools).run(request).await
            };
            assert!(native.cleanup_passed());
            drop(native);
            assert_eq!(result.err().map_or(0, |e| e.code), expected);
            assert_eq!(
                fs::read(tree.path.join("settled.out")).unwrap(),
                b"settled\n"
            );
            tools.finish().unwrap();
            assert_eq!(budget.count(), 0);
        }
    }

    #[tokio::test]
    async fn only_actual_settled_curl_failures_supply_retry_evidence() {
        use super::super::io::Namespace as _;

        for (mode, retry) in [
            ("exit1", true),
            ("exit7", true),
            ("exit124", true),
            ("exit143", false),
            ("timeout", true),
            ("preflight", false),
            ("overflow", false),
        ] {
            let body = match mode {
                "exit1" => "exit 1",
                "exit7" => "exit 7",
                "exit124" => "exit 124",
                "exit143" => "exit 143",
                "timeout" => "exec /bin/sleep 30",
                "preflight" => "exit 7",
                "overflow" => concat!(
                    "trap '' PIPE\ni=0\nwhile [ \"$i\" -lt 10000 ]; do\n",
                    "  printf '0123456789abcdef\\n' || break\n  i=$((i+1))\ndone\nexit 7"
                ),
                _ => unreachable!(),
            };
            let script = format!(
                concat!(
                    "#!/bin/sh\ncase \"$1\" in\n",
                    "  --net=/proc/*/fd/*) shift 2; exec \"$@\" ;;\nesac\n",
                    "printf 'inert curl stderr\\n' >&2\n{}\n"
                ),
                body
            );
            let (tree, tools, budget) = setup(script.as_bytes());
            let lease = Arc::new(Lease::open(&tree.roots, &tree.processes[0], &budget).unwrap());
            let mut request = command(&tree, false, "retry");
            request.program = Program::BenchmarkCurl;
            request.timeout = if mode == "timeout" {
                Duration::from_millis(100)
            } else {
                Duration::from_secs(4)
            };
            request.arguments = tools.curl_arguments(&lease, Endpoint::Ready).unwrap();
            nix::unistd::close(tree.private_file("retry-input", b"inert config\n")).unwrap();
            let input_path = tree.path.join("retry-input");
            let input = File::open(&input_path).unwrap();
            fs::remove_file(input_path).unwrap();
            request.stdin = Some(Arc::new(input));
            let proof = tools.curl_invocation(&request, lease.clone()).unwrap();
            request.benchmark = Some(proof.clone());
            if mode == "preflight" {
                fs::write(
                    tree.roots.proc.join("321/stat"),
                    super::super::namespace::fixture::stat(321, 100),
                )
                .unwrap();
            }
            let environment = Environment::new();
            let mut native = Native::new(&tree.path, &environment, Signals::new().unwrap());
            let error = Adapter::new(&mut native, &tools)
                .run(request)
                .await
                .unwrap_err();
            assert!(native.cleanup_passed());
            assert_eq!(proof.retryable(error.code), retry, "{mode}: {}", error.code);
            if mode == "timeout" || mode == "exit124" {
                assert_eq!(error.code, 124);
            }
            assert!(!proof.retryable(0));
            assert!(!proof.retryable(130));
            assert!(!proof.retryable(143));
            if mode == "exit7" {
                proof.outcome(true, Some(7), false, None, true);
                assert!(!proof.retryable(error.code));
            }
            drop(native);
            let closed = lease.finish();
            assert_eq!(closed.is_ok(), mode != "preflight");
            assert!(!proof.retryable(error.code));
            drop(proof);
            drop(lease);
            tools.finish().unwrap();
            assert_eq!(budget.count(), 0);
        }
    }

    #[test]
    fn reaped_exit_cannot_convert_an_internal_failure_into_a_retryable_exit() {
        for (completed, timed_out, error, expected) in [
            (false, false, 1, false),
            (true, false, 1, true),
            (false, true, 124, true),
            (false, true, 1, false),
        ] {
            let outcome = Outcome {
                completed,
                code: Some(1),
                timed_out,
                interrupted: None,
                group_settled: true,
            };
            assert_eq!(outcome.retryable(error), expected);
        }
    }
}
