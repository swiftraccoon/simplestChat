//! One invocation's bounded commands and its private copy of the sources.
//! What a command prints can hold source text, so both streams go to private
//! files in the invocation's directory, and only fixed codes and compact
//! summaries leave it. Every command has a deadline and a fixed environment
//! and leads its own process group, which is stopped with it.

use crate::{
    Fault, digest, files,
    record::{pretty, safe_name},
    require,
};
use rustix::process::{Pid, Signal, WaitId, WaitIdOptions, kill_process_group, waitid};
use serde_json::{Map, Value};
use std::{
    collections::BTreeSet,
    ffi::{OsStr, OsString},
    fs::{DirBuilder, File, OpenOptions},
    io::{Read, Write},
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::{DirBuilderExt, OpenOptionsExt},
        process::{CommandExt, ExitStatusExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

/// How often a quiet command is looked at.
const POLL: Duration = Duration::from_millis(100);
/// How long a killed leader has to be reaped.
const REAP: Duration = Duration::from_secs(5);
/// Where a command without a search path of the caller's looks.
const SEARCH: &str = "/usr/bin:/bin";

/// Why an invocation stopped: a fixed code, with the name of the check or of
/// the program where there is one. Never anything a command printed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// A fixed code.
    Fixed(&'static str),
    /// The check's command ended with a status the check does not accept.
    Command(&'static str),
    /// The program is nowhere on the search path.
    Prerequisite(&'static str),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fixed(code) => formatter.write_str(code),
            Self::Command(name) => write!(formatter, "scanner_command_failed_{name}"),
            Self::Prerequisite(name) => write!(formatter, "missing_security_prerequisite_{name}"),
        }
    }
}

impl std::error::Error for Failure {}

impl From<Fault> for Failure {
    fn from(fault: Fault) -> Self {
        Self::Fixed(fault.0)
    }
}

/// An answer of the invocation, or why it stopped.
pub type Outcome<T> = std::result::Result<T, Failure>;

/// The bounds of one invocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The most bytes one source file may hold.
    pub source: u64,
    /// The most bytes a snapshot may hold.
    pub tree: u64,
    /// The most names a snapshot may list.
    pub files: usize,
    /// The most bytes a command may write to either of its streams.
    pub report: u64,
    /// How long a command past its bounds has to end when asked.
    pub grace: Duration,
}

impl Limits {
    /// The reviewed bounds.
    pub const REVIEWED: Self = Self {
        source: 16 * 1024 * 1024,
        tree: 128 * 1024 * 1024,
        files: 20_000,
        report: 16 * 1024 * 1024,
        grace: Duration::from_secs(5),
    };
}

/// What one command is given besides its arguments.
#[derive(Clone, Copy, Debug)]
pub struct Run<'a> {
    /// The working directory; the sources' root when there is none.
    pub directory: Option<&'a Path>,
    /// How long the command may take.
    pub within: Duration,
    /// The exit statuses that are answers. A signal's is its negative number.
    pub accepted: &'a [i32],
    /// What the command reads.
    pub input: &'a [u8],
    /// Variables added to the fixed environment, or replacing its own.
    pub variables: &'a [(&'a str, &'a OsStr)],
}

impl Default for Run<'_> {
    fn default() -> Self {
        Self {
            directory: None,
            within: Duration::from_secs(180),
            accepted: &[0],
            input: &[],
            variables: &[],
        }
    }
}

/// Which copy of the sources a snapshot is: the one the checks read, or the
/// one taken after them to prove that nothing changed meanwhile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Snapshot {
    /// Taken before the checks.
    Source,
    /// Taken after them.
    Final,
}

impl Snapshot {
    /// The directory, and the stem of the manifest.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Final => "final-source",
        }
    }

    const fn inventory(self) -> &'static str {
        match self {
            Self::Source => "source-inventory",
            Self::Final => "final-source-inventory",
        }
    }
}

/// One line of an invocation's summary: fields in the order they were given.
pub type Note = Vec<(&'static str, Value)>;

/// One invocation: where the sources are, where its evidence goes, and what
/// its commands have answered so far.
#[derive(Debug)]
pub struct Context {
    root: PathBuf,
    output: PathBuf,
    limits: Limits,
    variables: Vec<(&'static str, OsString)>,
    sequence: u32,
    checks: Vec<Note>,
}

/// `name` below a caller's home, spelled as the reference spells a path:
/// trailing and repeated separators and current-directory steps are dropped,
/// and exactly two leading separators are kept.
fn below_home(home: &OsStr, name: &str) -> OsString {
    let mut given = home.as_bytes();
    while let [rest @ .., b'/'] = given {
        given = rest;
    }
    if given.is_empty() {
        given = b"/";
    }
    let mut path = match given.iter().take_while(|byte| **byte == b'/').count() {
        0 => Vec::new(),
        2 => b"//".to_vec(),
        _ => b"/".to_vec(),
    };
    let steps = given.split(|byte| *byte == b'/');
    for step in steps.filter(|step| !step.is_empty() && *step != b".") {
        path.extend_from_slice(step);
        path.push(b'/');
    }
    path.extend_from_slice(name.as_bytes());
    OsString::from_vec(path)
}

/// The environment of every command, with the new private directory `home`
/// as its home. Of the caller's own, only the search path, the Cargo and
/// rustup homes, the certificate stores and the temporary directory pass: no
/// credential, interpreter hook or scanner configuration does.
fn environment(
    home: &Path,
    ambient: &dyn Fn(&str) -> Option<OsString>,
    linux: bool,
) -> Outcome<Vec<(&'static str, OsString)>> {
    DirBuilder::new()
        .mode(0o700)
        .create(home)
        .map_err(|error| files::unusable(&error))?;
    let locale = if linux { "C.UTF-8" } else { "en_US.UTF-8" };
    let tools = |name: &str, below: &str| match ambient(name) {
        Some(given) => Ok(given),
        None => ambient("HOME")
            .map(|home| below_home(&home, below))
            .ok_or(Failure::Fixed("security_home_unset")),
    };
    let mut variables: Vec<(&'static str, OsString)> = vec![
        ("PATH", ambient("PATH").unwrap_or_else(|| SEARCH.into())),
        ("HOME", home.into()),
        ("LANG", locale.into()),
        ("LC_ALL", locale.into()),
        ("GIT_CONFIG_GLOBAL", "/dev/null".into()),
        ("GIT_CONFIG_SYSTEM", "/dev/null".into()),
        ("GIT_TERMINAL_PROMPT", "0".into()),
        ("PIP_CONFIG_FILE", "/dev/null".into()),
        ("PIP_DISABLE_PIP_VERSION_CHECK", "1".into()),
        ("PYTHONNOUSERSITE", "1".into()),
        ("PYTHONDONTWRITEBYTECODE", "1".into()),
        ("CARGO_HOME", tools("CARGO_HOME", ".cargo")?),
        ("RUSTUP_HOME", tools("RUSTUP_HOME", ".rustup")?),
        ("SEMGREP_SEND_METRICS", "off".into()),
        ("NO_COLOR", "1".into()),
    ];
    for name in ["SSL_CERT_FILE", "SSL_CERT_DIR", "TMPDIR"] {
        if let Some(given) = ambient(name) {
            variables.push((name, given));
        }
    }
    Ok(variables)
}

/// The first file named `name` that this account may execute in the
/// directories of the search path `path`. Only absolute directories are
/// looked in, so the answer never depends on the working directory.
pub fn executable(name: &'static str, path: Option<&OsStr>) -> Outcome<PathBuf> {
    std::env::split_paths(path.unwrap_or(OsStr::new(SEARCH)))
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join(name))
        .find(|candidate| {
            !candidate.is_dir()
                && rustix::fs::access(candidate, rustix::fs::Access::EXEC_OK).is_ok()
        })
        .ok_or(Failure::Prerequisite(name))
}

/// A new private file; a name that exists is never reused.
fn created(path: &Path) -> Outcome<File> {
    let mut options = OpenOptions::new();
    let opened = options.write(true).create_new(true).mode(0o600).open(path);
    Ok(opened.map_err(|error| files::unusable(&error))?)
}

/// How a stream ended.
enum Event {
    Closed,
    Over(&'static str),
    Unwritten,
}

/// Copy `source` into `file` until it closes. A stream that says more than
/// `limit` bytes is left unread from there on, and `over` is reported: the
/// file then holds exactly the limit.
fn copied(
    mut source: impl Read,
    mut file: impl Write,
    limit: u64,
    over: &'static str,
    events: &mpsc::Sender<Event>,
) {
    let mut kept = 0_u64;
    let mut chunk = vec![0_u8; 65536];
    let event = loop {
        let count = match source.read(&mut chunk) {
            Ok(0) => break Event::Closed,
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break Event::Closed,
        };
        let room = usize::try_from(limit - kept).map_or(count, |room| room.min(count));
        if file.write_all(&chunk[..room]).is_err() {
            break Event::Unwritten;
        }
        kept += room as u64;
        if room < count {
            break Event::Over(over);
        }
    };
    let _ = events.send(event);
}

/// Whether the leader has exited. Looking does not reap it, so the number of
/// its group stays this invocation's until the group has been signalled.
fn exited(pid: Pid) -> bool {
    let options = WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT;
    loop {
        match waitid(WaitId::Pid(pid), options) {
            Ok(status) => return status.is_some(),
            Err(rustix::io::Errno::INTR) => {}
            Err(_) => return true,
        }
    }
}

/// End the group: ask it to, give the leader `grace`, kill whatever is left
/// (a descendant may have ignored the request), then reap the leader.
fn stop(child: &mut Child, pid: Pid, grace: Duration) {
    let _ = kill_process_group(pid, Signal::TERM);
    let asked = Instant::now();
    while !exited(pid) && asked.elapsed() < grace {
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = kill_process_group(pid, Signal::KILL);
    let killed = Instant::now();
    while matches!(child.try_wait(), Ok(None)) && killed.elapsed() < REAP {
        std::thread::sleep(Duration::from_millis(10));
    }
}

impl Context {
    /// An invocation over the sources below `root` whose evidence goes to
    /// the existing private directory `output`. `ambient` reads the caller's
    /// environment.
    pub fn new(
        root: &Path,
        output: &Path,
        limits: Limits,
        ambient: &dyn Fn(&str) -> Option<OsString>,
    ) -> Outcome<Self> {
        let linux = cfg!(target_os = "linux");
        Ok(Self {
            root: root.to_path_buf(),
            output: output.to_path_buf(),
            limits,
            variables: environment(&output.join("home"), ambient, linux)?,
            sequence: 0,
            checks: Vec::new(),
        })
    }

    /// Where the sources are.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where the evidence goes.
    pub fn output(&self) -> &Path {
        &self.output
    }

    /// What the invocation has recorded so far.
    pub fn checks(&self) -> &[Note] {
        &self.checks
    }

    /// Record a line of the summary.
    pub fn note(&mut self, note: Note) {
        self.checks.push(note);
    }

    /// Run `argv` as the check `name` and answer its exit status and what it
    /// printed. Both streams are kept in new private files numbered in the
    /// order of the commands; the status and the time taken are recorded
    /// whether or not the status is one of the accepted answers.
    pub fn run<S: AsRef<OsStr>>(
        &mut self,
        name: &'static str,
        argv: &[S],
        how: &Run<'_>,
    ) -> Outcome<(i32, Vec<u8>)> {
        require(
            safe_name(name) && !name.contains(['/', '.']),
            "invalid_scanner_check_name",
        )?;
        self.sequence += 1;
        let stem = format!("{:02}-{name}", self.sequence);
        let kept = self.output.join(format!("{stem}.stdout"));
        let started = Instant::now();
        let output = created(&kept)?;
        let error = created(&self.output.join(format!("{stem}.stderr")))?;
        let status = self.pump(argv, how, (output, error))?;
        let elapsed = (started.elapsed().as_secs_f64() * 1000.0).round() / 1000.0;
        self.checks.push(vec![
            ("name", name.into()),
            ("exitStatus", status.into()),
            ("elapsedSeconds", elapsed.into()),
        ]);
        if !how.accepted.contains(&status) {
            return Err(Failure::Command(name));
        }
        let size = kept.metadata().map_err(|error| files::unusable(&error))?;
        if size.len() == 0 {
            return Ok((status, Vec::new()));
        }
        Ok((status, files::bounded(&kept, self.limits.report, false)?))
    }

    /// Run the command to its end, or stop it at its deadline or when a
    /// stream passes the report limit. A command is over when both streams
    /// are closed and its leader has exited, so one that leaves a descendant
    /// holding a stream runs until the deadline. What it prints goes to the
    /// two `kept` writers, standard output first.
    fn pump<S: AsRef<OsStr>, W: Write + Send + 'static>(
        &self,
        argv: &[S],
        how: &Run<'_>,
        kept: (W, W),
    ) -> Outcome<i32> {
        let (program, arguments) = argv
            .split_first()
            .ok_or(Failure::Fixed("invalid_scanner_command"))?;
        let fixed = self.variables.iter();
        let mut child = Command::new(program)
            .args(arguments)
            .current_dir(how.directory.unwrap_or(&self.root))
            .env_clear()
            .envs(fixed.map(|(name, value)| (*name, value.as_os_str())))
            .envs(how.variables.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(|error| files::unusable(&error))?;
        let pid = Pid::from_child(&child);
        let (sender, events) = mpsc::channel();
        let limit = self.limits.report;
        let mut open = 0;
        if let Some(mut stream) = child.stdin.take() {
            let input = how.input.to_vec();
            // A command that ends without reading is judged by its status.
            std::thread::spawn(move || stream.write_all(&input));
        }
        if let Some(stream) = child.stdout.take() {
            let (events, over) = (sender.clone(), "command_stdout_limit_exceeded");
            std::thread::spawn(move || copied(stream, kept.0, limit, over, &events));
            open += 1;
        }
        if let Some(stream) = child.stderr.take() {
            let over = "command_stderr_limit_exceeded";
            std::thread::spawn(move || copied(stream, kept.1, limit, over, &sender));
            open += 1;
        }
        let deadline = Instant::now() + how.within;
        let stopped = loop {
            if open == 0 && exited(pid) {
                break None;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break Some("command_timed_out");
            }
            if open == 0 {
                std::thread::sleep(left.min(POLL));
                continue;
            }
            match events.recv_timeout(left.min(POLL)) {
                Ok(Event::Closed) => open -= 1,
                Ok(Event::Over(code)) => break Some(code),
                Ok(Event::Unwritten) => break Some("tool_entry_unavailable"),
                Err(_) => {}
            }
        };
        if let Some(code) = stopped {
            stop(&mut child, pid, self.limits.grace);
            return Err(Failure::Fixed(code));
        }
        let status = child.wait().map_err(|error| files::unusable(&error))?;
        let signal = status.signal().map(|signal| -signal);
        status
            .code()
            .or(signal)
            .ok_or(Failure::Fixed("scanner_command_unobserved"))
    }

    /// Copy every tracked and every unignored file into a new private
    /// directory, with a manifest of their SHA-256 beside it, and answer the
    /// directory. `git` lists the files.
    pub fn snapshot(&mut self, which: Snapshot, git: &Path) -> Outcome<PathBuf> {
        let argv = [
            git.as_os_str(),
            OsStr::new("ls-files"),
            OsStr::new("-z"),
            OsStr::new("--cached"),
            OsStr::new("--others"),
            OsStr::new("--exclude-standard"),
        ];
        let (_, listing) = self.run(which.inventory(), &argv, &Run::default())?;
        let destination = self.output.join(which.label());
        let manifest = format!("{}-manifest.json", which.label());
        mirror(
            &listing,
            &self.root,
            &destination,
            &self.output.join(manifest),
            &self.limits,
        )?;
        Ok(destination)
    }
}

/// Copy the files a NUL-separated `listing` names from below `root` into the
/// new directory `destination` and write the manifest. A listed file that is
/// gone has no bytes to scan and is left out; a link, a directory or any
/// other special file is refused, as is a file below a linked directory.
fn mirror(
    listing: &[u8],
    root: &Path,
    destination: &Path,
    manifest: &Path,
    limits: &Limits,
) -> Outcome<()> {
    let text = std::str::from_utf8(listing).map_err(|_| Fault("security_source_path"))?;
    let names: BTreeSet<&str> = text.trim_end_matches('\0').split('\0').collect();
    require(names.len() <= limits.files, "security_source_count")?;
    let mut private = DirBuilder::new();
    private.mode(0o700);
    private
        .create(destination)
        .map_err(|error| files::unusable(&error))?;
    private.recursive(true);
    let mut total = 0_u64;
    let mut identities = Map::new();
    for name in names {
        require(safe_name(name), "security_source_path")?;
        let source = root.join(name);
        let metadata = match source.symlink_metadata() {
            Ok(metadata) => metadata,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                continue;
            }
            Err(error) => return Err(files::unusable(&error).into()),
        };
        // The last of them is empty: joined to the root it is the root with
        // a separator after it, which is the directory and never a link.
        let mut above = Path::new(name).ancestors().skip(1);
        let linked = above.any(|directory| root.join(directory).is_symlink());
        require(!linked, "security_source_parent_link")?;
        require(metadata.is_file(), "security_source_type")?;
        let content = if metadata.len() == 0 {
            Vec::new()
        } else {
            files::bounded(&source, limits.source, false)?
        };
        total += content.len() as u64;
        require(total <= limits.tree, "security_source_size")?;
        let target = destination.join(name);
        if let Some(directory) = target.parent() {
            private
                .create(directory)
                .map_err(|error| files::unusable(&error))?;
        }
        files::write_private(&target, &content, 0o600)?;
        identities.insert(name.to_owned(), digest(&content).into());
    }
    let document = pretty(&Value::Object(identities));
    files::write_private(manifest, document.as_bytes(), 0o600)?;
    Ok(())
}

#[cfg(test)]
mod tests;
