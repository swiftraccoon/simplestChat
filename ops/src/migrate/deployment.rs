//! What every step of a move reads and records on one machine: the journal
//! that binds the machine to the move, the deployment's own identity, the
//! containers and units that belong to it, and the database's quiescence.
//! Nothing here builds an image, deletes data or starts the destination.

use super::request::{MAX_PRIVATE, Request, private, read_object, site, taken};
use super::snapshot::SOCKET;
use super::value::{
    blank, counts, field, holds, integer, object_of, same, says, stripped, text, trimmed,
};
use crate::backup::file_digest;
use crate::host::command::Request as Command;
use crate::host::evidence::{ENVIRONMENT, Evidence, Streams};
use crate::host::failure::{Failure, Result, require};
use crate::host::fault::{object, set, string, timestamp};
use crate::host::{Clock, Host, hexadecimal, image_id, state};
use crate::json::{InOrder, Json};
use crate::text::python_lines;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

/// The public project, whose containers a move owns.
pub(super) const PROJECT: &str = "simplestchat-public";
/// The units that write to the database or read it on a schedule.
pub(super) const DB_UNITS: [&str; 10] = [
    "simplestchat-monitoring.timer",
    "simplestchat-monitoring-external.timer",
    "simplestchat-backup.timer",
    "simplestchat-backup-upload.timer",
    "simplestchat-backup-restore.timer",
    "simplestchat-monitoring.service",
    "simplestchat-monitoring-external.service",
    "simplestchat-backup.service",
    "simplestchat-backup-upload.service",
    "simplestchat-backup-restore.service",
];
/// Every managed unit a retired source stops.
pub(super) const RETIRE_UNITS: [&str; 12] = [
    "simplestchat-monitoring.timer",
    "simplestchat-monitoring-external.timer",
    "simplestchat-backup.timer",
    "simplestchat-backup-upload.timer",
    "simplestchat-backup-restore.timer",
    "simplestchat-monitoring.service",
    "simplestchat-monitoring-external.service",
    "simplestchat-backup.service",
    "simplestchat-backup-upload.service",
    "simplestchat-backup-restore.service",
    "simplestchat-turn-certificate.timer",
    "simplestchat-turn-certificate.service",
];
/// The protected configuration a recovery is bound to, each with its mode.
const CONFIGURATION: [(&str, u32); 7] = [
    ("app.env", 0o600),
    ("compose.public.yml", 0o600),
    ("images.json", 0o600),
    ("proxy.env", 0o600),
    ("compose.base.yml", 0o644),
    ("Caddyfile", 0o644),
    ("pg_hba.conf", 0o644),
];
const CONTAINER_FORMAT: &str = concat!(
    r#"{"id":{{json .Id}},"name":{{json .Name}},"image":{{json .Image}},"#,
    r#""running":{{json .State.Running}},"oom":{{json .State.OOMKilled}},"#,
    r#""restartPolicy":{{json .HostConfig.RestartPolicy.Name}},"#,
    r#""project":{{json (index .Config.Labels "com.docker.compose.project")}},"#,
    r#""service":{{json (index .Config.Labels "com.docker.compose.service")}},"#,
    r#""configHash":{{json (index .Config.Labels "com.docker.compose.config-hash")}}}"#,
);
const IDENTITY_SQL: &str = "SELECT jsonb_build_object(
 'systemIdentifier',(pg_control_system()).system_identifier::text,
 'serverVersion',current_setting('server_version_num'),
 'emptyDatabase',NOT EXISTS(SELECT FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
   WHERE n.nspname !~ '^pg_' AND n.nspname<>'information_schema' AND c.relkind IN ('r','p','S')),
 'otherDatabases',EXISTS(SELECT FROM pg_database
   WHERE NOT datistemplate AND datname NOT IN ('postgres','simplestchat')),
 'largeObjects',(SELECT count(*) FROM pg_largeobject_metadata));";
const CLIENTS_SQL: &str = "SELECT count(*) FROM pg_stat_activity WHERE datname=current_database()
AND pid<>pg_backend_pid() AND backend_type='client backend';";
const UNIT_PROPERTIES: [&str; 3] = ["LoadState", "ActiveState", "UnitFileState"];
const ACTIVE: [&str; 3] = ["active", "activating", "reloading"];
const SETTLED: [&str; 3] = ["inactive", "failed", "deactivating"];

/// Who owns the database's files, and the most an archive of the database
/// may hold.
#[derive(Clone, Copy, Debug)]
pub(super) struct Database {
    pub owner: u32,
    pub group: u32,
    pub archive: u64,
}

/// One move on one machine.
pub(super) struct Move<'a> {
    pub host: &'a Host<'a>,
    pub request: &'a Request,
    /// The move's private directory.
    pub directory: PathBuf,
    pub runner: Evidence<'a>,
    /// Admit commands again after a termination request, so that a stopped
    /// freeze still restores the services it stopped.
    pub rearm: &'a dyn Fn(),
    pub database: Database,
}

impl Move<'_> {
    pub(super) fn run(&mut self, argv: &[&str], seconds: f64) -> Result<Vec<u8>> {
        let argv = argv.iter().map(|word| (*word).to_owned()).collect();
        self.runner.run(argv, seconds, Streams::default())
    }

    pub(super) fn docker(&mut self, arguments: &[&str], seconds: f64) -> Result<Vec<u8>> {
        self.runner.docker(arguments, seconds, Streams::default())
    }

    /// Every project command is bound to the prepared public project.
    pub(super) fn compose(&mut self, arguments: &[&str], seconds: f64) -> Result<Vec<u8>> {
        let directory = self.host.paths.config.to_str().ok_or(Failure::Value)?;
        let environment = format!("{directory}/app.env");
        let project = format!("{directory}/compose.public.yml");
        let mut words = vec![
            "compose",
            "--project-name",
            PROJECT,
            "--project-directory",
            directory,
            "--env-file",
            &environment,
            "-f",
            &project,
        ];
        words.extend(arguments);
        self.runner.docker(&words, seconds, Streams::default())
    }
}

fn one() -> Json {
    integer(1)
}

/// Whether a record is of the first schema.
pub(super) fn first(value: &Json) -> bool {
    counts(value, "schemaVersion", 1)
}

/// Whether a record names exactly this move.
pub(super) fn bound(value: &Json, request: &Request) -> bool {
    same(value.get("request"), Some(&request.json()))
}

/// The move's own journal, when it has one.
pub(super) fn journal(session: &Move<'_>) -> Result<Option<Json>> {
    let path = session.directory.join("migration.json");
    if !taken(&path) {
        return Ok(None);
    }
    let value = read_object(session.host, &path, MAX_PRIVATE)?;
    let ours = first(&value) && bound(&value, session.request);
    require(ours, "migration_journal_identity")?;
    Ok(Some(value))
}

/// Record a phase durably before anything it stands for changes: first in
/// the record every release on this machine consults, then in the move's own
/// journal with `details`.
pub(super) fn record(
    session: &Move<'_>,
    phase: &str,
    details: &Json,
    finalized: bool,
) -> Result<Json> {
    let request = session.request.json();
    let mut value = details.clone();
    set(&mut value, "schemaVersion", one());
    set(&mut value, "operation", string("server_migration"));
    set(&mut value, "request", request.clone());
    set(&mut value, "phase", string(phase));
    set(&mut value, "finalized", Json::Bool(finalized));
    set(&mut value, "updatedAt", timestamp(session.host));
    let attempt = session.directory.to_str().ok_or(Failure::Value)?;
    let shared = object(vec![
        ("schemaVersion", one()),
        ("operation", string("server_migration")),
        ("request", request),
        ("attempt", string(attempt)),
        ("phase", string(phase)),
        ("finalized", Json::Bool(finalized)),
    ]);
    let release = session.host.paths.root.join("release-state.json");
    state::atomic_json(&release, &InOrder(&shared))?;
    let own = session.directory.join("migration.json");
    state::atomic_json(&own, &InOrder(&value))?;
    Ok(value)
}

/// The selected images and the application's settings, read privately. No
/// value of either leaves the move's own records.
pub(super) fn configuration(host: &Host<'_>) -> Result<(Json, BTreeMap<String, String>)> {
    let config = &host.paths.config;
    let images = read_object(host, &config.join("images.json"), MAX_PRIVATE)?;
    let path = config.join("app.env");
    private(host, &path, false, MAX_PRIVATE)?;
    let settings = String::from_utf8(std::fs::read(&path)?).map_err(|_| Failure::Value)?;
    let mut environment = BTreeMap::new();
    for line in python_lines(&settings) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let assigned = line.split_once('=');
        let assigned = assigned.filter(|(name, _)| !environment.contains_key(*name));
        let (name, value) =
            assigned.ok_or(Failure::Refused("ambiguous_application_environment"))?;
        environment.insert(name.to_owned(), value.to_owned());
    }
    let setting = |name: &str| environment.get(name).map(String::as_str);
    let origin = setting("ALLOWED_ORIGINS").unwrap_or_default();
    site(origin)?;
    let identified =
        setting("WEBAUTHN_ORIGIN") == Some(origin) && setting("RUN_MIGRATIONS") == Some("false");
    require(identified, "invalid_runtime_identity")?;
    let selected = |name: &str| images.get(name).ok_or(Failure::Value).and_then(text);
    require(
        hexadecimal(selected("revision")?, 40),
        "invalid_image_revision",
    )?;
    require(image_id(selected("serverImage")?), "invalid_server_image")?;
    Ok((images, environment))
}

/// The digest of every mounted configuration file: recovery is bound to
/// what the containers read, not only to their labels.
pub(super) fn configuration_hashes(host: &Host<'_>) -> Result<Json> {
    let mut hashes = Vec::new();
    for (name, mode) in CONFIGURATION {
        let path = host.paths.config.join(name);
        state::protected(&path, host.owner, false, &[mode], Some(MAX_PRIVATE))?;
        hashes.push((name, Json::String(file_digest(&path)?)));
    }
    Ok(object(hashes))
}

fn utf8(bytes: Vec<u8>) -> Result<String> {
    String::from_utf8(bytes).map_err(|_| Failure::Value)
}

/// The one container that carries both labels of a project's service, or
/// nothing when `optional` and there is none.
pub(super) fn container_of(
    session: &mut Move<'_>,
    project: &str,
    service: &str,
    optional: bool,
) -> Result<Option<Json>> {
    let owner = format!("label=com.docker.compose.project={project}");
    let role = format!("label=com.docker.compose.service={service}");
    let listing = [
        "ps",
        "--all",
        "--quiet",
        "--no-trunc",
        "--filter",
        &owner,
        "--filter",
        &role,
    ];
    let listed = utf8(session.docker(&listing, 10.0)?)?;
    let identifier = stripped(&listed);
    if identifier.is_empty() && optional {
        return Ok(None);
    }
    require(hexadecimal(identifier, 64), "owned_container_count")?;
    let inspection = ["inspect", "--format", CONTAINER_FORMAT, identifier];
    let value = state::decode(&session.docker(&inspection, 10.0)?)?;
    object_of(&value)?;
    let name = format!("/{project}-{service}-1");
    let labelled = says(&value, "id", identifier)
        && says(&value, "project", project)
        && says(&value, "service", service)
        && says(&value, "name", &name);
    let owned = labelled
        && image_id(value.get("image").ok_or(Failure::Value).and_then(text)?)
        && holds(&value, "oom", false);
    require(owned, "container_ownership_mismatch")?;
    Ok(Some(value))
}

/// One container of the public project.
pub(super) fn owned_container(
    session: &mut Move<'_>,
    service: &str,
    optional: bool,
) -> Result<Option<Json>> {
    container_of(session, PROJECT, service, optional)
}

/// One bounded transaction on the database's private socket.
pub(super) fn sql(
    session: &mut Move<'_>,
    database: &str,
    statement: &str,
    readonly: bool,
) -> Result<Vec<u8>> {
    require(hexadecimal(database, 64), "invalid_database_container")?;
    let prefix = if readonly {
        "BEGIN READ ONLY;"
    } else {
        "BEGIN;"
    };
    let payload = format!(
        "{prefix} SET LOCAL statement_timeout='30s'; SET LOCAL lock_timeout='3s'; \
         {statement}\nCOMMIT;\n"
    );
    let client = [
        "exec",
        "--interactive",
        "--user",
        "999:999",
        database,
        "timeout",
        "--signal=TERM",
        "--kill-after=2s",
        "40s",
        "psql",
        "--no-psqlrc",
        "--quiet",
        "--tuples-only",
        "--no-align",
        "--set",
        "ON_ERROR_STOP=on",
        "--host",
        SOCKET,
        "--username",
        "postgres",
        "--dbname",
        "simplestchat",
    ];
    let input = Streams {
        input: Some(payload.as_bytes()),
        ..Streams::default()
    };
    session.runner.docker(&client, 45.0, input)
}

/// The cluster's identity; a cluster holding another database or large
/// objects is not one a move can account for.
pub(super) fn database_identity(session: &mut Move<'_>, database: &str) -> Result<Json> {
    let value = state::decode(&sql(session, database, IDENTITY_SQL, true)?)?;
    object_of(&value)?;
    let cluster = value
        .get("systemIdentifier")
        .ok_or(Failure::Value)
        .and_then(text)?;
    let numbered = !cluster.is_empty() && cluster.bytes().all(|byte| byte.is_ascii_digit());
    require(numbered, "database_system_identity")?;
    let alone = holds(&value, "otherDatabases", false) && counts(&value, "largeObjects", 0);
    require(alone, "unrelated_database_contents")?;
    Ok(value)
}

/// The machine itself, whatever names lead to it.
pub(super) fn machine(host: &Host<'_>) -> Result<String> {
    let recorded = utf8(std::fs::read(host.paths.anchor.join("etc/machine-id"))?)?;
    let identity = stripped(&recorded);
    require(hexadecimal(identity, 32), "invalid_machine_identity")?;
    Ok(identity.to_owned())
}

/// The deployment's identity and the state of what it owns, for a source or
/// for a destination. A source must be running whole; a destination must
/// hold an empty database and no running application.
pub(super) fn inspect(session: &mut Move<'_>, source: bool) -> Result<Json> {
    let host = session.host;
    let request = session.request;
    let (images, environment) = configuration(host)?;
    let expected = if source {
        &request.source
    } else {
        &request.destination
    };
    let configured = environment.get("ALLOWED_ORIGINS") == Some(expected);
    require(configured, "host_origin_mismatch")?;
    let revision = field(&images, "revision")?;
    if !source {
        let selected = revision.string() == Some(request.revision.as_str());
        require(selected, "target_revision_mismatch")?;
    }
    let mut containers = Vec::new();
    for service in ["postgres", "simplestchat", "caddy"] {
        containers.push((service, owned_container(session, service, !source)?));
    }
    let database = containers[0].1.clone();
    let identity = match &database {
        Some(database) if holds(database, "running", true) => {
            let identifier = text(field(database, "id")?)?.to_owned();
            Some(database_identity(session, &identifier)?)
        }
        _ => None,
    };
    let known = |name: &str| {
        identity
            .as_ref()
            .and_then(|identity| identity.get(name).cloned())
    };
    let empty = known("emptyDatabase").unwrap_or(Json::Bool(database.is_none()));
    let identifier = machine(host)?;
    let name = site(expected)?;
    let party = environment.get("WEBAUTHN_RP_ID").ok_or(Failure::Key)?;
    let listed = containers.iter();
    let listed = listed.map(|(service, value)| (*service, value.clone().unwrap_or(Json::Null)));
    let result = object(vec![
        ("machineId", Json::String(identifier)),
        ("domain", string(name)),
        ("rpId", string(party)),
        ("revision", revision.clone()),
        (
            "databaseSystemIdentifier",
            known("systemIdentifier").unwrap_or(Json::Null),
        ),
        ("emptyDatabase", empty),
        (
            "postgresVersion",
            known("serverVersion").unwrap_or(Json::Null),
        ),
        (
            "postgresSelector",
            images.get("postgresImage").cloned().unwrap_or(Json::Null),
        ),
        ("containers", object(listed.collect())),
        ("configurationSha256", configuration_hashes(host)?),
    ]);
    if source {
        let mut running = true;
        for (_, value) in &containers {
            let value = value.as_ref().ok_or(Failure::Value)?;
            running = running && holds(value, "running", true);
        }
        require(running, "source_services_not_running")?;
        let application = containers[1].1.as_ref().ok_or(Failure::Value)?;
        let selected = same(
            Some(field(application, "image")?),
            Some(field(&images, "serverImage")?),
        );
        require(selected, "source_application_image_mismatch")?;
    } else {
        require(
            holds(&result, "emptyDatabase", true),
            "target_database_not_empty",
        )?;
        let idle = |value: &Option<Json>| {
            value
                .as_ref()
                .is_none_or(|value| holds(value, "running", false))
        };
        let quiet = containers[1..].iter().all(|(_, value)| idle(value));
        require(quiet, "target_application_running")?;
    }
    Ok(result)
}

/// Whether each managed unit is installed, running and enabled. A unit the
/// machine does not have is an observation; an answer that could mean
/// anything else is a refusal.
pub(super) fn units(session: &Move<'_>, names: &[&str]) -> Result<Json> {
    let mut result = Vec::new();
    for name in names {
        let properties = format!("--property={}", UNIT_PROPERTIES.join(","));
        let argv = ["/usr/bin/systemctl", "show", name, &properties];
        let command = Command {
            environment: &ENVIRONMENT,
            ..Command::fixed(
                argv.map(str::to_owned).to_vec(),
                10.0,
                4096,
                "ambiguous_managed_unit",
            )
        };
        let (succeeded, output) = session.host.commands.attempt(&command)?;
        let output = utf8(output)?;
        let mut value: BTreeMap<&str, &str> = BTreeMap::new();
        for line in python_lines(&output) {
            let assigned = line.split_once('=');
            let assigned = assigned.filter(|(key, _)| !value.contains_key(key));
            let (key, item) = assigned.ok_or(Failure::Refused("ambiguous_managed_unit"))?;
            value.insert(key, item);
        }
        let (load, activity) = (
            value.get("LoadState").copied(),
            value.get("ActiveState").copied(),
        );
        let file = value.get("UnitFileState").copied();
        let absent = load == Some("not-found")
            && activity == Some("inactive")
            && file.is_none_or(str::is_empty);
        let state =
            activity.is_some_and(|state| ACTIVE.contains(&state) || SETTLED.contains(&state));
        let present = succeeded && load == Some("loaded") && state && file.is_some();
        let known = value.keys().all(|key| UNIT_PROPERTIES.contains(key));
        require(known && (absent || present), "ambiguous_managed_unit")?;
        let record = object(vec![
            ("loaded", Json::Bool(load == Some("loaded"))),
            (
                "active",
                Json::Bool(activity.is_some_and(|state| ACTIVE.contains(&state))),
            ),
            ("enabled", string(file.unwrap_or_default())),
        ]);
        result.push((*name, record));
    }
    Ok(object(result))
}

/// The installed units of a recorded observation, in recorded order; with
/// `active`, only those that were running.
pub(super) fn unit_names(values: &Json, active: bool) -> Result<Vec<String>> {
    let Json::Object(entries, order) = values else {
        return Err(Failure::Value);
    };
    let managed = order
        .iter()
        .all(|name| RETIRE_UNITS.contains(&name.as_str()));
    require(managed, "unowned_unit_in_journal")?;
    let mut selected = Vec::new();
    for name in order {
        let value = object_of(&entries[name])?;
        if holds(value, "loaded", true) && (!active || holds(value, "active", true)) {
            selected.push(name.clone());
        }
    }
    Ok(selected)
}

/// Stop timers and writers in flight before the database is captured.
pub(super) fn stop_units(session: &mut Move<'_>, values: &Json) -> Result<()> {
    let selected = unit_names(values, false)?;
    if selected.is_empty() {
        return Ok(());
    }
    let mut argv = vec!["/usr/bin/systemctl", "stop"];
    argv.extend(selected.iter().map(String::as_str));
    session.run(&argv, 120.0).map(|_| ())
}

/// Refuse work on the data while either public endpoint runs.
pub(super) fn stopped(session: &mut Move<'_>) -> Result<()> {
    for service in ["simplestchat", "caddy"] {
        let value = owned_container(session, service, true)?;
        let down = value.is_none_or(|value| holds(&value, "running", false));
        require(down, "public_endpoint_running")?;
    }
    Ok(())
}

/// Require that no other client is connected: a stopped application is not
/// yet a quiet database.
pub(super) fn no_clients(session: &mut Move<'_>, database: &str) -> Result<()> {
    let answer = sql(session, database, CLIENTS_SQL, true)?;
    let alone = trimmed(&answer) == b"0";
    require(alone, "database_clients_still_present")
}

/// Require the database's container to run the image the installed
/// selection pins by digest.
pub(super) fn pinned_postgres(session: &mut Move<'_>, value: &Json, selector: &str) -> Result<()> {
    let pinned = selector.rsplit_once("@sha256:");
    let pinned = pinned.is_some_and(|(name, hash)| !name.is_empty() && hexadecimal(hash, 64))
        && !selector.chars().any(blank);
    require(pinned, "postgres_not_pinned")?;
    let inspection = ["image", "inspect", "--format", "{{.Id}}", selector];
    let image = utf8(session.docker(&inspection, 10.0)?)?;
    let same = value.get("image").and_then(Json::string) == Some(stripped(&image));
    require(same, "postgres_image_mismatch")
}

/// Require readiness within `seconds`, on loopback or through the public
/// origin with its certificate verified. An answer that is not ready yet is
/// asked again; a probe that outlives its own deadline is a failure.
pub(super) fn ready(session: &mut Move<'_>, origin: Option<&str>, seconds: u64) -> Result<()> {
    let clock: &dyn Clock = session.host.clock;
    let deadline = clock.monotonic() + Duration::from_secs(seconds);
    let url = format!("{}/ready", origin.unwrap_or("http://127.0.0.1:3000"));
    let scheme = if origin.is_some() { "=https" } else { "=http" };
    let probe = [
        "/usr/bin/curl",
        "--disable",
        "--noproxy",
        "*",
        "--proto",
        scheme,
        "--fail",
        "--silent",
        "--show-error",
        "--max-time",
        "2",
        &url,
    ];
    loop {
        let answered = session.run(&probe, 5.0).and_then(|answer| {
            let value = state::decode(&answer)?;
            object_of(&value)?;
            require(says(&value, "status", "ready"), "Application is not ready")
        });
        match answered {
            Ok(()) => return Ok(()),
            Err(Failure::Refused(_) | Failure::Command | Failure::Value) => {
                let late = clock.monotonic() >= deadline;
                require(!late, "Application readiness deadline exceeded")?;
                clock.sleep(Duration::from_millis(500));
            }
            Err(failure) => return Err(failure),
        }
    }
}
