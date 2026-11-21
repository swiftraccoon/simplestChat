//! `release backup verify` against real PostgreSQL: archives made in the
//! owned cluster the way a deployment makes them, verified by the operation
//! itself. Its container is a new database of that cluster and every tool
//! the container would run is the installed one on that database's address;
//! the archive, the restore, the migration ledger, `restore-verify.sql`,
//! `pg_amcheck`, the cleanup and the published evidence are the operation's
//! own. These run only inside the disposable cluster that `simplestchat-ci
//! database-check restore` owns and selects; anywhere else, a broad
//! `--include-ignored` run included, they return at once.

use super::super::control::{Cause, Owner};
use super::super::files::hex;
use super::container_tests::{
    self, Client, Engine, Installed, Seen, pinned_reference, tagged_version,
};
use super::process::Native;
use super::{Access, CommandOutput, Executor, Invocation, Options, Role, execution, policy};
use crate::test_support::{Fixture, sha256_hex};
use serde_json::{Value, json};
use sha2::{Digest, Sha384};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const SELECTOR: &str = "RESTORE_DATABASE_CHECK";
/// The release attempt every prepared host here records.
const ATTEMPT: &str = "release.restorefixture";
/// The first migration after which a session must carry its family.
const SESSION_FAMILY: u32 = 22;
/// The daemon every request of the operation names.
pub(super) const DAEMON: [&str; 2] = ["--host", "unix:///var/run/docker.sock"];
/// The container's own connection, which the installed tools replace.
const CONTAINER: [&str; 6] = [
    "-h",
    policy::SOCKET,
    "-U",
    "postgres",
    "-d",
    policy::DATABASE,
];
/// The bound the container's own `timeout` puts on a long tool.
const BOUNDED: [&str; 4] = ["timeout", "--signal=TERM", "--kill-after=2s", "120s"];
/// Prints one answer of the daemon.
pub(super) const PRINTF: &str = "/usr/bin/printf";

/// The owning check's database, or nothing anywhere else.
fn database() -> Option<String> {
    let set = |name: &str| std::env::var(name).is_ok_and(|value| value == "1");
    if !(set(SELECTOR) && set("DISPOSABLE_TEST_DATABASE")) {
        return None;
    }
    Some(std::env::var("TEST_DATABASE_URL").expect("the owning check's database"))
}

/// Where an installed PostgreSQL tool is.
fn installed(name: &str) -> PathBuf {
    let path = std::env::var_os("PATH").unwrap();
    let found = std::env::split_paths(&path).map(|directory| directory.join(name));
    let found = found.into_iter().find(|candidate| candidate.is_file());
    found.unwrap_or_else(|| panic!("{name} is required"))
}

/// Run one PostgreSQL tool with `source` on its standard input: what it
/// printed, or what it complained of when it failed.
fn tool(name: &str, arguments: &[&str], source: &[u8]) -> std::result::Result<String, String> {
    let mut command = Command::new(installed(name));
    command.args(arguments).stdin(Stdio::piped());
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(source).unwrap();
    let output = child.wait_with_output().unwrap();
    if output.status.success() {
        Ok(String::from_utf8(output.stdout).unwrap())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned())
    }
}

/// Statements run in one database of the owned cluster.
fn attempted(url: &str, source: &str) -> std::result::Result<String, String> {
    let arguments = ["-X", "-qAt", "--set", "ON_ERROR_STOP=1", url];
    tool("psql", &arguments, source.as_bytes())
}

fn sql(url: &str, source: &str) -> String {
    attempted(url, source).unwrap_or_else(|complaint| panic!("{complaint}"))
}

/// The address of another database of the same cluster.
fn beside(base: &str, name: &str) -> String {
    let (address, options) = match base.split_once('?') {
        Some((address, options)) => (address, format!("?{options}")),
        None => (base, String::new()),
    };
    let (cluster, _) = address.rsplit_once('/').unwrap();
    format!("{cluster}/{name}{options}")
}

/// A name no other database of the cluster has.
fn unique(prefix: &str) -> String {
    let mut random = [0_u8; 8];
    getrandom::fill(&mut random).unwrap();
    format!("{prefix}_{}", hex(&random))
}

/// The databases a test made in the owned cluster, dropped when it ends.
struct Made<'a> {
    base: &'a str,
    names: Vec<String>,
}

impl Made<'_> {
    /// A new empty database owned by `postgres`, or a copy of `template`.
    fn database(&mut self, template: Option<&str>) -> (String, String) {
        let name = unique("restore_source");
        let copied = template.map_or_else(String::new, |from| format!(" TEMPLATE {from}"));
        sql(
            self.base,
            &format!("CREATE DATABASE {name}{copied} OWNER postgres;"),
        );
        self.names.push(name.clone());
        let url = beside(self.base, &name);
        (name, url)
    }
}

impl Drop for Made<'_> {
    fn drop(&mut self) {
        for name in &self.names {
            let _ = attempted(
                self.base,
                &format!("DROP DATABASE IF EXISTS {name} WITH (FORCE);"),
            );
        }
    }
}

/// The roles a deployment's database has. A source needs them; a
/// verification must not find the two runtime roles, which a new container
/// lacks until the operation makes them.
const ROLES: &str = "DO $$ BEGIN
IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='postgres') THEN
 CREATE ROLE postgres NOLOGIN; END IF;
IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='simplestchat_app') THEN
 CREATE ROLE simplestchat_app NOLOGIN; END IF;
IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='simplestchat_migrate') THEN
 CREATE ROLE simplestchat_migrate NOLOGIN; END IF;
END $$;";

/// What a deployment's database holds before its first migration: the
/// public schema closed to everyone but the two roles, the trigram
/// extension and the migration ledger.
const START: &str = "SET ROLE postgres;
REVOKE ALL ON SCHEMA public FROM PUBLIC;
GRANT USAGE, CREATE ON SCHEMA public TO simplestchat_migrate;
GRANT USAGE ON SCHEMA public TO simplestchat_app;
CREATE EXTENSION pg_trgm;
SET ROLE simplestchat_migrate;
CREATE TABLE public._sqlx_migrations(version BIGINT PRIMARY KEY, description TEXT NOT NULL,
 installed_on TIMESTAMPTZ NOT NULL DEFAULT now(), success BOOLEAN NOT NULL,
 checksum BYTEA NOT NULL, execution_time BIGINT NOT NULL);";

/// The only grants that existed before migration 022; an earlier archive
/// carries these, and the current grants follow every migration.
const EARLIER_GRANTS: &str = "BEGIN;
GRANT SELECT, INSERT, UPDATE ON public.users TO simplestchat_app;
GRANT SELECT, INSERT, UPDATE, DELETE
    ON public.webauthn_credentials, public.sessions, public.rooms, public.room_roles,
       public.room_states, public.room_reports, public.moderation_events, public.invites
    TO simplestchat_app;
GRANT SELECT, INSERT, DELETE ON public.invite_redemptions TO simplestchat_app;
COMMIT;";

/// Rows written before migration 022, each table with a count no other
/// counted table has: three users, two rooms, four earlier sessions, no
/// credential, one open incident and five resolved ones.
const EARLIER_ROWS: &str = "INSERT INTO public.users(email, display_name)
 SELECT 'fixture' || n || '@example.test', 'Fixture ' || n FROM generate_series(1, 3) n;
INSERT INTO public.rooms(id, owner_id, display_name)
 SELECT 'fixture-' || n, (SELECT id FROM public.users WHERE email = 'fixture1@example.test'),
  'Fixture ' || n FROM generate_series(1, 2) n;
INSERT INTO public.sessions(user_id, refresh_token_hash, expires_at)
 SELECT (SELECT id FROM public.users WHERE email = 'fixture1@example.test'),
  encode(sha256(('earlier refresh ' || n)::bytea), 'hex'), now() + interval '1 day'
 FROM generate_series(1, 4) n;
INSERT INTO operations.alerts(incident_key, rule, severity, resource, first_seen, last_seen,
  resolved_at)
 SELECT encode(sha256(('incident ' || n)::bytea), 'hex'),
  CASE WHEN n = 1 THEN 'DatabaseUnavailable' ELSE 'CollectorStale' END,
  CASE WHEN n = 1 THEN 'critical' ELSE 'warning' END, '{}', now(), now(),
  CASE WHEN n > 1 THEN now() END
 FROM generate_series(1, 6) n;
INSERT INTO operations.alert_cursor VALUES (true, now());";

/// Six sessions signed in after migration 022 removed the earlier ones.
const CURRENT_SESSIONS: &str = "INSERT INTO public.sessions(user_id, refresh_token_hash,
  refresh_token_family_hash, expires_at)
 SELECT (SELECT id FROM public.users WHERE email = 'fixture1@example.test'),
  encode(sha256(('current refresh ' || n)::bytea), 'hex'),
  encode(sha256(('current family ' || n)::bytea), 'hex'), now() + interval '1 day'
 FROM generate_series(1, 6) n;";

/// The Ansible tree, whose statements and grants a host installs.
fn files() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("ansible")
}

/// One packaged migration and the checksum its ledger row records.
struct Migration {
    version: u32,
    description: String,
    source: String,
    checksum: String,
}

/// Every packaged migration, in version order.
fn migrations() -> Vec<Migration> {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../migrations");
    let mut found = Vec::new();
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        let Some(stem) = name.strip_suffix(".sql") else {
            continue;
        };
        let (version, description) = stem.split_once('_').unwrap();
        let bytes = fs::read(&path).unwrap();
        found.push(Migration {
            version: version.parse().unwrap(),
            description: description.replace('_', " "),
            checksum: hex(&Sha384::digest(&bytes)),
            source: String::from_utf8(bytes).unwrap(),
        });
    }
    found.sort_by_key(|migration| migration.version);
    found
}

/// The ledger the given migrations leave, as a manifest and an outcome
/// record it: version to checksum.
fn ledger<'a>(applied: impl IntoIterator<Item = &'a Migration>) -> BTreeMap<String, String> {
    let entries = applied.into_iter();
    let pairs =
        entries.map(|migration| (migration.version.to_string(), migration.checksum.clone()));
    pairs.collect()
}

/// Run one migration as the migration role, as a deployment's migration
/// step does, and record it in the ledger in the same transaction unless
/// it must run outside one.
fn migrate(url: &str, migration: &Migration) {
    let Migration {
        version,
        description,
        source,
        checksum,
    } = migration;
    let transactional = !source.starts_with("-- no-transaction");
    let (open, close) = if transactional {
        ("BEGIN;\nSET LOCAL ROLE simplestchat_migrate;\n", "COMMIT;")
    } else {
        ("SET ROLE simplestchat_migrate;\n", "")
    };
    // psql sends standard input one statement at a time, so a concurrent
    // index stays outside both a transaction and a multi-statement execute.
    sql(
        url,
        &format!(
            "{open}{source}\nINSERT INTO public._sqlx_migrations(version, description, \
             success, checksum, execution_time) VALUES ({version}, '{description}', true, \
             decode('{checksum}', 'hex'), 0);\n{close}"
        ),
    );
}

/// A deployment's database through every migration before `until`, with
/// its earlier grants, the operational schema and the earlier rows.
fn earlier(url: &str, all: &[Migration], until: u32) -> BTreeMap<String, String> {
    sql(url, START);
    let applied: Vec<_> = all.iter().filter(|m| m.version < until).collect();
    for migration in &applied {
        migrate(url, migration);
    }
    sql(url, EARLIER_GRANTS);
    let schema = fs::read_to_string(files().join("files/monitoring-schema.sql")).unwrap();
    sql(url, &format!("SET ROLE postgres;\n{schema}"));
    sql(url, EARLIER_ROWS);
    ledger(applied)
}

/// The same database brought to the current release: the remaining
/// migrations, the reviewed runtime grants, the optional external
/// operations schema and sessions signed in since.
fn upgrade(url: &str, all: &[Migration], from: u32) {
    for migration in all.iter().filter(|m| m.version >= from) {
        migrate(url, migration);
    }
    let grants = files().join("templates/public-runtime-grants.sql.j2");
    sql(url, &fs::read_to_string(grants).unwrap());
    let external = fs::read_to_string(files().join("files/monitoring-external-schema.sql"));
    sql(url, &format!("SET ROLE postgres;\n{}", external.unwrap()));
    sql(url, CURRENT_SESSIONS);
}

/// The archive at `path` names no role a new container lacks: never the
/// owned cluster's own superuser, which owns nothing a deployment holds.
fn portable(base: &str, path: &Path) {
    let authority = base
        .split_once("://")
        .and_then(|(_, rest)| rest.split_once('@'));
    let (login, _) = authority.expect("the owning check's superuser");
    // Only the role name can occur in an archive; a URL may also carry more.
    let superuser = login.split_once(':').map_or(login, |(name, _)| name);
    let file = path.to_str().unwrap();
    let arguments = ["--schema-only", "--file", "-", file];
    let script = tool("pg_restore", &arguments, b"").unwrap();
    assert!(
        !script.contains(superuser),
        "the archive names the owned cluster's superuser"
    );
}

/// A complete custom-format archive of one database, as a release takes.
fn dump(url: &str, path: &Path) -> Vec<u8> {
    let file = path.to_str().unwrap();
    tool("pg_dump", &["--format=custom", "--file", file, url], b"").unwrap();
    fs::read(path).unwrap()
}

fn directory(path: &Path, mode: u32) {
    fs::DirBuilder::new()
        .mode(0o700)
        .recursive(true)
        .create(path)
        .unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

fn write(path: &Path, bytes: &[u8], mode: u32) {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
    file.set_permissions(fs::Permissions::from_mode(mode))
        .unwrap();
}

/// The reviewed release every prepared host here holds.
fn revision() -> String {
    sha256_hex(b"restore fixture release")[..40].to_owned()
}

/// A prepared host's private files for one recorded release attempt: its
/// backup, the outcome recording the backup's digest and ledger, the
/// reviewed release manifest and the installed `restore-verify.sql`.
pub(super) struct Prepared {
    pub(super) access: Access,
    _fixture: Fixture,
}

impl Prepared {
    /// A host whose container client is an inert file the stand-in daemon
    /// never executes.
    pub(super) fn new(
        backup: &[u8],
        digest: &str,
        recorded: &BTreeMap<String, String>,
        packaged: &BTreeMap<String, String>,
    ) -> Self {
        Self::build(backup, digest, recorded, packaged, None)
    }

    /// A host whose container client is a copy of the installed `client`,
    /// owned like the rest of the host by the test's user, on the real
    /// daemon socket.
    fn engine(
        backup: &[u8],
        digest: &str,
        recorded: &BTreeMap<String, String>,
        packaged: &BTreeMap<String, String>,
        client: &Path,
    ) -> Self {
        Self::build(backup, digest, recorded, packaged, Some(client))
    }

    fn build(
        backup: &[u8],
        digest: &str,
        recorded: &BTreeMap<String, String>,
        packaged: &BTreeMap<String, String>,
        client: Option<&Path>,
    ) -> Self {
        let fixture = Fixture::new();
        let base = &fixture.0;
        let results = base.join("public/results").join(ATTEMPT);
        let release = base.join("public/releases").join(revision());
        for (path, mode) in [
            (base.join("public"), 0o700),
            (results.clone(), 0o700),
            (release.clone(), 0o700),
            (base.join("config"), 0o700),
            (base.join("work"), 0o700),
            (base.join("state"), 0o750),
            (base.join("library"), 0o700),
            (base.join("tools"), 0o700),
        ] {
            directory(&path, mode);
        }
        let revision = revision();
        let outcome = json!({"passed": true, "revision": revision, "backupSha256": digest,
            "backupMigrations": recorded});
        write(
            &results.join("outcome.json"),
            &serde_json::to_vec(&outcome).unwrap(),
            0o600,
        );
        write(&results.join("database-before.dump"), backup, 0o600);
        let manifest = json!({"schemaVersion": 1, "revision": revision,
            "platform": "linux/amd64",
            "archiveSha256": sha256_hex(b"restore fixture application archive"),
            "imageTag": format!("simplestchat-release/production:{revision}"),
            "migrations": packaged, "createdAt": "2026-10-24T00:00:00Z"});
        write(
            &release.join("release.json"),
            &serde_json::to_vec(&manifest).unwrap(),
            0o600,
        );
        let statements = fs::read(files().join("files/restore-verify.sql")).unwrap();
        write(&base.join("library/restore-verify.sql"), &statements, 0o644);
        let tool = base.join("tools/docker");
        if let Some(client) = client {
            fs::copy(client, &tool).unwrap();
            fs::set_permissions(&tool, fs::Permissions::from_mode(0o700)).unwrap();
        } else {
            write(&tool, b"the daemon's client is never executed here", 0o700);
        }
        let access = Access {
            root: base.join("public"),
            config: base.join("config"),
            work: base.join("work"),
            state: base.join("state"),
            library: base.join("library"),
            docker: tool,
            socket: client.map(|_| PathBuf::from("/var/run/docker.sock")),
            uid: rustix::process::geteuid().as_raw(),
            nonce: None,
            timestamp: None,
            virtual_poll: false,
            fault: None,
        };
        Self {
            access,
            _fixture: fixture,
        }
    }
}

/// What the container's database held when the container was removed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Removed {
    /// The restored users table.
    restored: bool,
    /// The extension `pg_amcheck --install-missing` installs where it checks.
    checked: bool,
}

/// The one container a verification created.
struct Container {
    name: String,
    id: String,
    database: String,
    removed: bool,
}

/// A container daemon whose engine is the owned cluster. The container a
/// verification creates is a new database owned by `postgres`; each tool
/// the container would run is the installed one on that database's
/// address, started by the operation's own process owner; removing the
/// container drops the database. Each request must carry what the
/// operation gives its real daemon, and each answer is what that daemon
/// would print. The container's isolation, its image and its own
/// `timeout` are not exercised.
struct Daemon<'a> {
    base: &'a str,
    client: PathBuf,
    image: String,
    container: Option<Container>,
    roles: Vec<Role>,
    statuses: Vec<Option<i32>>,
    removed: Option<Removed>,
}

/// One printed answer of the daemon.
pub(super) fn answer(text: &str) -> (PathBuf, Vec<String>) {
    (PRINTF.into(), vec!["%s".to_owned(), text.to_owned()])
}

impl<'a> Daemon<'a> {
    fn new(base: &'a str, client: &Path) -> Self {
        Self {
            base,
            client: client.to_owned(),
            image: format!("sha256:{}", sha256_hex(b"restore fixture image")),
            container: None,
            roles: Vec::new(),
            statuses: Vec::new(),
            removed: None,
        }
    }

    fn live(&self) -> Option<&Container> {
        self.container
            .as_ref()
            .filter(|container| !container.removed)
    }

    fn address(&self) -> String {
        let container = self.live().expect("a live container");
        beside(self.base, &container.database)
    }

    /// The words the container runs after `exec`, for its one container.
    fn inside<'w>(&self, words: &'w [&'w str], input: bool) -> &'w [&'w str] {
        let prefix: &[&str] = if input {
            &["exec", "-i", "--user", "999:999"]
        } else {
            &["exec", "--user", "999:999"]
        };
        let rest = words
            .strip_prefix(prefix)
            .expect("an exec in the container");
        assert_eq!(Some(rest[0]), self.live().map(|c| c.id.as_str()));
        &rest[1..]
    }

    /// The installed `name` with the options the container's would get, on
    /// the container's database.
    fn client(&self, words: &[&str], name: &str) -> (PathBuf, Vec<String>) {
        let words = words.strip_prefix(&[name]).expect("the container's tool");
        let options = words.strip_suffix(&CONTAINER).expect("its own connection");
        let mut arguments: Vec<String> = options.iter().map(|&word| word.to_owned()).collect();
        let address = self.address();
        match name {
            "psql" => arguments.push(address),
            "pg_restore" => arguments.extend(["--dbname".to_owned(), address]),
            _ => arguments.extend(["--maintenance-db".to_owned(), address.clone(), address]),
        }
        (installed(name), arguments)
    }

    fn create(&mut self, words: &[&str]) -> (PathBuf, Vec<String>) {
        assert_eq!(words[0], "create");
        let value = |option: &str| {
            let at = words.iter().position(|word| *word == option).unwrap();
            words[at + 1]
        };
        let name = value("--name").to_owned();
        assert_eq!(value("--label"), format!("{}={name}", policy::LABEL));
        assert!(words.contains(&self.image.as_str()));
        assert!(self.container.is_none(), "one container per verification");
        let unique = name.strip_prefix("scpub-restore-").unwrap();
        let database = format!("restore_{unique}");
        sql(
            self.base,
            &format!("CREATE DATABASE {database} OWNER postgres;"),
        );
        let id = sha256_hex(name.as_bytes());
        let printed = format!("{id}\n");
        self.container = Some(Container {
            name,
            id,
            database,
            removed: false,
        });
        answer(&printed)
    }

    /// The containers `ps` lists for `selectors`: any live container of the
    /// operation's label, or this one by its label and name while it is live.
    fn listed(&self, selectors: &[&str]) -> String {
        let label = format!("label={}", policy::LABEL);
        let listed = match selectors {
            ["--filter", only] if *only == label => self.live(),
            ["--no-trunc", "--filter", labelled, "--filter", named] => {
                let container = self.container.as_ref().expect("a created container");
                assert_eq!(*labelled, format!("{label}={}", container.name));
                assert_eq!(*named, format!("name=^/{}$", container.name));
                self.live()
            }
            _ => panic!("unexpected selectors {selectors:?}"),
        };
        listed.map_or_else(String::new, |container| format!("{}\n", container.id))
    }

    fn remove(&mut self, words: &[&str]) -> (PathBuf, Vec<String>) {
        let container = self.live().expect("a live container");
        assert_eq!(words, ["rm", "--force", "--volumes", container.id.as_str()]);
        let printed = format!("{}\n", container.id);
        let database = container.database.clone();
        let seen = sql(
            &self.address(),
            "SELECT to_regclass('public.users') IS NOT NULL, \
             EXISTS (SELECT FROM pg_extension WHERE extname = 'amcheck');",
        );
        self.removed = Some(Removed {
            restored: seen.starts_with('t'),
            checked: seen.trim_end().ends_with('t'),
        });
        sql(
            self.base,
            &format!("DROP DATABASE {database} WITH (FORCE);"),
        );
        self.container.as_mut().unwrap().removed = true;
        answer(&printed)
    }

    /// What the operation's request runs here: a tool on the container's
    /// database, or the daemon's printed answer.
    fn command(&mut self, role: Role, words: &[&str]) -> (PathBuf, Vec<String>) {
        let id = self.live().map(|container| container.id.clone());
        match role {
            Role::Inspect => {
                let live = "simplestchat-public-postgres-1";
                assert_eq!(words, ["inspect", "--format", "{{.Image}}", live]);
                answer(&format!("{}\n", self.image))
            }
            Role::Prior | Role::CleanupSelect | Role::CleanupConfirm => {
                let selectors = words.strip_prefix(&["ps", "--all", "--quiet"]).unwrap();
                answer(&self.listed(selectors))
            }
            Role::Create => self.create(words),
            Role::Start => {
                assert_eq!(words, ["start", id.as_deref().unwrap()]);
                answer(&format!("{}\n", id.unwrap()))
            }
            Role::Ready => {
                let check = ["/bin/sh", "-c", policy::READY_TEST];
                assert_eq!(self.inside(words, false), check);
                let address = self.address();
                (
                    installed("pg_isready"),
                    vec!["-q".into(), "-d".into(), address],
                )
            }
            Role::Sql | Role::Ledger | Role::Verify => {
                let words = self.inside(words, true);
                self.client(words, "psql")
            }
            Role::Restore => {
                let words = self.inside(words, true);
                let words = words.strip_prefix(&BOUNDED).unwrap();
                self.client(words, "pg_restore")
            }
            Role::Amcheck => {
                let words = self.inside(words, false);
                let words = words.strip_prefix(&BOUNDED).unwrap();
                self.client(words, "pg_amcheck")
            }
            Role::CleanupInspect => {
                let container = self.live().expect("a live container");
                assert_eq!((words[0], words[1]), ("inspect", "--format"));
                assert_eq!(words[3..], [container.id.as_str()]);
                let metadata = json!({"id": container.id, "name": format!("/{}", container.name),
                    "image": self.image, "owner": container.name});
                answer(&format!("{metadata}\n"))
            }
            Role::CleanupRemove => self.remove(words),
        }
    }
}

impl Executor for Daemon<'_> {
    async fn run(&mut self, owner: &mut Owner, request: Invocation<'_>) -> CommandOutput {
        self.roles.push(request.role);
        assert_eq!(request.program, self.client);
        let words: Vec<&str> = request
            .arguments
            .iter()
            .map(|w| w.to_str().unwrap())
            .collect();
        let words = words
            .strip_prefix(&DAEMON)
            .expect("the operation's own daemon");
        let (program, arguments) = self.command(request.role, words);
        let local = Invocation {
            role: request.role,
            program,
            arguments: arguments.into_iter().map(OsString::from).collect(),
            input: request.input,
            stdout: request.stdout,
            stderr: request.stderr,
            body_end: request.body_end,
            settlement_end: request.settlement_end,
        };
        let mut native = Native;
        let output = native.run(owner, local).await;
        self.statuses.push(output.status);
        output
    }
}

impl Drop for Daemon<'_> {
    fn drop(&mut self) {
        if let Some(container) = self.live() {
            let statement = format!(
                "DROP DATABASE IF EXISTS {} WITH (FORCE);",
                container.database
            );
            let _ = attempted(self.base, &statement);
        }
    }
}

/// A restore refused after it completed: the data is there, never checked.
const RESTORED_UNCHECKED: Removed = Removed {
    restored: true,
    checked: false,
};

/// Where, beneath the runner's own temporary directory, a check leaves the
/// evidence a failed run may publish.
pub(super) const EVIDENCE: &str = "simplestchat-restore-evidence.";

/// Roles whose standard error can quote archive rows: `pg_restore` echoes
/// the row it failed on and `pg_amcheck` reports what it read. Their text is
/// never printed or published, only its size and the fixed phrases it holds.
const WITHHELD: [Role; 2] = [Role::Restore, Role::Amcheck];

/// The fixed phrases a withheld error is summarized by.
const PHRASES: [&str; 14] = [
    "pg_restore: error:",
    "pg_amcheck: error:",
    "end of file",
    "unsupported version",
    "does not exist",
    "already exists",
    "permission denied",
    "could not connect",
    "No space left on device",
    "out of memory",
    "corrupt",
    "duplicate key value",
    "invalid input syntax",
    "violates",
];

/// The fixed phrases `text` contains.
fn mentions(text: &str) -> Vec<&'static str> {
    let found = PHRASES.iter().copied();
    found.filter(|phrase| text.contains(phrase)).collect()
}

/// What the engine under the operation saw.
struct Observed {
    roles: Vec<Role>,
    statuses: Vec<Option<i32>>,
    created: bool,
    removed: Option<Removed>,
    /// What a real engine held afterwards; nothing for the cluster stand-in.
    seen: Option<Seen>,
}

/// What one verification left: its verdict, its private evidence and what
/// the engine saw.
struct Verified {
    passed: bool,
    failure: Option<String>,
    cause: Option<Cause>,
    attempt: PathBuf,
    restore: Option<Value>,
    outcome: Option<Value>,
    freshness: Option<Value>,
    snapshot: bool,
    roles: Vec<Role>,
    statuses: Vec<Option<i32>>,
    created: bool,
    removed: Option<Removed>,
    seen: Option<Seen>,
    /// The prepared host, kept while its evidence is read.
    _prepared: Prepared,
}

fn json_file(path: &Path) -> Option<Value> {
    let bytes = fs::read(path).ok()?;
    Some(serde_json::from_slice(&bytes).unwrap())
}

/// Run the operation itself over the prepared host, with `executor` as its
/// container engine: whether it passed, its failure and its first cause.
pub(super) fn operate(
    access: &Access,
    executor: &mut impl Executor,
) -> (bool, Option<String>, Option<Cause>) {
    let whole = Duration::from_secs(1200);
    let reserve = Duration::from_secs(120);
    let mut owner = Owner::new(Instant::now(), whole, reserve).unwrap();
    let options = Options {
        attempt: ATTEMPT.into(),
        whole,
        reserve,
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let outcome = runtime.block_on(execution::execute(&options, access, &mut owner, executor));
    drop(runtime);
    let (passed, failure) = (outcome.passed, outcome.failure.clone());
    outcome.finish(&mut owner);
    (passed, failure, owner.finish().err())
}

impl Verified {
    /// The prepared host's evidence once the operation has reported.
    fn read(
        prepared: Prepared,
        reported: (bool, Option<String>, Option<Cause>),
        observed: Observed,
    ) -> Self {
        let state = &prepared.access.state;
        let mut attempts = fs::read_dir(state.join("restores")).unwrap();
        let attempt = attempts.next().unwrap().unwrap().path();
        assert!(attempts.next().is_none());
        let (passed, failure, cause) = reported;
        Self {
            passed,
            failure,
            cause,
            restore: json_file(&attempt.join("restore.json")),
            outcome: json_file(&attempt.join("outcome.json")),
            freshness: json_file(&state.join("restore-verified.timestamp")),
            snapshot: attempt.join("snapshot.dump").exists(),
            attempt,
            roles: observed.roles,
            statuses: observed.statuses,
            created: observed.created,
            removed: observed.removed,
            seen: observed.seen,
            _prepared: prepared,
        }
    }

    /// One command's number as its evidence names it, and its private
    /// standard error.
    fn stderr(&self, index: usize) -> (String, Vec<u8>) {
        let number = format!("{:03}", index + 1);
        let path = self.attempt.join(format!("{number}.stderr"));
        (number, fs::read(path).unwrap_or_default())
    }

    /// Every command's role and status with the tail of its private
    /// standard error, for a failure message; a withheld error appears only
    /// as its size and fixed phrases.
    fn diagnostics(&self) -> String {
        let mut text = format!("failure {:?}, cause {:?}", self.failure, self.cause);
        for (index, role) in self.roles.iter().enumerate() {
            let (number, bytes) = self.stderr(index);
            let status = self.statuses.get(index).copied().flatten();
            let told = String::from_utf8_lossy(&bytes);
            let told = if WITHHELD.contains(role) {
                format!(
                    "{} bytes withheld, mentioning {:?}",
                    bytes.len(),
                    mentions(&told)
                )
            } else {
                told[told.len().saturating_sub(1024)..].to_owned()
            };
            text.push_str(&format!("\n{number} {role:?} exit {status:?}: {told}"));
        }
        if let Some(seen) = &self.seen {
            text.push_str(&format!("\nengine: {seen:?}"));
        }
        text
    }

    /// Copy what may be published, beneath the runner's temporary directory
    /// when the runner has one: the aggregate `restore.json` and
    /// `outcome.json`, each command's role, status and sizes, the standard
    /// error of every role that cannot quote archive rows, and what a real
    /// engine reported. No standard output is copied.
    fn publish(&self, case: &str) {
        let Some(temporary) = std::env::var_os("RUNNER_TEMP") else {
            return;
        };
        let process = format!("{EVIDENCE}{}", std::process::id());
        let directory = Path::new(&temporary).join(process).join(case);
        fs::create_dir_all(&directory).unwrap();
        for name in ["restore.json", "outcome.json"] {
            if let Ok(bytes) = fs::read(self.attempt.join(name)) {
                fs::write(directory.join(name), bytes).unwrap();
            }
        }
        let mut commands = Vec::new();
        for (index, role) in self.roles.iter().enumerate() {
            let (number, bytes) = self.stderr(index);
            let withheld = WITHHELD.contains(role);
            if !withheld && !bytes.is_empty() {
                fs::write(directory.join(format!("{number}.stderr")), &bytes).unwrap();
            }
            let stdout = self.attempt.join(format!("{number}.stdout"));
            let stdout = fs::metadata(stdout).map_or(0, |metadata| metadata.len());
            commands.push(json!({"number": index + 1, "role": format!("{role:?}"),
                "status": self.statuses.get(index).copied().flatten(),
                "stdoutBytes": stdout, "stderrBytes": bytes.len(),
                "stderrPublished": !withheld,
                "mentions": mentions(&String::from_utf8_lossy(&bytes))}));
        }
        let engine = self.seen.as_ref().map(|seen| {
            json!({"inside": seen.inside.as_ref().map(|inside| json!({
                    "serverVersionNum": inside.version, "restored": inside.restored,
                    "checked": inside.checked, "image": inside.image,
                    "volumes": inside.volumes})).map_err(|_| "unobserved"),
                "labelled": seen.labelled, "volumesLeft": seen.volumes_left})
        });
        let summary = json!({"case": case, "passed": self.passed, "failure": self.failure,
            "commands": commands, "engine": engine});
        let summary = serde_json::to_vec_pretty(&summary).unwrap();
        fs::write(directory.join("commands.json"), summary).unwrap();
    }

    /// The command the operation reported as failed: its role and its
    /// private standard error.
    fn refused_command(&self) -> (Role, String) {
        let failure = self.failure.as_deref().unwrap_or_default();
        let number = failure
            .strip_prefix("Command ")
            .and_then(|rest| rest.strip_suffix(" failed; inspect private output"))
            .unwrap_or_else(|| panic!("{}", self.diagnostics()));
        let index: usize = number.parse().unwrap();
        let (_, bytes) = self.stderr(index - 1);
        (self.roles[index - 1], String::from_utf8(bytes).unwrap())
    }

    /// A refused verification of `case` publishes nothing and leaves no
    /// container.
    fn refused(&self, case: &str) {
        assert!(!self.passed, "{case} verified: {}", self.diagnostics());
        assert!(self.failure.is_some() && self.cause.is_some(), "{case}");
        assert!(self.outcome.is_none() && self.freshness.is_none(), "{case}");
        assert!(!self.snapshot, "{case}");
        if let Some(restore) = &self.restore {
            assert_eq!(restore["verified"], false, "{case}");
            assert_eq!(restore["cleanupPassed"], self.created, "{case}");
        }
        if self.created {
            assert!(self.removed.is_some(), "{case}: {}", self.diagnostics());
        }
    }

    /// Nothing the operation created in a real engine remains: no container
    /// carries its label and no volume its container mounted exists.
    fn gone(&self, case: &str) {
        let seen = self.seen.as_ref().expect("a real engine");
        assert_eq!(seen.labelled, "", "{case}: {}", self.diagnostics());
        assert!(
            seen.volumes_left.is_empty(),
            "{case}: {:?}",
            seen.volumes_left
        );
    }
}

/// Verify the prepared host's backup with the operation itself, the owned
/// cluster standing in for its container engine; `case` names its
/// published evidence.
fn verify(base: &str, prepared: Prepared, case: &str) -> Verified {
    // As in a new container: no runtime role exists until the operation
    // makes them. Only this cluster's own databases can hold their grants.
    sql(
        base,
        "DROP ROLE IF EXISTS simplestchat_app; DROP ROLE IF EXISTS simplestchat_migrate;",
    );
    let mut daemon = Daemon::new(base, &prepared.access.docker);
    let reported = operate(&prepared.access, &mut daemon);
    let observed = Observed {
        roles: std::mem::take(&mut daemon.roles),
        statuses: std::mem::take(&mut daemon.statuses),
        created: daemon.container.is_some(),
        removed: daemon.removed,
        seen: None,
    };
    drop(daemon);
    let verified = Verified::read(prepared, reported, observed);
    verified.publish(case);
    verified
}

/// The pinned production image the owning check pulled.
struct Pinned {
    reference: String,
    id: String,
    client: PathBuf,
}

/// The pinned image, when the owning check selects the real-engine mode;
/// nothing anywhere else. Once selected, a missing client or image fails.
fn pinned() -> Option<Pinned> {
    let selected = std::env::var(container_tests::SELECTOR).is_ok_and(|value| value == "1");
    if !selected {
        return None;
    }
    let client = installed("docker");
    let reference = pinned_reference();
    let inspect = [
        "image",
        "inspect",
        "--format",
        "{{.Id}}",
        reference.as_str(),
    ];
    let printed = Installed(client.clone()).docker(&inspect);
    let printed = printed.unwrap_or_else(|complaint| panic!("the pulled image: {complaint}"));
    let id = printed.trim().to_owned();
    assert!(policy::image(&id), "{id}");
    Some(Pinned {
        reference,
        id,
        client,
    })
}

/// Verify the prepared host's backup with the operation itself in a real
/// container of the pinned image: the operation's requests go unchanged to
/// the installed client but for the live image inspection, which the
/// pinned image's ID answers. The engine's guard removes the operation's
/// container only after what remained has been recorded.
fn verify_inside(pinned: &Pinned, prepared: Prepared, case: &str) -> Verified {
    let client = Installed(pinned.client.clone());
    let program = prepared.access.docker.clone();
    let mut engine = Engine::new(Native, client, &program, &pinned.id);
    let reported = operate(&prepared.access, &mut engine);
    let seen = engine.finish();
    let removed = seen.inside.as_ref().ok().map(|inside| Removed {
        restored: inside.restored,
        checked: inside.checked,
    });
    let observed = Observed {
        roles: std::mem::take(&mut engine.roles),
        statuses: std::mem::take(&mut engine.statuses),
        created: engine.created(),
        removed,
        seen: Some(seen),
    };
    drop(engine);
    let verified = Verified::read(prepared, reported, observed);
    verified.publish(case);
    verified
}

/// The counts the current backup's restore must publish.
fn current_counts() -> Value {
    json!({"users": 3, "rooms": 2, "sessions": 6, "credentials": 0,
        "activeIncidents": 1, "resolvedIncidents": 5})
}

/// From the pre-upgrade half of
/// `test_restore_preserves_incidents_privacy_and_rejects_truncated_archives`:
/// a safety backup taken before migration 022 restores and verifies against
/// its own recorded ledger, with its earlier schema and sessions, while the
/// reviewed release already packages later migrations; recorded with a
/// migration its archive lacks, it is refused once restored. A backup taken
/// right after migration 022 is refused when its session family is optional.
#[test]
#[ignore = "runs only in the cluster `database-check restore` owns"]
fn a_pre_upgrade_backup_verifies_against_its_own_recorded_ledger() {
    let Some(url) = database() else { return };
    let base = url;
    sql(&base, ROLES);
    let all = migrations();
    let packaged = ledger(&all);
    let scratch = Fixture::new();
    let mut made = Made {
        base: &base,
        names: Vec::new(),
    };
    let (_, source) = made.database(None);
    let recorded = earlier(&source, &all, SESSION_FAMILY);
    assert!(!recorded.contains_key(&SESSION_FAMILY.to_string()));
    // The archive holds the earlier schema: no chat tables, and a session
    // family that may still be missing.
    let earlier_schema = "SELECT to_regclass('public.chat_messages') IS NULL
        AND to_regclass('public.chat_inbox') IS NULL
        AND to_regclass('public.chat_read_cursors') IS NULL
        AND NOT (SELECT attnotnull FROM pg_attribute WHERE attrelid = 'public.sessions'::regclass
          AND attname = 'refresh_token_family_hash');";
    assert_eq!(sql(&source, earlier_schema), "t\n");
    let archive = dump(&source, &scratch.0.join("pre-upgrade.dump"));
    let family = SESSION_FAMILY.to_string();
    let at_family = all.iter().find(|m| m.version == SESSION_FAMILY).unwrap();
    migrate(&source, at_family);
    let optional =
        "ALTER TABLE public.sessions ALTER COLUMN refresh_token_family_hash DROP NOT NULL;";
    sql(&source, optional);
    let optional = dump(&source, &scratch.0.join("optional-family.dump"));
    drop(made);
    let digest = sha256_hex(&archive);

    let prepared = Prepared::new(&archive, &digest, &recorded, &packaged);
    let verified = verify(&base, prepared, "pre-upgrade");
    assert!(verified.passed, "{}", verified.diagnostics());
    let counts = json!({"users": 3, "rooms": 2, "sessions": 4, "credentials": 0,
        "activeIncidents": 1, "resolvedIncidents": 5});
    assert_eq!(verified.restore.as_ref().unwrap()["counts"], counts);
    assert_eq!(verified.outcome.as_ref().unwrap()["counts"], counts);
    assert!(verified.freshness.is_some());

    let mut claimed = recorded.clone();
    claimed.insert(family.clone(), packaged[&family].clone());
    let prepared = Prepared::new(&archive, &digest, &claimed, &packaged);
    let refused = verify(&base, prepared, "pre-upgrade-claiming-022");
    refused.refused("a backup recorded with migration 022");
    assert_eq!(
        refused.failure.as_deref(),
        Some("Restored migration ledger differs from release")
    );
    assert_eq!(refused.removed, Some(RESTORED_UNCHECKED));

    let digest = sha256_hex(&optional);
    let prepared = Prepared::new(&optional, &digest, &claimed, &packaged);
    let refused = verify(&base, prepared, "optional-family");
    refused.refused("an optional family after migration 022");
    let (role, stderr) = refused.refused_command();
    assert_eq!(role, Role::Verify);
    assert!(
        stderr.contains("Restored session-family requirement differs"),
        "{stderr}"
    );
    assert_eq!(refused.removed, Some(RESTORED_UNCHECKED));
}

/// What the current backup is changed by before it is taken, and what
/// `restore-verify.sql` must then refuse it for.
const DRIFT: [(&str, &str); 10] = [
    (
        "ALTER TABLE public.users DROP COLUMN recovery_key_hash;",
        "column \"recovery_key_hash\" does not exist",
    ),
    (
        "ALTER TABLE operations.alerts DROP COLUMN release_revision;",
        "column \"release_revision\" does not exist",
    ),
    (
        "ALTER TABLE public.sessions ALTER COLUMN refresh_token_family_hash DROP NOT NULL;",
        "Restored session-family requirement differs",
    ),
    (
        "ALTER TABLE public.sessions RENAME COLUMN refresh_token_family_hash \
         TO fixture_missing_family;",
        "Restored session-family requirement differs",
    ),
    (
        "ALTER TABLE operations.external_status RENAME TO fixture_missing;",
        "Incomplete external operations schema",
    ),
    (
        "GRANT SELECT ON operations.external_runs TO simplestchat_app;",
        "Restored role privacy differs",
    ),
    (
        "GRANT USAGE ON SCHEMA operations TO simplestchat_app;",
        "Restored role privacy differs",
    ),
    (
        "GRANT INSERT ON public._sqlx_migrations TO simplestchat_app;",
        "Restored role privacy differs",
    ),
    (
        "ALTER TABLE public.users ADD CONSTRAINT restore_fixture CHECK (length(email) > 0) \
         NOT VALID;",
        "Unvalidated constraints or invalid indexes",
    ),
    (
        "DROP INDEX operations.alerts_active_key;",
        "Incident uniqueness or sequence differs",
    ),
];

/// From `test_restore_preserves_incidents_privacy_and_rejects_truncated_archives`
/// and its `verify_current_session_schema` and `verify_external_schema`: a
/// complete current backup, with its open and resolved incidents, the
/// optional external schema and the private runtime grants, restores,
/// passes the reviewed checks and `pg_amcheck`, and only then publishes its
/// counts and freshness; each drift in `DRIFT` is refused by the check
/// that names it, and nothing is published.
#[test]
#[ignore = "runs only in the cluster `database-check restore` owns"]
fn a_current_backup_keeps_incidents_and_refuses_privacy_and_schema_drift() {
    let Some(url) = database() else { return };
    let base = url;
    sql(&base, ROLES);
    let all = migrations();
    let packaged = ledger(&all);
    let scratch = Fixture::new();
    let mut made = Made {
        base: &base,
        names: Vec::new(),
    };
    let (name, source) = made.database(None);
    earlier(&source, &all, SESSION_FAMILY);
    upgrade(&source, &all, SESSION_FAMILY);
    let archive = dump(&source, &scratch.0.join("current.dump"));
    portable(&base, &scratch.0.join("current.dump"));
    let mut drifted = Vec::new();
    for (index, (change, reason)) in DRIFT.iter().enumerate() {
        let (_, copy) = made.database(Some(&name));
        sql(&copy, change);
        let path = scratch.0.join(format!("drift-{index}.dump"));
        drifted.push((dump(&copy, &path), *change, *reason));
    }
    drop(made);

    let digest = sha256_hex(&archive);
    let prepared = Prepared::new(&archive, &digest, &packaged, &packaged);
    let verified = verify(&base, prepared, "current");
    assert!(verified.passed, "{}", verified.diagnostics());
    assert_eq!(verified.cause, None);
    let restore = verified.restore.clone().unwrap();
    let container = restore["container"].as_str().unwrap();
    assert_eq!(restore["containerId"], sha256_hex(container.as_bytes()));
    let image = format!("sha256:{}", sha256_hex(b"restore fixture image"));
    let mut expected = json!({"container": container, "containerId": restore["containerId"],
        "postgresImage": image, "cleanupPassed": true, "verified": true,
        "counts": current_counts()});
    assert_eq!(restore, expected);
    let outcome = verified.outcome.clone().unwrap();
    expected["backupSha256"] = json!(digest);
    expected["release"] = json!(revision());
    expected["finishedAt"] = outcome["finishedAt"].clone();
    assert_eq!(outcome, expected);
    expected["evidence"] = json!(verified.attempt.to_str().unwrap());
    assert_eq!(verified.freshness, Some(expected));
    assert!(!verified.snapshot);
    // The restored database was structurally checked and then removed.
    assert_eq!(
        verified.removed,
        Some(Removed {
            restored: true,
            checked: true
        })
    );
    let left = format!(
        "SELECT count(*) FROM pg_database WHERE datname = 'restore_{}';",
        container.strip_prefix("scpub-restore-").unwrap()
    );
    assert_eq!(sql(&base, &left), "0\n");

    for (index, (archive, change, reason)) in drifted.into_iter().enumerate() {
        let digest = sha256_hex(&archive);
        let prepared = Prepared::new(&archive, &digest, &packaged, &packaged);
        let refused = verify(&base, prepared, &format!("drift-{index}"));
        refused.refused(change);
        let (role, stderr) = refused.refused_command();
        assert_eq!(role, Role::Verify, "{change}");
        assert!(stderr.contains(reason), "{change}: {stderr}");
        assert_eq!(refused.removed, Some(RESTORED_UNCHECKED), "{change}");
    }
}

/// From the truncated half of
/// `test_restore_preserves_incidents_privacy_and_rejects_truncated_archives`:
/// an archive cut short is refused by its restore, which leaves nothing
/// restored behind it; one that differs from its recorded digest is refused
/// before any container exists.
#[test]
#[ignore = "runs only in the cluster `database-check restore` owns"]
fn a_truncated_backup_never_verifies() {
    let Some(url) = database() else { return };
    let base = url;
    sql(&base, ROLES);
    let all = migrations();
    let packaged = ledger(&all);
    let scratch = Fixture::new();
    let mut made = Made {
        base: &base,
        names: Vec::new(),
    };
    let (_, source) = made.database(None);
    earlier(&source, &all, SESSION_FAMILY);
    upgrade(&source, &all, SESSION_FAMILY);
    let archive = dump(&source, &scratch.0.join("current.dump"));
    drop(made);
    let truncated = &archive[..archive.len() - 16];

    let recorded = sha256_hex(truncated);
    let prepared = Prepared::new(truncated, &recorded, &packaged, &packaged);
    let refused = verify(&base, prepared, "truncated");
    refused.refused("a truncated archive");
    let (role, stderr) = refused.refused_command();
    assert_eq!(role, Role::Restore);
    let said = mentions(&stderr);
    assert!(said.contains(&"pg_restore: error:"), "{said:?}");
    // One transaction: the tables restored before the archive ended are gone.
    assert_eq!(
        refused.removed,
        Some(Removed {
            restored: false,
            checked: false
        })
    );

    let original = sha256_hex(&archive);
    let prepared = Prepared::new(truncated, &original, &packaged, &packaged);
    let refused = verify(&base, prepared, "altered-digest");
    refused.refused("an archive that differs from its digest");
    assert_eq!(
        refused.failure.as_deref(),
        Some("Release backup digest differs")
    );
    assert!(refused.roles.is_empty() && !refused.created);
}

/// From the container half of
/// `test_restore_preserves_incidents_privacy_and_rejects_truncated_archives`:
/// in real containers of the pinned production image, which the operation
/// creates, runs and removes through the installed client, the current
/// backup verifies on that image's own PostgreSQL and `pg_amcheck`, a
/// privacy drift is refused by its `restore-verify.sql`, and a truncated
/// archive by its `pg_restore`; nothing the operation created remains. Without
/// the owning check's container selector it returns at once.
#[test]
#[ignore = "runs only in the cluster `database-check restore` owns"]
fn a_real_container_of_the_pinned_image_verifies_and_refuses() {
    let Some(url) = database() else { return };
    let Some(pinned) = pinned() else { return };
    let base = url;
    let version = tagged_version(&pinned.reference).expect("a versioned image");
    sql(&base, ROLES);
    let all = migrations();
    let packaged = ledger(&all);
    let scratch = Fixture::new();
    let mut made = Made {
        base: &base,
        names: Vec::new(),
    };
    let (name, source) = made.database(None);
    earlier(&source, &all, SESSION_FAMILY);
    upgrade(&source, &all, SESSION_FAMILY);
    let archive = dump(&source, &scratch.0.join("current.dump"));
    let (_, copy) = made.database(Some(&name));
    sql(
        &copy,
        "GRANT USAGE ON SCHEMA operations TO simplestchat_app;",
    );
    let widened = dump(&copy, &scratch.0.join("widened.dump"));
    for archive in ["current.dump", "widened.dump"] {
        portable(&base, &scratch.0.join(archive));
    }
    drop(made);
    let inside = |verified: &Verified| {
        let seen = verified.seen.as_ref().expect("a real engine");
        let inside = seen.inside.clone();
        inside.unwrap_or_else(|why| panic!("{why}: {}", verified.diagnostics()))
    };
    let prepare = |backup: &[u8], digest: &str| {
        Prepared::engine(backup, digest, &packaged, &packaged, &pinned.client)
    };

    let digest = sha256_hex(&archive);
    let verified = verify_inside(&pinned, prepare(&archive, &digest), "inside-current");
    assert!(verified.passed, "{}", verified.diagnostics());
    assert_eq!(verified.cause, None);
    let seen = inside(&verified);
    assert_eq!(seen.version, version, "the image's own PostgreSQL");
    assert_eq!(seen.image, pinned.id, "created from the pinned image");
    assert!(seen.restored && seen.checked, "{seen:?}");
    let restore = verified.restore.clone().unwrap();
    let id = restore["containerId"].as_str().unwrap();
    assert!(policy::hex(id, 64), "{id}");
    let mut expected = json!({"container": restore["container"], "containerId": id,
        "postgresImage": pinned.id, "cleanupPassed": true, "verified": true,
        "counts": current_counts()});
    assert_eq!(restore, expected);
    let outcome = verified.outcome.clone().unwrap();
    expected["backupSha256"] = json!(digest);
    expected["release"] = json!(revision());
    expected["finishedAt"] = outcome["finishedAt"].clone();
    assert_eq!(outcome, expected);
    expected["evidence"] = json!(verified.attempt.to_str().unwrap());
    assert_eq!(verified.freshness, Some(expected));
    assert!(!verified.snapshot);
    verified.gone("the current backup");

    let digest = sha256_hex(&widened);
    let refused = verify_inside(&pinned, prepare(&widened, &digest), "inside-widened");
    refused.refused("a widened grant");
    let (role, stderr) = refused.refused_command();
    assert_eq!(role, Role::Verify);
    assert!(stderr.contains("Restored role privacy differs"), "{stderr}");
    assert_eq!(refused.removed, Some(RESTORED_UNCHECKED));
    assert_eq!(inside(&refused).version, version);
    refused.gone("a widened grant");

    let truncated = &archive[..archive.len() - 16];
    let digest = sha256_hex(truncated);
    let refused = verify_inside(&pinned, prepare(truncated, &digest), "inside-truncated");
    refused.refused("a truncated archive");
    let (role, stderr) = refused.refused_command();
    assert_eq!(role, Role::Restore);
    let said = mentions(&stderr);
    assert!(said.contains(&"pg_restore: error:"), "{said:?}");
    let restored = Removed {
        restored: false,
        checked: false,
    };
    assert_eq!(refused.removed, Some(restored));
    refused.gone("a truncated archive");
}
