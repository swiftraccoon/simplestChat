//! The scanners of the image gate, each in a container the gate owns: of the
//! separately trusted scanner base, under a label of its own, without a
//! network unless it fetches the vulnerability database, as an account that
//! is not the superuser, on a root file system it cannot write and within
//! explicit bounds. The application in the image is never run. Whatever
//! becomes of a scanner, its container is removed by its proved identity
//! and by nothing else, and two scanners at most run at once.

use crate::{
    Fault, Result,
    document::Document,
    files,
    image::{
        archive, cache,
        diagnostic::{self, Scanned},
        outputs, small_hex,
    },
    require,
};
use std::{
    ffi::OsString,
    fs::{DirBuilder, File},
    io::Read,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

/// The scanners.
pub const TOOLS: [&str; 3] = ["syft", "grype", "gitleaks"];
/// The label that holds the owner of a scanner's container.
pub const LABEL: &str = "simplestchat.security.image";
/// The only places a scanner may be given something to read.
const MOUNTS: [&str; 3] = ["/input", "/layers", "/db"];
/// How many scanners may run at once, over every branch.
pub const SLOTS: usize = 2;
/// How long a command of the engine may take that sets nothing up.
const QUICK: u64 = 30;
/// How long the scanner base may take to arrive.
const PULL: u64 = 300;
/// How often a scanner that waits for a slot looks for cancellation.
const WAIT: Duration = Duration::from_millis(100);
/// The account a scanner runs as when the gate is the superuser's.
const NOBODY: u32 = 65_532;

/// The bounds of the scanners.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The most bytes a scanner may write.
    pub output: u64,
    /// The most bytes the database's update may hold while it fetches the
    /// replacement beside the database it was seeded with.
    pub update: u64,
    /// The most entries a scanner's output may hold.
    pub entries: u64,
    /// How long a scanner may take, in seconds.
    pub seconds: u64,
    /// The bytes of a scanner's scratch space.
    pub scratch: u64,
    /// The bytes of the scratch space of the archive's catalog.
    pub catalog: u64,
    /// The most scanners one invocation may run.
    pub scanners: u32,
    /// The most bytes of an engine command's log the diagnostic reads.
    pub log: u64,
}

impl Limits {
    /// The reviewed bounds.
    pub const REVIEWED: Self = Self {
        output: 768 << 20,
        update: 8 << 30,
        entries: 20_000,
        seconds: 900,
        scratch: 256 << 20,
        catalog: 1 << 30,
        scanners: 8,
        log: 4 << 20,
    };
}

/// The platforms an image is scanned for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    /// 64-bit x86.
    Amd64,
    /// 64-bit Arm.
    Arm64,
}

impl Platform {
    /// The platform named `text` as the engine names it, if it is one.
    pub fn parsed(text: &str) -> Option<Self> {
        match text {
            "linux/amd64" => Some(Self::Amd64),
            "linux/arm64" => Some(Self::Arm64),
            _ => None,
        }
    }

    /// Its name as the engine has it.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Amd64 => "linux/amd64",
            Self::Arm64 => "linux/arm64",
        }
    }

    /// Its name as the lock of the scanners has it.
    pub const fn tools(self) -> &'static str {
        match self {
            Self::Amd64 => "linux-x86_64",
            Self::Arm64 => "linux-aarch64",
        }
    }
}

/// What runs the container engine for a sandbox.
pub trait Engine: Send {
    /// The name of the engine's program.
    fn name(&self) -> &str;
    /// Where the private log of the next command will be, when commands
    /// are logged.
    fn log(&self) -> Option<PathBuf>;
    /// Run the engine with `arguments` for at most `seconds`, stopped as
    /// lost once `healthy` says that what it needs is gone, and answer its
    /// exit status and what it printed, trimmed. A status other than 0 is
    /// refused unless `failing` admits it.
    fn command(
        &mut self,
        arguments: &[OsString],
        seconds: u64,
        failing: bool,
        healthy: &mut dyn FnMut() -> bool,
    ) -> Result<(i32, String)>;
    /// The same engine for a branch, whose commands are logged in `logs`
    /// and numbered from the first.
    fn branch(&self, logs: &Path) -> Box<dyn Engine>;
}

/// The slots scanners run in, shared by every branch of a sandbox.
#[derive(Debug)]
pub struct Slots {
    free: Mutex<usize>,
    freed: Condvar,
}

/// A slot that is held until it is dropped.
struct Slot<'a>(&'a Slots);

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        *self.0.free.lock().unwrap_or_else(PoisonError::into_inner) += 1;
        self.0.freed.notify_one();
    }
}

impl Slots {
    /// `count` free slots.
    pub fn new(count: usize) -> Self {
        Self {
            free: Mutex::new(count),
            freed: Condvar::new(),
        }
    }

    /// A free slot, once there is one. A scanner that is still waiting
    /// when `cancelled` is set is refused.
    fn taken(&self, cancelled: &AtomicBool) -> Result<Slot<'_>> {
        let mut free = self.free.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if *free > 0 {
                *free -= 1;
                return Ok(Slot(self));
            }
            let waited = self.freed.wait_timeout(free, WAIT);
            free = waited.unwrap_or_else(PoisonError::into_inner).0;
            if *free == 0 {
                require(!cancelled.load(Ordering::SeqCst), "image_scan_cancelled")?;
            }
        }
    }
}

/// A database cache that seeds the scanner's own update.
#[derive(Clone, Debug)]
pub struct Cache {
    /// Where it is.
    pub directory: PathBuf,
    /// The pin of the scanner its receipt must be of.
    pub pin: String,
    /// The bounds of its files.
    pub limits: cache::Limits,
}

/// One scanner invocation.
#[derive(Clone, Copy, Debug)]
pub struct Scan<'a> {
    /// The scanner.
    pub tool: &'a str,
    /// Its arguments.
    pub arguments: &'a [&'a str],
    /// The new private directory it writes to, as `/output`.
    pub destination: &'a Path,
    /// What it reads: each place and what is there, read only.
    pub mounts: &'a [(&'a str, &'a Path)],
    /// Whether it fetches the database, with a network and nothing to read.
    pub online: bool,
    /// Variables of its environment besides the fixed ones.
    pub variables: &'a [(&'a str, &'a str)],
}

/// One isolated engine and the scanners that run in it.
pub struct Sandbox {
    /// What runs the engine.
    pub(crate) engine: Box<dyn Engine>,
    /// Where the evidence goes.
    pub(crate) output: PathBuf,
    /// The read-only bundle of the verified scanners.
    pub(crate) tools: PathBuf,
    /// The identity of the scanner base.
    pub(crate) base: String,
    /// The platform scanned for.
    pub(crate) platform: Platform,
    /// How many scanners this branch has started.
    pub(crate) sequence: u32,
    /// What the names of this branch's evidence begin with.
    pub(crate) prefix: String,
    /// How many scanners this branch may start.
    pub(crate) allowance: u32,
    /// The slots shared by every branch.
    pub(crate) slots: Arc<Slots>,
    /// The cancellation shared by every branch.
    pub(crate) cancelled: Arc<AtomicBool>,
    /// The database cache, when there is one.
    pub(crate) cache: Option<Cache>,
    /// The bounds of the scanners.
    pub(crate) limits: Limits,
    /// The account the scanners run as.
    pub(crate) identity: (u32, u32),
    /// Whether the gate is the superuser's, which hands outputs to that
    /// account.
    pub(crate) superuser: bool,
}

/// The account the scanners run as: the gate's own, or one of nobody's
/// when the gate is the superuser's.
fn account() -> ((u32, u32), bool) {
    let uid = rustix::process::getuid().as_raw();
    if uid == 0 {
        ((NOBODY, NOBODY), true)
    } else {
        ((uid, rustix::process::getgid().as_raw()), false)
    }
}

/// Make the new directory `path`, which only its owner may use.
fn private(path: &Path) -> Result<()> {
    let made = DirBuilder::new().mode(0o700).create(path);
    made.map_err(|error| files::unusable(&error))
}

/// Give `path` the permissions `mode`.
fn opened(path: &Path, mode: u32) -> Result<()> {
    let permissions = std::fs::Permissions::from_mode(mode);
    std::fs::set_permissions(path, permissions).map_err(|error| files::unusable(&error))
}

/// Publish `value` as the new private evidence file `path`, sorted as the
/// reference writes it.
fn written(path: &Path, value: &Document) -> Result<()> {
    archive::new_file(path, value.clone().sorted().pretty().as_bytes())
}

/// The owner of a new container: a random version 4 identifier.
fn owner() -> Result<String> {
    let mut bytes = [0_u8; 16];
    let mut random = File::open("/dev/urandom").map_err(|error| files::unusable(&error))?;
    random
        .read_exact(&mut bytes)
        .map_err(|error| files::unusable(&error))?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(crate::hexadecimal(&bytes))
}

/// `pieces` joined as one argument.
fn joined(pieces: &[&std::ffi::OsStr]) -> OsString {
    let mut joined = OsString::new();
    for piece in pieces {
        joined.push(piece);
    }
    joined
}

/// Text arguments.
fn arguments(texts: &[&str]) -> Vec<OsString> {
    texts.iter().map(OsString::from).collect()
}

/// The time since `started`, to the millisecond, in seconds.
fn elapsed(started: Instant) -> Document {
    let seconds = (started.elapsed().as_secs_f64() * 1000.0).round() / 1000.0;
    serde_json::Number::from_f64(seconds).map_or(Document::Null, Document::Number)
}

impl Sandbox {
    /// A sandbox whose engine is `engine`, whose evidence goes to `output`
    /// and whose scanners come from `tools` and run in the base `base`, for
    /// `platform`, within `limits`.
    pub fn new(
        engine: Box<dyn Engine>,
        output: &Path,
        tools: &Path,
        base: &str,
        platform: Platform,
        limits: Limits,
    ) -> Self {
        let (identity, superuser) = account();
        Self {
            engine,
            output: output.to_path_buf(),
            tools: tools.to_path_buf(),
            base: base.to_owned(),
            platform,
            sequence: 0,
            prefix: String::new(),
            allowance: limits.scanners,
            slots: Arc::new(Slots::new(SLOTS)),
            cancelled: Arc::new(AtomicBool::new(false)),
            cache: None,
            limits,
            identity,
            superuser,
        }
    }

    /// Whether the sandbox was cancelled.
    pub fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// A branch named `name` that may start `allowance` scanners: its own
    /// command log in the new directory `<name>-commands`, its evidence
    /// named after it, and only the slots and the cancellation shared.
    pub fn branch(&self, name: &str, allowance: u32) -> Result<Self> {
        let logs = self.output.join(format!("{name}-commands"));
        private(&logs)?;
        Ok(Self {
            engine: self.engine.branch(&logs),
            output: self.output.clone(),
            tools: self.tools.clone(),
            base: self.base.clone(),
            platform: self.platform,
            sequence: 0,
            prefix: format!("{name}-"),
            allowance,
            slots: Arc::clone(&self.slots),
            cancelled: Arc::clone(&self.cancelled),
            cache: self.cache.clone(),
            limits: self.limits,
            identity: self.identity,
            superuser: self.superuser,
        })
    }

    /// Run the scanner `scan` once a slot is free, and answer its exit
    /// status and what it printed. The slot is held until the container is
    /// removed, or the sandbox cancelled because its removal is not proved.
    pub fn run(&mut self, scan: &Scan<'_>) -> Result<(i32, String)> {
        let slots = Arc::clone(&self.slots);
        let _slot = slots.taken(&self.cancelled)?;
        self.scanned(scan)
    }

    /// Run one verified scanner and remove its exact container whatever
    /// becomes of it. Its evidence is the container's record once the
    /// removal is proved, and the diagnostic in any case. A failure to keep
    /// the diagnostic is answered first, then one of the removal, then a
    /// container state the diagnostic refuses, an output past its budget,
    /// and the failure of the scanner's own command.
    fn scanned(&mut self, scan: &Scan<'_>) -> Result<(i32, String)> {
        require(TOOLS.contains(&scan.tool), "image_scanner_unknown")?;
        let update = scan.tool == "grype" && scan.arguments == ["db", "update"];
        require(
            !scan.online || (update && scan.mounts.is_empty()),
            "image_online_artifact_mount",
        )?;
        require(!self.cancelled(), "image_scan_cancelled")?;
        require(self.sequence < self.allowance, "image_scanner_count")?;
        self.sequence += 1;
        let owner = owner()?;
        let name = format!("simplestchat-security-{owner}");
        let (uid, gid) = self.identity;
        private(scan.destination)?;
        if self.superuser {
            let handed = std::os::unix::fs::chown(scan.destination, Some(uid), Some(gid));
            handed.map_err(|error| files::unusable(&error))?;
        }
        if scan.online
            && let Some(seed) = &self.cache
        {
            let account = self.superuser.then_some(self.identity);
            cache::restore(
                &seed.directory,
                scan.destination,
                account,
                &seed.pin,
                &seed.limits,
            )?;
        }
        let limit = if scan.online {
            self.limits.update
        } else {
            self.limits.output
        };
        let catalog = scan
            .arguments
            .starts_with(&["scan", "docker-archive:/input/image.tar"]);
        let scratch = if scan.tool == "syft" && catalog {
            self.limits.catalog
        } else {
            self.limits.scratch
        };
        let argv = self.command_line(scan, &name, &owner, scratch)?;
        let log = self.engine.log();
        let started = Instant::now();
        let (cancelled, destination) =
            (Arc::clone(&self.cancelled), scan.destination.to_path_buf());
        let entries = self.limits.entries;
        let mut healthy = move || {
            !cancelled.load(Ordering::SeqCst) && outputs::usage(&destination, limit, entries).within
        };
        let result = self
            .engine
            .command(&argv, self.limits.seconds, true, &mut healthy);
        let mut owned = None;
        let mut removal = self.removed(&name, &owner, &mut owned);
        let cleaned = removal.is_ok();
        if let (true, Some(record)) = (cleaned, &owned) {
            let path = self.evidence("container");
            removal = written(&path, record);
        }
        if !cleaned {
            self.cancelled.store(true, Ordering::SeqCst);
        }
        let scanned = Scanned {
            tool: scan.tool,
            status: result.as_ref().ok().map(|(status, _)| *status),
            owned: owned.as_ref(),
            cleaned,
            scratch,
            log: log.as_deref(),
        };
        let reported = self.reported(&scanned, started, scan.destination, limit);
        let (valid, within) = reported?;
        removal?;
        require(valid, "image_scanner_container_state")?;
        require(within, "image_scanner_output_limit")?;
        result
    }

    /// The engine's command line for `scan` in the container `name` owned
    /// by `owner`, with `scratch` bytes of scratch space.
    fn command_line(
        &self,
        scan: &Scan<'_>,
        name: &str,
        owner: &str,
        scratch: u64,
    ) -> Result<Vec<OsString>> {
        let (uid, gid) = self.identity;
        let network = if scan.online { "bridge" } else { "none" };
        let scratch = format!("/tmp:rw,noexec,nosuid,nodev,size={scratch},mode=1777");
        let label = format!("{LABEL}={owner}");
        let user = format!("{uid}:{gid}");
        let updates = format!("GRYPE_DB_AUTO_UPDATE={}", scan.online);
        let mut argv = arguments(&[
            "run",
            "--name",
            name,
            "--label",
            &label,
            "--platform",
            self.platform.name(),
            "--pull=never",
            "--network",
            network,
            "--read-only",
            "--user",
            &user,
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--pids-limit=128",
            "--cpus=2",
            "--memory=2g",
            "--memory-swap=2g",
            "--ulimit",
            "nofile=1024:1024",
            "--tmpfs",
            &scratch,
        ]);
        for variable in [
            "HOME=/tmp",
            "XDG_CONFIG_HOME=/tmp/config",
            "LC_ALL=C",
            "SYFT_CHECK_FOR_APP_UPDATE=false",
            "GRYPE_CHECK_FOR_APP_UPDATE=false",
            &updates,
            "GRYPE_DB_VALIDATE_BY_HASH_ON_START=true",
            "GRYPE_DB_VALIDATE_AGE=true",
            "GRYPE_DB_MAX_ALLOWED_BUILT_AGE=120h",
            "GRYPE_EXTERNAL_SOURCES_ENABLE=false",
            "SYFT_ENRICH_ALL=false",
        ] {
            argv.extend(arguments(&["--env", variable]));
        }
        argv.push("--mount".into());
        argv.push(joined(&[
            "type=bind,src=".as_ref(),
            self.tools.as_os_str(),
            ",dst=/tools,readonly".as_ref(),
        ]));
        argv.push("--mount".into());
        argv.push(joined(&[
            "type=bind,src=".as_ref(),
            scan.destination.as_os_str(),
            ",dst=/output".as_ref(),
        ]));
        let mut variables = scan.variables.to_vec();
        variables.sort_unstable();
        for (key, value) in variables {
            argv.extend(arguments(&["--env", &format!("{key}={value}")]));
        }
        let mut mounts = scan.mounts.to_vec();
        mounts.sort_unstable_by_key(|(target, _)| *target);
        for (target, source) in mounts {
            require(MOUNTS.contains(&target), "image_scanner_mount")?;
            argv.push("--mount".into());
            argv.push(joined(&[
                "type=bind,src=".as_ref(),
                source.as_os_str(),
                ",dst=".as_ref(),
                target.as_ref(),
                ",readonly".as_ref(),
            ]));
        }
        let entrypoint = format!("/tools/{}", scan.tool);
        argv.extend(arguments(&["--entrypoint", &entrypoint, &self.base]));
        argv.extend(arguments(scan.arguments));
        Ok(argv)
    }

    /// The path of this branch's evidence of the kind `kind` for the
    /// scanner it started last.
    fn evidence(&self, kind: &str) -> PathBuf {
        let name = format!("{kind}-{}{:02}.json", self.prefix, self.sequence);
        self.output.join(name)
    }

    /// Prove the container `name` the one `owner` started, remove it by its
    /// identity and prove it gone. What the engine said of it is kept in
    /// `owned` once its identity is proved.
    fn removed(&mut self, name: &str, owner: &str, owned: &mut Option<Document>) -> Result<()> {
        let record = self.inspected(name, owner)?;
        let identity = record
            .get("id")
            .and_then(Document::text)
            .unwrap_or_default()
            .to_owned();
        *owned = Some(record);
        let removal = arguments(&["container", "rm", "--force", &identity]);
        self.engine.command(&removal, QUICK, false, &mut || true)?;
        let filter = format!("id={identity}");
        let left = arguments(&["ps", "--all", "--quiet", "--no-trunc", "--filter", &filter]);
        let (_, remaining) = self.engine.command(&left, QUICK, false, &mut || true)?;
        require(remaining.is_empty(), "image_container_cleanup_failed")
    }

    /// What the engine says of the container `name`: only its identity,
    /// image, owner and state, never its environment. It must be the one
    /// `owner` started, of the scanner base, with an identity of 64 digits.
    fn inspected(&mut self, name: &str, owner: &str) -> Result<Document> {
        let field = if self.engine.name() == "podman" {
            ".ID"
        } else {
            ".Id"
        };
        let format = [
            r#"{"id":{{json "#,
            field,
            r#"}},"image":{{json .Image}},"owner":{{json (index .Config.Labels ""#,
            LABEL,
            r#"")}},"state":{{json .State.Status}},"exitStatus":{{json .State.ExitCode}},"#,
            r#""oomKilled":{{json .State.OOMKilled}}}"#,
        ]
        .concat();
        let asked = arguments(&["container", "inspect", "--format", &format, name]);
        let (status, said) = self.engine.command(&asked, QUICK, true, &mut || true)?;
        let unavailable = Fault("image_container_identity_unavailable");
        require(status == 0, unavailable.0)?;
        let record = Document::read(said.as_bytes()).map_err(|_| unavailable)?;
        require(record.fields().is_some(), unavailable.0)?;
        let text = |name: &str| record.get(name).and_then(Document::text);
        let unprefixed = |image: &str| image.strip_prefix("sha256:").unwrap_or(image).to_owned();
        let same = text("image").is_some_and(|image| unprefixed(image) == unprefixed(&self.base));
        let exact = text("id").is_some_and(|identity| small_hex(identity, 64));
        let proved = text("owner") == Some(owner) && same && exact;
        require(proved, "image_container_identity_changed")?;
        Ok(record)
    }

    /// Keep the diagnostic of `scanned`, which began at `started` and wrote
    /// to `destination` within `limit` bytes, and answer whether its
    /// container state was valid and its output within its budget.
    fn reported(
        &self,
        scanned: &Scanned<'_>,
        started: Instant,
        destination: &Path,
        limit: u64,
    ) -> Result<(bool, bool)> {
        let mut record = diagnostic::diagnostic(scanned, &self.limits)?;
        let valid = record.get("containerStateValid") == Some(&Document::Bool(true));
        record.put("elapsedSeconds", elapsed(started));
        let usage = outputs::usage(destination, limit, self.limits.entries);
        record.put("output", usage.record());
        if let Some(mut bounds) = record.get("limits").cloned() {
            bounds.put("outputBytes", limit.into());
            record.put("limits", bounds);
        }
        written(&self.evidence("scanner-result"), &record)?;
        Ok((valid, usage.within))
    }
}

/// What a sandbox is prepared from.
pub struct Setup<'a> {
    /// Where its evidence goes: an existing private directory.
    pub output: &'a Path,
    /// The caller's environment.
    pub ambient: &'a dyn Fn(&str) -> Option<OsString>,
    /// Where the verified executable of a scanner is, for a platform as
    /// the lock of the scanners names it.
    pub locate: &'a mut dyn FnMut(&'static str, &'static str) -> Result<PathBuf>,
    /// The platform scanned for.
    pub platform: Platform,
    /// The reviewed reference of the scanner base.
    pub base: &'a str,
    /// The cancellation the sandbox observes.
    pub cancelled: Arc<AtomicBool>,
    /// The database cache, when there is one.
    pub cache: Option<Cache>,
    /// The bounds of the scanners.
    pub limits: Limits,
}

/// Prepare a sandbox for `setup`: an empty configuration of the engine,
/// with no registry credentials, a fixed environment of the engine with no
/// more of the caller's than its search path, home and runtime directory,
/// a log of the engine's commands, the bundle of the verified scanners and
/// the scanner base pulled and held to its identity. `engine` makes what
/// runs the engine from its environment and the directory of its log.
pub fn prepare(
    setup: Setup<'_>,
    engine: &mut dyn FnMut(Vec<(&'static str, OsString)>, PathBuf) -> Box<dyn Engine>,
) -> Result<Sandbox> {
    let configuration = setup.output.join("engine-config");
    private(&configuration)?;
    let credentials = configuration.join("config.json");
    archive::new_file(&credentials, b"{}\n")?;
    let mut environment: Vec<(&'static str, OsString)> = ["PATH", "HOME", "XDG_RUNTIME_DIR"]
        .into_iter()
        .filter_map(|name| (setup.ambient)(name).map(|value| (name, value)))
        .collect();
    environment.extend([
        ("LC_ALL", "C".into()),
        ("LANG", "C".into()),
        ("DOCKER_CONFIG", configuration.into()),
        ("REGISTRY_AUTH_FILE", credentials.into()),
    ]);
    let logs = setup.output.join("scanner-commands");
    private(&logs)?;
    let engine = engine(environment, logs);
    let tools = bundle(setup.locate, setup.output, setup.platform)?;
    let mut sandbox = Sandbox::new(
        engine,
        setup.output,
        &tools,
        setup.base,
        setup.platform,
        setup.limits,
    );
    sandbox.cancelled = setup.cancelled;
    sandbox.cache = setup.cache;
    let cancelled = Arc::clone(&sandbox.cancelled);
    let mut healthy = move || !cancelled.load(Ordering::SeqCst);
    let pull = arguments(&["pull", "--platform", setup.platform.name(), setup.base]);
    sandbox.engine.command(&pull, PULL, false, &mut healthy)?;
    let asked = arguments(&["image", "inspect", "--format", "{{.Id}}", setup.base]);
    let (_, identity) = sandbox.engine.command(&asked, QUICK, false, &mut healthy)?;
    let identity = identity.strip_prefix("sha256:").unwrap_or(&identity);
    require(small_hex(identity, 64), "image_scanner_base_identity")?;
    sandbox.base = format!("sha256:{identity}");
    Ok(sandbox)
}

/// Copy the regular file `source` to the new file `destination`, which only
/// its owner may write until it is opened to be run.
fn copied(source: &Path, destination: &Path) -> Result<()> {
    use rustix::fs::{Mode, OFlags};
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let opened = rustix::fs::open(source, flags, Mode::empty());
    let mut reader = File::from(opened.map_err(|error| files::unusable(&error.into()))?);
    let regular = reader
        .metadata()
        .map_err(|error| files::unusable(&error))?
        .is_file();
    require(regular, "nonregular_tool_file")?;
    let mut options = std::fs::OpenOptions::new();
    std::os::unix::fs::OpenOptionsExt::mode(options.write(true).create_new(true), 0o600);
    let mut writer = options
        .open(destination)
        .map_err(|error| files::unusable(&error))?;
    std::io::copy(&mut reader, &mut writer).map_err(|error| files::unusable(&error))?;
    Ok(())
}

/// The read-only bundle of the scanners in the new directory
/// `scanner-tools` below `output`: only the bytes of each verified
/// executable, which `locate` finds for the lock's name of `platform`, and
/// the fixed configuration the secret scanner reads.
pub fn bundle(
    locate: &mut dyn FnMut(&'static str, &'static str) -> Result<PathBuf>,
    output: &Path,
    platform: Platform,
) -> Result<PathBuf> {
    let bundle = output.join("scanner-tools");
    private(&bundle)?;
    for name in TOOLS {
        let source = locate(name, platform.tools())?;
        let destination = bundle.join(name);
        copied(&source, &destination)?;
        opened(&destination, 0o555)?;
    }
    let configuration = bundle.join("gitleaks.toml");
    archive::new_file(&configuration, b"[extend]\nuseDefault = true\n")?;
    let ignored = bundle.join("empty.ignore");
    archive::new_file(&ignored, b"")?;
    opened(&configuration, 0o444)?;
    opened(&ignored, 0o444)?;
    opened(&bundle, 0o555)?;
    Ok(bundle)
}

#[cfg(test)]
#[path = "sandbox_tests.rs"]
pub(crate) mod tests;
