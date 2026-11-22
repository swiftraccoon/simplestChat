//! `simplestchat-ops release infrastructure-guest ACTION --run-id RUN`: one
//! check of the marked disposable guest after the playbook that prepares it.
//! Root, Linux x86_64, Debian 13, the QEMU vendor, the run's private marker
//! and selection, the installed executable and the Docker socket must agree
//! before anything else is read. Every command leaves private numbered
//! evidence in a fresh attempt directory of the fixture root; only the
//! receipt, aggregate fields or four fixed labels, reaches standard output.
//! Blocking: each command has its own deadline and every poll a phase
//! deadline; a termination signal stops the running command and refuses.

use super::model::{self, Fault, within};
use super::policy::{self, Owner, Selection};

use super::{Action, Diagnostic, receipt};
use crate::backup::receipts;
use crate::backup::{RestoreLimits, verify_restore};
use crate::host::command::{Commands, Interruptible};
use crate::host::evidence::{Evidence, Streams};
use crate::host::failure::{Failure, Result, require};
use crate::host::{Host, Paths, System, hexadecimal, image_id, state};
use crate::json::Json;
use crate::release::assets::{self, Workload};
use crate::release::manifest::Manifest;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

type Checked<T> = std::result::Result<T, Fault>;

const USAGE: &str = "Usage: simplestchat-ops release infrastructure-guest \
snapshot|database|backup-restore --run-id RUN\n";
const IMAGE_FORMAT: &str = concat!(
    "{\"id\":{{json .Id}},\"os\":{{json .Os}},\"architecture\":{{json .Architecture}},",
    "\"user\":{{json .Config.User}},\"labels\":{{json .Config.Labels}},",
    "\"cmd\":{{json .Config.Cmd}},\"entrypoint\":{{json (index .Config \"Entrypoint\")}},",
    "\"workingDir\":{{json .Config.WorkingDir}},",
    "\"hasHealthcheck\":{{if index .Config \"Healthcheck\"}}true{{else}}false{{end}},",
    "\"envNames\":[{{range $i, $v := .Config.Env}}{{if $i}},{{end}}",
    "{{json (index (split $v \"=\") 0)}}{{end}}]}",
);
pub(super) const INSPECTION: &str = concat!(
    "{\"id\":{{json .Id}},\"name\":{{json .Name}},\"image\":{{json .Image}},",
    "\"user\":{{json .Config.User}},\"labels\":{{json .Config.Labels}},",
    "\"host\":{{json .HostConfig}},\"state\":{{json .State}}}",
);
const LEDGER: &str = "SELECT version, success, encode(checksum, 'hex') \
FROM public._sqlx_migrations ORDER BY version";
/// The synthetic rows the database action adds: one account and room, one
/// active and one resolved operational incident.
pub(super) const FIXTURE_SQL: &[u8] = b"BEGIN;
SET LOCAL statement_timeout='5s';
INSERT INTO public.users(id,email,display_name)
VALUES ('00000000-0000-0000-0000-000000000001','fixture@example.test','Fixture');
INSERT INTO public.rooms(id,owner_id,display_name)
VALUES ('fixture','00000000-0000-0000-0000-000000000001','Fixture');
INSERT INTO operations.alerts(incident_key,rule,severity,resource,first_seen,last_seen,resolved_at)
VALUES (repeat('a',64),'DatabaseUnavailable','critical','{}',now(),now(),NULL),
       (repeat('b',64),'CollectorStale','warning','{}',now(),now(),now());
INSERT INTO operations.alert_cursor VALUES (true,now());
COMMIT;
";
const BACKUP_SERVICE: &str = "simplestchat-backup.service";
const BACKUP_TIMER: &str = "simplestchat-backup.timer";
const RESTORE_LABEL: &str = "clinic.research.simplestchat.restore";
const MAX_BACKUP: u64 = 64 * 1024 * 1024;

/// The fixed locations the guest reads; tests move them into a scratch tree.
pub(super) struct Layout {
    pub fixture: PathBuf,
    pub marker: PathBuf,
    pub library: PathBuf,
    pub executable: PathBuf,
    pub socket: PathBuf,
    pub vendor: PathBuf,
    pub os_release: [PathBuf; 2],
    pub config: PathBuf,
    pub root: PathBuf,
    pub units: PathBuf,
    pub launcher: PathBuf,
}

impl Layout {
    fn fixed() -> Self {
        Self {
            fixture: "/root/simplestchat-vm-fixture".into(),
            marker: "/etc/simplestchat-vm-fixture".into(),
            library: "/usr/local/libexec/simplestchat-public".into(),
            executable: "/proc/self/exe".into(),
            socket: "/run/docker.sock".into(),
            vendor: "/sys/class/dmi/id/sys_vendor".into(),
            os_release: ["/etc/os-release".into(), "/usr/lib/os-release".into()],
            config: "/etc/simplestchat-public".into(),
            root: "/srv/simplestchat-public".into(),
            units: "/etc/systemd/system".into(),
            launcher: "/usr/local/bin/simplestchat-public".into(),
        }
    }
}

/// The uid and gid of each owner role, and the identity and platform the
/// guest runs as.
pub(super) struct Identity {
    pub root: (u32, u32),
    pub postgres: (u32, u32),
    pub caddy: (u32, u32),
    pub euid: u32,
    pub system: String,
    pub machine: String,
}

impl Identity {
    fn native() -> Self {
        let uname = rustix::system::uname();
        Self {
            root: (0, 0),
            postgres: (999, 999),
            caddy: (10001, 10001),
            euid: rustix::process::geteuid().as_raw(),
            system: uname.sysname().to_string_lossy().into_owned(),
            machine: uname.machine().to_string_lossy().into_owned(),
        }
    }

    fn of(&self, owner: Owner) -> (u32, u32) {
        match owner {
            Owner::Root => self.root,
            Owner::Postgres => self.postgres,
            Owner::Caddy => self.caddy,
        }
    }
}

/// The workload lock the database and restore work hold.
pub(super) type Lock<'a> = &'a dyn Fn() -> std::result::Result<Workload, assets::Failure>;

/// One guest action's view of the machine.
pub(super) struct Guest<'a> {
    pub host: &'a Host<'a>,
    pub layout: Layout,
    pub identity: Identity,
    pub lock: Lock<'a>,
}

fn words(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

impl Guest<'_> {
    /// One bounded ordinary file, opened without following a link, owned by
    /// `owner` with exactly `mode`, one link, and unchanged while read.
    fn private_file(&self, path: &Path, owner: Owner, mode: u32) -> Result<Vec<u8>> {
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(
                (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32,
            )
            .open(path)?;
        let before = file.metadata()?;
        let (uid, gid) = self.identity.of(owner);
        let exact = before.is_file()
            && before.uid() == uid
            && before.gid() == gid
            && before.mode() & 0o7777 == mode
            && before.nlink() == 1
            && (1..=policy::MAX_FILE).contains(&before.len());
        require(exact, policy::FILE_METADATA)?;
        let mut data = Vec::new();
        Read::take(&mut file, policy::MAX_FILE + 1).read_to_end(&mut data)?;
        let after = file.metadata()?;
        let stamp = |m: &std::fs::Metadata| {
            (
                m.len(),
                m.mtime(),
                m.mtime_nsec(),
                m.ctime(),
                m.ctime_nsec(),
            )
        };
        let unchanged = data.len() as u64 == before.len() && stamp(&before) == stamp(&after);
        require(unchanged, policy::FILE_CHANGED)?;
        Ok(data)
    }

    /// A directory, not a link, owned by `owner` with exactly `mode`.
    fn directory(&self, path: &Path, owner: Owner, mode: u32) -> Result<Value> {
        let metadata = std::fs::symlink_metadata(path)?;
        let (uid, gid) = self.identity.of(owner);
        let exact = metadata.is_dir()
            && metadata.uid() == uid
            && metadata.gid() == gid
            && metadata.mode() & 0o7777 == mode;
        require(exact, policy::DIRECTORY_METADATA)?;
        Ok(json!({"uid":uid,"gid":gid,"mode":mode}))
    }

    /// The run selection once the platform, the private marker, the release
    /// selection, the installed executable and the Docker socket agree.
    pub(super) fn guard(&self, run: &str) -> Checked<Selection> {
        let identity = &self.identity;
        require(identity.euid == identity.root.0, policy::ROOT)?;
        let platform = identity.system == "Linux" && identity.machine == "x86_64";
        require(platform, policy::PLATFORM)?;
        require(hexadecimal(run, 32), policy::RUN_ID)?;
        let release = self
            .layout
            .os_release
            .iter()
            .find_map(|path| std::fs::read(path).ok())
            .ok_or(Failure::Os)?;
        let fields = crate::release::preflight::os_release(&release);

        let debian = fields.is_some_and(|(id, version)| id == "debian" && version == "13");
        require(debian, policy::DISTRIBUTION)?;
        let mut vendor = Vec::new();
        std::fs::File::open(&self.layout.vendor)
            .and_then(|file| file.take(256).read_to_end(&mut vendor))
            .map_err(Failure::from)?;
        require(policy::qemu(&vendor), policy::HYPERVISOR)?;
        let fixture = self.directory(&self.layout.fixture, Owner::Root, 0o700);
        within("guard", "fixture_directory", fixture.map_err(Fault::Bare))?;
        let marker = self
            .private_file(&self.layout.marker, Owner::Root, 0o600)
            .and_then(|marker| require(marker == format!("{run}\n").as_bytes(), policy::MARKER));
        within("guard", "marker", marker.map_err(Fault::Bare))?;
        let selection = self
            .private_file(
                &self.layout.fixture.join("selection.json"),
                Owner::Root,
                0o600,
            )
            .and_then(|data| policy::selection(&data, run));
        let chosen = within("guard", "selection", selection.map_err(Fault::Bare))?;
        let library = self.directory(&self.layout.library, Owner::Root, 0o755);
        within("guard", "library_directory", library.map_err(Fault::Bare))?;
        within("guard", "helper", self.executable().map_err(Fault::Bare))?;
        within("guard", "docker_socket", self.socket().map_err(Fault::Bare))?;
        Ok(chosen)
    }

    /// The Docker socket: a socket, not a link, owned by root with mode 0660.
    fn socket(&self) -> Result<()> {
        let socket = std::fs::symlink_metadata(&self.layout.socket)?;
        let exact = socket.file_type().is_socket()
            && socket.uid() == self.identity.root.0
            && socket.mode() & 0o7777 == 0o660;
        require(exact, policy::DOCKER_SOCKET)
    }

    /// The running image is the installed executable: a root 0755 regular
    /// file with one link, and the very inode this process runs.
    fn executable(&self) -> Result<()> {
        let installed = self.layout.library.join("simplestchat-ops");
        let metadata = std::fs::symlink_metadata(&installed)?;
        let (uid, gid) = self.identity.root;
        let exact = metadata.is_file()
            && metadata.uid() == uid
            && metadata.gid() == gid
            && metadata.mode() & 0o7777 == 0o755
            && metadata.nlink() == 1
            && metadata.len() > 0;
        require(exact, policy::FILE_METADATA)?;
        let running = std::fs::metadata(&self.layout.executable)?;
        let same = (running.dev(), running.ino()) == (metadata.dev(), metadata.ino());
        require(same, policy::HELPER_ORIGIN)
    }

    fn compose(&self, runner: &mut Evidence<'_>, arguments: &[&str]) -> Result<Vec<u8>> {
        let config = self.layout.config.to_str().ok_or(Failure::Value)?;
        let environment = format!("{config}/app.env");
        let file = format!("{config}/compose.public.yml");
        let mut words = vec![
            "compose",
            "--project-name",
            model::PROJECT,
            "--project-directory",
            config,
            "--env-file",
            &environment,
            "-f",
            &file,
        ];
        words.extend_from_slice(arguments);
        runner.docker(&words, 30.0, Streams::default())
    }

    fn launcher(
        &self,
        runner: &mut Evidence<'_>,
        arguments: &[&str],
        seconds: f64,
    ) -> Result<Vec<u8>> {
        let launcher = self.layout.launcher.to_str().ok_or(Failure::Value)?;
        let mut argv = vec![launcher.to_owned()];
        argv.extend(words(arguments));
        runner.run(argv, seconds, Streams::default())
    }

    fn properties(
        &self,
        runner: &mut Evidence<'_>,
        unit: &str,
        names: &[&str],
        seconds: f64,
    ) -> Result<BTreeMap<String, String>> {
        let selected = format!("--property={}", names.join(","));
        let argv = words(&["/usr/bin/systemctl", "show", unit, "--no-pager", &selected]);
        policy::properties(&runner.run(argv, seconds, Streams::default())?, names)
    }

    fn systemctl(
        &self,
        runner: &mut Evidence<'_>,
        verb: &str,
        unit: &str,
        seconds: f64,
    ) -> Result<()> {
        let argv = words(&["/usr/bin/systemctl", verb, unit]);
        runner.run(argv, seconds, Streams::default()).map(|_| ())
    }

    /// Every protected directory and configuration file, its exact owner and
    /// mode, the generated secrets and the rendered environments; the record
    /// keeps metadata and digests, never the bytes.
    fn public_files(&self) -> Checked<serde_json::Map<String, Value>> {
        let mut record = serde_json::Map::new();
        for (name, owner, mode) in policy::DIRECTORIES {
            let path = match name {
                None => self.layout.config.clone(),
                Some("") => self.layout.root.clone(),
                Some(name) => self.layout.root.join(name),
            };
            let slot = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_owned();
            let checked = self.directory(&path, owner, mode).map_err(Fault::Bare);
            let value = within("snapshot_files", &slot, checked)?;
            record.insert(path.to_string_lossy().into_owned(), value);
        }
        let names: BTreeSet<String> = std::fs::read_dir(&self.layout.config)
            .map_err(Failure::from)?
            .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
            .collect::<std::io::Result<_>>()
            .map_err(Failure::from)?;
        policy::configuration_names(&names)?;
        let mut contents = BTreeMap::new();
        for (name, owner, mode) in policy::FILES {
            let path = self.layout.config.join(name);
            let read = self.private_file(&path, owner, mode).map_err(Fault::Bare);
            let data = within("snapshot_files", name, read)?;
            let (uid, gid) = self.identity.of(owner);
            record.insert(
                path.to_string_lossy().into_owned(),
                json!({"uid":uid,"gid":gid,"mode":mode,"bytes":data.len(),
                    "sha256":model::sha256(&data)}),
            );
            contents.insert(name, data);
        }
        let text = |name: &str| contents.get(name).map(Vec::as_slice).unwrap_or_default();
        policy::configuration_values(
            text("secrets.json"),
            text("app.env"),
            text("migration.env"),
            text("proxy.env"),
            text("postgres-admin-password"),
        )?;
        Ok(record)
    }

    /// Compare the protected configuration of two `public.yml` runs.
    fn snapshot(&self, runner: &mut Evidence<'_>, chosen: &Selection) -> Checked<Value> {
        let mut files = within("snapshot_files", "unspecified", self.public_files())?;
        let mut units = serde_json::Map::new();
        for name in ["benchmark", "image-build"] {
            let unit = format!("simplestchat-{name}.service");
            let checked = (|| -> Result<(BTreeMap<String, String>, Vec<u8>)> {
                let value = self.properties(runner, &unit, &policy::UNIT_PROPERTIES, 30.0)?;
                policy::unit(&value, &unit, false)?;
                let content =
                    self.private_file(&self.layout.units.join(&unit), Owner::Root, 0o644)?;
                Ok((value, content))
            })();
            let (value, content) = within("snapshot_units", &unit, checked.map_err(Fault::Bare))?;
            files.insert(
                unit.clone(),
                json!({"sha256":model::sha256(&content),"uid":0,"gid":0,"mode":0o644}),
            );
            units.insert(unit, json!(value));
        }
        self.private_file(&self.layout.launcher, Owner::Root, 0o755)?;
        let present = self.compose(runner, &["ps", "--all", "--quiet"])?;
        require(policy::empty(&present), policy::PROJECT_PRESENT)?;
        let configuration = (|| -> Checked<(usize, String)> {
            let model = self.launcher(
                runner,
                &["--profile", "maintenance", "config", "--format", "json"],
                30.0,
            )?;
            model::compose(&model::object(&model)?)
        })();
        let (services, digest) = within("snapshot_compose", "unspecified", configuration)?;
        let record = json!({"schemaVersion":1,"runId":chosen.run,"files":files,"units":units,
            "compose":{"sha256":digest,"services":services}});
        let path = self.layout.fixture.join("snapshot.json");
        let previous = if std::fs::symlink_metadata(&path).is_ok() {
            Some(self.private_file(&path, Owner::Root, 0o600)?)
        } else {
            None
        };
        let encoded = serde_json::to_vec(&record).map_err(|_| Failure::Value)?;
        let passes = policy::snapshot(previous.as_deref(), &model::object(&encoded)?)?;
        let mut stored = record;
        stored["passes"] = passes.into();
        state::atomic_json(&path, &stored)?;
        Ok(json!({"configurationFiles":policy::FILES.len(),"idempotent":passes == 2}))
    }

    /// The one listed container of `service`, inspected and owned.
    fn inspect_service(&self, runner: &mut Evidence<'_>, service: &str) -> Result<Json> {
        let known = model::SERVICES.iter().any(|(name, _)| *name == service);
        require(known, "Unknown fixture service")?;
        let listing = self.compose(runner, &["ps", "--all", "--quiet", service])?;
        let identifier = policy::listed(&listing)?;
        let inspection = runner.docker(
            &["inspect", "--format", INSPECTION, &identifier],
            30.0,
            Streams::default(),
        )?;
        policy::owned(&identifier, &inspection, service)
    }

    /// The release's manifest and the server and database images the staged
    /// release selected, each confirmed locally.
    fn artifact(
        &self,
        runner: &mut Evidence<'_>,
        chosen: &Selection,
    ) -> Result<(Manifest, String, String)> {
        let directory = self.layout.root.join("releases").join(&chosen.revision);
        self.directory(&directory, Owner::Root, 0o700)?;
        let bytes = self.private_file(&directory.join("release.json"), Owner::Root, 0o600)?;
        let manifest = Manifest::decode(&bytes).map_err(|_| Failure::Value)?;
        let images = model::object(&self.private_file(
            &self.layout.config.join("images.json"),
            Owner::Root,
            0o600,
        )?)?;
        let staged = model::object(&self.private_file(
            &directory.join("staged.json"),
            Owner::Root,
            0o600,
        )?)?;
        let (server, postgres) = policy::selected(
            &chosen.revision,
            &manifest.fields().revision,
            &model::sha256(&bytes),
            &images,
            &staged,
            |server| {
                let arguments = ["image", "inspect", "--format", IMAGE_FORMAT, server];
                let inspection = runner.docker(&arguments, 30.0, Streams::default())?;
                policy::image(&inspection, &chosen.revision)
            },
        )?;
        let arguments = ["image", "inspect", "--format", "{{.Id}}", &postgres];
        let identity = runner.docker(&arguments, 30.0, Streams::default())?;
        let identity = String::from_utf8(identity).map_err(|_| Failure::Value)?;
        let identity = identity.trim().to_owned();
        require(image_id(&identity), "Fixture PostgreSQL image is missing")?;
        Ok((manifest, server, identity))
    }

    fn sql(
        &self,
        runner: &mut Evidence<'_>,
        database: &str,
        source: &[u8],
        seconds: f64,
    ) -> Result<Vec<u8>> {
        let arguments = [
            "exec",
            "--interactive",
            "--user",
            "999:999",
            database,
            "timeout",
            "--signal=TERM",
            "--kill-after=2s",
            "30s",
            "psql",
            "--no-psqlrc",
            "--set",
            "ON_ERROR_STOP=on",
            "--host",
            "/run/simplestchat-postgres",
            "--username",
            "postgres",
            "--dbname",
            "simplestchat",
            "--tuples-only",
            "--no-align",
            "--quiet",
        ];
        let streams = Streams {
            input: Some(source),
            ..Streams::default()
        };
        runner.docker(&arguments, seconds, streams)
    }

    fn ledger(
        &self,
        runner: &mut Evidence<'_>,
        database: &str,
        seconds: f64,
    ) -> Result<BTreeMap<String, String>> {
        let arguments = [
            "exec",
            "--user",
            "999:999",
            database,
            "psql",
            "--no-psqlrc",
            "--set",
            "ON_ERROR_STOP=on",
            "--host",
            "/run/simplestchat-postgres",
            "--username",
            "postgres",
            "--dbname",
            "simplestchat",
            "--tuples-only",
            "--no-align",
            "--field-separator",
            " ",
            "--command",
            LEDGER,
        ];
        policy::ledger(&runner.docker(&arguments, seconds, Streams::default())?)
    }

    /// Seconds left before `deadline`, at most `cap`; none left refuses.
    fn left(&self, deadline: Duration, cap: f64) -> Result<f64> {
        let remaining = deadline
            .saturating_sub(self.host.clock.monotonic())
            .as_secs_f64();
        require(remaining > 0.0, "Fixture phase deadline exceeded")?;
        Ok(remaining.min(cap))
    }

    /// Wait at most `seconds` for the application's loopback readiness.
    fn ready(&self, runner: &mut Evidence<'_>, seconds: u64) -> Result<()> {
        let deadline = self.host.clock.monotonic() + Duration::from_secs(seconds);
        let argv = words(&[
            "/usr/bin/curl",
            "--disable",
            "--noproxy",
            "*",
            "--proto",
            "=http",
            "--fail",
            "--silent",
            "--show-error",
            "--max-time",
            "2",
            "http://127.0.0.1:3000/ready",
        ]);
        loop {
            let answer = match runner.run(argv.clone(), 5.0, Streams::default()) {
                Ok(answer) => policy::ready(&answer),
                Err(Failure::Refused(_) | Failure::Command | Failure::Value) => false,
                Err(failure) => return Err(failure),
            };
            if answer {
                return Ok(());
            }
            let expired = self.host.clock.monotonic() >= deadline;
            require(!expired, "Application readiness deadline exceeded")?;
            self.host.clock.sleep(Duration::from_millis(500));
        }
    }

    /// Run the real migrator to completion of the release's ledger, then
    /// remove exactly the container this created.
    fn migrate(
        &self,
        runner: &mut Evidence<'_>,
        database: &str,
        image: &str,
        expected: &BTreeMap<String, String>,
    ) -> Result<()> {
        let create = [
            "--profile",
            "maintenance",
            "create",
            "--no-build",
            "--pull",
            "never",
            "migrate",
        ];
        self.launcher(runner, &create, 60.0)?;
        let value = self.inspect_service(runner, "migrate")?;
        let identifier = value
            .get("id")
            .and_then(Json::string)
            .ok_or(Failure::Value)?
            .to_owned();
        let image_matches = value.get("image").and_then(Json::string) == Some(image);
        require(image_matches, "Unexpected migration image")?;
        let waited = (|| -> Result<()> {
            runner.docker(&["start", &identifier], 30.0, Streams::default())?;
            let deadline = self.host.clock.monotonic() + Duration::from_secs(120);
            loop {
                let actual = (|| -> Result<BTreeMap<String, String>> {
                    self.left(deadline, 30.0)?;
                    let value = self.inspect_service(runner, "migrate")?;
                    policy::running(&value, "migrate", image)?;
                    let seconds = self.left(deadline, 30.0)?;
                    self.ledger(runner, database, seconds)
                })();
                let actual = match actual {
                    Ok(actual) => actual,
                    Err(Failure::Refused(_) | Failure::Command | Failure::Timeout) => {
                        BTreeMap::new()
                    }
                    Err(failure) => return Err(failure),
                };
                if actual == *expected {
                    return Ok(());
                }
                let expired = self.host.clock.monotonic() >= deadline;
                require(!expired, "Packaged migration ledger was not ready")?;
                self.host.clock.sleep(Duration::from_millis(500));
            }
        })();
        let removed = self.remove_migrate(runner, image, &identifier);
        removed.and(waited)
    }

    fn remove_migrate(
        &self,
        runner: &mut Evidence<'_>,
        image: &str,
        identifier: &str,
    ) -> Result<()> {
        let value = self.inspect_service(runner, "migrate")?;
        let same = value.get("id").and_then(Json::string) == Some(identifier)
            && value.get("image").and_then(Json::string) == Some(image);
        require(same, "Migration container identity changed")?;
        runner.docker(
            &["stop", "--time", "10", identifier],
            20.0,
            Streams::default(),
        )?;
        runner.docker(&["rm", identifier], 15.0, Streams::default())?;
        let left = self.compose(runner, &["ps", "--all", "--quiet", "migrate"])?;
        require(policy::empty(&left), "Migration container cleanup failed")
    }

    /// Create the fixture database through the real migrator, add the
    /// synthetic rows and start the application without its proxy.
    fn database(&self, runner: &mut Evidence<'_>, chosen: &Selection) -> Checked<Value> {
        let record = model::object(&self.private_file(
            &self.layout.fixture.join("snapshot.json"),
            Owner::Root,
            0o600,
        )?)?;
        let idempotent = record.get("runId").and_then(Json::string) == Some(chosen.run.as_str())
            && record
                .get("passes")
                .is_some_and(|passes| passes.semantic_eq(&Json::Integer("2".into())));
        require(idempotent, "Public idempotence must pass first")?;
        let absent = !self.layout.fixture.join("database.json").exists();
        require(absent, "Fixture database was already created")?;
        let workload = (self.lock)().map_err(|_| Failure::Os)?;
        let created = self.create_database(runner, chosen);
        let released = workload.finish().map_err(|_| Failure::Os);
        let created = created?;
        released?;
        Ok(created)
    }

    fn create_database(&self, runner: &mut Evidence<'_>, chosen: &Selection) -> Result<Value> {
        let (manifest, image, database_image) = self.artifact(runner, chosen)?;
        let present = self.compose(runner, &["ps", "--all", "--quiet"])?;
        require(
            policy::empty(&present),
            "Fixture public project must start empty",
        )?;
        let up = [
            "up",
            "--detach",
            "--no-deps",
            "--no-build",
            "--pull",
            "never",
            "--wait",
            "--wait-timeout",
            "120",
            "postgres",
        ];
        self.launcher(runner, &up, 130.0)?;
        let postgres = self.inspect_service(runner, "postgres")?;
        let identifier = policy::running(&postgres, "postgres", &database_image)?;
        self.migrate(runner, &identifier, &image, &manifest.fields().migrations)?;
        let grants = self.private_file(
            &self.layout.config.join("runtime-grants.sql"),
            Owner::Root,
            0o600,
        )?;
        self.sql(runner, &identifier, &grants, 35.0)?;
        let schema = self.private_file(
            &self.layout.library.join("monitoring-schema.sql"),
            Owner::Root,
            0o644,
        )?;
        self.sql(runner, &identifier, &schema, 35.0)?;
        self.sql(runner, &identifier, FIXTURE_SQL, 35.0)?;
        let verify = self.private_file(
            &self.layout.library.join("restore-verify.sql"),
            Owner::Root,
            0o644,
        )?;
        let counted = model::object(&self.sql(runner, &identifier, &verify, 35.0)?)?;
        require(
            policy::counted(&counted),
            "Fixture source counts or privacy differ",
        )?;
        let up = [
            "up",
            "--detach",
            "--no-deps",
            "--no-build",
            "--pull",
            "never",
            "simplestchat",
        ];
        self.launcher(runner, &up, 60.0)?;
        self.ready(runner, 60)?;
        let application = self.inspect_service(runner, "simplestchat")?;
        let application = policy::running(&application, "simplestchat", &image)?;
        let proxy = self.compose(runner, &["ps", "--all", "--quiet", "caddy"])?;
        require(policy::empty(&proxy), "Fixture proxy must remain absent")?;
        state::atomic_json(
            &self.layout.fixture.join("database.json"),
            &json!({"schemaVersion":1,"runId":chosen.run,"postgres":identifier,
                "application":application,"postgresImage":database_image,"serverImage":image}),
        )?;
        let migrations = manifest.fields().migrations.len();
        Ok(json!({"migrations":migrations,"ready":true,"counts":counts()}))
    }

    /// The backup target, script and units, the service's effective sandbox
    /// and the enabled timer.
    fn backup_units(&self, runner: &mut Evidence<'_>) -> Result<()> {
        self.directory(
            &self.layout.root.join("backups/nightly"),
            Owner::Root,
            0o700,
        )?;
        self.private_file(
            &self.layout.library.join("backup-nightly.sh"),
            Owner::Root,
            0o700,
        )?;
        for name in ["service", "timer"] {
            let unit = self
                .layout
                .units
                .join(format!("simplestchat-backup.{name}"));
            self.private_file(&unit, Owner::Root, 0o644)?;
        }
        let mut names = policy::UNIT_PROPERTIES.to_vec();
        names.extend(policy::BACKUP_SANDBOX.iter().map(|(name, _)| *name));
        let service = self.properties(runner, BACKUP_SERVICE, &names, 30.0)?;
        let timer_names: Vec<&str> = policy::BACKUP_TIMER.iter().map(|(name, _)| *name).collect();
        let timer = self.properties(runner, BACKUP_TIMER, &timer_names, 30.0)?;
        policy::backup_units(&service, &timer)
    }

    /// Take one backup through the installed service while its timer is
    /// paused, then restore it in the isolated verifier.
    fn backup_restore(&self, runner: &mut Evidence<'_>, chosen: &Selection) -> Checked<Value> {
        self.backup_units(runner)?;
        self.public_files()?;
        let state = model::object(&self.private_file(
            &self.layout.fixture.join("database.json"),
            Owner::Root,
            0o600,
        )?)?;
        let owned = state.get("runId").and_then(Json::string) == Some(chosen.run.as_str());
        require(owned, "Fixture database ownership differs")?;
        let (manifest, image, database_image) = self.artifact(runner, chosen)?;
        let postgres = self.inspect_service(runner, "postgres")?;
        let database = policy::running(&postgres, "postgres", &database_image)?;
        let unchanged = state.get("postgres").and_then(Json::string) == Some(database.as_str())
            && {
                let application = self.inspect_service(runner, "simplestchat")?;
                let application = policy::running(&application, "simplestchat", &image)?;
                state.get("application").and_then(Json::string) == Some(application.as_str())
            };
        require(unchanged, "Fixture container changed before backup")?;
        self.systemctl(runner, "stop", BACKUP_TIMER, 30.0)?;
        let backed = self.paused(runner, &manifest, &database, &database_image);
        let restarted = self.systemctl(runner, "start", BACKUP_TIMER, 30.0);
        restarted.and(backed)?;
        self.backup_units(runner)?;
        self.ready(runner, 30)?;
        Ok(
            json!({"archiveValidated":true,"restoreVerified":true,"cleanupPassed":true,
            "counts":counts()}),
        )
    }

    fn paused(
        &self,
        runner: &mut Evidence<'_>,
        manifest: &Manifest,
        database: &str,
        database_image: &str,
    ) -> Result<()> {
        let deadline = self.host.clock.monotonic() + Duration::from_secs(310);
        loop {
            let seconds = self.left(deadline, 30.0)?;
            let state = self.properties(runner, BACKUP_SERVICE, &["ActiveState"], seconds)?;
            let active = state
                .get("ActiveState")
                .map(String::as_str)
                .unwrap_or_default();
            if policy::settled(active)? {
                break;
            }
            self.host.clock.sleep(Duration::from_millis(500));
        }
        let target = self.layout.root.join("backups/nightly");
        let occupied = self.host.clock.monotonic() + Duration::from_secs(3);
        loop {
            let stamp = self.host.clock.now().format("%Y%m%dT%H%M%SZ");
            if !target.join(format!("{stamp}.logs")).exists() {
                break;
            }
            let expired = self.host.clock.monotonic() >= occupied;
            require(!expired, "Fixture backup timestamp is occupied")?;
            self.host.clock.sleep(Duration::from_millis(100));
        }
        let before = receipts_in(&target)?;
        self.systemctl(runner, "start", BACKUP_SERVICE, 310.0)?;
        let names = ["Result", "ExecMainStatus", "ActiveState"];
        let service = self.properties(runner, BACKUP_SERVICE, &names, 30.0)?;
        require(
            policy::backup_completed(&service),
            "Backup service did not complete",
        )?;
        let created: Vec<PathBuf> = receipts_in(&target)?.difference(&before).cloned().collect();
        let [receipt] = created.as_slice() else {
            return Err(Failure::Refused(
                "Backup service did not publish exactly one new receipt",
            ));
        };
        self.verify_backup(runner, receipt, manifest, database, database_image)
    }

    fn verify_backup(
        &self,
        runner: &mut Evidence<'_>,
        receipt: &Path,
        manifest: &Manifest,
        database: &str,
        database_image: &str,
    ) -> Result<()> {
        let workload = (self.lock)().map_err(|_| Failure::Os)?;
        let verified = self.verify_locked(runner, receipt, manifest, database, database_image);
        let released = workload.finish().map_err(|_| Failure::Os);
        verified.and(released)
    }

    fn verify_locked(
        &self,
        runner: &mut Evidence<'_>,
        receipt: &Path,
        manifest: &Manifest,
        database: &str,
        database_image: &str,
    ) -> Result<()> {
        let now = self.host.clock.now().timestamp_micros() as f64 / 1_000_000.0;
        let validated = receipts::inspected(receipt, self.identity.root.0, now, None)?;
        let packaged = &manifest.fields().migrations;
        let postgres = self.inspect_service(runner, "postgres")?;
        let running = policy::running(&postgres, "postgres", database_image)?;
        policy::backup_identity(
            &running,
            database,
            &validated.record,
            database_image,
            packaged,
            || self.ledger(runner, database, 30.0),
        )?;
        let attempt = state::private_directory(&self.layout.fixture, "restore.")?;
        let snapshot = attempt.join("snapshot.dump");
        let digest = validated
            .record
            .get("sha256")
            .and_then(Json::string)
            .unwrap_or_default();
        let restored = copy_archive(&validated.dump, &snapshot, digest, self.identity.root.0)
            .and_then(|()| {
                let mut restore = Evidence::new(self.host, attempt.clone());
                let mut migrations: Vec<(String, String)> = packaged
                    .iter()
                    .map(|(version, checksum)| (version.clone(), checksum.clone()))
                    .collect();
                migrations.sort_by_key(|(version, _)| version.parse::<u64>().unwrap_or(u64::MAX));
                let limits = RestoreLimits {
                    memory_mib: 512,
                    data_mib: 256,
                };
                let statements = self.layout.library.join("restore-verify.sql");
                verify_restore(
                    &mut restore,
                    self.host.clock,
                    database_image,
                    &snapshot,
                    &migrations,
                    limits,
                    &statements,
                )
            });
        let removed = match std::fs::remove_file(&snapshot) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(Failure::Os),
            _ => Ok(()),
        };
        let report = restored?;
        removed?;
        policy::restored(
            report.verified,
            report.cleanup_passed,
            report.counts.as_ref(),
        )?;
        let label = format!("label={RESTORE_LABEL}");
        let left = runner.docker(
            &["ps", "--all", "--quiet", "--filter", &label],
            30.0,
            Streams::default(),
        )?;
        require(policy::empty(&left), "Fixture restore container remains")
    }
}

fn counts() -> Value {
    let pairs = model::COUNT_NAMES.iter().zip(policy::COUNTS);
    Value::Object(
        pairs
            .map(|(name, count)| ((*name).to_owned(), count.into()))
            .collect(),
    )
}

/// Every backup receipt in `target`.
fn receipts_in(target: &Path) -> Result<BTreeSet<PathBuf>> {
    let mut found = BTreeSet::new();
    for entry in std::fs::read_dir(target)? {
        let path = entry?.path();
        let receipt = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".receipt.json") && name.len() > 13);
        if receipt {
            found.insert(path);
        }
    }
    Ok(found)
}

/// Copy one private archive of at most 64 MiB, read without following a
/// link, to a new private `target`, requiring the copied bytes to hash to
/// `digest`.
fn copy_archive(source: &Path, target: &Path, digest: &str, owner: u32) -> Result<()> {
    use sha2::Digest;
    let mut incoming = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(source)?;
    let info = incoming.metadata()?;
    let safe = info.is_file()
        && info.uid() == owner
        && info.mode() & 0o7777 == 0o600
        && (1..=MAX_BACKUP).contains(&info.len());
    require(safe, "Unsafe or oversized release backup")?;
    let mut outgoing = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(target)?;
    let mut hash = sha2::Sha256::new();
    let mut size = 0_u64;
    let mut block = vec![0_u8; 1024 * 1024];
    loop {
        let count = incoming.read(&mut block)?;
        if count == 0 {
            break;
        }
        size += count as u64;
        require(size <= MAX_BACKUP, "Release backup exceeded its bound")?;
        hash.update(&block[..count]);
        outgoing.write_all(&block[..count])?;
    }
    outgoing.flush()?;
    outgoing.sync_all()?;
    let actual: String = hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    require(
        size == info.len() && actual == digest,
        "Release backup digest differs",
    )
}

/// Run `action` for `run` and return its receipt and exit status.
pub(super) fn execute(guest: &Guest<'_>, action: Action, run: &str) -> (Value, u8) {
    let outcome = (|| -> std::result::Result<Value, Diagnostic> {
        let chosen = guest
            .guard(run)
            .map_err(|fault| fault.placed("guard", "unspecified"))?;
        rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o077));
        let attempt =
            state::private_directory(&guest.layout.fixture, &format!("{}.", action.name()))
                .map_err(|failure| Fault::Bare(failure).placed("unknown", "unspecified"))?;
        let mut runner = Evidence::new(guest.host, attempt.clone());
        let result = match action {
            Action::Snapshot => guest.snapshot(&mut runner, &chosen),
            Action::Database => guest.database(&mut runner, &chosen),
            Action::BackupRestore => guest.backup_restore(&mut runner, &chosen),
        };
        let fields = result.map_err(|fault| fault.placed(action.name(), "unspecified"))?;
        let report = receipt(run, action, &Ok(fields.clone()));
        state::atomic_json(&attempt.join("outcome.json"), &report)
            .map_err(|failure| Fault::Bare(failure).placed("report", "unspecified"))?;
        Ok(fields)
    })();
    let status = u8::from(outcome.is_err());
    (receipt(run, action, &outcome), status)
}

fn parse(arguments: &[OsString]) -> Option<(Action, String)> {
    let words: Vec<&str> = arguments
        .iter()
        .map(|word| word.to_str())
        .collect::<Option<_>>()?;
    let [action, "--run-id", run] = words.as_slice() else {
        return None;
    };

    Some((Action::named(action)?, (*run).to_owned()))
}

/// Run one guest action from `arguments` (`ACTION --run-id RUN`). A syntax
/// error exits 2 with the usage; a run id that is not 32 lowercase
/// hexadecimal digits exits 1 with no receipt; otherwise the receipt is
/// printed as one line and the action's success exits 0, its failure 1.
pub fn run(arguments: &[OsString]) -> ExitCode {
    let Some((action, run)) = parse(arguments) else {
        let _ = std::io::stderr().write_all(USAGE.as_bytes());
        return ExitCode::from(2);
    };
    if !hexadecimal(&run, 32) {
        let _ = std::io::stderr().write_all(b"Invalid disposable VM ownership token.\n");
        return ExitCode::FAILURE;
    }
    let stop = Arc::new(AtomicBool::new(false));
    let handled = [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT]
        .iter()
        .all(|signal| signal_hook::flag::register(*signal, Arc::clone(&stop)).is_ok());
    let commands = Interruptible {
        stop: &stop,
        refusal: "Fixture interrupted",
    };
    let clock = System::new();
    let host = Host {
        commands: &commands as &dyn Commands,
        clock: &clock,
        paths: Paths::fixed(),
        owner: 0,
    };
    let guest = Guest {
        host: &host,
        layout: Layout::fixed(),
        identity: Identity::native(),
        lock: &Workload::acquire,
    };
    let (report, status) = if handled {
        execute(&guest, action, &run)
    } else {
        let failed = Fault::Bare(Failure::Os).placed("unknown", "unspecified");
        (receipt(&run, action, &Err(failed)), 1)
    };
    let line = format!("{}\n", serde_json::to_string(&report).unwrap_or_default());
    let printed = std::io::stdout().write_all(line.as_bytes()).is_ok();
    if succeeded(status, printed, stop.load(Ordering::SeqCst)) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// An action succeeds only with status 0, its receipt printed, and no stop
/// signal received.
fn succeeded(status: u8, printed: bool, stopped: bool) -> bool {
    status == 0 && printed && !stopped
}

#[cfg(test)]
#[path = "guest_tests.rs"]
mod tests;
