//! Build or export one unsigned production release through an original owner.
//!
//! Run from a clean checkout root with a configured local Unix Docker endpoint.
//! The output image is Linux/amd64; the controller supports Linux and macOS.
//! No signing, registry push, host installation or running-container change occurs.

#[path = "build_control.rs"]
mod control;
#[path = "build_files.rs"]
mod files;
#[path = "build_guard.rs"]
mod guard;
#[path = "build_licenses.rs"]
mod licenses;
#[path = "build_options.rs"]
mod options;
#[path = "build_process.rs"]
mod process;
#[path = "build_source.rs"]
mod source;

use control::{Failure, Owner, Result};
use files::{Dir, Output};
use guard::Guards;
use options::Options;
use process::{Completion, Invocation, Role};
use source::{Environment, Tool, Tree};
use std::{
    ffi::{OsStr, OsString},
    fs::Metadata,
    io::Write,
    os::fd::AsFd,
    os::unix::{fs::FileTypeExt, fs::MetadataExt},
    path::PathBuf,
    process::ExitCode,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

trait Executor {
    async fn run(&mut self, owner: &mut Owner, invocation: Invocation) -> Completion;
}
struct Native;
impl Executor for Native {
    async fn run(&mut self, owner: &mut Owner, invocation: Invocation) -> Completion {
        let capture = invocation.capture;
        let mut completed = process::run(owner, invocation).await;
        if capture && let Ok(report) = &mut completed.result {
            match std::str::from_utf8(&report.inspection) {
                Ok(value) => {
                    report.inspection = value.trim_matches(python_space).as_bytes().to_vec()
                }
                Err(_) => {
                    owner.record(Failure::Utf8);
                    completed.result = Err(Failure::Utf8);
                }
            }
        }
        completed
    }
}
#[cfg(test)]
struct FixtureInputs {
    pub socket: bool,
    pub guards: Option<(PathBuf, PathBuf)>,
}
struct Inputs {
    pub root: PathBuf,
    pub environment: Environment,
    #[cfg(test)]
    pub fixtures: Option<FixtureInputs>,
}
struct Attempt {
    pub result: Result<PathBuf>,
    pub output: Option<PathBuf>,
}

fn timestamp() -> Result<String> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Failure::Io)?
        .as_secs();
    let seconds = i64::try_from(seconds).map_err(|_| Failure::Io)?;
    Ok(chrono::DateTime::<chrono::Utc>::from_timestamp(seconds, 0)
        .ok_or(Failure::Io)?
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string())
}
fn text(bytes: &[u8]) -> Result<&str> {
    std::str::from_utf8(bytes).map_err(|_| Failure::Utf8)
}
fn python_space(character: char) -> bool {
    character.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&character)
}
fn docker_builder(value: &str) -> bool {
    let mut drivers = Vec::new();
    for offset in std::iter::once(0).chain(value.match_indices('\n').map(|(offset, _)| offset + 1))
    {
        let Some(tail) = value[offset..].strip_prefix("Driver:") else {
            continue;
        };
        if !tail.chars().next().is_some_and(python_space) {
            continue;
        }
        let tail = tail.trim_start_matches(python_space);
        let end = tail.find(python_space).unwrap_or(tail.len());
        if end == 0 {
            continue;
        }
        if tail[end..]
            .split('\n')
            .next()
            .unwrap_or_default()
            .chars()
            .all(python_space)
        {
            drivers.push(&tail[..end]);
        }
    }
    drivers == ["docker"]
}
fn decode_metadata(bytes: &[u8], maximum: usize) -> Result<crate::json::Json> {
    // The reference receives str from strict UTF-8 subprocess/read_text decoding.
    // Its json.loads(str) does not accept a BOM or auto-detect UTF-16/32 bytes.
    let decoded = std::str::from_utf8(bytes).map_err(|_| Failure::Utf8)?;
    if decoded.starts_with('\u{feff}') || bytes.contains(&0) {
        return Err(Failure::Docker);
    }
    crate::json::decode_limited(bytes, maximum).map_err(|_| Failure::Docker)
}
fn json(bytes: &[u8]) -> Result<crate::json::Json> {
    decode_metadata(bytes, 2 * 1024 * 1024)
}

struct Socket {
    alias: source::Alias,
    canonical: PathBuf,
    stamp: Metadata,
    parent: Dir,
}
impl Socket {
    fn open(endpoint: &str, owner: &mut Owner, end: Instant) -> Result<Self> {
        let alias = PathBuf::from(endpoint.strip_prefix("unix://").ok_or(Failure::Docker)?);
        let alias = source::Alias::open(&alias, owner, end)?;
        let canonical = alias.canonical().to_owned();
        let parent = Dir::open(canonical.parent().ok_or(Failure::Docker)?)?;
        let stamp = match canonical.symlink_metadata() {
            Ok(stamp) => stamp,
            Err(_) => {
                let _ = parent.finish();
                return Err(Failure::Docker);
            }
        };
        if !stamp.file_type().is_socket() {
            let _ = parent.finish();
            return Err(Failure::Docker);
        }
        Ok(Self {
            alias,
            canonical,
            stamp,
            parent,
        })
    }
    fn check(&self) -> Result<()> {
        self.parent.check()?;
        self.alias.check()?;
        let stamp = self
            .canonical
            .symlink_metadata()
            .map_err(|_| Failure::Changed)?;
        if !stamp.file_type().is_socket()
            || stamp.dev() != self.stamp.dev()
            || stamp.ino() != self.stamp.ino()
            || stamp.mode() != self.stamp.mode()
            || stamp.uid() != self.stamp.uid()
            || stamp.gid() != self.stamp.gid()
            || stamp.ctime() != self.stamp.ctime()
            || stamp.ctime_nsec() != self.stamp.ctime_nsec()
        {
            return Err(Failure::Changed);
        }
        Ok(())
    }
    fn finish(self) -> Result<()> {
        let checked = self.check();
        let closed = self.parent.finish();
        checked.and(closed)
    }
}

struct Controller<'a, E> {
    owner: &'a mut Owner,
    executor: &'a mut E,
    inputs: Inputs,
    root: Option<Dir>,
    git: Option<Tool>,
    docker: Option<Tool>,
    temporary: Option<Output>,
    output: Option<Output>,
    guards: Option<Guards>,
    tree: Option<Tree>,
    socket: Option<Socket>,
    endpoint: Option<String>,
    docker_environment: Option<Environment>,
    revision: Option<String>,
    sequence: usize,
    phase: Instant,
    active: bool,
    verification: Duration,
    started_at: String,
}
impl<E: Executor> Controller<'_, E> {
    fn check(&mut self) -> Result<()> {
        self.owner.before(self.phase)?;
        if let Some(root) = &self.root {
            root.check()?;
        }
        if let Some(tool) = &self.git {
            tool.check()?;
        }
        if let Some(tool) = &self.docker {
            tool.check()?;
        }
        if let Some(output) = &self.output {
            output.check()?;
        }
        if let Some(temporary) = &self.temporary {
            temporary.check()?;
        }
        if let Some(guards) = &self.guards {
            guards.check()?;
        }
        if let Some(socket) = &self.socket {
            socket.check()?;
        }
        if let Some(tree) = &self.tree {
            tree.check(self.owner, self.phase)?;
        }
        Ok(())
    }
    async fn command(
        &mut self,
        role: Role,
        arguments: Vec<OsString>,
        cap: Duration,
        capture: bool,
        allow_failure: bool,
    ) -> Result<(i32, Vec<u8>)> {
        self.check()?;
        let program = match role {
            Role::Git => self.git.as_ref().ok_or(Failure::Source)?.path(),
            Role::Docker => self.docker.as_ref().ok_or(Failure::Docker)?.path(),
        }
        .to_owned();
        let environment = if role == Role::Docker {
            self.docker_environment
                .as_ref()
                .unwrap_or(&self.inputs.environment)
        } else {
            &self.inputs.environment
        }
        .clone();
        let limit = if self.active {
            self.phase
        } else {
            self.phase
                .checked_sub(control::RETIRE_AND_DRAIN)
                .ok_or(Failure::Deadline)?
        };
        let future = if self.verification == Duration::ZERO {
            Duration::ZERO
        } else {
            self.verification
        };
        let body_end = self.owner.body(limit, cap, future)?;
        self.sequence += 1;
        let stem = format!(
            "{:02}-{}",
            self.sequence,
            if role == Role::Git { "git" } else { "docker" }
        );
        let log_name = format!("{stem}.log");
        let retained = self.output.is_some();
        let output = self
            .output
            .as_mut()
            .or(self.temporary.as_mut())
            .ok_or(Failure::Io)?;
        output.create_file(&log_name)?;
        if retained {
            let recorded_arguments: Vec<_> = std::iter::once(program.as_os_str())
                .chain(arguments.iter().map(OsString::as_os_str))
                .map(|value| value.to_string_lossy().into_owned())
                .collect();
            output.json(
                &format!("{stem}.command.json"),
                &serde_json::json!({
                    "argv": recorded_arguments,
                    "timeoutSeconds": cap.as_secs_f64(),
                }),
                self.owner,
                self.phase,
            )?;
        }
        self.check()?;
        let log = self
            .output
            .as_ref()
            .or(self.temporary.as_ref())
            .ok_or(Failure::Io)?
            .get(&log_name)?
            .writer()?;
        let cwd = if role == Role::Docker
            && arguments.get(2).is_some_and(|value| value == "buildx")
            && arguments.get(3).is_some_and(|value| value == "build")
        {
            self.tree.as_ref().ok_or(Failure::Source)?.path().to_owned()
        } else {
            self.inputs.root.clone()
        };
        let archive_output = role == Role::Docker
            && !capture
            && !allow_failure
            && arguments.len() == 7
            && arguments[2..6].iter().map(|value| value.to_str()).eq([
                Some("image"),
                Some("save"),
                Some("--platform"),
                Some("linux/amd64"),
            ]);
        let archive = if archive_output {
            let writer = self
                .output
                .as_ref()
                .ok_or(Failure::Io)
                .and_then(|output| output.get("image.tar"))
                .and_then(|file| file.writer());
            match writer {
                Ok(writer) => Some(writer),
                Err(error) => {
                    self.owner.record(error);
                    if let Err(error) = files::close(log) {
                        self.owner.record(error);
                    }
                    return Err(error);
                }
            }
        } else {
            None
        };
        let completion = self
            .executor
            .run(
                self.owner,
                Invocation {
                    role,
                    program,
                    arguments,
                    cwd,
                    environment,
                    body_end,
                    log,
                    archive,
                    capture,
                    allow_failure,
                },
            )
            .await;
        // Record an observed command refusal before pending signals or evidence writes.
        if let Err(error) = completion.result.as_ref() {
            self.owner.record(*error);
        }
        let output = self
            .output
            .as_mut()
            .or(self.temporary.as_mut())
            .ok_or(Failure::Io)?;
        if let Ok(report) = &completion.result
            && (completion.observation.exit_status != Some(report.status)
                || !completion.observation.group_retired
                || !completion.observation.reaped
                || !completion.observation.stdout_eof
                || !completion.observation.stderr_eof
                || !completion.observation.stdout_closed
                || !completion.observation.stderr_closed
                || !completion.observation.log_closed
                || !completion.observation.archive_closed
                || archive_output != completion.observation.archive.is_some()
                || completion.observation.log.as_ref().is_none_or(|seal| {
                    seal.bytes != report.log.bytes
                        || seal.sha256 != report.log.sha256
                        || !files::same(&seal.metadata, &report.log.metadata)
                }))
        {
            self.owner.record(Failure::Changed);
        }
        let sealed = output
            .get_mut(&log_name)?
            .seal_cleanup(self.owner, self.owner.end());
        if let Err(error) = sealed.as_ref() {
            self.owner.record(*error);
        }
        if let (Ok(digest), Some(observed)) = (&sealed, &completion.observation.log) {
            if digest != &observed.sha256
                || output.get(&log_name)?.length() != observed.bytes
                || !files::same(
                    &observed.metadata,
                    &output
                        .get(&log_name)?
                        .file()
                        .metadata()
                        .map_err(|_| Failure::Io)?,
                )
            {
                self.owner.record(Failure::Changed);
            }
        } else if completion.result.is_ok() {
            self.owner.record(Failure::Changed);
        }
        if archive_output {
            let sealed = output
                .get_mut("image.tar")?
                .seal_cleanup(self.owner, self.owner.end());
            if let Err(error) = sealed.as_ref() {
                self.owner.record(*error);
            }
            if let (Ok(digest), Some(observed)) = (&sealed, &completion.observation.archive) {
                let file = output.get("image.tar")?;
                let metadata = file.file().metadata().map_err(|_| Failure::Io);
                if let Err(error) = metadata.as_ref() {
                    self.owner.record(*error);
                }
                if digest != &observed.sha256
                    || file.length() != observed.bytes
                    || observed.bytes != completion.observation.stdout_bytes
                    || observed.sha256 != completion.observation.stdout_sha256
                    || !metadata
                        .as_ref()
                        .is_ok_and(|metadata| files::same(&observed.metadata, metadata))
                {
                    self.owner.record(Failure::Changed);
                }
            } else if completion.result.is_ok() {
                self.owner.record(Failure::Changed);
            }
        }
        if retained {
            let observation = &completion.observation;
            let cleanup_errors: Vec<_> = observation
                .cleanup_errors
                .iter()
                .map(|error| error.code())
                .collect();
            let written = output.json_cleanup(
                &format!("{stem}.outcome.json"),
                &serde_json::json!({
                    "pid": observation.pid,
                    "exitStatus": observation.exit_status,
                    "timedOut": observation.timed_out,
                    "outputLimited": observation.output_limited,
                    "elapsedSeconds": observation.elapsed.as_secs_f64(),
                    "cleanup": {
                        "groupRetired":observation.group_retired,
                        "reaped":observation.reaped,
                        "stdoutEof":observation.stdout_eof,
                        "stderrEof":observation.stderr_eof,
                        "stdoutClosed":observation.stdout_closed,
                        "stderrClosed":observation.stderr_closed,
                        "logClosed":observation.log_closed,
                        "archiveClosed":archive_output.then_some(observation.archive_closed),
                        "archiveBytes":observation.archive.as_ref().map(|seal| seal.bytes),
                        "archiveSha256":observation.archive.as_ref().map(|seal| &seal.sha256),
                        "archiveIdentity":observation.archive.as_ref().map(|seal| {
                            serde_json::json!({
                                "device":seal.metadata.dev(), "inode":seal.metadata.ino(),
                            })
                        }),
                        "stdoutBytes":observation.stdout_bytes,
                        "stderrBytes":observation.stderr_bytes,
                        "stdoutSha256":observation.stdout_sha256,
                        "stderrSha256":observation.stderr_sha256,
                    },
                    "cleanupError": cleanup_errors,
                }),
                self.owner,
                self.owner.end(),
            );
            if let Err(error) = written {
                self.owner.record(error);
            }
        } else if let Err(error) = output.discard(&log_name) {
            self.owner.record(error);
        }
        let report = completion.result?;
        self.check()?;
        Ok((report.status, report.inspection))
    }
    async fn git(
        &mut self,
        arguments: &[&str],
        capture: bool,
        allow: bool,
    ) -> Result<(i32, Vec<u8>)> {
        self.command(
            Role::Git,
            arguments.iter().map(OsString::from).collect(),
            Duration::from_secs(30),
            capture,
            allow,
        )
        .await
    }
    async fn docker(
        &mut self,
        arguments: Vec<OsString>,
        cap: Duration,
        capture: bool,
    ) -> Result<Vec<u8>> {
        let mut selected = vec![
            OsString::from("--host"),
            OsString::from(self.endpoint.as_ref().ok_or(Failure::Docker)?),
        ];
        selected.extend(arguments);
        self.command(Role::Docker, selected, cap, capture, false)
            .await
            .map(|(_, bytes)| bytes)
    }
    async fn probe(&mut self, arguments: &[&str]) -> Result<Vec<u8>> {
        self.docker(
            arguments.iter().map(OsString::from).collect(),
            Duration::from_secs(30),
            true,
        )
        .await
    }
    async fn clean(&mut self) -> Result<String> {
        let (_, revision) = self
            .git(&["rev-parse", "--verify", "HEAD"], true, false)
            .await?;
        let revision = source::revision(&revision)?;
        let (_, status) = self
            .git(
                &["status", "--porcelain=v1", "--untracked-files=all"],
                true,
                false,
            )
            .await?;
        if !text(&status)?.is_empty() {
            return Err(Failure::Source);
        }
        Ok(revision)
    }
    async fn preflight(&mut self, build: bool) -> Result<()> {
        let context = self.inputs.environment.get(OsStr::new("DOCKER_CONTEXT"));
        let supplied = self.inputs.environment.get(OsStr::new("DOCKER_HOST"));
        let endpoint = if context.is_none_or(|value| value.is_empty())
            && supplied.is_some_and(|value| !value.is_empty())
        {
            supplied
                .unwrap()
                .to_str()
                .ok_or(Failure::Docker)?
                .to_owned()
        } else {
            let (_, bytes) = self
                .command(
                    Role::Docker,
                    [
                        "context",
                        "inspect",
                        "--format",
                        "{{.Endpoints.docker.Host}}",
                    ]
                    .iter()
                    .map(OsString::from)
                    .collect(),
                    Duration::from_secs(30),
                    true,
                    false,
                )
                .await?;
            text(&bytes)?.to_owned()
        };
        if !endpoint.starts_with("unix:///")
            || endpoint.len() <= 8
            || endpoint.as_bytes().iter().any(|byte| *byte <= 32)
        {
            return Err(Failure::Docker);
        }
        #[cfg(test)]
        let fixture = self
            .inputs
            .fixtures
            .as_ref()
            .is_some_and(|fixture| fixture.socket);
        #[cfg(not(test))]
        let fixture = false;
        if !fixture {
            self.socket = Some(Socket::open(&endpoint, self.owner, self.phase)?);
        }
        self.endpoint = Some(endpoint);
        let mut environment = self.inputs.environment.clone();
        for name in [
            "DOCKER_CONTEXT",
            "DOCKER_HOST",
            "DOCKER_TLS_VERIFY",
            "DOCKER_CERT_PATH",
            "DOCKER_API_VERSION",
            "BUILDX_BUILDER",
            "BUILDKIT_HOST",
        ] {
            environment.remove(OsStr::new(name));
        }
        self.docker_environment = Some(environment);
        let help = self.probe(&["image", "save", "--help"]).await?;
        let platform = text(&help)?
            .split('\n')
            .filter(|line| {
                line.starts_with([' ', '\t'])
                    && line
                        .trim_start_matches([' ', '\t'])
                        .strip_prefix("--platform")
                        .is_some_and(|tail| tail.is_empty() || tail.starts_with([' ', '\t']))
            })
            .count();
        if platform != 1 {
            return Err(Failure::Docker);
        }
        let api = self
            .probe(&["version", "--format", "{{.Server.APIVersion}}"])
            .await?;
        let api = text(&api)?;
        let (major, minor) = api.split_once('.').ok_or(Failure::Docker)?;
        let component = |value: &str| -> Result<u16> {
            if value.is_empty()
                || value.len() > 3
                || (value.len() > 1 && value.starts_with('0'))
                || !value.bytes().all(|byte| byte.is_ascii_digit())
            {
                return Err(Failure::Docker);
            }
            value.parse().map_err(|_| Failure::Docker)
        };
        if (component(major)?, component(minor)?) < (1, 48) {
            return Err(Failure::Docker);
        }
        for label in [
            "com.docker.compose.project=simplestchat-public",
            "simplestchat.benchmark.run",
        ] {
            let running = self
                .probe(&["ps", "--quiet", "--filter", &format!("label={label}")])
                .await?;
            if !text(&running)?.is_empty() {
                return Err(Failure::Docker);
            }
        }
        if build {
            let builder = self.probe(&["buildx", "inspect", "default"]).await?;
            if !docker_builder(text(&builder)?) {
                return Err(Failure::Docker);
            }
        }
        Ok(())
    }
    async fn selected(&mut self, image: &str, tag: &str, revision: &str) -> Result<Vec<String>> {
        const FORMAT: &str = concat!(
            "{\"id\":{{json .Id}},\"os\":{{json .Os}},\"architecture\":{{json .Architecture}},",
            "\"user\":{{json .Config.User}},\"labels\":{{json .Config.Labels}},",
            "\"rootfs\":{{json .RootFS}},\"cmd\":{{json .Config.Cmd}},",
            "\"entrypoint\":{{json (index .Config \"Entrypoint\")}}}",
        );
        let inspection = self
            .probe(&["image", "inspect", "--format", FORMAT, image])
            .await?;
        let value = json(&inspection)?;
        if value.get("id").and_then(crate::json::Json::string) != Some(image)
            || value.get("os").and_then(crate::json::Json::string) != Some("linux")
            || value
                .get("architecture")
                .and_then(crate::json::Json::string)
                != Some("amd64")
            || value.get("user").and_then(crate::json::Json::string) != Some("10001:10001")
            || value
                .get("cmd")
                .and_then(crate::json::Json::array)
                .is_none_or(|values| {
                    values.len() != 1 || values[0].string() != Some("/app/simplestChat")
                })
            || !matches!(
                value.get("entrypoint"),
                Some(crate::json::Json::Null) | None
            ) && !value
                .get("entrypoint")
                .and_then(crate::json::Json::array)
                .is_some_and(|values| values.is_empty())
            || value
                .get("labels")
                .and_then(|value| value.get("org.opencontainers.image.revision"))
                .and_then(crate::json::Json::string)
                != Some(revision)
            || value
                .get("rootfs")
                .and_then(|value| value.get("Type"))
                .and_then(crate::json::Json::string)
                != Some("layers")
        {
            return Err(Failure::Docker);
        }
        let layers = value
            .get("rootfs")
            .and_then(|value| value.get("Layers"))
            .and_then(crate::json::Json::array)
            .ok_or(Failure::Docker)?;
        if layers.is_empty() || layers.len() > 1000 {
            return Err(Failure::Docker);
        }
        let layers: Vec<String> = layers
            .iter()
            .map(|layer| {
                layer
                    .string()
                    .filter(|value| {
                        value
                            .strip_prefix("sha256:")
                            .is_some_and(|value| options::hex(value, 64))
                    })
                    .map(str::to_owned)
                    .ok_or(Failure::Docker)
            })
            .collect::<Result<_>>()?;
        let existing = self
            .probe(&[
                "image",
                "ls",
                "--quiet",
                "--no-trunc",
                "--filter",
                &format!("reference={tag}"),
            ])
            .await?;
        let existing = text(&existing)?;
        if !existing.is_empty() && existing != image {
            return Err(Failure::Docker);
        }
        if existing.is_empty() {
            self.probe(&["image", "tag", image, tag]).await?;
        }
        self.verify_tag(tag, image).await?;
        Ok(layers)
    }
    async fn verify_tag(&mut self, tag: &str, image: &str) -> Result<()> {
        let selected = self
            .probe(&["image", "inspect", "--format", "{{.Id}}", tag])
            .await?;
        if text(&selected)? != image {
            return Err(Failure::Docker);
        }
        Ok(())
    }
    async fn perform(&mut self, options: &Options) -> Result<PathBuf> {
        let output_path =
            source::output_path(&options.output, &self.inputs.root, &self.inputs.environment)?;
        self.root = Some(Dir::open(&self.inputs.root)?);
        self.temporary = Some(source::temporary(self.owner, self.phase)?);
        self.git = Some(Tool::resolve(
            "git",
            &self.inputs.root,
            &self.inputs.environment,
            self.owner,
            self.phase,
        )?);
        if let Ok(relative) = output_path.strip_prefix(&self.inputs.root) {
            let (status, _) = self
                .command(
                    Role::Git,
                    vec![
                        "check-ignore".into(),
                        "--quiet".into(),
                        "--".into(),
                        relative.as_os_str().into(),
                    ],
                    Duration::from_secs(30),
                    true,
                    true,
                )
                .await?;
            if status != 0 {
                return Err(Failure::Input);
            }
        }
        let revision = self.clean().await?;
        self.revision = Some(revision.clone());
        let parent = Dir::open(output_path.parent().ok_or(Failure::Input)?)?;
        self.output = Some(Output::create(
            parent,
            output_path.file_name().ok_or(Failure::Input)?,
        )?);
        self.docker = Some(Tool::resolve(
            "docker",
            &self.inputs.root,
            &self.inputs.environment,
            self.owner,
            self.phase,
        )?);
        #[cfg(test)]
        let guards = if let Some(paths) = self
            .inputs
            .fixtures
            .as_ref()
            .and_then(|fixture| fixture.guards.as_ref())
        {
            Guards::fixture(&paths.0, &paths.1, self.owner, self.phase)?
        } else {
            Guards::open(self.owner, self.phase)?
        };
        #[cfg(not(test))]
        let guards = Guards::open(self.owner, self.phase)?;
        self.guards = Some(guards);
        self.preflight(options.image.is_none()).await?;
        self.temporary.as_mut().unwrap().create_file("source.tar")?;
        let archive = self.temporary.as_ref().unwrap().get("source.tar")?.path();
        self.command(
            Role::Git,
            vec![
                "archive".into(),
                "--format=tar".into(),
                format!("--output={}", archive.display()).into(),
                revision.clone().into(),
            ],
            Duration::from_secs(30),
            false,
            false,
        )
        .await?;
        let temporary = self.temporary.as_mut().unwrap();
        temporary
            .get_mut("source.tar")?
            .seal(self.owner, self.phase)?;
        let path = temporary.path().to_owned();
        self.tree = Some(Tree::extract(
            temporary.get_mut("source.tar")?,
            &path,
            &revision,
            self.owner,
            self.phase,
        )?);
        let (inputs, migrations) = self
            .tree
            .as_ref()
            .unwrap()
            .metadata(self.owner, self.phase)?;
        self.output.as_mut().unwrap().json(
            "source.json",
            &serde_json::json!({"revision":revision,"inputsSha256":inputs}),
            self.owner,
            self.phase,
        )?;
        self.phase = self.owner.phase(
            Duration::from_secs(options.seconds),
            self.verification + Duration::from_secs(14),
        )?;
        self.active = true;
        let tag = format!("simplestchat-release/production:{revision}");
        let layers = if let Some(image) = &options.image {
            Some(self.selected(image, &tag, &revision).await?)
        } else {
            let context = self.tree.as_ref().unwrap().path().as_os_str().to_owned();
            self.docker(
                vec![
                    "buildx".into(),
                    "build".into(),
                    "--builder".into(),
                    "default".into(),
                    "--platform".into(),
                    "linux/amd64".into(),
                    "--pull".into(),
                    "--progress".into(),
                    "plain".into(),
                    "--target".into(),
                    "production".into(),
                    "--provenance=false".into(),
                    "--sbom=false".into(),
                    "--build-arg".into(),
                    format!("SOURCE_REVISION={revision}").into(),
                    "--label".into(),
                    format!("org.opencontainers.image.revision={revision}").into(),
                    "--tag".into(),
                    tag.clone().into(),
                    "--load".into(),
                    context,
                ],
                Duration::from_secs(options.seconds),
                false,
            )
            .await?;
            None
        };
        self.owner.before(self.phase)?;
        self.output.as_mut().unwrap().create_file("image.tar")?;
        self.docker(
            vec![
                "image".into(),
                "save".into(),
                "--platform".into(),
                "linux/amd64".into(),
                tag.clone().into(),
            ],
            Duration::from_secs(300),
            false,
        )
        .await?;
        self.active = false;
        self.phase = self
            .owner
            .phase(self.verification, Duration::from_secs(14))?;
        self.verification = Duration::ZERO;
        if let Some(image) = &options.image {
            self.verify_tag(&tag, image).await?;
        }
        if self.clean().await? != revision {
            return Err(Failure::Source);
        }
        self.check()?;
        let output = self.output.as_mut().unwrap();
        let archive_sha = output.get_mut("image.tar")?.seal(self.owner, self.phase)?;
        let candidate = serde_json::json!({
            "schemaVersion":1,"revision":revision,"platform":"linux/amd64",
            "archiveSha256":archive_sha,"imageTag":tag,
            "migrations":migrations,"createdAt":timestamp()?,
        });
        output.json("release.candidate.json", &candidate, self.owner, self.phase)?;
        let bytes = output.get("release.candidate.json")?.bytes(
            crate::release::manifest::MAX_BYTES,
            self.owner,
            self.phase,
        )?;
        let manifest = crate::release::manifest::Manifest::decode(&bytes).map_err(|error| {
            let _ = writeln!(
                std::io::stderr().lock(),
                "Release archive diagnostic: manifest_decode {}",
                error.code()
            );
            Failure::Archive
        })?;
        let file = output.get_mut("image.tar")?;
        let mut original = file.writer()?;
        let verified =
            crate::release::archive::verify_retained_archive(&mut original, &manifest, || {
                self.owner.before(self.phase).is_ok()
            });
        if let Err(error) = &verified {
            self.owner
                .record(self.owner.primary().unwrap_or(Failure::Archive));
            let _ = writeln!(
                std::io::stderr().lock(),
                "Release archive diagnostic: verify_retained {}",
                error.code()
            );
        }
        let closed = files::close(original);
        if let Err(error) = closed.as_ref() {
            self.owner.record(*error);
        }
        let unchanged = file.check(false);
        if let Err(error) = unchanged.as_ref() {
            self.owner.record(*error);
        }
        let verified = verified.map_err(|_| self.owner.primary().unwrap_or(Failure::Archive))?;
        closed?;
        unchanged?;
        if let Some(layers) = layers {
            let config = json(verified.config_bytes())?;
            if config
                .get("rootfs")
                .and_then(|value| value.get("type"))
                .and_then(crate::json::Json::string)
                != Some("layers")
                || config
                    .get("rootfs")
                    .and_then(|value| value.get("diff_ids"))
                    .and_then(crate::json::Json::array)
                    .is_none_or(|values| {
                        values.len() != layers.len()
                            || values
                                .iter()
                                .zip(&layers)
                                .any(|(value, expected)| value.string() != Some(expected.as_str()))
                    })
            {
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "Release archive diagnostic: rootfs_mismatch"
                );
                return Err(Failure::Archive);
            }
        }
        self.check()?;
        self.output
            .as_mut()
            .unwrap()
            .get_mut("release.candidate.json")?
            .linked(OsStr::new("release.json"), self.owner, self.phase)?;
        self.check()?;
        Ok(output_path)
    }
    fn finish(mut self, options: &Options) -> Attempt {
        let output_path = self.output.as_ref().map(|output| output.path().to_owned());
        for result in [
            self.tree
                .take()
                .map(|tree| tree.remove(self.owner, self.owner.end())),
            self.guards.take().map(Guards::finish),
            self.socket.take().map(Socket::finish),
            self.git.take().map(Tool::finish),
            self.docker.take().map(Tool::finish),
            self.root.take().map(Dir::finish),
            self.temporary
                .take()
                .map(|output| output.remove(self.owner, self.owner.end())),
        ]
        .into_iter()
        .flatten()
        {
            if let Err(error) = result {
                self.owner.record(error);
            }
        }
        let _ = self.owner.check();
        if let Some(mut output) = self.output.take() {
            match timestamp() {
                Ok(finished_at) => {
                    let mut outcome = serde_json::json!({
                        "schemaVersion":1,"revision":self.revision,"startedAt":self.started_at,
                        "finishedAt":finished_at,"passed":self.owner.primary().is_none(),
                        "error":self.owner.primary().map(Failure::code),
                    });
                    if let Some(image) = &options.image {
                        outcome["exportedImageId"] = image.clone().into();
                    }
                    if let Err(error) =
                        output.json_cleanup("outcome.json", &outcome, self.owner, self.owner.end())
                    {
                        self.owner.record(error);
                    }
                }
                Err(error) => self.owner.record(error),
            }
            if let Err(error) = output.finish() {
                self.owner.record(error);
            }
        }
        let _ = self.owner.check();
        Attempt {
            result: self
                .owner
                .primary()
                .map_or_else(|| output_path.clone().ok_or(Failure::Io), Err),
            output: output_path,
        }
    }
}

async fn execute(
    options: Options,
    inputs: Inputs,
    owner: &mut Owner,
    executor: &mut impl Executor,
) -> Attempt {
    let verification = if options.image.is_some() {
        control::VERIFICATION.min(Duration::from_secs(options.seconds))
    } else {
        control::VERIFICATION
    };
    let phase = match owner.phase(control::SETUP, verification + Duration::from_secs(14)) {
        Ok(phase) => phase,
        Err(error) => {
            return Attempt {
                result: Err(error),
                output: None,
            };
        }
    };
    let started_at = match timestamp() {
        Ok(value) => value,
        Err(error) => {
            return Attempt {
                result: Err(error),
                output: None,
            };
        }
    };
    let mut controller = Controller {
        owner,
        executor,
        inputs,
        root: None,
        git: None,
        docker: None,
        temporary: None,
        output: None,
        guards: None,
        tree: None,
        socket: None,
        endpoint: None,
        docker_environment: None,
        revision: None,
        sequence: 0,
        phase,
        active: false,
        verification,
        started_at,
    };
    if let Err(error) = controller.perform(&options).await {
        controller.owner.record(error);
    }
    controller.finish(&options)
}

fn bounded_line(output: impl AsFd, owner: &Owner, value: &[u8]) -> Result<()> {
    let previous = rustix::fs::fcntl_getfl(&output).map_err(|_| Failure::Io)?;
    rustix::fs::fcntl_setfl(&output, previous | rustix::fs::OFlags::NONBLOCK)
        .map_err(|_| Failure::Io)?;
    let written = (|| {
        let mut offset = 0;
        while offset < value.len() {
            owner.cleanup_before(owner.end())?;
            match rustix::io::write(&output, &value[offset..]) {
                Ok(0) => return Err(Failure::Io),
                Ok(count) => offset += count,
                Err(rustix::io::Errno::AGAIN) => std::thread::sleep(Duration::from_millis(10)),
                Err(rustix::io::Errno::INTR) => {}
                Err(_) => return Err(Failure::Io),
            }
        }
        Ok(())
    })();
    let restored = rustix::fs::fcntl_setfl(&output, previous).map_err(|_| Failure::Close);
    written.and(restored)
}

#[cfg(test)]
thread_local! {
    static COMPLETION_PROBE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static COMPLETION_OFFSET: std::cell::Cell<Option<Duration>> = const {
        std::cell::Cell::new(None)
    };
    static COMPLETION_MASK: std::cell::Cell<Option<nix::sys::stat::Mode>> = const {
        std::cell::Cell::new(None)
    };
    static BODY_CLOSED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static COMPLETION_AFTER_BODY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
fn operation_mode(options: &Options) -> control::Mode {
    if options.image.is_some() {
        control::Mode::Export
    } else {
        control::Mode::Build
    }
}
/// Own the dedicated CLI's umask around its complete synchronous runtime body.
/// The callback supplies no new command, clock or selection authority.
fn lifecycle(
    started: Instant,
    mode: control::Mode,
    seconds: u64,
    operation: impl FnOnce() -> ExitCode,
) -> ExitCode {
    let end = match Owner::deadline(started, mode, seconds) {
        Ok(end) => end,
        Err(_) => return ExitCode::FAILURE,
    };
    let previous = nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o077));
    let result = operation();
    let _ = nix::sys::stat::umask(previous);
    // The original owner/signals/runtime are gone, and the process umask has
    // been restored. Their completion may not extend the original deadline.
    let completed = Instant::now();
    #[cfg(test)]
    let completed = if COMPLETION_PROBE.get() {
        let observed = nix::sys::stat::umask(nix::sys::stat::Mode::empty());
        let _ = nix::sys::stat::umask(observed);
        COMPLETION_MASK.set(Some(observed));
        COMPLETION_AFTER_BODY.set(BODY_CLOSED.get());
        COMPLETION_OFFSET
            .get()
            .and_then(|offset| end.checked_add(offset))
            .unwrap_or(completed)
    } else {
        completed
    };
    if result == ExitCode::SUCCESS && completed >= end {
        ExitCode::FAILURE
    } else {
        result
    }
}

/// Execute the complete unsigned build/export operation. Usage errors are2;
/// every operational failure, original deadline or INT/TERM is1; success is0.
/// Run from a clean checkout root. Existing output paths are never overwritten.
/// See [`crate::release::build`] and the repository's Ops operator guide,
/// "Production release build and export", for this standalone CLI contract.
/// It temporarily owns the process umask and output descriptor flags and restores
/// them before returning; call it as one operation in its dedicated CLI process.
///
/// # Panics
///
/// Call a valid operation from a synchronous CLI outside an entered Tokio
/// runtime. Starting or dropping this operation's runtime from an existing
/// asynchronous runtime context can panic. Help, notices and invalid arguments
/// return before creating a runtime or changing the process umask.
///
/// Help and `--licenses` need no runtime, Git, Docker, configuration or writable
/// current directory. The notices cover the builder's explicit Tokio/runtime use:
///
/// ```
/// use simplestchat_ops::release::build;
/// use std::{ffi::OsString, process::ExitCode};
/// assert_eq!(build::run(&[OsString::from("--help")]), ExitCode::SUCCESS);
/// ```
pub fn run(arguments: &[OsString]) -> ExitCode {
    if arguments.len() == 1 && arguments[0] == "--licenses" {
        return if licenses::stdout().is_ok() {
            ExitCode::SUCCESS
        } else {
            let _ = writeln!(
                std::io::stderr().lock(),
                "Release build notices could not be written."
            );
            ExitCode::FAILURE
        };
    }
    let started = Instant::now();
    let options = match Options::parse(arguments) {
        Ok(Some(options)) => options,
        Ok(None) => {
            return if writeln!(std::io::stdout().lock(), "{}", options::USAGE).is_ok() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            };
        }
        Err(_) => {
            let _ = writeln!(std::io::stderr().lock(), "{}", options::USAGE);
            return ExitCode::from(2);
        }
    };
    lifecycle(started, operation_mode(&options), options.seconds, || {
        run_options(started, options)
    })
}
fn run_options(started: Instant, options: Options) -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => return ExitCode::FAILURE,
    };
    let result = runtime.block_on(async {
        let mut owner = match Owner::new(started, operation_mode(&options), options.seconds) {
            Ok(owner) => owner,
            Err(_) => return ExitCode::FAILURE,
        };
        let root = match std::env::current_dir().and_then(|path| path.canonicalize()) {
            Ok(root) => root,
            Err(_) => return ExitCode::FAILURE,
        };
        let inputs = Inputs {
            root,
            environment: std::env::vars_os().collect(),
            #[cfg(test)]
            fixtures: None,
        };
        let attempt = execute(options, inputs, &mut owner, &mut Native).await;
        let mut passed = attempt.result.is_ok();
        if let Some(output) = attempt.output {
            let line = format!("Release evidence: {} (passed={passed})\n", output.display());
            if let Err(error) = bounded_line(std::io::stdout(), &owner, line.as_bytes()) {
                owner.record(error);
                passed = false;
            }
        }
        if passed && owner.check().is_ok() {
            ExitCode::SUCCESS
        } else {
            let line = format!(
                "Release build failed: {}\n",
                owner.primary().unwrap_or(Failure::Io)
            );
            let _ = bounded_line(std::io::stderr(), &owner, line.as_bytes());
            ExitCode::FAILURE
        }
    });
    drop(runtime);
    result
}

#[cfg(test)]
#[path = "build_budget_tests.rs"]
mod budget_tests;
#[cfg(test)]
#[path = "build_custody_tests.rs"]
mod custody_tests;
#[cfg(test)]
#[path = "build_json_tests.rs"]
mod json_tests;
#[cfg(test)]
#[path = "build_tests.rs"]
mod tests;
