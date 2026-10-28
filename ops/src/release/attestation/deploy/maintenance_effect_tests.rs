//! Whole maintenance state transitions over real files and flock ownership.
//! External daemon/SQL/launcher completions are finite synthetic observations;
//! these tests establish no Docker, signature or native pipe execution proof.

use super::*;
use crate::release::{attestation::maintain, workload::Lease};
use sha2::{Digest, Sha256};
use std::{
    cell::Cell,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{DirBuilderExt, FileExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    rc::Rc,
};

const REVISION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OLD_REVISION: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const DUMP: &[u8] = b"PGDMP-synthetic-maintenance-listed-backup";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    None,
    Launcher,
    SecondLock,
    SelectedChanged,
    DatabaseReplaced,
    FinalLedger,
    Settings,
    Identity,
    Dump,
    List,
    Receipt,
}

fn id(byte: char) -> String {
    byte.to_string().repeat(64)
}
fn old_image() -> String {
    format!("sha256:{}", id('b'))
}
fn digest(bytes: &[u8]) -> String {
    super::super::super::files::hex(&Sha256::digest(bytes))
}
fn write(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(mode)
        .open(path)?;
    let result = file
        .write_all(bytes)
        .and_then(|()| file.flush())
        .and_then(|()| file.set_permissions(fs::Permissions::from_mode(mode)));
    let closed = nix::unistd::close(file).map_err(io::Error::from);
    result.and(closed)
}
fn lock_available(path: &Path) -> io::Result<bool> {
    let file = OpenOptions::new().read(true).write(true).open(path)?;
    let acquired = rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive);
    let result = match acquired {
        Ok(()) => rustix::fs::flock(&file, rustix::fs::FlockOperation::Unlock)
            .map(|()| true)
            .map_err(io::Error::from),
        Err(rustix::io::Errno::WOULDBLOCK) => Ok(false),
        Err(error) => Err(io::Error::from(error)),
    };
    let closed = nix::unistd::close(file).map_err(io::Error::from);
    result.and_then(|value| closed.map(|()| value))
}
fn release(lease: Lease) -> bool {
    let (unlocked, file) = lease.release();
    let closed = file.is_some_and(|file| nix::unistd::close(file).is_ok());
    unlocked.is_ok() && closed
}
fn migration_rows(values: &BTreeMap<String, String>, packaged: bool) -> Vec<u8> {
    values
        .iter()
        .map(|(version, checksum)| {
            if packaged {
                format!("{checksum}  /app/migrations/{version}_fixture.sql\n")
            } else {
                format!("{version} t {checksum}\n")
            }
        })
        .collect::<String>()
        .into_bytes()
}
fn asset_tar() -> Vec<u8> {
    let bytes = b"synthetic-browser-asset";
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_mode(0o644);
    header.set_size(bytes.len() as u64);
    header.set_entry_type(tar::EntryType::Regular);
    header.set_cksum();
    builder
        .append_data(&mut header, "assets/app-abcdef.js", &bytes[..])
        .unwrap();
    builder.into_inner().unwrap()
}

struct Prepared {
    data: effect_tests::Data,
    new_image: String,
    migrations: BTreeMap<String, String>,
    candidate: Option<PathBuf>,
}
impl Prepared {
    fn new(fault: Fault) -> Self {
        let mut data = effect_tests::Data::new();
        let release = data.access.root.join("releases").join(REVISION);
        let mut raw: Value =
            serde_json::from_slice(&fs::read(release.join("release.json")).unwrap()).unwrap();
        let migrations = BTreeMap::from([
            ("1".into(), "1".repeat(96)),
            ("3".into(), "3".repeat(96)),
            ("7".into(), "7".repeat(96)),
        ]);
        raw["migrations"] = json!(migrations);
        write(
            &release.join("release.json"),
            &serde_json::to_vec(&raw).unwrap(),
            0o600,
        )
        .unwrap();
        // The inert archive graph still binds this exact config digest. Only
        // synthetic packaged SQL observations below exercise the migration map.
        let file = File::open(release.join("image.tar")).unwrap();
        let mut archive = tar::Archive::new(file);
        let mut config = Vec::new();
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            if entry.path().unwrap() == Path::new("config.json") {
                entry.read_to_end(&mut config).unwrap();
            }
        }
        nix::unistd::close(archive.into_inner()).unwrap();
        let new_image = format!("sha256:{}", digest(&config));
        let environment = format!(
            "SIMPLESTCHAT_IMAGE={}\nRUN_MIGRATIONS=false\nWEBAUTHN_ORIGIN=https://fixture.test\n",
            old_image()
        );
        write(
            &data.access.config.join("app.env"),
            environment.as_bytes(),
            0o600,
        )
        .unwrap();
        let compose = format!(
            concat!(
                "services:\n  simplestchat:\n    image: \"{}\"\n",
                "    env_file:\n      - ./app.env\n  migrate:\n    image: \"{}\"\n",
                "  postgres: {{}}\n  caddy: {{}}\n"
            ),
            old_image(),
            old_image()
        );
        write(
            &data.access.config.join("compose.public.yml"),
            compose.as_bytes(),
            0o600,
        )
        .unwrap();
        write(
            &data.access.config.join("images.json"),
            &serde_json::to_vec(&json!({
            "revision":OLD_REVISION,"serverImage":old_image()}))
            .unwrap(),
            0o600,
        )
        .unwrap();
        let caddy = b"synthetic protected proxy\n";
        write(&data.access.config.join("Caddyfile"), caddy, 0o644).unwrap();
        write(
            &data.access.config.join("browser-assets-installed.json"),
            &serde_json::to_vec(&json!({"schemaVersion":1,"caddyfileSha256":digest(caddy)}))
                .unwrap(),
            0o600,
        )
        .unwrap();
        let source = data
            .access
            .root
            .parent()
            .unwrap()
            .join("sources")
            .join(REVISION)
            .join(".git");
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(source)
            .unwrap();
        let candidate = if matches!(fault, Fault::Identity | Fault::Settings) {
            let path = data.access.config.join("app.env.candidate");
            let change = if fault == Fault::Identity {
                "JWT_SECRET=changed\n"
            } else {
                "MAX_CONNECTIONS=200\n"
            };
            write(&path, format!("{environment}{change}").as_bytes(), 0o600).unwrap();
            Some(path)
        } else {
            None
        };
        if fault == Fault::Receipt {
            data.access.fault = Some(super::super::files::Fault::BackupReceiptWrite);
        }
        Self {
            data,
            new_image,
            migrations,
            candidate,
        }
    }
}

struct Daemon {
    root: PathBuf,
    config: PathBuf,
    work: PathBuf,
    docker: PathBuf,
    curl: PathBuf,
    new_image: String,
    migrations: BTreeMap<String, String>,
    launched: Rc<Cell<bool>>,
    fault: Fault,
    commands: Vec<(Role, Vec<String>)>,
    exact: bool,
    dump_inode: Option<(u64, u64)>,
    listed_original: bool,
    backup_before_stop: bool,
    validator_live: bool,
    asset_live: bool,
}
impl Daemon {
    fn rendered(&self, arguments: &[String]) -> io::Result<Vec<u8>> {
        let at = arguments
            .iter()
            .position(|word| word == "-f")
            .ok_or_else(|| io::Error::other("missing fixed Compose file"))?;
        let file = Path::new(
            arguments
                .get(at + 1)
                .ok_or_else(|| io::Error::other("missing file"))?,
        );
        let name = file
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("");
        let image = if name == "candidate-compose.yml" || self.launched.get() {
            self.new_image.clone()
        } else {
            old_image()
        };
        let environment_path = if name == "reviewed-compose.yml" {
            file.parent().unwrap().join("reviewed.env")
        } else if name == "candidate-compose.yml" {
            let text = fs::read_to_string(file)?;
            if text.contains("candidate.env\"") {
                file.parent().unwrap().join("candidate.env")
            } else {
                self.config.join("app.env")
            }
        } else {
            self.config.join("app.env")
        };
        let text = fs::read_to_string(environment_path)?;
        let mut environment = BTreeMap::<String, String>::new();
        for line in text
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
        {
            let (name, value) = line
                .split_once('=')
                .ok_or_else(|| io::Error::other("fixture env"))?;
            environment.insert(name.into(), value.into());
        }
        let service = |migrate: bool| {
            let mut environment = if migrate {
                BTreeMap::new()
            } else {
                environment.clone()
            };
            environment.insert(
                "RUN_MIGRATIONS".into(),
                if migrate { "true" } else { "false" }.into(),
            );
            json!({"image":image,"user":"10001:10001","working_dir":"/app",
                "read_only":true,"init":true,"cap_drop":["ALL"],
                "security_opt":["no-new-privileges:true"],"environment":environment,
                "volumes":[{"type":"bind","source":self.root.join("postgres-socket"),
                    "target":"/run/simplestchat-postgres","read_only":true,
                    "bind":{"create_host_path":true}}],
                "tmpfs":[if migrate {"/tmp:rw,nosuid,nodev,noexec,size=32m,mode=1777"}
                    else {"/tmp:rw,nosuid,nodev,noexec,size=64m,mode=1777"}]})
        };
        serde_json::to_vec(
            &json!({"services":{"simplestchat":service(false),"migrate":service(true),
            "postgres":{"image":"fixture-postgres"},"caddy":{"volumes":[{
                "type":"bind","source":self.root.join("browser-assets"),
                "target":"/run/simplestchat-browser-assets","read_only":true,
                "bind":{"create_host_path":true}}]}}}),
        )
        .map_err(io::Error::other)
    }
    fn container(&self, value: &str) -> Value {
        let (service, image) = if value == id('1') {
            (
                "simplestchat",
                if self.launched.get() {
                    self.new_image.clone()
                } else {
                    old_image()
                },
            )
        } else if value == id('2') || value == id('4') {
            ("postgres", "sha256:fixture-postgres".into())
        } else {
            ("caddy", "sha256:fixture-proxy".into())
        };
        json!({"id":value,"image":image,"configHash":format!("hash-{service}"),
            "state":{"Running":true,"OOMKilled":false,"StartedAt":"fixture-start",
                "Health":{"Status":"healthy"}},"restarts":0})
    }
}
impl Executor for Daemon {
    async fn run(&mut self, owner: &mut Owner, request: Invocation<'_>) -> CommandOutput {
        let mut arguments = request
            .arguments
            .iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        self.commands.push((request.role, arguments.clone()));
        self.exact &= request.body_end > owner.now()
            && request.body_end < request.settlement_end
            && request.settlement_end <= owner.body_end()
            && lock_available(&self.work.join("workload.lock")).is_ok_and(|available| !available);
        let mut stdout = Vec::new();
        let mut error = None;
        let mut status = 0;
        let mut bulk = None;
        let curl = request.role == Role::Ready;
        self.exact &= request.program
            == if curl {
                self.curl.clone()
            } else {
                self.docker.clone()
            };
        if !curl {
            if arguments.first().map(String::as_str) != Some("--host")
                || arguments.get(1).map(String::as_str) != Some("unix:///var/run/docker.sock")
            {
                error = Some(Cause::Input);
            } else {
                drop(arguments.drain(..2));
            }
        }
        let has = |word: &str| arguments.iter().any(|value| value == word);
        let last = arguments.last().map(String::as_str).unwrap_or("");
        match request.role {
            Role::Load
                if arguments.first().is_some_and(|value| value == "image") && has("load") => {}
            Role::Inspect
                if arguments.first().is_some_and(|value| value == "image") && has("inspect") =>
            {
                let old = last == old_image();
                let image = if old {
                    old_image()
                } else {
                    self.new_image.clone()
                };
                let revision = if old { OLD_REVISION } else { REVISION };
                stdout = serde_json::to_vec(&json!({"id":image,
                    "os":"linux","architecture":"amd64","user":"10001:10001",
                    "labels":{"org.opencontainers.image.revision":revision},
                    "cmd":["/app/simplestChat"],"entrypoint":null,"workingDir":"/app",
                    "hasHealthcheck":false,"envNames":["PATH"]}))
                .unwrap();
            }
            Role::Inspect
                if arguments.first().is_some_and(|value| value == "inspect")
                    && [id('1'), id('2'), id('3'), id('4')]
                        .iter()
                        .any(|id| id == last) =>
            {
                stdout = serde_json::to_vec(&self.container(last)).unwrap();
            }
            Role::Create
                if arguments.first().is_some_and(|value| value == "create")
                    && has("none")
                    && has("--read-only")
                    && last.contains("sha384sum") =>
            {
                self.exact &= !self.validator_live;
                self.validator_live = true;
                stdout = format!("{}\n", id('5')).into_bytes();
            }
            Role::Start if arguments == ["start".to_owned(), id('5')] => {}
            Role::Wait if arguments == ["wait".to_owned(), id('5')] => stdout = b"0\n".to_vec(),
            Role::Logs if arguments == ["logs".to_owned(), id('5')] => {
                stdout = migration_rows(&self.migrations, true);
            }
            Role::Remove
                if arguments.first().is_some_and(|value| value == "rm") && last == id('5') =>
            {
                self.exact &= self.validator_live;
                self.validator_live = false;
            }
            Role::Compose if has("ps") && last == "migrate" => {}
            Role::Compose if has("ps") && ["simplestchat", "postgres", "caddy"].contains(&last) => {
                let value = match last {
                    "simplestchat" => id('1'),
                    "caddy" => id('3'),
                    _ if self.launched.get() && self.fault == Fault::DatabaseReplaced => id('4'),
                    _ => id('2'),
                };
                stdout = format!("{value}\n").into_bytes();
            }
            Role::Compose if has("config") && has("--hash") => {
                stdout = format!("{last} hash-{last}\n").into_bytes();
            }
            Role::Compose if has("config") && has("--format") && last == "json" => {
                match self.rendered(&arguments) {
                    Ok(bytes) => stdout = bytes,
                    Err(_) => error = Some(Cause::Input),
                }
            }
            Role::Query if last == LEDGER => {
                let mut values = self.migrations.clone();
                if !self.launched.get() || self.fault == Fault::FinalLedger {
                    values.remove("3");
                }
                stdout = migration_rows(&values, false);
            }
            Role::AssetCreate
                if arguments.first().is_some_and(|value| value == "create")
                    && has("none")
                    && has("--read-only")
                    && last == "exit 0" =>
            {
                self.exact &= !self.asset_live;
                self.asset_live = true;
                stdout = format!("{}\n", id('6')).into_bytes();
            }
            Role::AssetCopy
                if arguments
                    == [
                        "cp".to_owned(),
                        format!("{}:/app/web/dist/assets", id('6')),
                        "-".into(),
                    ] =>
            {
                self.exact &= self.asset_live;
                bulk = Some(asset_tar());
            }
            Role::AssetRemove if arguments == ["rm".to_owned(), id('6')] => {
                self.exact &= self.asset_live;
                self.asset_live = false;
            }
            Role::Size if last == "SELECT pg_database_size(current_database())" => {
                stdout = b"1\n".to_vec()
            }
            Role::Dump if last == "custom" && has("pg_dump") => {
                if self.fault == Fault::Dump {
                    status = 7;
                } else {
                    bulk = Some(DUMP.to_vec());
                }
            }
            Role::List if last == "--list" && has("pg_restore") => {
                let mut bytes = vec![0; DUMP.len()];
                self.listed_original = request.input.is_some_and(|file| {
                    file.read_exact_at(&mut bytes, 0).is_ok()
                        && bytes == DUMP
                        && file.metadata().ok().map(|m| (m.dev(), m.ino())) == self.dump_inode
                });
                self.exact &= self.listed_original;
                if self.fault == Fault::List {
                    status = 7;
                }
                stdout = b"synthetic completed archive listing\n".to_vec();
            }
            Role::Ready if last.ends_with("/ready") && has("--disable") && has("--noproxy") => {
                stdout = b"{\"status\":\"ready\"}\n".to_vec();
            }
            Role::Stop if has("stop") && last == "simplestchat" && has("30") => {
                let journal = fs::read(self.root.join("release-state.json"))
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
                self.backup_before_stop = journal.as_ref().is_some_and(|journal| {
                    journal["attempt"].as_str().is_some_and(|attempt| {
                        let attempt = Path::new(attempt);
                        fs::read(attempt.join("database-before.dump"))
                            .is_ok_and(|bytes| bytes == DUMP)
                            && attempt.join("database-before.receipt.json").exists()
                            && journal["phase"] == "maintenance_stop"
                            && journal["finalized"] == false
                    })
                });
                self.exact &= self.backup_before_stop;
            }
            Role::ProxyStop if has("stop") && last == "caddy" && has("3") => {
                self.exact &= self.backup_before_stop;
            }
            _ => error = Some(Cause::Input),
        }
        let mut bulk_bytes = 0;
        let mut bulk_sha256 = digest(b"");
        if let Some(bytes) = bulk {
            if let Some(output) = request.output {
                if bytes.len() as u64 <= output.maximum
                    && output.file.write_all_at(&bytes, 0).is_ok()
                {
                    bulk_bytes = bytes.len() as u64;
                    bulk_sha256 = digest(&bytes);
                    if request.role == Role::Dump {
                        self.dump_inode = output.file.metadata().ok().map(|m| (m.dev(), m.ino()));
                    }
                } else {
                    error = Some(Cause::Io);
                }
            } else {
                error = Some(Cause::Input);
            }
        }
        if let Some(cause) = error {
            owner.record(cause);
            self.exact = false;
        }
        CommandOutput {
            stdout,
            stderr: Vec::new(),
            status: Some(status),
            error,
            settled: true,
            stdout_complete: true,
            bulk_bytes,
            bulk_sha256,
            bulk_complete: error.is_none(),
        }
    }
}

struct BorrowedLauncher {
    root: PathBuf,
    config: PathBuf,
    work: PathBuf,
    new_image: String,
    launched: Rc<Cell<bool>>,
    fault: Fault,
    calls: usize,
    admitted: bool,
    released: bool,
    contender: Option<Lease>,
}
impl maintain::Launcher for BorrowedLauncher {
    fn run(
        &mut self,
        owner: &mut Owner,
        ceiling: Instant,
        observed: &mut Option<Value>,
    ) -> Result<()> {
        self.calls += 1;
        owner.check(Window::Body)?;
        let journal: Value = serde_json::from_slice(
            &fs::read(self.root.join("release-state.json")).map_err(|_| Cause::Io)?,
        )
        .map_err(|_| Cause::Policy)?;
        let selected: Value = serde_json::from_slice(
            &fs::read(self.config.join("images.json")).map_err(|_| Cause::Io)?,
        )
        .map_err(|_| Cause::Policy)?;
        let attempt = Path::new(journal["attempt"].as_str().ok_or(Cause::Policy)?);
        let dump = fs::read(attempt.join("database-before.dump")).map_err(|_| Cause::Io)?;
        let receipt: Value = serde_json::from_slice(
            &fs::read(attempt.join("database-before.receipt.json")).map_err(|_| Cause::Io)?,
        )
        .map_err(|_| Cause::Policy)?;
        let environment = fs::read_to_string(self.config.join("app.env")).map_err(|_| Cause::Io)?;
        let compose =
            fs::read_to_string(self.config.join("compose.public.yml")).map_err(|_| Cause::Io)?;
        let version: Value = serde_json::from_slice(
            &fs::read(self.root.join("browser-assets/public/app-version.json"))
                .map_err(|_| Cause::Io)?,
        )
        .map_err(|_| Cause::Policy)?;
        self.admitted = ceiling <= owner.body_end()
            && ceiling > owner.now()
            && journal["phase"] == "maintenance_selected"
            && journal["finalized"] == true
            && selected["serverImage"] == self.new_image
            && selected["revision"] == REVISION
            && dump == DUMP
            && receipt["sha256"] == digest(DUMP)
            && receipt["bytes"] == DUMP.len() as u64
            && environment
                .lines()
                .any(|line| line == format!("SIMPLESTCHAT_IMAGE={}", self.new_image))
            && compose
                .matches(&format!("image: \"{}\"", self.new_image))
                .count()
                == 2
            && version["revision"] == REVISION;
        let acquire = || {
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(self.work.join("workload.lock"))
                .ok()
                .and_then(|file| Lease::acquire(file).ok())
        };
        let lease = acquire().ok_or(Cause::Io)?;
        self.admitted &= lock_available(&self.work.join("workload.lock")).is_ok_and(|free| !free);
        self.released = release(lease);
        if !self.admitted || !self.released {
            return control::failed(owner, Cause::Changed);
        }
        self.launched.set(true);
        *observed = Some(json!({"syntheticLauncher":true,"actualFreshFlock":true}));
        if self.fault == Fault::Launcher {
            return control::failed(owner, Cause::Command);
        }
        if self.fault == Fault::SecondLock {
            self.contender = acquire();
        }
        if self.fault == Fault::SelectedChanged {
            write(
                &self.config.join("app.env"),
                b"unreviewed selection\n",
                0o600,
            )
            .map_err(|_| Cause::Io)?;
        }
        Ok(())
    }
}

struct Observed {
    report: Value,
    passed: bool,
    finished: Result<()>,
    daemon_exact: bool,
    commands: Vec<(Role, Vec<String>)>,
    launcher_calls: usize,
    launcher_admitted: bool,
    launcher_released: bool,
    contender_released: bool,
    lock_available: bool,
    journal: Value,
    selected: Value,
    shared_closes: (usize, usize),
    record_closes: (usize, usize),
    candidate_consumed: bool,
}
fn operation(fault: Fault) -> Observed {
    let fixture = Prepared::new(fault);
    let access = &fixture.data.access;
    let launched = Rc::new(Cell::new(false));
    let mut daemon = Daemon {
        root: access.root.clone(),
        config: access.config.clone(),
        work: access.work.clone(),
        docker: access.docker.clone(),
        curl: access.curl.clone(),
        new_image: fixture.new_image.clone(),
        migrations: fixture.migrations.clone(),
        launched: launched.clone(),
        fault,
        commands: Vec::new(),
        exact: true,
        dump_inode: None,
        listed_original: false,
        backup_before_stop: false,
        validator_live: false,
        asset_live: false,
    };
    let mut launcher = BorrowedLauncher {
        root: access.root.clone(),
        config: access.config.clone(),
        work: access.work.clone(),
        new_image: fixture.new_image,
        launched,
        fault,
        calls: 0,
        admitted: false,
        released: false,
        contender: None,
    };
    let mut owner = Owner::new(
        Instant::now(),
        Duration::from_secs(1800),
        Duration::from_secs(60),
    )
    .unwrap();
    let options = maintain::Options {
        revision: REVISION.into(),
        whole: Duration::from_secs(1800),
        reserve: Duration::from_secs(60),
        candidate_env: fixture.candidate.clone(),
        browser_assets_upgrade: false,
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    super::super::super::files::start_probe(None);
    super::super::files::start_probe();
    let outcome = runtime.block_on(maintenance::execute(
        &options,
        access,
        &mut owner,
        &mut daemon,
        &mut launcher,
    ));
    drop(runtime);
    let contender_released = launcher.contender.take().is_none_or(release);
    let report = serde_json::from_slice(&outcome.bytes).unwrap();
    let journal =
        serde_json::from_slice(&fs::read(access.root.join("release-state.json")).unwrap()).unwrap();
    let selected =
        serde_json::from_slice(&fs::read(access.config.join("images.json")).unwrap()).unwrap();
    let lock_available = lock_available(&access.work.join("workload.lock")).unwrap();
    let candidate_consumed = fixture.candidate.as_ref().is_none_or(|path| !path.exists());
    let shared_closes = super::super::super::files::take_probe();
    let record_closes = super::super::files::take_probe();
    let finished = owner.finish();
    Observed {
        report,
        passed: outcome.passed,
        finished,
        daemon_exact: daemon.exact,
        commands: daemon.commands,
        launcher_calls: launcher.calls,
        launcher_admitted: launcher.admitted,
        launcher_released: launcher.released,
        contender_released,
        lock_available,
        journal,
        selected,
        shared_closes,
        record_closes,
        candidate_consumed,
    }
}
fn closed(observed: &Observed) {
    assert!(
        observed.daemon_exact,
        "finite provider rejected a command: {:?}",
        observed.commands
    );
    assert!(observed.lock_available && observed.contender_released);
    assert!(observed.shared_closes.0 > 0 && observed.record_closes.0 > 0);
    assert_eq!((observed.shared_closes.1, observed.record_closes.1), (0, 0));
}

#[test]
fn maintain_completes_real_selection_and_lock_handoffs_with_a_hole_in_the_applied_subset() {
    let observed = operation(Fault::None);
    closed(&observed);
    assert!(observed.passed, "{}", observed.report);
    assert_eq!(observed.finished, Ok(()));
    assert_eq!(observed.report["phase"], "complete");
    assert_eq!(
        observed.report["backupMigrations"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["1", "7"]
    );
    assert!(observed.report.get("rollbackAttempted").is_none());
    assert_eq!(observed.launcher_calls, 1);
    assert!(observed.launcher_admitted && observed.launcher_released);
    assert_eq!(observed.journal["phase"], "complete");
    assert_eq!(observed.journal["finalized"], true);
    assert_eq!(observed.selected["revision"], REVISION);
    let stops = observed
        .commands
        .iter()
        .filter(|(role, _)| matches!(role, Role::Stop | Role::ProxyStop))
        .map(|(_, arguments)| arguments.last().unwrap().as_str())
        .collect::<Vec<_>>();
    assert_eq!(stops, ["simplestchat", "caddy"]);
}

#[test]
fn launcher_failure_keeps_the_durable_selection_and_never_rolls_back() {
    let observed = operation(Fault::Launcher);
    closed(&observed);
    assert!(!observed.passed);
    assert_eq!(observed.finished, Err(Cause::Command));
    assert_eq!(observed.launcher_calls, 1);
    assert!(observed.launcher_admitted && observed.launcher_released);
    assert_eq!(observed.report["launcherPassed"], false);
    assert!(observed.report.get("rollbackAttempted").is_none());
    assert_eq!(observed.selected["revision"], REVISION);
    assert_eq!(observed.journal["phase"], "maintenance_selected");
    assert_eq!(observed.journal["finalized"], true);
    assert!(!observed.commands.iter().any(|(role, _)| *role == Role::Up));
}

#[test]
fn reviewed_settings_are_selected_and_the_original_candidate_is_consumed_before_launcher() {
    let observed = operation(Fault::Settings);
    closed(&observed);
    assert!(observed.passed, "{}", observed.report);
    assert_eq!(observed.finished, Ok(()));
    assert_eq!(
        observed.report["environmentChanges"],
        json!(["MAX_CONNECTIONS"])
    );
    assert!(observed.candidate_consumed);
    assert!(observed.launcher_admitted && observed.launcher_released);
    assert_eq!(observed.journal["phase"], "complete");
}

#[test]
fn second_lock_and_selected_input_tampering_refuse_after_the_launcher_without_rollback() {
    for fault in [Fault::SecondLock, Fault::SelectedChanged] {
        let observed = operation(fault);
        closed(&observed);
        assert!(!observed.passed, "{fault:?}");
        assert!(observed.finished.is_err());
        assert_eq!(observed.launcher_calls, 1);
        assert!(observed.launcher_admitted && observed.launcher_released);
        assert_eq!(observed.report["launcherPassed"], true);
        assert_eq!(observed.selected["revision"], REVISION);
        assert!(observed.report.get("rollbackAttempted").is_none());
        assert_eq!(observed.journal["phase"], "maintenance_selected");
    }
}

#[test]
fn final_database_identity_and_exact_ledger_are_required_after_successful_launcher() {
    for fault in [Fault::DatabaseReplaced, Fault::FinalLedger] {
        let observed = operation(fault);
        closed(&observed);
        assert!(!observed.passed, "{fault:?}");
        assert_eq!(observed.finished, Err(Cause::Policy));
        assert_eq!(observed.launcher_calls, 1);
        assert_eq!(observed.report["phase"], "verify");
        assert_eq!(observed.journal["phase"], "maintenance_selected");
        assert_eq!(observed.selected["revision"], REVISION);
    }
}

#[test]
fn identity_and_backup_refusals_never_stop_the_application_or_invoke_the_launcher() {
    for fault in [Fault::Identity, Fault::Dump, Fault::List, Fault::Receipt] {
        let observed = operation(fault);
        closed(&observed);
        assert!(!observed.passed, "{fault:?}");
        assert!(observed.finished.is_err());
        assert_eq!(observed.launcher_calls, 0);
        assert!(
            !observed
                .commands
                .iter()
                .any(|(role, _)| matches!(role, Role::Stop | Role::ProxyStop | Role::Up))
        );
        assert_eq!(observed.selected["revision"], OLD_REVISION);
        if fault == Fault::Identity {
            assert!(
                !observed
                    .commands
                    .iter()
                    .any(|(role, _)| *role == Role::Dump)
            );
        }
    }
}
