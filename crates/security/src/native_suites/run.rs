//! A suite's operations: the image prepared from exactly its inputs, a
//! suite compiled and kept for later, and a suite run. Every step inside a
//! sandbox is a fixed command this host issues there: nothing of this
//! repository's runs in the image. What the reference's worker did in the
//! sandbox as one command is a phase here: bounded as a whole as that
//! command was, each step within its own bounds as well, and a step that
//! fails is the worker's failure, which the phase reports as the
//! reference's host reported the worker's exit status.

use super::{
    MIB,
    compiled::{ELF_HEADER_BYTES, MAX_BINARY, binary_identity, compiled_receipt, machine},
    corpus::{Case, UNIT_FIXTURES, corpus},
    engine::{ASKED, Engine, checked_image, cleanup, words},
    hexadecimal,
    inputs::inputs_digest,
    read_regular,
    sandbox::{
        INPUT_LABEL, canonical_image_id, compile_commands, replay_args, sandbox_args, target,
        worker_environment,
    },
};
use crate::{
    Fault, Result,
    budget::{Budget, budget},
    digest,
    document::Document,
    paths::canonical,
    require,
};
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};

/// How long a build, or one sanitizer suite, may take.
pub const BUILD_SECONDS: Duration = Duration::from_secs(1200);
/// How long a suite's whole run in its sandbox may take: a build and then
/// its tests. The sandbox's own process lives as long.
const RUN_SECONDS: Duration = Duration::from_secs(1200 + 900);
/// How long the preparation of an image may take.
pub const PREPARE_SECONDS: Duration = Duration::from_secs(2400);
/// How long one replayed input may take.
const REPLAY_SECONDS: Duration = Duration::from_secs(10);
/// How long a copy into or out of a sandbox may take.
const COPY_SECONDS: Duration = Duration::from_secs(120);
/// The most bytes of a phase's log.
pub const MAX_LOG: u64 = 16 * MIB;
/// The most bytes one step of a worker phase may print on either stream.
const STEP_LOG: u64 = MAX_LOG / 2;
/// The LLVM release of the image's toolchain.
const LLVM_VERSION: &str = "23.1.2";
/// Where a kept executable is restored in a sandbox.
const RESTORED: &str = "/work/native-program";
/// The worker's copy in a sandbox.
const WORKER: &str = "/work/worker";
/// A script that writes its standard input to the new private file `$1`.
const WRITE: &str = "umask 077 && set -C && exec /usr/bin/cat > \"$1\"";
/// Where the image lists the packages it was built from.
const BUILDER_RPMS: &str = "/opt/check/builder-rpms.txt";
/// How a file in a sandbox is looked at: the type and permissions of its
/// mode in hexadecimal, its size, and when it last changed, to the
/// nanosecond.
const STATED: &str = "--format=%f %s %.9Y";
/// Where what a command says that nobody keeps goes.
const DROPPED: &str = "/dev/null";
/// The bits of a mode that name the type of its file.
const KIND: u32 = 0o170_000;
/// The type of a regular file.
const REGULAR: u32 = 0o100_000;
/// The type of a link.
const LINK: u32 = 0o120_000;
/// What the reference reported for a file it could not read or write, a
/// directory it could not make and a value it could not take: the error of
/// its interpreter, which its entry point named so.
const UNUSABLE: &str = "invalid_native_security_input";

/// What a suite's operations act through and on.
pub struct Host<'a> {
    /// The checkout.
    pub root: &'a Path,
    /// The local engine.
    pub engine: &'a mut dyn Engine,
    /// This host's architecture as an image names it, or why it has none;
    /// asked only where the reference first asked it.
    pub architecture: Result<&'static str>,
    /// What a build may use here, asked as a sandbox is made.
    pub budget: &'a dyn Fn() -> Result<Budget>,
    /// The identity of this run's sandbox.
    pub run_id: String,
    /// How long this run has lasted.
    pub clock: &'a dyn Fn() -> Duration,
}

/// A record of `fields`.
fn record<const N: usize>(fields: [(&str, Document); N]) -> Document {
    Document::record(fields)
}

/// A number of seconds, rounded to milliseconds as the reference rounds it:
/// to the nearer of the two of the exact value, and to the even one of a
/// tie.
fn seconds(elapsed: Duration) -> Document {
    let rounded = format!("{:.3}", elapsed.as_secs_f64()).parse::<f64>().ok();
    let number = rounded.and_then(serde_json::Number::from_f64);
    number.map_or(Document::Null, Document::Number)
}

/// What an engine printed, as the reference read it: ASCII, without the
/// whitespace around it.
fn stripped(printed: &[u8]) -> Result<String> {
    require(printed.is_ascii(), UNUSABLE)?;
    let space = |letter: char| matches!(letter, ' ' | '\t'..='\r' | '\x1c'..='\x1f');
    let text = String::from_utf8_lossy(printed);
    Ok(text.trim_matches(space).to_owned())
}

/// Publish `value` as the new private file `path`, sorted and indented.
pub fn write_json(path: &Path, value: &Document) -> Result<()> {
    let text = format!("{}\n", value.clone().sorted().pretty().trim_end());
    crate::files::write_private(path, text.as_bytes(), 0o600).map_err(|_| Fault(UNUSABLE))
}

/// The phase `phase`'s two new private logs below `output`.
fn logs(output: &Path, phase: &str) -> Result<(PathBuf, PathBuf)> {
    let paths = (
        output.join(format!("{phase}.stdout.log")),
        output.join(format!("{phase}.stderr.log")),
    );
    for path in [&paths.0, &paths.1] {
        crate::files::write_private(path, b"", 0o600).map_err(|_| Fault(UNUSABLE))?;
    }
    Ok(paths)
}

/// The image toolchain's archive for `architecture`, as the build arguments
/// that name it.
fn toolchain(root: &Path, architecture: &str) -> Result<Vec<(String, String)>> {
    let pin = read_regular(&root.join("security/native/toolchain.json"), MIB)?;
    let pin = Document::read(&pin).map_err(|_| Fault("native_toolchain_unread"))?;
    let platform = match architecture {
        "amd64" => "linux-x86_64",
        _ => "linux-aarch64",
    };
    let asset = pin
        .get("platforms")
        .and_then(|platforms| platforms.get(platform));
    let asset = asset.ok_or(Fault("native_toolchain_unread"))?;
    let text = |name: &str| asset.get(name).and_then(Document::text).map(str::to_owned);
    let bytes = asset
        .get("bytes")
        .and_then(Document::whole)
        .map(|bytes| bytes.to_string());
    let named = [
        ("LLVM_URL", text("url")),
        ("LLVM_SHA256", text("sha256")),
        ("LLVM_BYTES", bytes),
        ("LLVM_DIRECTORY", text("directory")),
    ];
    let mut arguments = Vec::new();
    for (name, value) in named {
        arguments.push((
            name.to_owned(),
            value.ok_or(Fault("native_toolchain_unread"))?,
        ));
    }
    Ok(arguments)
}

/// A new private directory below `parent`, removed again when it is
/// dropped.
pub struct Temporary(pub PathBuf);

impl Temporary {
    /// A new private directory `prefix-…` below `parent`.
    pub fn new(parent: &Path, prefix: &str) -> Result<Self> {
        let mut random = [0_u8; 16];
        let mut source = fs::File::open("/dev/urandom").map_err(|_| Fault(UNUSABLE))?;
        source
            .read_exact(&mut random)
            .map_err(|_| Fault(UNUSABLE))?;
        let path = parent.join(format!("{prefix}-{}", crate::hexadecimal(&random)));
        let created = fs::DirBuilder::new().mode(0o700).create(&path);
        created.map_err(|_| Fault(UNUSABLE))?;
        Ok(Self(path))
    }
}

impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Build the image of exactly the inputs below the checkout, for this
/// host's architecture, and answer its identity.
pub fn prepare(host: &mut Host<'_>, output: &Path) -> Result<Document> {
    let context = Temporary::new(&std::env::temp_dir(), "native-security-context")?;
    let inputs = inputs_digest(host.root, Some(&context.0))?;
    let architecture = host.architecture?;
    let iid = output.join("image.id");
    let dockerfile = context.0.join("build/native-security.Dockerfile");
    let mut arguments = vec![
        "build".to_owned(),
        format!("--platform=linux/{architecture}"),
        "--file".to_owned(),
        dockerfile.to_string_lossy().into_owned(),
        "--label".to_owned(),
        format!("{INPUT_LABEL}={inputs}"),
    ];
    for (name, value) in toolchain(host.root, architecture)? {
        arguments.extend(["--build-arg".to_owned(), format!("{name}={value}")]);
    }
    arguments.extend([
        "--iidfile".to_owned(),
        iid.to_string_lossy().into_owned(),
        context.0.to_string_lossy().into_owned(),
    ]);
    let (stdout, stderr) = logs(output, "prepare")?;
    let status = host.engine.run(
        &arguments,
        &[],
        PREPARE_SECONDS,
        (&stdout, &stderr),
        (MAX_LOG, MAX_LOG),
    )?;
    require(status == 0, "builder_failed")?;
    let image = canonical_image_id(&stripped(&read_regular(&iid, 80)?)?)?;
    let info = checked_image(host.engine, &image, &inputs, architecture)?;
    Ok(record([
        ("schemaVersion", 1_u64.into()),
        ("status", "passed".into()),
        ("imageId", image.as_str().into()),
        ("inputsSha256", inputs.as_str().into()),
        (
            "architecture",
            info.get("Architecture").cloned().unwrap_or(Document::Null),
        ),
    ]))
}

/// Why a worker phase stopped.
enum Stop {
    /// A fault of the host's own, which is the run's.
    Host(Fault),
    /// A failure of the worker's, by the code the reference's worker printed
    /// for it.
    Worker(&'static str),
}

impl From<Fault> for Stop {
    fn from(fault: Fault) -> Self {
        Self::Host(fault)
    }
}

/// What a step of a worker phase answers.
type Step<T> = std::result::Result<T, Stop>;

/// Fail a step as the worker's failure `code` unless `held`.
fn failed(held: bool, code: &'static str) -> Step<()> {
    if held {
        Ok(())
    } else {
        Err(Stop::Worker(code))
    }
}

/// `done`, which the reference's worker reckoned: a refusal of it is the
/// worker's failure.
fn inner<T>(done: Result<T>) -> Step<T> {
    done.map_err(|Fault(code)| Stop::Worker(code))
}

/// What is left of the time before `deadline` at `now`; nothing may start
/// once that is over.
fn left(deadline: Duration, now: Duration) -> Result<Duration> {
    let left = deadline.saturating_sub(now);
    require(!left.is_zero(), "command_timed_out")?;
    Ok(left)
}

/// The first word of what a command printed, if it printed text.
fn first(printed: &[u8]) -> Option<&str> {
    std::str::from_utf8(printed).ok()?.split_whitespace().next()
}

/// Append `line` to the log `path` in what is left of its room: the
/// reference's host stopped its worker as soon as the worker said more.
fn append(path: &Path, line: &[u8]) -> Result<()> {
    let unwritable = |_| Fault("native_log_unwritable");
    let mut file = fs::OpenOptions::new()
        .append(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)
        .map_err(unwritable)?;
    let used = file.metadata().map_err(unwritable)?.len();
    let room = usize::try_from(MAX_LOG.saturating_sub(used)).unwrap_or(usize::MAX);
    let kept = room.min(line.len());
    file.write_all(&line[..kept]).map_err(unwritable)?;
    require(kept == line.len(), "command_stderr_limit_exceeded")
}

/// A run's own sandbox.
struct Sandbox<'a> {
    /// The engine it is reached through.
    engine: &'a mut dyn Engine,
    /// Its full identity.
    identifier: String,
    /// How long the run has lasted.
    clock: &'a dyn Fn() -> Duration,
    /// This host's architecture.
    architecture: &'static str,
    /// The processors the sandbox was given, which its environment names as
    /// its compilers too.
    workers: u64,
}

impl Sandbox<'_> {
    /// The engine's command that runs `argv` in the sandbox as the image's
    /// user: in `directory` where one is named, and reading its standard
    /// input where it is `given` one.
    fn exec(&self, argv: &[String], given: bool, directory: Option<&str>) -> Vec<String> {
        let mut arguments = vec!["exec".to_owned()];
        if given {
            arguments.push("--interactive".to_owned());
        }
        if let Some(directory) = directory {
            arguments.extend(["--workdir".to_owned(), directory.to_owned()]);
        }
        arguments.push(self.identifier.clone());
        arguments.extend(argv.iter().cloned());
        arguments
    }

    /// What the fixed command `argv` printed in the sandbox before
    /// `deadline`; `failed` if it ended with a status other than 0.
    fn printed(
        &mut self,
        argv: &[&str],
        deadline: Duration,
        failed: &'static str,
    ) -> Result<Vec<u8>> {
        let within = left(deadline, (self.clock)())?;
        let arguments = self.exec(&words(argv), false, None);
        match self.engine.ask(&arguments, within) {
            Err(Fault("command_failed")) => Err(Fault(failed)),
            answered => answered,
        }
    }

    /// Import the kept executable `data`, whose SHA-256 is `checksum`, as the
    /// sandbox's restored program, all of it within the time a copy may
    /// take: it is written, made its owner's alone to run, and must then be
    /// there whole. A step that fails is `compiled_import_failed`; only a
    /// bound of the host's own is reported as itself, as the reference's
    /// receiver was one command of the host's.
    fn import(&mut self, data: &[u8], checksum: &str) -> Result<()> {
        const FAILED: &str = "compiled_import_failed";
        let deadline = (self.clock)() + COPY_SECONDS;
        let dropped = Path::new(DROPPED);
        let steps: [(&[&str], &[u8]); 2] = [
            (&["/bin/sh", "-c", WRITE, "sh", RESTORED], data),
            (&["/usr/bin/chmod", "700", RESTORED], &[]),
        ];
        for (argv, input) in steps {
            let within = left(deadline, (self.clock)())?;
            let arguments = self.exec(&words(argv), !input.is_empty(), None);
            let ran = self
                .engine
                .run(&arguments, input, within, (dropped, dropped), (MIB, MIB));
            require(ran? == 0, FAILED)?;
        }
        let size = self.printed(
            &["/usr/bin/stat", "--format=%s", RESTORED],
            deadline,
            FAILED,
        )?;
        let size = first(&size).and_then(|size| size.parse::<u64>().ok());
        require(size == Some(data.len() as u64), FAILED)?;
        let summed = self.printed(&["/usr/bin/sha256sum", RESTORED], deadline, FAILED)?;
        require(first(&summed) == Some(checksum), FAILED)
    }

    /// Copy the suite `mode`'s built executable out of the sandbox into the
    /// new private file `binary`, within the time a copy may take and in at
    /// most the bytes an executable may have.
    fn export(&mut self, mode: &str, binary: &Path) -> Result<()> {
        crate::files::write_private(binary, b"", 0o600).map_err(|_| Fault(UNUSABLE))?;
        let path = format!("/work/build/{}", target(mode)?);
        let arguments = self.exec(&words(&["/usr/bin/cat", &path]), false, None);
        let logs = (binary, Path::new(DROPPED));
        let status = self
            .engine
            .run(&arguments, &[], COPY_SECONDS, logs, (MAX_BINARY, MIB))?;
        require(status == 0, "compiled_export_failed")
    }

    /// Do `work` as the sandbox's phase `phase`, which the reference's
    /// worker was one command for: with its two new logs below `output`,
    /// and all of it within `bound`. A failure of the worker's is reported
    /// as the exit status 1 that `failed` names, once the line the
    /// reference's worker printed for it ends the phase's log of complaints,
    /// as far as that log has room.
    fn phase<T>(
        &mut self,
        output: &Path,
        phase: &str,
        bound: Duration,
        failed: &'static str,
        work: impl FnOnce(&mut Worker<'_, '_>) -> Step<T>,
    ) -> Result<T> {
        let logs = logs(output, phase)?;
        let deadline = (self.clock)() + bound;
        let mut worker = Worker {
            sandbox: self,
            deadline,
            logs,
            ambient: Vec::new(),
        };
        match work(&mut worker) {
            Ok(done) => Ok(done),
            Err(Stop::Host(fault)) => Err(fault),
            Err(Stop::Worker(code)) => {
                let line = format!("Native security check failed: {code}\n");
                append(&worker.logs.1, line.as_bytes())?;
                Err(Fault(failed))
            }
        }
    }
}

/// A phase of the run's sandbox that the reference's worker was one command
/// for: when it must be over, its two logs, and the image's environment.
struct Worker<'s, 'a> {
    /// The sandbox.
    sandbox: &'s mut Sandbox<'a>,
    /// When the phase must be over, on the run's clock.
    deadline: Duration,
    /// The phase's two logs.
    logs: (PathBuf, PathBuf),
    /// The image's environment, as the sandbox sees it.
    ambient: Vec<(String, String)>,
}

impl Worker<'_, '_> {
    /// How long the run has lasted.
    fn now(&self) -> Duration {
        (self.sandbox.clock)()
    }

    /// How long a step may take: its own bound `own`, where it has one that
    /// is shorter than what is left of the phase, or else what is left; and
    /// whether that is its own. Nothing starts once the phase is over.
    fn within(&self, own: Option<Duration>) -> Step<(Duration, bool)> {
        let left = left(self.deadline, self.now())?;
        Ok(match own {
            Some(own) if own < left => (own, true),
            _ => (left, false),
        })
    }

    /// What a step may print on each stream: half of what the phase's log
    /// may hold, or what is left of that where less is; and whether that is
    /// its own. Half of what is left is still the step's own: the reference's
    /// host saw too much only once the worker's command did.
    fn room(&self) -> Step<((u64, u64), (bool, bool))> {
        let used = |path: &Path| {
            let metadata = fs::metadata(path).map_err(|_| Fault("native_log_unwritable"));
            metadata.map(|metadata| metadata.len())
        };
        let share = |used: u64| {
            let left = MAX_LOG.saturating_sub(used);
            if STEP_LOG <= left {
                (STEP_LOG, true)
            } else {
                (left, false)
            }
        };
        let (output, error) = (share(used(&self.logs.0)?), share(used(&self.logs.1)?));
        Ok(((output.0, error.0), (output.1, error.1)))
    }

    /// Run `argv` in the sandbox with `input`, in `directory` where one is
    /// named, within its own bound `own` where it has one, with its two
    /// streams in the phase's logs. A status other than 0 is the worker's
    /// failure `failed`; a bound that was the step's own is the worker's
    /// failure as well, and one that was the phase's is the host's.
    fn run(
        &mut self,
        argv: &[String],
        input: &[u8],
        own: Option<Duration>,
        directory: Option<&str>,
        failed: &'static str,
    ) -> Step<()> {
        let (within, timed) = self.within(own)?;
        let (room, limited) = self.room()?;
        let arguments = self.sandbox.exec(argv, !input.is_empty(), directory);
        let logs = (self.logs.0.as_path(), self.logs.1.as_path());
        match self
            .sandbox
            .engine
            .run(&arguments, input, within, logs, room)
        {
            Ok(0) => Ok(()),
            Ok(_) => Err(Stop::Worker(failed)),
            Err(Fault(code @ "command_timed_out")) if timed => Err(Stop::Worker(code)),
            Err(Fault(code @ "command_stdout_limit_exceeded")) if limited.0 => {
                Err(Stop::Worker(code))
            }
            Err(Fault(code @ "command_stderr_limit_exceeded")) if limited.1 => {
                Err(Stop::Worker(code))
            }
            Err(fault) => Err(Stop::Host(fault)),
        }
    }

    /// What the fixed command `argv` printed in the sandbox, as a read of
    /// the reference's worker in its own process: within what is left of the
    /// phase, which is the host's to bound, and a failure as its
    /// interpreter's where it ends with a status other than 0. What it may
    /// print is bounded by the engine alone, so that bound is the step's.
    fn read(&mut self, argv: &[&str]) -> Step<Vec<u8>> {
        let (within, _) = self.within(None)?;
        let arguments = self.sandbox.exec(&words(argv), false, None);
        match self.sandbox.engine.ask(&arguments, within) {
            Ok(printed) => Ok(printed),
            Err(Fault("command_failed")) => Err(Stop::Worker(UNUSABLE)),
            Err(Fault(
                code @ ("command_stdout_limit_exceeded" | "command_stderr_limit_exceeded"),
            )) => Err(Stop::Worker(code)),
            Err(fault) => Err(Stop::Host(fault)),
        }
    }

    /// Write `data` to the new private file `path` in the sandbox.
    fn write(&mut self, path: &str, data: &[u8]) -> Step<()> {
        let script = words(&["/bin/sh", "-c", WRITE, "sh", path]);
        self.run(&script, data, None, None, UNUSABLE)
    }

    /// Make the directory `path` in the sandbox, `parents` and all where it
    /// may be there already.
    fn directory(&mut self, path: &str, parents: bool) -> Step<()> {
        let argv = if parents {
            words(&["/usr/bin/mkdir", "-p", path])
        } else {
            words(&["/usr/bin/mkdir", path])
        };
        self.run(&argv, &[], None, None, UNUSABLE)
    }

    /// Read the image's environment as the sandbox sees it, each variable
    /// whole.
    fn environment(&mut self) -> Step<()> {
        let printed = self.read(&["/usr/bin/env", "-0"])?;
        let printed = String::from_utf8(printed).map_err(|_| Stop::Worker(UNUSABLE))?;
        self.ambient = printed
            .split('\0')
            .filter_map(|entry| entry.split_once('='))
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect();
        Ok(())
    }

    /// Run `argv` in the worker's copy with only the prepared environment
    /// and the parser family `family`, within its own bound `own`, and
    /// answer how long it took.
    fn command(&mut self, argv: &[String], own: Duration, family: Option<&str>) -> Step<Duration> {
        let ambient = &self.ambient;
        let lookup = |name: &str| {
            let found = ambient.iter().find(|(held, _)| held == name);
            found.map(|(_, value)| value.clone())
        };
        let mut command = words(&["/usr/bin/env", "-i"]);
        for (name, value) in inner(worker_environment(&lookup, family))? {
            command.push(format!("{name}={value}"));
        }
        command.extend(argv.iter().cloned());
        let started = self.now();
        self.run(
            &command,
            &[],
            Some(own),
            Some(WORKER),
            "worker_command_failed",
        )?;
        Ok(self.now().saturating_sub(started))
    }

    /// Restore the worker's sources and every reviewed unit fixture in the
    /// sandbox.
    fn sources(&mut self, cases: &[Case]) -> Step<()> {
        let copy = [
            "/usr/bin/cp",
            "-R",
            "--preserve=mode,timestamps",
            "/opt/worker",
            WORKER,
        ];
        // The reference's worker first required the image's copy of the
        // worker, which is what a copy that fails here misses.
        self.run(&words(&copy), &[], None, None, "worker_sandbox_required")?;
        self.directory("/work/worker/test/data", false)?;
        for case in cases {
            if let Some((_, fixture)) = UNIT_FIXTURES
                .iter()
                .find(|(name, _)| *name == case.identifier)
            {
                // The upstream reader leaves out a file's last byte; every
                // reviewed byte is kept.
                let mut data = case.data.clone();
                data.push(b'\n');
                self.write(&format!("/work/worker/test/data/{fixture}"), &data)?;
            }
        }
        Ok(())
    }

    /// The mode, size and time of last change of `path` in the sandbox, not
    /// through a final link.
    fn stated(&mut self, path: &str) -> Step<(u32, u64, String)> {
        let printed = self.read(&["/usr/bin/stat", STATED, path])?;
        let text = String::from_utf8(printed).ok();
        let parsed = text.as_deref().and_then(|text| {
            let fields: Vec<&str> = text.split_whitespace().collect();
            let [mode, size, changed] = fields[..] else {
                return None;
            };
            let mode = u32::from_str_radix(mode, 16).ok()?;
            Some((mode, size.parse().ok()?, changed.to_owned()))
        });
        parsed.ok_or(Stop::Worker(UNUSABLE))
    }

    /// The size and SHA-256 of the executable `path` in the sandbox, held
    /// to the rules of a kept one: an ordinary native one of this host's
    /// architecture, never read through a final link, and refused if it
    /// changes while it is read.
    fn identity(&mut self, path: &str) -> Step<(u64, String)> {
        let (mode, size, changed) = self.stated(path)?;
        // The reference opened it without following a final link, which its
        // interpreter refused in its own words.
        failed(mode & KIND != LINK, UNUSABLE)?;
        failed(mode & KIND == REGULAR, "compiled_binary_not_regular")?;
        let sized = (ELF_HEADER_BYTES..=MAX_BINARY).contains(&size);
        failed(sized, "compiled_binary_size")?;
        let header = self.read(&["/usr/bin/head", "--bytes=20", path])?;
        let machine = inner(machine(self.sandbox.architecture))?;
        let elf = header.len() == 20
            && header[..6] == *b"\x7fELF\x02\x01"
            && u16::from_le_bytes([header[18], header[19]]) == machine;
        failed(elf, "compiled_binary_architecture")?;
        let summed = self.read(&["/usr/bin/sha256sum", path])?;
        let checksum = first(&summed).filter(|checksum| hexadecimal(checksum, 64));
        let checksum = checksum.ok_or(Stop::Worker(UNUSABLE))?.to_owned();
        let (_, after, again) = self.stated(path)?;
        failed((after, again) == (size, changed), "compiled_binary_changed")?;
        Ok((size, checksum))
    }

    /// The whole of the regular file `path` in the sandbox, of at most
    /// `limit` bytes, read as the reference's worker read one: never through
    /// a final link, and refused if it changes while it is read.
    fn regular(&mut self, path: &str, limit: u64) -> Step<Vec<u8>> {
        let (mode, size, _) = self.stated(path)?;
        failed(mode & KIND != LINK, UNUSABLE)?;
        failed(mode & KIND == REGULAR, "input_not_regular")?;
        failed(size <= limit, "input_size_exceeded")?;
        let bytes = format!("--bytes={}", limit + 1);
        let data = self.read(&["/usr/bin/head", &bytes, path])?;
        failed(data.len() as u64 == size, "input_changed")?;
        Ok(data)
    }

    /// Build the suite `mode` with as many compilers as the sandbox's own
    /// budget allows, each phase of it timed, and answer the build's record.
    fn compile(&mut self, mode: &str) -> Step<Document> {
        // The reference's worker reckoned its budget inside the sandbox, of
        // the processors and memory the sandbox was given and the compilers
        // its environment names, which are all the sandbox's processors: this
        // is that budget.
        let workers = self.sandbox.workers;
        let memory = (workers * 2048 + 1024) * MIB;
        let budget = inner(budget(workers, memory, Some(&workers.to_string())))?;
        let mut timings = Vec::new();
        for (phase, argv) in inner(compile_commands(mode, budget.workers))? {
            let elapsed = self.command(&argv, BUILD_SECONDS, None)?;
            timings.push((phase.to_owned(), seconds(elapsed)));
        }
        let built = format!("/work/build/{}", inner(target(mode))?);
        let (size, checksum) = self.identity(&built)?;
        Ok(record([
            ("status", "built".into()),
            ("mode", mode.into()),
            ("workers", budget.workers.into()),
            ("phaseSeconds", Document::Record(timings)),
            ("binaryBytes", size.into()),
            ("binarySha256", checksum.as_str().into()),
        ]))
    }
}

/// Do `work` in a new sandbox of this run, of the image `image` of the
/// inputs `inputs`, whose ownership is recorded in `output` first, and
/// remove exactly that sandbox afterwards, whatever came of the work.
fn owned<T>(
    host: &mut Host<'_>,
    image: &str,
    inputs: &str,
    output: &Path,
    work: impl FnOnce(&mut Sandbox<'_>) -> Result<T>,
) -> Result<T> {
    let run_id = host.run_id.clone();
    let ownership = record([
        ("schemaVersion", 1_u64.into()),
        ("runId", run_id.as_str().into()),
        ("imageId", image.into()),
        ("inputsSha256", inputs.into()),
    ]);
    write_json(&output.join("ownership.json"), &ownership)?;
    let done = (|| {
        let budget = (host.budget)()?;
        let architecture = host.architecture?;
        let arguments = sandbox_args(image, &run_id, inputs, budget, architecture)?;
        let mut created = arguments[..arguments.len() - 1].to_vec();
        created.extend([
            "--entrypoint=/usr/bin/sleep".to_owned(),
            image.to_owned(),
            RUN_SECONDS.as_secs().to_string(),
        ]);
        let identifier = stripped(&host.engine.ask(&created, ASKED)?)?;
        require(hexadecimal(&identifier, 64), "container_identity")?;
        host.engine.ask(&words(&["start", &identifier]), ASKED)?;
        let mut sandbox = Sandbox {
            engine: &mut *host.engine,
            identifier,
            clock: host.clock,
            architecture,
            workers: budget.workers,
        };
        work(&mut sandbox)
    })();
    // A failed removal is what is reported, as the reference's would be.
    cleanup(host.engine, &run_id, image)?;
    done
}

/// What a suite asks for: its mode, its image and a kept build.
pub struct Request<'a> {
    /// The suite.
    pub mode: &'a str,
    /// The image it runs in.
    pub image: &'a str,
    /// Where its build is kept, or is to be.
    pub compiled: Option<&'a Path>,
}

/// Compile the suite `request` asks for and keep it in its new directory,
/// published only once its executable is proved and its sandbox removed.
pub fn compile(host: &mut Host<'_>, request: &Request<'_>, output: &Path) -> Result<Document> {
    let directory = request
        .compiled
        .ok_or(Fault("compiled_directory_required"))?;
    require(canonical(directory), "compiled_path")?;
    require(!directory.exists(), "compiled_already_exists")?;
    let inputs = inputs_digest(host.root, None)?;
    let architecture = host.architecture?;
    checked_image(host.engine, request.image, &inputs, architecture)?;
    // The root, the one directory without a parent, is its own.
    let parent = directory.parent().unwrap_or(directory);
    let temporary = Temporary::new(parent, "native-compiled")?;
    let stage = temporary.0.join("artifact");
    let made = fs::DirBuilder::new().mode(0o700).create(&stage);
    made.map_err(|_| Fault(UNUSABLE))?;
    let mode = request.mode;
    let cases = corpus(host.root)?;
    let binary = stage.join("binary");
    let built = owned(host, request.image, &inputs, output, |sandbox| {
        let built = sandbox.phase(
            output,
            "compile",
            BUILD_SECONDS,
            "native_build_failed_exit_1",
            |worker| {
                worker.environment()?;
                worker.sources(&cases)?;
                worker.compile(mode)
            },
        )?;
        sandbox.export(mode, &binary)?;
        let executable = fs::set_permissions(&binary, fs::Permissions::from_mode(0o755));
        executable.map_err(|_| Fault(UNUSABLE))?;
        let (size, checksum) = binary_identity(&binary, architecture)?;
        let same = built.get("binaryBytes").and_then(Document::whole) == Some(i128::from(size))
            && built.get("binarySha256").and_then(Document::text) == Some(checksum.as_str());
        require(same, "compiled_copy_mismatch")?;
        Ok((size, checksum, built))
    });
    let (size, checksum, built) = built?;
    let receipt = record([
        ("schemaVersion", 1_u64.into()),
        ("status", "built".into()),
        ("mode", mode.into()),
        ("imageId", request.image.into()),
        ("inputsSha256", inputs.as_str().into()),
        ("architecture", architecture.into()),
        ("binaryBytes", size.into()),
        ("binarySha256", checksum.as_str().into()),
        ("worker", built),
    ]);
    write_json(&stage.join("receipt.json"), &receipt)?;
    fs::rename(&stage, directory).map_err(|_| Fault(UNUSABLE))?;
    let mut answered = receipt;
    answered.put("cleanup", "verified".into());
    Ok(answered)
}

/// Run the whole suite `request` asks for in a new sandbox: a fresh build,
/// or the kept one imported and proved again, and then every sanitizer test
/// or every reviewed input.
pub fn run(host: &mut Host<'_>, request: &Request<'_>, output: &Path) -> Result<Document> {
    let inputs = inputs_digest(host.root, None)?;
    let architecture = host.architecture?;
    let info = checked_image(host.engine, request.image, &inputs, architecture)?;
    let mode = request.mode;
    let kept = match request.compiled {
        Some(directory) => {
            let receipt = compiled_receipt(directory, mode, request.image, &inputs, architecture)?;
            let checksum = receipt
                .get("binarySha256")
                .and_then(Document::text)
                .ok_or(Fault("compiled_receipt_mismatch"))?;
            Some((directory, checksum.to_owned()))
        }
        None => None,
    };
    let cases = corpus(host.root)?;
    let run_id = host.run_id.clone();
    let worker = owned(host, request.image, &inputs, output, |sandbox| {
        if let Some((directory, checksum)) = &kept {
            let data = read_regular(&directory.join("binary"), MAX_BINARY)?;
            require(digest(&data) == *checksum, "compiled_transfer_changed")?;
            sandbox.import(&data, checksum)?;
        }
        sandbox.phase(
            output,
            "native",
            RUN_SECONDS,
            "native_failed_exit_1",
            |worker| {
                let started = worker.now();
                worker.environment()?;
                worker.sources(&cases)?;
                let (binary, build) = match &kept {
                    Some((_, checksum)) => {
                        let (_, found) = worker.identity(RESTORED)?;
                        failed(found == *checksum, "compiled_runtime_mismatch")?;
                        (RESTORED.to_owned(), Document::Record(Vec::new()))
                    }
                    None => {
                        let build = worker.compile(mode)?;
                        (format!("/work/build/{}", inner(target(mode))?), build)
                    }
                };
                let tested = worker.now();
                let mut completed = Vec::new();
                if mode == "asan" || mode == "ubsan" {
                    let argv = [binary, "--invisibles".to_owned()];
                    worker.command(&argv, BUILD_SECONDS, None)?;
                    completed.push(mode.into());
                } else {
                    worker.directory("/work/corpus", false)?;
                    for case in &cases {
                        let directory = format!("/work/corpus/{}", case.family);
                        worker.directory(&directory, true)?;
                        worker.write(&format!("{directory}/{}", case.identifier), &case.data)?;
                    }
                    for case in &cases {
                        let path = format!("/work/corpus/{}/{}", case.family, case.identifier);
                        let argv = replay_args(&binary, &path);
                        worker.command(&argv, REPLAY_SECONDS, Some(&case.family))?;
                        completed.push(case.identifier.as_str().into());
                    }
                }
                let elapsed = seconds(worker.now().saturating_sub(started));
                let testing = seconds(worker.now().saturating_sub(tested));
                let rpms = worker.regular(BUILDER_RPMS, MIB)?;
                Ok(record([
                    ("status", "passed".into()),
                    ("mode", mode.into()),
                    ("completed", Document::List(completed)),
                    ("elapsedSeconds", elapsed),
                    ("testSeconds", testing),
                    ("build", build),
                    ("llvmVersion", LLVM_VERSION.into()),
                    ("opensslInstrumented", false.into()),
                    ("builderRpmSha256", digest(&rpms).as_str().into()),
                ]))
            },
        )
    })?;
    Ok(record([
        ("schemaVersion", 1_u64.into()),
        ("status", "passed".into()),
        ("mode", mode.into()),
        ("runId", run_id.as_str().into()),
        ("imageId", request.image.into()),
        ("inputsSha256", inputs.as_str().into()),
        (
            "architecture",
            info.get("Architecture").cloned().unwrap_or(Document::Null),
        ),
        ("compiledReused", request.compiled.is_some().into()),
        ("worker", worker),
        ("cleanup", "verified".into()),
    ]))
}
