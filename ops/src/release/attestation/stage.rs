//! The complete fixed host staging transaction. This does not attest inputs or
//! change an installed caller. Daemon observations are separate from archive facts.
mod files;
mod options;
#[cfg(test)]
mod oracle_tests;
mod process;
#[cfg(test)]
mod tests;

use super::{
    control::{self, Cause, Owner, Result, Window},
    files::{Dir, Retained},
};
use crate::json::Json;
use crate::release::manifest::Manifest;
use options::Options;
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    io::{self, Write},
    os::unix::fs::MetadataExt,
    path::PathBuf,
    process::ExitCode,
    time::{Duration, Instant},
};

const IMAGE_FORMAT: &str = concat!(
    "{\"id\":{{json .Id}},\"os\":{{json .Os}},\"architecture\":{{json .Architecture}},",
    "\"user\":{{json .Config.User}},\"labels\":{{json .Config.Labels}},",
    "\"cmd\":{{json .Config.Cmd}},\"entrypoint\":{{json (index .Config \"Entrypoint\")}},",
    "\"workingDir\":{{json .Config.WorkingDir}},",
    "\"hasHealthcheck\":{{if index .Config \"Healthcheck\"}}true{{else}}false{{end}},",
    "\"envNames\":[{{range $i, $v := .Config.Env}}{{if $i}},{{end}}",
    "{{json (index (split $v \"=\") 0)}}{{end}}]}",
);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Load,
    Inspect,
    Create,
    Start,
    Wait,
    Logs,
    Remove,
}
struct Invocation {
    role: Role,
    program: PathBuf,
    arguments: Vec<OsString>,
    body_end: Instant,
    settlement_end: Instant,
    cleanup: bool,
}
struct CommandOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    status: Option<i32>,
    error: Option<Cause>,
    settled: bool,
    stdout_complete: bool,
}
trait Executor {
    async fn run(&mut self, owner: &mut Owner, request: Invocation) -> CommandOutput;
}
struct Native;
impl Executor for Native {
    async fn run(&mut self, owner: &mut Owner, request: Invocation) -> CommandOutput {
        let _ = request.role;
        let completed = process::run(
            owner,
            process::Invocation {
                program: request.program,
                arguments: request.arguments,
                environment: BTreeMap::from([
                    ("PATH".into(), "/usr/sbin:/usr/bin:/sbin:/bin".into()),
                    ("LC_ALL".into(), "C".into()),
                ]),
                body_end: request.body_end,
                settlement_end: request.settlement_end,
                cleanup: request.cleanup,
            },
        )
        .await;
        Self::project(completed, request.cleanup)
    }
}
impl Native {
    fn project(completed: process::Completion, cleanup: bool) -> CommandOutput {
        let o = &completed.observation;
        let stdout_complete = o.stdout_body_eof
            && !o.stdout_limited
            && o.stdout_bytes == completed.stdout.len() as u64
            && o.stdout_sha256 == super::files::hex(&sha2::Sha256::digest(&completed.stdout));
        let settled = o.pid.is_some()
            && o.group_retired
            && o.reaped
            && o.stdin_close_attempted
            && o.stdout_close_attempted
            && o.stderr_close_attempted
            && o.stdin_closed
            && o.stdout_closed
            && o.stderr_closed
            && o.stdout_eof
            && o.stderr_eof
            && o.stdout_body_eof
            && o.stderr_body_eof
            && !o.stdout_limited
            && !o.stderr_limited
            && !o.timed_out
            && o.cleanup_errors.is_empty()
            && o.stdout_bytes == completed.stdout.len() as u64
            && o.stderr_bytes == completed.stderr.len() as u64
            && o.stdout_sha256 == super::files::hex(&sha2::Sha256::digest(&completed.stdout))
            && o.stderr_sha256 == super::files::hex(&sha2::Sha256::digest(&completed.stderr))
            && o.elapsed <= Duration::from_secs(310);
        CommandOutput {
            stdout: completed.stdout,
            stderr: completed.stderr,
            status: o.exit_status,
            error: if cleanup && settled && o.exit_status == Some(0) {
                None
            } else {
                completed.result.err()
            },
            settled,
            stdout_complete,
        }
    }
}

use sha2::Digest;

struct Access {
    root: PathBuf,
    config: PathBuf,
    work: PathBuf,
    docker: PathBuf,
    socket: Option<PathBuf>,
    uid: u32,
    #[cfg(test)]
    free_bytes: Option<u64>,
    #[cfg(test)]
    fault: Option<files::Fault>,
}
impl Access {
    fn fixed() -> Self {
        Self {
            root: "/srv/simplestchat-public".into(),
            config: "/etc/simplestchat-public".into(),
            work: "/run/simplestchat-bench".into(),
            docker: "/usr/bin/docker".into(),
            socket: Some("/var/run/docker.sock".into()),
            uid: 0,
            #[cfg(test)]
            free_bytes: None,
            #[cfg(test)]
            fault: None,
        }
    }
}
struct Socket {
    path: PathBuf,
    canonical: PathBuf,
    stamp: std::fs::Metadata,
}
impl Socket {
    fn check(&self, owner: &mut Owner) -> Result<()> {
        owner.check(Window::Body)?;
        let actual = control::io(owner, self.path.canonicalize())?;
        let stamp = control::io(owner, self.canonical.symlink_metadata())?;
        if actual != self.canonical
            || self.stamp.dev() != stamp.dev()
            || self.stamp.ino() != stamp.ino()
            || self.stamp.mode() != stamp.mode()
            || self.stamp.uid() != stamp.uid()
            || self.stamp.gid() != stamp.gid()
        {
            return control::failed(owner, Cause::Changed);
        }
        owner.check(Window::Body)
    }
}
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Report {
    action: &'static str,
    revision: String,
    started_at: String,
    passed: bool,
    phase: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    finished_at: Option<String>,
}
struct Outcome {
    bytes: Vec<u8>,
    passed: bool,
}
#[derive(Default)]
struct State {
    directories: Vec<Dir>,
    originals: Vec<Retained>,
    records: files::Records,
    lock: Option<Retained>,
    socket: Option<Socket>,
    archive: Option<Retained>,
    manifest: Option<Retained>,
    tool: Option<Retained>,
    tool_sha: Option<String>,
    tool_stamp: Option<std::fs::Metadata>,
    tool_directory: Option<Dir>,
    root: Option<Dir>,
    destination: Option<Dir>,
    attempt: Option<Dir>,
    validator: Option<String>,
    validator_window: Option<Instant>,
    commands: usize,
    pending_logs: Vec<(String, Vec<u8>)>,
    failure: Option<String>,
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
fn timestamp() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}
fn bytes(value: &impl serde::Serialize) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|_| Cause::Io)?;
    bytes.push(b'\n');
    if bytes.len() > 2 * 1024 * 1024 {
        return Err(Cause::Bounds);
    }
    Ok(bytes)
}
fn json_value(bytes: &[u8]) -> Result<Json> {
    crate::json::decode_limited(bytes, 2 * 1024 * 1024).map_err(|_| Cause::Policy)
}
fn text<'a>(value: &'a Json, name: &str) -> Option<&'a str> {
    value.get(name).and_then(Json::string)
}
// Fixed stdout compatibility for the unchanged Python3.14 operation. These
// borrow the bounded capture; they are not a general file or protocol parser.
fn python_space(value: char) -> bool {
    value.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&value)
}
fn python_strip(value: &str) -> &str {
    value.trim_matches(python_space)
}
fn python_byte_strip(value: &[u8]) -> &[u8] {
    let white = |b: &u8| matches!(*b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c);
    let start = value.iter().position(|b| !white(b)).unwrap_or(value.len());
    let end = value
        .iter()
        .rposition(|b| !white(b))
        .map_or(start, |at| at + 1);
    &value[start..end]
}
fn python_line_break(value: char) -> bool {
    matches!(
        value,
        '\n' | '\r'
            | '\u{b}'
            | '\u{c}'
            | '\u{1c}'
            | '\u{1d}'
            | '\u{1e}'
            | '\u{85}'
            | '\u{2028}'
            | '\u{2029}'
    )
}
fn python_lines(value: &str) -> impl Iterator<Item = &str> {
    let mut after_cr = false;
    value
        .split_inclusive(python_line_break)
        .filter_map(move |part| {
            let skip = after_cr && part == "\n";
            let last = part.chars().next_back();
            after_cr = last == Some('\r');
            if skip || part.is_empty() {
                return None;
            }
            let end = last
                .filter(|c| python_line_break(*c))
                .map_or(part.len(), |c| part.len() - c.len_utf8());
            Some(&part[..end])
        })
}
const PYTHON_DECIMAL_DIGITS: usize = 4300;
fn exact_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
impl State {
    fn policy<T>(&mut self, owner: &mut Owner, message: &'static str) -> Result<T> {
        self.failure = Some(message.into());
        control::failed(owner, Cause::Policy)
    }
    fn directory(&mut self, path: &std::path::Path, uid: u32, owner: &mut Owner) -> Result<Dir> {
        let dir = Dir::open(path, owner)?;
        self.directories.push(dir.clone());
        dir.protected(uid, owner)?;
        Ok(dir)
    }
    fn setup(&mut self, access: &Access, revision: &str, owner: &mut Owner) -> Result<Manifest> {
        let root = self.directory(&access.root, access.uid, owner)?;
        self.root = Some(root.clone());
        let _ = self.directory(&access.config, access.uid, owner)?;
        let work = if access.work.exists() {
            self.directory(&access.work, access.uid, owner)?
        } else {
            let parent = Dir::open(access.work.parent().ok_or(Cause::Input)?, owner)?;
            self.directories.push(parent.clone());
            let dir = parent.create_child(access.work.file_name().ok_or(Cause::Input)?, owner)?;
            self.directories.push(dir.clone());
            dir
        };
        self.lock = Some(if work.exists("workload.lock".as_ref(), owner)? {
            Retained::open(work.clone(), "workload.lock", owner)?
        } else {
            Retained::create(work.clone(), "workload.lock", owner)?
        });
        self.lock.as_ref().unwrap().protected(access.uid, owner)?;
        self.lock.as_ref().unwrap().lock(owner)?;
        for (dir, name, message) in [
            (
                &work,
                "current.json",
                "Resolve unfinished benchmark cleanup before releasing",
            ),
            (
                &root,
                "release-state.json",
                "Inspect and recover the unfinished release before starting another",
            ),
        ] {
            if dir.exists(name.as_ref(), owner)? {
                self.originals
                    .push(Retained::open(dir.clone(), name, owner)?);
                let input = self.originals.last().unwrap();
                input.protected(access.uid, owner)?;
                let value = json_value(&input.bytes(16384, owner)?)?;
                if !matches!(value.get("schemaVersion"), Some(Json::Integer(v)) if v == "1")
                    || value.get("finalized").and_then(Json::boolean) != Some(true)
                {
                    return self.policy(owner, message);
                }
                if name == "release-state.json" {
                    self.records.initial_journal = self.originals.pop();
                }
            }
        }
        let releases = self.directory(&access.root.join("releases"), access.uid, owner)?;
        let destination = self.directory(&releases.path().join(revision), access.uid, owner)?;
        self.destination = Some(destination.clone());
        self.manifest = Some(Retained::open(destination.clone(), "release.json", owner)?);
        self.archive = Some(Retained::open(destination, "image.tar", owner)?);
        self.manifest
            .as_ref()
            .unwrap()
            .protected(access.uid, owner)?;
        self.archive
            .as_ref()
            .unwrap()
            .protected(access.uid, owner)?;
        let manifest = Manifest::decode(
            &self
                .manifest
                .as_ref()
                .unwrap()
                .bytes(crate::release::manifest::MAX_BYTES, owner)?,
        )
        .map_err(|_| Cause::Policy)?;
        if manifest.fields().revision != revision {
            return self.policy(owner, "Release directory identity differs");
        }
        let results = self.directory(&access.root.join("results"), access.uid, owner)?;
        let mut random = [0; 16];
        getrandom::fill(&mut random).map_err(|_| Cause::Io)?;
        owner.check(Window::Body)?;
        let attempt = results.create_child(
            format!("release.{}", super::files::hex(&random)).as_ref(),
            owner,
        )?;
        self.directories.push(attempt.clone());
        self.attempt = Some(attempt);
        let tool_dir = Dir::open(access.docker.parent().ok_or(Cause::Input)?, owner)?;
        self.directories.push(tool_dir.clone());
        self.tool_directory = Some(tool_dir.clone());
        self.tool = Some(Retained::open(
            tool_dir,
            access.docker.file_name().ok_or(Cause::Input)?,
            owner,
        )?);
        let metadata = control::io(owner, self.tool.as_ref().unwrap().file().metadata())?;
        if metadata.uid() != access.uid || metadata.mode() & 0o111 == 0 {
            return control::failed(owner, Cause::Policy);
        }
        self.tool
            .as_ref()
            .unwrap()
            .bound(256 * 1024 * 1024, owner)?;
        self.tool_stamp = Some(metadata);
        self.tool_sha = Some(self.tool.as_ref().unwrap().hash(owner)?);
        if let Some(path) = &access.socket {
            let canonical = control::io(owner, path.canonicalize())?;
            let stamp = control::io(owner, canonical.symlink_metadata())?;
            use std::os::unix::fs::FileTypeExt;
            if !stamp.file_type().is_socket() || stamp.uid() != access.uid {
                return control::failed(owner, Cause::Policy);
            }
            self.socket = Some(Socket {
                path: path.clone(),
                canonical,
                stamp,
            });
        }
        Ok(manifest)
    }
    fn originals(&self, owner: &mut Owner) -> Result<()> {
        self.records.check_guard(owner)?;
        for file in self
            .originals
            .iter()
            .chain(self.manifest.iter())
            .chain(self.archive.iter())
            .chain(self.tool.iter())
        {
            file.check(owner, Window::Body)?;
        }
        if self.tool.as_ref().unwrap().hash(owner)? != *self.tool_sha.as_ref().unwrap() {
            return control::failed(owner, Cause::Changed);
        }
        if let Some(socket) = &self.socket {
            socket.check(owner)?;
        }
        Ok(())
    }
    fn cleanup_witness(&self, access: &Access, owner: &mut Owner) -> Result<()> {
        use std::os::unix::fs::FileExt;
        owner.final_work()?;
        self.tool_directory
            .as_ref()
            .ok_or(Cause::Changed)?
            .final_namespace(owner)?;
        let tool = self.tool.as_ref().ok_or(Cause::Changed)?;
        let first = self.tool_stamp.as_ref().ok_or(Cause::Changed)?;
        let same = |now: &std::fs::Metadata| {
            first.dev() == now.dev()
                && first.ino() == now.ino()
                && first.mode() == now.mode()
                && first.uid() == now.uid()
                && first.gid() == now.gid()
                && first.nlink() == now.nlink()
                && first.len() == now.len()
                && (
                    first.mtime(),
                    first.mtime_nsec(),
                    first.ctime(),
                    first.ctime_nsec(),
                ) == (now.mtime(), now.mtime_nsec(), now.ctime(), now.ctime_nsec())
        };
        let current = control::io(owner, tool.file().metadata())?;
        let visible = control::io(owner, access.docker.symlink_metadata())?;
        if !same(&current) || !same(&visible) {
            return control::failed(owner, Cause::Changed);
        }
        let mut buffer = vec![0; 65536];
        let mut offset = 0_u64;
        let mut hash = sha2::Sha256::new();
        loop {
            owner.final_work()?;
            let count = control::io(owner, tool.file().read_at(&mut buffer, offset))?;
            owner.final_work()?;
            if count == 0 {
                break;
            }
            offset = offset.checked_add(count as u64).ok_or(Cause::Bounds)?;
            if offset > first.len() {
                return control::failed(owner, Cause::Changed);
            }
            hash.update(&buffer[..count]);
        }
        if offset != first.len()
            || self.tool_sha.as_deref() != Some(super::files::hex(&hash.finalize()).as_str())
            || !same(&control::io(owner, tool.file().metadata())?)
        {
            return control::failed(owner, Cause::Changed);
        }
        if let Some(socket) = &self.socket {
            let actual = control::io(owner, socket.path.canonicalize())?;
            let now = control::io(owner, socket.canonical.symlink_metadata())?;
            if actual != socket.canonical
                || socket.stamp.dev() != now.dev()
                || socket.stamp.ino() != now.ino()
                || socket.stamp.mode() != now.mode()
                || socket.stamp.uid() != now.uid()
                || socket.stamp.gid() != now.gid()
            {
                return control::failed(owner, Cause::Changed);
            }
        }
        owner.final_work()
    }
    fn journal(&mut self, phase: &str, finalized: bool, owner: &mut Owner) -> Result<()> {
        let value = json!({"schemaVersion":1,"attempt":self.attempt.as_ref().unwrap().path(),
            "finalized":finalized,"phase":phase});
        self.records.write(
            self.root.as_ref().unwrap(),
            "release-state.json",
            &bytes(&value)?,
            owner,
            false,
        )
    }
    async fn command(
        &mut self,
        access: &Access,
        role: Role,
        arguments: Vec<OsString>,
        cleanup: bool,
        owner: &mut Owner,
        executor: &mut impl Executor,
    ) -> Result<Vec<u8>> {
        if cleanup {
            self.cleanup_witness(access, owner)?;
        } else {
            self.originals(owner)?;
        }
        let now = owner.now();
        let cap = match role {
            Role::Load => 300,
            Role::Wait => 20,
            _ => 30,
        };
        let phase_end = if cleanup {
            owner.body_end()
        } else {
            self.validator_window.unwrap_or(owner.body_end())
        };
        let body_end = now
            .checked_add(Duration::from_secs(cap))
            .ok_or(Cause::Deadline)?
            .min(
                phase_end
                    .checked_sub(Duration::from_secs(10))
                    .ok_or(Cause::Deadline)?,
            );
        if body_end <= now {
            return control::failed(owner, Cause::Deadline);
        }
        let settlement_end = body_end
            .checked_add(Duration::from_secs(10))
            .ok_or(Cause::Deadline)?
            .min(phase_end);
        self.commands += 1;
        if self.commands > 20 {
            return control::failed(owner, Cause::Bounds);
        }
        let mut argv: Vec<OsString> = vec!["--host".into(), "unix:///var/run/docker.sock".into()];
        argv.extend(arguments);
        let output = executor
            .run(
                owner,
                Invocation {
                    role,
                    program: access.docker.clone(),
                    arguments: argv,
                    body_end,
                    settlement_end,
                    cleanup,
                },
            )
            .await;
        // Capture known daemon effects before any fallible log write or
        // filesystem recheck can interrupt the policy owner.
        if output.status == Some(0) {
            if role == Role::Create && output.stdout_complete {
                if let Ok(id) = std::str::from_utf8(&output.stdout)
                    && exact_hex(python_strip(id), 64)
                {
                    self.validator = Some(python_strip(id).into());
                }
            } else if role == Role::Remove {
                self.validator = None;
            }
        }
        if let Some(cause) = output.error {
            owner.record(cause);
            if self.failure.is_none() && cause != Cause::Command {
                self.failure = Some(cause_name(cause).into());
            }
        }
        if !output.settled {
            owner.record(Cause::Close);
        }
        if output.status != Some(0) {
            owner.record(Cause::Command);
            if self.failure.is_none() {
                self.failure = Some(format!(
                    "Command {:03} failed; inspect private output",
                    self.commands
                ));
            }
        }
        let mut logs_ok = true;
        for (name, data) in [
            (format!("{:03}.stdout", self.commands), &output.stdout),
            (format!("{:03}.stderr", self.commands), &output.stderr),
        ] {
            if cleanup || owner.error().is_some() {
                if self.pending_logs.len() >= 4 {
                    owner.record(Cause::Bounds);
                    logs_ok = false;
                } else {
                    self.pending_logs.push((name, data.clone()));
                }
            } else if let Err(cause) =
                self.records
                    .write(self.attempt.as_ref().unwrap(), &name, data, owner, false)
            {
                owner.record(cause);
                logs_ok = false;
                if self.pending_logs.len() < 4 {
                    self.pending_logs.push((name, data.clone()));
                }
            }
        }
        if cleanup {
            let checked = self.cleanup_witness(access, owner);
            if checked.is_err() {
                owner.record(checked.err().unwrap());
            }
            if checked.is_ok()
                && output.settled
                && output.status == Some(0)
                && output.error.is_none()
                && logs_ok
            {
                Ok(output.stdout)
            } else {
                Err(owner.error().unwrap_or(Cause::Close))
            }
        } else {
            self.originals(owner)?;
            owner.check(Window::Body)?;
            Ok(output.stdout)
        }
    }
    async fn identity(
        &mut self,
        access: &Access,
        selector: &str,
        revision: &str,
        archive: &crate::release::archive::VerifiedArchive,
        owner: &mut Owner,
        executor: &mut impl Executor,
    ) -> Result<String> {
        let data = self
            .command(
                access,
                Role::Inspect,
                vec![
                    "image".into(),
                    "inspect".into(),
                    "--format".into(),
                    IMAGE_FORMAT.into(),
                    selector.into(),
                ],
                false,
                owner,
                executor,
            )
            .await?;
        let value = json_value(&data)?;
        let identifier = text(&value, "id").unwrap_or("");
        if !identifier
            .strip_prefix("sha256:")
            .is_some_and(|v| exact_hex(v, 64))
        {
            return self.policy(owner, "Missing local content-addressed image ID");
        }
        if text(&value, "os") != Some("linux")
            || text(&value, "architecture") != Some("amd64")
            || text(&value, "user") != Some("10001:10001")
        {
            return self.policy(owner, "Unexpected image platform or runtime user");
        }
        if value
            .get("labels")
            .and_then(|v| text(v, "org.opencontainers.image.revision"))
            != Some(revision)
        {
            return self.policy(owner, "Image revision mismatch");
        }
        if !value
            .get("cmd")
            .and_then(Json::array)
            .is_some_and(|v| v.len() == 1 && v[0].string() == Some("/app/simplestChat"))
            || !matches!(value.get("entrypoint"), Some(Json::Null))
                && !value
                    .get("entrypoint")
                    .and_then(Json::array)
                    .is_some_and(<[Json]>::is_empty)
        {
            return self.policy(owner, "Unexpected image entrypoint");
        }
        if text(&value, "workingDir") != Some("/app")
            || value.get("hasHealthcheck").and_then(Json::boolean) != Some(false)
        {
            return self.policy(owner, "Unexpected image execution defaults");
        }
        let env = value
            .get("envNames")
            .and_then(Json::array)
            .ok_or(Cause::Policy)?;
        let mut names = BTreeSet::new();
        for name in env {
            let name = name.string().ok_or(Cause::Policy)?;
            if !names.insert(name) {
                return self.policy(owner, "runtime_environment_duplicate");
            }
            if name.is_empty()
                || !name.bytes().enumerate().all(|(i, b)| {
                    b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit())
                })
                || name.starts_with("LD_")
                || name.starts_with("OPENSSL_")
                || ["GLIBC_TUNABLES", "GCONV_PATH", "LOCPATH"].contains(&name)
            {
                return self.policy(owner, "runtime_environment_override");
            }
        }
        if !archive.accepts_runtime_id(identifier) {
            return self.policy(owner, "Imported image differs from verified archive graph");
        }
        Ok(identifier.into())
    }
    async fn stage(
        &mut self,
        access: &Access,
        manifest: &Manifest,
        owner: &mut Owner,
        executor: &mut impl Executor,
    ) -> Result<()> {
        let verified = self.archive.as_mut().unwrap().archive(manifest, owner)?;

        let destination = self.destination.as_ref().unwrap().clone();
        if destination.exists("staged.json".as_ref(), owner)? {
            self.originals
                .push(Retained::open(destination, "staged.json", owner)?);
            let record = self.originals.last().unwrap();
            record.protected(access.uid, owner)?;
            let prior = json_value(&record.bytes(16384, owner)?)?;
            if text(&prior, "manifestSha256") != Some(manifest.sha256()) {
                return self.policy(owner, "Staged release changed");
            }
            let selected = text(&prior, "serverImage").ok_or(Cause::Policy)?;
            let image = self
                .identity(
                    access,
                    selected,
                    &manifest.fields().revision,
                    &verified,
                    owner,
                    executor,
                )
                .await?;
            if image != selected {
                return self.policy(owner, "Staged image missing");
            }
            return Ok(());
        }
        #[cfg(test)]
        let free = match access.free_bytes {
            Some(value) => value,
            None => self.root.as_ref().unwrap().free_bytes(owner)?,
        };
        #[cfg(not(test))]
        let free = self.root.as_ref().unwrap().free_bytes(owner)?;
        let needed = self
            .archive
            .as_ref()
            .unwrap()
            .len()
            .checked_mul(2)
            .and_then(|v| v.checked_add(1024 * 1024 * 1024))
            .ok_or(Cause::Bounds)?;
        if free <= needed {
            return self.policy(
                owner,
                "Insufficient free space to import image with safe headroom",
            );
        }
        // Refuse before any daemon effect if even the known validator tail
        // cannot fit. This is admission, not a completion-time promise.
        let latest = owner
            .body_end()
            .checked_sub(Duration::from_secs(40))
            .ok_or(Cause::Deadline)?;
        if owner.now() >= latest {
            return control::failed(owner, Cause::Deadline);
        }
        self.journal("import_image", false, owner)?;
        self.command(
            access,
            Role::Load,
            vec![
                "image".into(),
                "load".into(),
                "--input".into(),
                self.destination
                    .as_ref()
                    .unwrap()
                    .path()
                    .join("image.tar")
                    .into_os_string(),
            ],
            false,
            owner,
            executor,
        )
        .await?;
        let image = self
            .identity(
                access,
                &manifest.fields().image_tag,
                &manifest.fields().revision,
                &verified,
                owner,
                executor,
            )
            .await?;
        let migrations = self.migrations(access, &image, owner, executor).await?;
        if migrations != manifest.fields().migrations {
            return self.policy(owner, "Archive migration manifest differs from image");
        }
        let value = json!({"schemaVersion":1,"revision":manifest.fields().revision,
            "serverImage":image,"manifestSha256":manifest.sha256(),"stagedAt":timestamp()});
        self.records.write(
            self.destination.as_ref().unwrap(),
            "staged.json",
            &bytes(&value)?,
            owner,
            false,
        )?;
        self.journal("staged", true, owner)?;
        Ok(())
    }
    async fn migrations(
        &mut self,
        access: &Access,
        image: &str,
        owner: &mut Owner,
        executor: &mut impl Executor,
    ) -> Result<BTreeMap<String, String>> {
        // Keep daemon retirement30 + the original child TERM5/reap5 inside B.
        let end = owner
            .body_end()
            .checked_sub(Duration::from_secs(40))
            .ok_or(Cause::Deadline)?;
        owner.begin_stage_validator(end)?;
        self.validator_window = Some(end);
        let mut random = [0; 16];
        getrandom::fill(&mut random).map_err(|_| Cause::Io)?;
        let name = format!("scpub-release-validate-{}", super::files::hex(&random));
        self.journal("validate_image", false, owner)?;
        self.records.write(
            self.attempt.as_ref().unwrap(),
            "validation-name.txt",
            name.as_bytes(),
            owner,
            false,
        )?;
        let mut args = [
            "create",
            "--name",
            &name,
            "--init",
            "--network",
            "none",
            "--pull",
            "never",
            "--read-only",
            "--user",
            "10001:10001",
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges",
            "--memory",
            "128m",
            "--cpus",
            "0.5",
            "--pids-limit",
            "32",
            "--log-driver",
            "local",
            "--log-opt",
            "max-size=1m",
            "--log-opt",
            "max-file=1",
            "--log-opt",
            "compress=false",
            "--entrypoint",
            "/usr/bin/timeout",
            image,
            "--signal=TERM",
            "--kill-after=2s",
            "10s",
            "/bin/sh",
            "-c",
            "for file in /app/migrations/*.sql; do sha384sum \"$file\" || exit; done",
        ]
        .into_iter()
        .map(OsString::from)
        .collect::<Vec<_>>();
        let raw = self
            .command(
                access,
                Role::Create,
                std::mem::take(&mut args),
                false,
                owner,
                executor,
            )
            .await?;
        let id = python_strip(std::str::from_utf8(&raw).map_err(|_| Cause::Policy)?);
        if !exact_hex(id, 64) {
            return self.policy(owner, "Uncertain validation container creation");
        }
        self.validator = Some(id.into());
        self.command(
            access,
            Role::Start,
            vec!["start".into(), id.into()],
            false,
            owner,
            executor,
        )
        .await?;
        let waited = self
            .command(
                access,
                Role::Wait,
                vec!["wait".into(), id.into()],
                false,
                owner,
                executor,
            )
            .await?;
        if python_byte_strip(&waited) != b"0" {
            return self.policy(owner, "Packaged migration validation failed");
        }
        let output = self
            .command(
                access,
                Role::Logs,
                vec!["logs".into(), id.into()],
                false,
                owner,
                executor,
            )
            .await?;
        let mut result = BTreeMap::new();
        for line in python_lines(std::str::from_utf8(&output).map_err(|_| Cause::Policy)?) {
            let Some((hash, path)) = line.split_once("  /app/migrations/") else {
                return self.policy(owner, "Unexpected packaged migration output");
            };
            let Some((version, name)) = path.strip_suffix(".sql").and_then(|v| v.split_once('_'))
            else {
                return self.policy(owner, "Unexpected packaged migration output");
            };
            if !exact_hex(hash, 96)
                || version.is_empty()
                || !version.bytes().all(|b| b.is_ascii_digit())
                || name.is_empty()
                || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            {
                return self.policy(owner, "Unexpected packaged migration output");
            }
            // Pinned CPython's default conversion counts all digits, including
            // leading zeros. Within that admitted domain normalization is
            // textual; no u64 bound may move map mismatch before normal rm.
            if version.len() > PYTHON_DECIMAL_DIGITS {
                return self.policy(owner, "ValueError");
            }
            let trimmed = version.trim_start_matches('0');
            let version = if trimmed.is_empty() { "0" } else { trimmed }.to_owned();
            if result.insert(version, hash.to_owned()).is_some() {
                return self.policy(owner, "Duplicate packaged migration version");
            }
        }
        if result.is_empty() {
            return self.policy(owner, "No packaged migrations");
        }
        self.command(
            access,
            Role::Remove,
            vec!["rm".into(), id.into()],
            false,
            owner,
            executor,
        )
        .await?;
        self.validator = None;
        owner.end_stage_validator();
        self.validator_window = None;
        owner.check(Window::Body)?;
        self.journal("image_validated", true, owner)?;
        Ok(result)
    }
    fn close_inputs(&mut self, owner: &mut Owner) {
        for file in self
            .originals
            .drain(..)
            .chain(self.archive.take())
            .chain(self.manifest.take())
            .chain(self.tool.take())
        {
            file.finish(owner);
        }
    }
    fn finish(mut self, owner: &mut Owner) {
        self.close_inputs(owner);
        if let Some(lock) = self.lock.take() {
            lock.finish(owner);
        }
        self.records.finish(owner);
        self.root.take();
        self.tool_directory.take();
        self.destination.take();
        self.attempt.take();
        for dir in self.directories.into_iter().rev() {
            dir.finish(owner);
        }
    }
}
async fn execute(
    options: &Options,
    access: &Access,
    owner: &mut Owner,
    executor: &mut impl Executor,
    started: Instant,
) -> Outcome {
    let _ = started;
    let mut state = State::default();
    #[cfg(test)]
    {
        state.records.fault = access.fault;
    }
    let mut report = Report {
        action: "stage",
        revision: options.revision.clone(),
        started_at: timestamp(),
        passed: false,
        phase: "stage",
        failure: None,
        finished_at: None,
    };
    let result = async {
        let manifest = state.setup(access, &options.revision, owner)?;
        state.stage(access, &manifest, owner, executor).await?;
        state.originals(owner)?;
        owner.check(Window::Body)
    }
    .await;
    if let Err(cause) = result {
        owner.record(cause);
    }
    if state.validator_window.take().is_some() {
        owner.end_stage_validator();
    }
    if state.failure.is_none() {
        state.failure = owner.error().map(|cause| cause_name(cause).into());
    }
    if let Some(id) = state.validator.take() {
        let admitted = owner.begin_stage_cleanup();
        if admitted.is_ok() {
            let _ = state
                .command(
                    access,
                    Role::Remove,
                    vec!["rm".into(), "--force".into(), id.into()],
                    true,
                    owner,
                    executor,
                )
                .await;
        }
        owner.end_stage_cleanup();
    }
    if let Some(attempt) = &state.attempt {
        for (name, data) in state.pending_logs.drain(..) {
            let _ = state.records.write(attempt, &name, &data, owner, true);
        }
    }
    if owner.error().is_none() {
        report.phase = "complete";
    }
    // Like the reference's main(), outcome persistence is outside the flock.
    if let Some(lock) = state.lock.take() {
        lock.finish(owner);
    }
    report.passed = owner.error().is_none();
    report.failure = state
        .failure
        .take()
        .or_else(|| owner.error().map(|cause| cause_name(cause).into()));
    let mut outcome_written = true;
    if let Some(attempt) = &state.attempt {
        report.finished_at = Some(timestamp());
        if let Ok(data) = bytes(&report) {
            outcome_written = state
                .records
                .write(attempt, "outcome.json", &data, owner, true)
                .is_ok();
        } else {
            outcome_written = false;
            owner.record(Cause::Io);
        }
    }
    let evidence = state.attempt.as_ref().map(Dir::path);
    let had_attempt = evidence.is_some();
    state.finish(owner);
    report.passed &= owner.check(Window::Final).is_ok();
    let mut value = serde_json::to_value(&report).unwrap_or_else(|_| json!({"passed":false}));
    if let Some(path) = evidence.and_then(|path| path.to_str().map(str::to_owned)) {
        value["evidence"] = path.into();
    } else if report.finished_at.is_some() {
        owner.record(Cause::Input);
        report.passed = false;
        value["passed"] = false.into();
    }
    Outcome {
        passed: report.passed,
        bytes: if outcome_written && had_attempt {
            bytes(&value).unwrap_or_default()
        } else {
            Vec::new()
        },
    }
}
#[cfg(test)]
thread_local! {
    static COMPLETION_OFFSET: std::cell::Cell<Option<Duration>> = const {
        std::cell::Cell::new(None)
    };
    static FRAME_CLOSED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static COMPLETION_AFTER_FRAME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
fn lifecycle(
    started: Instant,
    whole: Duration,
    operation: impl FnOnce() -> (bool, Option<Owner>),
) -> ExitCode {
    let Some(end) = started.checked_add(whole) else {
        return ExitCode::from(2);
    };
    let mask = nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o077));
    let (passed, owner) = operation();
    nix::sys::stat::umask(mask);
    let finished = owner.map_or(Err(Cause::Input), Owner::finish);
    let completed = Instant::now();
    #[cfg(test)]
    let completed = match COMPLETION_OFFSET.get() {
        Some(offset) => {
            COMPLETION_AFTER_FRAME.set(FRAME_CLOSED.get());
            end.checked_add(offset).unwrap_or(end)
        }
        None => completed,
    };
    if passed && finished.is_ok() && completed < end {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
/// Stage one received immutable image on the fixed prepared Linux host.
///
/// Root owns explicit whole/finalization budgets and trusted Docker access.
/// Success changes no running app, database, proxy or installed helper route.
/// A nonzero exit invalidates an earlier complete report. Kernel I/O and reap
/// stalls are outside a hard wall guarantee.
///
/// # Panics
///
/// A valid operation must run outside an entered Tokio runtime. Runtime start
/// or drop inside another runtime can panic.
///
/// ```
/// use simplestchat_ops::release::stage;
/// use std::{ffi::OsString, process::ExitCode};
/// assert_eq!(stage::run(&[OsString::from("--help")]), ExitCode::SUCCESS);
/// ```
pub fn run(args: &[OsString]) -> ExitCode {
    let started = Instant::now();
    if args.len() == 1 && args[0] == "--licenses" {
        return if super::acquire::licenses::stdout().is_ok() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }
    let options = match Options::parse(args) {
        Ok(None) => {
            return if super::prepare::stdout(format!("{}\n", options::USAGE).as_bytes()).is_ok() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            };
        }
        Ok(Some(options)) => options,
        Err(_) => {
            let _ = writeln!(io::stderr(), "{}", options::USAGE);
            return ExitCode::from(2);
        }
    };
    lifecycle(started, options.whole, || {
        let mut owner = match Owner::new(started, options.whole, options.reserve) {
            Ok(owner) => owner,
            Err(_) => return (false, None),
        };
        if rustix::process::geteuid().as_raw() != 0 || rustix::system::uname().sysname() != c"Linux"
        {
            owner.record(Cause::Policy);
            return (false, Some(owner));
        }
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
        let outcome = runtime.block_on(execute(
            &options,
            &Access::fixed(),
            &mut owner,
            &mut Native,
            started,
        ));
        drop(runtime);
        let output = super::prepare::stdout(&outcome.bytes);
        if output.is_err() {
            owner.record(Cause::Io);
        }
        (outcome.passed && output.is_ok(), Some(owner))
    })
}
