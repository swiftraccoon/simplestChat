//! Opt-in encryption of one failed-image RPM database; never an image verdict.
//!
//! The hosted runner's OpenSSL supplies CMS/GCM and RSA-OAEP/SHA-256. Originals
//! and namespaces remain held through its two bounded children. Kernel I/O and
//! reap latency retain the existing SDK boundary; late completion is refused.

use super::release_controller::files::Directory;
use crate::tooling::{self, Environment, Failure, Result};
use rustix::fs::{Mode, OFlags};
use serde::Serialize;
use sha2::{Digest, Sha256};
use simplestchat_ops::owned_process::{self, CompletionSignals, Group};
use std::{
    ffi::{OsStr, OsString},
    fs::{self, File, Metadata},
    io::Write,
    os::{
        fd::AsRawFd,
        unix::{
            fs::{FileExt, MetadataExt},
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
    time::{Duration, Instant},
};

const MAXIMUM: u64 = 256 * 1024 * 1024;
const CERTIFICATE: u64 = 16 * 1024;
const INPUT: &str = "image/archive/layers/001/usr/lib/sysimage/rpm/rpmdb.sqlite";
const USAGE: &str = "usage: xtask ci rpm-diagnostic --root ABSOLUTE --output ABSOLUTE";
fn failure() -> Failure {
    Failure::new(1, "Encrypted RPM diagnostic refused.")
}
fn io<T>(value: std::io::Result<T>) -> Result<T> {
    value.map_err(|_| failure())
}
fn close(file: File) -> Result<()> {
    nix::unistd::close(file).map_err(|_| failure())
}
fn text<'a>(env: &'a Environment, key: &str) -> Result<&'a str> {
    env.get(OsStr::new(key))
        .and_then(|v| v.to_str())
        .ok_or_else(failure)
}
fn number(value: &str) -> Result<u64> {
    value
        .parse::<u64>()
        .ok()
        .filter(|n| *n > 0 && n.to_string() == value)
        .ok_or_else(failure)
}
fn same(a: &Metadata, b: &Metadata) -> bool {
    binding(a, b)
        && a.nlink() == b.nlink()
        && a.len() == b.len()
        && (a.mtime(), a.mtime_nsec(), a.ctime(), a.ctime_nsec())
            == (b.mtime(), b.mtime_nsec(), b.ctime(), b.ctime_nsec())
}
fn binding(a: &Metadata, b: &Metadata) -> bool {
    (a.dev(), a.ino(), a.mode(), a.uid(), a.gid()) == (b.dev(), b.ino(), b.mode(), b.uid(), b.gid())
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
struct Context {
    root: PathBuf,
    output: PathBuf,
    github_output: PathBuf,
    revision: String,
    run: u64,
    attempt: u64,
    certificate: Vec<u8>,
}
impl Context {
    fn new(args: &[String], env: &Environment, system: &str) -> Result<Self> {
        if args.len() != 4
            || args[0] != "--root"
            || args[2] != "--output"
            || system != "Linux"
            || text(env, "GITHUB_ACTIONS")? != "true"
            || text(env, "GITHUB_REPOSITORY")? != "swiftraccoon/simplestChat"
            || text(env, "GITHUB_EVENT_NAME")? != "push"
            || text(env, "GITHUB_REF")? != "refs/heads/main"
            || text(env, "RUNNER_ENVIRONMENT")? != "github-hosted"
            || text(env, "RUNNER_OS")? != "Linux"
            || text(env, "RUNNER_ARCH")? != "X64"
            || env.get(OsStr::new("ACT")).is_some_and(|v| !v.is_empty())
        {
            return Err(failure());
        }
        let temporary = PathBuf::from(text(env, "RUNNER_TEMP")?);
        let root = PathBuf::from(&args[1]);
        let output = PathBuf::from(&args[3]);
        let github_output = PathBuf::from(text(env, "GITHUB_OUTPUT")?);
        let revision = text(env, "GITHUB_SHA")?.to_string();
        let certificate = text(env, "RPM_DIAGNOSTIC_RECIPIENT")?.as_bytes().to_vec();
        if !temporary.is_absolute()
            || !github_output.is_absolute()
            || root != temporary.join("production-security")
            || output != temporary.join("encrypted-rpm-diagnostic")
            || revision.len() != 40
            || !revision
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || certificate.is_empty()
            || certificate.len() as u64 > CERTIFICATE
        {
            return Err(failure());
        }
        Ok(Self {
            root,
            output,
            github_output,
            revision,
            run: number(text(env, "GITHUB_RUN_ID")?)?,
            attempt: number(text(env, "GITHUB_RUN_ATTEMPT")?)?,
            certificate,
        })
    }
}
struct Control<'a> {
    signals: &'a CompletionSignals,
    body: Instant,
    end: Instant,
}
impl Control<'_> {
    fn check(&self) -> Result<()> {
        if self.signals.status().is_some() || Instant::now() >= self.body {
            Err(failure())
        } else {
            Ok(())
        }
    }
}
fn open(parent: &Directory, name: &OsStr, flags: OFlags) -> Result<File> {
    parent.verify()?;
    rustix::fs::openat(
        parent.file(),
        name,
        flags | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::from_bits_truncate(0o600),
    )
    .map(File::from)
    .map_err(|_| failure())
}
fn descriptor(file: &File) -> OsString {
    format!("/proc/{}/fd/{}", std::process::id(), file.as_raw_fd()).into()
}
fn digest(file: &File, maximum: u64, control: &Control<'_>) -> Result<(u64, String)> {
    let before = io(file.metadata())?;
    if !before.is_file() || before.len() == 0 || before.len() > maximum {
        return Err(failure());
    }
    let mut buffer = [0; 65536];
    let mut offset = 0;
    let mut hash = Sha256::new();
    loop {
        control.check()?;
        let count = match file.read_at(&mut buffer, offset) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            value => io(value)?,
        };
        if count == 0 {
            break;
        }
        offset = offset.checked_add(count as u64).ok_or_else(failure)?;
        if offset > before.len() {
            return Err(failure());
        }
        hash.update(&buffer[..count]);
    }
    if offset != before.len() || !same(&before, &io(file.metadata())?) {
        return Err(failure());
    }
    control.check()?;
    Ok((offset, hex(&hash.finalize())))
}
struct Source {
    parent: Option<Directory>,
    file: Option<File>,
    name: OsString,
    before: Metadata,
}
impl Source {
    fn open(path: &Path) -> Result<Option<Self>> {
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Ok(meta) if meta.is_file() => {}
            _ => return Err(failure()),
        }
        let parent = Directory::open(path.parent().ok_or_else(failure)?)?;
        let name = path.file_name().ok_or_else(failure)?.to_owned();
        let file = match open(&parent, &name, OFlags::RDONLY) {
            Ok(file) => file,
            Err(error) => {
                let _ = parent.finish();
                return Err(error);
            }
        };
        let observed = file.metadata();
        let before = match observed {
            Ok(meta) if meta.is_file() && meta.len() > 0 && meta.len() <= MAXIMUM => meta,
            _ => {
                let _ = close(file);
                let _ = parent.finish();
                return Err(failure());
            }
        };
        Ok(Some(Self {
            parent: Some(parent),
            file: Some(file),
            name,
            before,
        }))
    }
    fn check(&self) -> Result<()> {
        let parent = self.parent.as_ref().ok_or_else(failure)?;
        let held = self.file.as_ref().ok_or_else(failure)?;
        let visible = open(parent, &self.name, OFlags::RDONLY)?;
        let observed = visible.metadata();
        let closed = close(visible);
        if !same(&self.before, &io(held.metadata())?) || !same(&self.before, &io(observed)?) {
            return Err(failure());
        }
        closed?;
        parent.verify()
    }
    fn finish(&mut self) -> Result<()> {
        let checked = self.check();
        let closed = self.file.take().map(close).transpose();
        let parent = self.parent.take().map(Directory::finish).transpose();
        checked?;
        closed?;
        parent?;
        Ok(())
    }
}
struct Artifacts {
    directory: Option<Directory>,
    files: Vec<(&'static str, File, Option<Metadata>)>,
}
impl Artifacts {
    fn create(path: &Path) -> Result<Self> {
        let parent = Directory::open(path.parent().ok_or_else(failure)?)?;
        let made = rustix::fs::mkdirat(
            parent.file(),
            path.file_name().ok_or_else(failure)?,
            Mode::from_bits_truncate(0o700),
        )
        .map_err(|_| failure());
        let closed = parent.finish();
        made?;
        closed?;
        let directory = Directory::open(path)?;
        let profile = io(directory.file().metadata());
        if !profile.is_ok_and(|m| {
            m.mode() & 0o7777 == 0o700 && m.uid() == rustix::process::getuid().as_raw()
        }) {
            let _ = directory.finish();
            return Err(failure());
        }
        Ok(Self {
            directory: Some(directory),
            files: Vec::new(),
        })
    }
    fn add(&mut self, name: &'static str, bytes: &[u8]) -> Result<usize> {
        let file = open(
            self.directory.as_ref().ok_or_else(failure)?,
            OsStr::new(name),
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL,
        )?;
        self.files.push((name, file, None));
        let index = self.files.len() - 1;
        let file = &mut self.files[index].1;
        io(file.write_all(bytes))?;
        io(file.sync_all())?;
        if !bytes.is_empty() {
            self.seal(index)?;
        }
        Ok(index)
    }
    fn file(&self, index: usize) -> &File {
        &self.files[index].1
    }
    fn seal(&mut self, index: usize) -> Result<()> {
        self.files[index].2 = Some(io(self.file(index).metadata())?);
        Ok(())
    }
    fn finish(&mut self, passed: bool) -> Result<()> {
        let Some(directory) = self.directory.take() else {
            return Err(failure());
        };
        let mut error = directory.verify().err();
        for (name, file, sealed) in self.files.drain(..) {
            let before = file.metadata().map_err(|_| failure());
            let visible = open(&directory, OsStr::new(name), OFlags::RDONLY);
            let mut matched = false;
            match visible {
                Ok(visible) => {
                    let after = visible.metadata().map_err(|_| failure());
                    matched = before.as_ref().is_ok_and(|a| {
                        after
                            .as_ref()
                            .is_ok_and(|b| binding(a, b) && a.nlink() == 1 && b.nlink() == 1)
                    });
                    if passed
                        && !sealed.as_ref().is_some_and(|expected| {
                            before.as_ref().is_ok_and(|a| same(expected, a))
                                && after.as_ref().is_ok_and(|b| same(expected, b))
                        })
                    {
                        error.get_or_insert_with(failure);
                    }
                    if let Err(cause) = close(visible) {
                        error.get_or_insert(cause);
                    }
                }
                Err(cause) => {
                    error.get_or_insert(cause);
                }
            }
            if !matched {
                error.get_or_insert_with(failure);
            }
            if let Err(cause) = close(file) {
                error.get_or_insert(cause);
            }
            if matched
                && (!passed || matches!(name, "recipient.pem" | "canonical.pem"))
                && rustix::fs::unlinkat(directory.file(), name, rustix::fs::AtFlags::empty())
                    .is_err()
            {
                error.get_or_insert_with(failure);
            }
        }
        if let Err(cause) = directory.finish() {
            error.get_or_insert(cause);
        }
        error.map_or(Ok(()), Err)
    }
}
trait Crypto {
    async fn run(
        &mut self,
        args: &[OsString],
        destination: &File,
        cap: Duration,
        control: &Control<'_>,
    ) -> Result<()>;
}
// Private construction keeps production fixed; inert tests can select their
// actual installed OpenSSL provider without adding a runtime override.
struct Native {
    program: PathBuf,
}
fn terminal(end: Instant, control: &Control<'_>) -> Result<()> {
    if Instant::now() >= end {
        return Err(failure());
    }
    control.check()
}
impl Crypto for Native {
    async fn run(
        &mut self,
        args: &[OsString],
        destination: &File,
        cap: Duration,
        control: &Control<'_>,
    ) -> Result<()> {
        control.check()?;
        let end = (Instant::now() + cap).min(control.body);
        let mut command = Command::new(&self.program);
        command
            .args(args)
            .env_clear()
            .env("LC_ALL", "C")
            .env("PATH", "/usr/bin:/bin")
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut group = Group::new(command.spawn().map_err(|_| failure())?);
        let pid = group.pid();
        drop(command);
        let observed = loop {
            if control.check().is_err() || Instant::now() >= end {
                break Err(failure());
            }
            tokio::select! {
                ended = owned_process::exited(pid) => {
                    break ended.map_err(|_| failure()).and_then(|()| terminal(end, control));
                },
                _ = tokio::time::sleep(Duration::from_millis(20)) => {},
            }
        };
        if observed.is_err() {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::TERM);
            let grace = (Instant::now() + Duration::from_secs(5)).min(control.end);
            let _ = tokio::time::timeout_at(grace.into(), owned_process::exited(pid)).await;
        }
        let retired = owned_process::finish_before(pid, control.end).await;
        let terminal =
            tokio::time::timeout_at(control.end.into(), owned_process::exited(pid)).await;
        if !matches!(terminal, Ok(Ok(()))) {
            return Err(failure());
        }
        let status = group.reap().await.map_err(|_| failure())?;
        observed?;
        if !retired || !status.success() {
            return Err(failure());
        }
        io(destination.sync_all())?;
        control.check()
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Manifest<'a> {
    schema_version: u8,
    revision: &'a str,
    run_id: u64,
    run_attempt: u64,
    recipient_sha256: String,
    input_sha256: String,
    input_bytes: u64,
    ciphertext_sha256: String,
    ciphertext_bytes: u64,
}
async fn encrypt(
    context: &Context,
    source: &Source,
    output: &mut Artifacts,
    crypto: &mut impl Crypto,
    control: &Control<'_>,
) -> Result<()> {
    let certificate = output.add("recipient.pem", &context.certificate)?;
    let canonical = output.add("canonical.pem", &[])?;
    crypto
        .run(
            &[
                "x509".into(),
                "-in".into(),
                descriptor(output.file(certificate)),
                "-outform".into(),
                "PEM".into(),
                "-out".into(),
                descriptor(output.file(canonical)),
            ],
            output.file(canonical),
            Duration::from_secs(5),
            control,
        )
        .await?;
    let (size, hash) = digest(output.file(canonical), CERTIFICATE, control)?;
    let recipient = hex(&Sha256::digest(&context.certificate));
    if size != context.certificate.len() as u64 || hash != recipient {
        return Err(failure());
    }
    output.seal(canonical)?;
    let (_, original_certificate) = digest(output.file(certificate), CERTIFICATE, control)?;
    if original_certificate != recipient {
        return Err(failure());
    }
    source.check()?;
    let input = source.file.as_ref().ok_or_else(failure)?;
    let (input_bytes, input_sha256) = digest(input, MAXIMUM, control)?;
    source.check()?;
    let ciphertext = output.add("rpmdb.cms.der", &[])?;
    crypto
        .run(
            &[
                "cms".into(),
                "-encrypt".into(),
                "-binary".into(),
                "-aes-256-gcm".into(),
                "-outform".into(),
                "DER".into(),
                "-in".into(),
                descriptor(input),
                "-out".into(),
                descriptor(output.file(ciphertext)),
                "-recip".into(),
                descriptor(output.file(certificate)),
                "-keyopt".into(),
                "rsa_padding_mode:oaep".into(),
                "-keyopt".into(),
                "rsa_oaep_md:sha256".into(),
            ],
            output.file(ciphertext),
            Duration::from_secs(30),
            control,
        )
        .await?;
    io(output.file(ciphertext).sync_all())?;
    let (ciphertext_bytes, ciphertext_sha256) =
        digest(output.file(ciphertext), input_bytes + 65536, control)?;
    output.seal(ciphertext)?;
    if digest(input, MAXIMUM, control)? != (input_bytes, input_sha256.clone()) {
        return Err(failure());
    }
    source.check()?;
    if digest(output.file(certificate), CERTIFICATE, control)?.1 != recipient {
        return Err(failure());
    }
    let manifest = Manifest {
        schema_version: 1,
        revision: &context.revision,
        run_id: context.run,
        run_attempt: context.attempt,
        recipient_sha256: recipient,
        input_sha256,
        input_bytes,
        ciphertext_sha256,
        ciphertext_bytes,
    };
    let bytes = serde_json::to_vec(&manifest).map_err(|_| failure())?;
    let index = output.add("manifest.json", &bytes)?;
    if digest(output.file(index), 1024, control)?.1 != hex(&Sha256::digest(&bytes)) {
        return Err(failure());
    }
    control.check()
}
async fn operation(
    context: &Context,
    crypto: &mut impl Crypto,
    control: &Control<'_>,
) -> Result<bool> {
    control.check()?;
    let Some(mut source) = Source::open(&context.root.join(INPUT))? else {
        return Ok(false);
    };
    let mut output = match Artifacts::create(&context.output) {
        Ok(output) => output,
        Err(error) => {
            let _ = source.finish();
            return Err(error);
        }
    };
    let result = encrypt(context, &source, &mut output, crypto, control).await;
    let files = output.finish(result.is_ok());
    let source = source.finish();
    result?;
    files?;
    source?;
    control.check()?;
    Ok(true)
}
pub(super) fn run(_repository: &Path, args: &[String]) -> anyhow::Result<ExitCode> {
    if args == ["--help"] || args == ["-h"] {
        println!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    }
    let started = Instant::now();
    let env = tooling::inherited_env();
    let host = rustix::system::uname();
    let result = (|| -> Result<()> {
        let context = Context::new(args, &env, host.sysname().to_str().unwrap_or(""))?;
        let mut signals = CompletionSignals::new().map_err(|_| failure())?;
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(_) => {
                signals.finish().map_err(|_| failure())?;
                return Err(failure());
            }
        };
        let end = started + Duration::from_secs(50);
        let control = Control {
            signals: &signals,
            body: end - Duration::from_secs(10),
            end,
        };
        let mut crypto = Native {
            program: "/usr/bin/openssl".into(),
        };
        let encrypted = runtime.block_on(operation(&context, &mut crypto, &control));
        drop(runtime);
        let signal = signals.finish().map_err(|_| failure());
        let ready = encrypted?;
        if signal?.is_some() || Instant::now() >= end {
            return Err(failure());
        }
        if ready {
            let descriptor = rustix::fs::open(
                &context.github_output,
                OFlags::WRONLY
                    | OFlags::APPEND
                    | OFlags::NOFOLLOW
                    | OFlags::NONBLOCK
                    | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|_| failure())?;
            let mut file = File::from(descriptor);
            let written = if file.metadata().is_ok_and(|m| m.is_file()) {
                file.write_all(b"ready=true\n").map_err(|_| failure())
            } else {
                Err(failure())
            };
            let closed = close(file);
            written?;
            closed?;
            if Instant::now() >= end {
                return Err(failure());
            }
        }
        Ok(())
    })();
    if result.is_ok() {
        Ok(ExitCode::SUCCESS)
    } else {
        eprintln!("Encrypted RPM diagnostic refused.");
        Ok(ExitCode::from(1))
    }
}

#[cfg(test)]
#[path = "rpm_diagnostic_tests.rs"]
mod tests;
