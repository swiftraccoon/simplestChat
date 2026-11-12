//! Preparation of an explicitly requested reboot of the prepared host, and
//! recovery from it. This never reboots the host. Preparation records
//! private, durable evidence and stops only the public application's three
//! existing containers. After the boot identity changed, recovery starts
//! those same containers in order; on the original boot a request can only be
//! cancelled, which stays a failed reboot. No image is built or pulled, no
//! container is created or replaced, and nothing is migrated.

mod profile;
#[cfg(test)]
mod tests;

pub(crate) use profile::image_identity;

use crate::host::command::Interruptible;
use crate::host::evidence::Evidence;
use crate::host::fault::{
    DIRECTORY, Fault, KEY, Numbered, Result, VALUE, digest, field, file, object, protected,
    require, set, string, text, timestamp, utf8,
};
use crate::host::{Clock, Host, Paths, System, boot_identity, emit, hexadecimal, image_id, state};
use crate::json::{InOrder, Json, python_dump};
use crate::release::assets::{self, Takeover, Workload};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const SERVICES: [&str; 3] = ["simplestchat", "caddy", "postgres"];
/// The protected configuration a recovery is bound to, each with its mode.
const CONFIGURATION: [(&str, u32); 7] = [
    ("compose.public.yml", 0o600),
    ("app.env", 0o600),
    ("images.json", 0o600),
    ("proxy.env", 0o600),
    ("compose.base.yml", 0o644),
    ("Caddyfile", 0o644),
    ("pg_hba.conf", 0o644),
];
/// Managed relay and monitoring containers, each with its project and service.
const COMPANIONS: [(&str, &str, &str); 3] = [
    ("simplestchat-turn-turn-1", "simplestchat-turn", "turn"),
    (
        "simplestchat-monitoring-prometheus-1",
        "simplestchat-monitoring",
        "prometheus",
    ),
    (
        "simplestchat-monitoring-node-exporter-1",
        "simplestchat-monitoring",
        "node-exporter",
    ),
];
const CONTAINER_FORMAT: &str = concat!(
    r#"{"id":{{json .Id}},"image":{{json .Image}},"state":{{json .State}},"#,
    r#""configHash":{{json (index .Config.Labels "com.docker.compose.config-hash")}}}"#,
);
/// What an image inspection must say for its identity to be judged.
pub(crate) const IMAGE_FORMAT: &str = concat!(
    r#"{"id":{{json .Id}},"os":{{json .Os}},"architecture":{{json .Architecture}},"#,
    r#""user":{{json .Config.User}},"labels":{{json .Config.Labels}},"#,
    r#""cmd":{{json .Config.Cmd}},"entrypoint":{{json (index .Config "Entrypoint")}},"#,
    r#""workingDir":{{json .Config.WorkingDir}},"#,
    r#""hasHealthcheck":{{if index .Config "Healthcheck"}}true{{else}}false{{end}},"#,
    r#""envNames":[{{range $i, $v := .Config.Env}}{{if $i}},{{end}}"#,
    r#"{{json (index (split $v "=") 0)}}{{end}}]}"#,
);
const WAIT_SUPPORT: &str =
    "Installed Compose must support start --wait and --wait-timeout before reboot preparation";
const UNFINISHED: &str = "Inspect and recover the unfinished release before starting another";
const CANCELLED: &str = "Requested reboot did not complete; original-boot services recovered";

/// What the command was asked to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Prepare,
    Resume,
    Cancel,
}

impl Action {
    fn name(self) -> &'static str {
        match self {
            Self::Prepare => "prepare",
            Self::Resume => "resume",
            Self::Cancel => "cancel",
        }
    }
}

fn one() -> Json {
    Json::Integer("1".to_owned())
}

/// What the earlier helper's patterns took for blank: Unicode white space
/// and the four information separators.
fn space(character: char) -> bool {
    character.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&character)
}

/// The commands of one evidence directory, bound to the prepared project.
struct Session<'a> {
    commands: Numbered<'a>,
    host: &'a Host<'a>,
    /// Admit commands again after a termination request, so that one
    /// request stops a preparation and still lets it recover its services.
    rearm: &'a dyn Fn(),
}

impl Session<'_> {
    fn run(&mut self, argv: &[&str], seconds: f64) -> Result<Vec<u8>> {
        self.commands.run(argv, seconds)
    }

    fn docker(&mut self, arguments: &[&str], seconds: f64) -> Result<Vec<u8>> {
        self.commands.docker(arguments, seconds)
    }

    /// Every project command is bound to the prepared public project.
    fn compose(&mut self, arguments: &[&str], seconds: f64) -> Result<Vec<u8>> {
        let directory = self.host.paths.config.to_str().ok_or(VALUE)?.to_owned();
        let environment = format!("{directory}/app.env");
        let project = format!("{directory}/compose.public.yml");
        let mut words = vec![
            "compose",
            "--project-name",
            "simplestchat-public",
            "--project-directory",
            &directory,
            "--env-file",
            &environment,
            "-f",
            &project,
        ];
        words.extend(arguments);
        self.docker(&words, seconds)
    }
}

/// The digest of every protected configuration file, in the fixed order.
fn configuration(host: &Host<'_>) -> Result<Json> {
    let mut hashes = Vec::new();
    for (name, mode) in CONFIGURATION {
        let path = host.paths.config.join(name);
        protected(host.owner, &path, file(mode, 1024 * 1024))?;
        hashes.push((name, Json::String(digest(&std::fs::read(&path)?))));
    }
    Ok(object(hashes))
}

/// The one container a listing names, with its inspection; any other
/// listing is the caller's refusal.
fn inspected(session: &mut Session<'_>, listed: &str, refusal: &str) -> Result<(String, Json)> {
    let identifier = listed.trim();
    require(hexadecimal(identifier, 64), refusal)?;
    let inspection = ["inspect", "--format", CONTAINER_FORMAT, identifier];
    let value = state::decode(&session.docker(&inspection, 10.0)?)?;
    value.object().ok_or(VALUE)?;
    Ok((identifier.to_owned(), value))
}

/// Whether an inspection is of `identifier` and names an image and a
/// configuration digest.
fn identified(value: &Json, identifier: &str) -> Result<bool> {
    Ok(field(value, "id")?.string() == Some(identifier)
        && image_id(text(field(value, "image")?)?)
        && hexadecimal(text(field(value, "configHash")?)?, 64))
}

/// Exactly one existing container of a project service, running or stopped.
fn container(session: &mut Session<'_>, service: &str) -> Result<Json> {
    let listed = utf8(session.compose(&["ps", "--all", "--quiet", service], 10.0)?)?;
    let one = format!("Exactly one existing {service} container is required");
    let (identifier, value) = inspected(session, &listed, &one)?;
    let known = identified(&value, &identifier)?;
    require(known, &format!("Invalid {service} container identity"))?;
    let state = field(&value, "state")?;
    state.object().ok_or(VALUE)?;
    let alive = state.get("OOMKilled").and_then(Json::boolean) == Some(false);
    require(alive, &format!("{service} was OOM-killed"))?;
    Ok(value)
}

/// The fields that tell a container from its replacement.
fn identity(value: &Json) -> Result<Json> {
    let kept = ["id", "image", "configHash"];
    let kept = kept.map(|name| field(value, name).map(|item| (name, item.clone())));
    let [id, image, hash] = kept;
    Ok(object(vec![id?, image?, hash?]))
}

fn flag(value: &Json, name: &str) -> Option<bool> {
    let state = value.get("state");
    state
        .and_then(|state| state.get(name))
        .and_then(Json::boolean)
}

/// The health a container's state reports, when it reports one.
fn health(state: &Json) -> Result<Option<&str>> {
    let Some(health) = state.get("Health") else {
        return Ok(None);
    };
    let health = health.object().ok_or(VALUE)?;
    Ok(health.get("Status").and_then(Json::string))
}

/// An existing managed container with its ownership labels, or nothing.
fn companion(session: &mut Session<'_>, name: &str) -> Result<Option<Json>> {
    let known = COMPANIONS.iter().find(|(known, ..)| *known == name);
    let (_, project, service) = known.ok_or(KEY)?;
    let exact = format!("name=^/{name}$");
    let owner = format!("label=com.docker.compose.project={project}");
    let role = format!("label=com.docker.compose.service={service}");
    let listing = [
        "ps",
        "--all",
        "--no-trunc",
        "--quiet",
        "--filter",
        &exact,
        "--filter",
        &owner,
        "--filter",
        &role,
    ];
    let listed = utf8(session.docker(&listing, 10.0)?)?;
    if listed.trim().is_empty() {
        return Ok(None);
    }
    let ambiguous = "Ambiguous managed companion container";
    let (identifier, value) = inspected(session, &listed, ambiguous)?;
    let known = identified(&value, &identifier)? && {
        let state = field(&value, "state")?;
        state.object().ok_or(VALUE)?;
        state.get("Running").and_then(Json::boolean).is_some()
    };
    require(known, "Invalid managed companion identity")?;
    Ok(Some(value))
}

/// Which managed companions exist, and whether each was running.
fn capture_companions(session: &mut Session<'_>) -> Result<Json> {
    let mut values = Vec::new();
    for (name, ..) in COMPANIONS {
        if let Some(value) = companion(session, name)? {
            let mut record = identity(&value)?;
            let running = field(field(&value, "state")?, "Running")?.clone();
            set(&mut record, "running", running);
            values.push((name, record));
        }
    }
    Ok(object(values))
}

/// The recorded companions in recorded order.
fn recorded_companions(saved: &Json) -> Result<Vec<(String, Json)>> {
    let Json::Object(entries, order) = field(saved, "companions")? else {
        return Err(VALUE);
    };
    let listed = |name: &String| COMPANIONS.iter().any(|(known, ..)| known == name);
    require(
        order.iter().all(listed),
        "Unexpected managed companion selection",
    )?;
    let records = order.iter();
    let records = records.filter_map(|name| Some((name.clone(), entries.get(name)?.clone())));
    Ok(records.collect())
}

/// Refuse a replaced or vanished companion before anything is started; one
/// that was absent or deliberately stopped stays so.
fn validate_companions(session: &mut Session<'_>, saved: &Json) -> Result<BTreeMap<String, Json>> {
    let mut values = BTreeMap::new();
    for (name, previous) in recorded_companions(saved)? {
        previous.object().ok_or(VALUE)?;
        let known = previous.get("running").and_then(Json::boolean).is_some();
        require(known, "Missing companion running state")?;
        let Some(current) = companion(session, &name)? else {
            let gone = "Managed companion disappeared during reboot";
            return Err(Fault::Refused(gone.to_owned()));
        };
        let same = identity(&current)?.semantic_eq(&identity(&previous)?);
        require(same, "Managed companion changed during reboot")?;
        values.insert(name, current);
    }
    Ok(values)
}

/// Start again only the unchanged companions that were running; never create
/// or replace one.
fn restore_companions(session: &mut Session<'_>, saved: &Json) -> Result<()> {
    let values = validate_companions(session, saved)?;
    for (name, previous) in recorded_companions(saved)? {
        if field(&previous, "running")?.boolean() != Some(true) {
            continue;
        }
        let value = values.get(&name).ok_or(KEY)?;
        if field(field(value, "state")?, "Running")?.boolean() != Some(true) {
            session.docker(&["start", text(field(value, "id")?)?], 30.0)?;
        }
        let resumed = match companion(session, &name)? {
            Some(current) => {
                identity(&current)?.semantic_eq(&identity(value)?)
                    && flag(&current, "Running") == Some(true)
            }
            None => false,
        };
        require(resumed, "Managed companion did not resume")?;
    }
    Ok(())
}

/// The selected application identity, verified against the local image.
fn selection(session: &mut Session<'_>) -> Result<Json> {
    let path = session.host.paths.config.join("images.json");
    let value = state::decode(&std::fs::read(path)?)?;
    value.object().ok_or(VALUE)?;
    let text_of = |name: &str| value.get(name).ok_or(VALUE).and_then(text);
    let (revision, image) = (text_of("revision")?, text_of("serverImage")?);
    let selected = hexadecimal(revision, 40) && image_id(image);
    require(selected, "Invalid deployed image selection")?;
    let inspection = ["image", "inspect", "--format", IMAGE_FORMAT, image];
    let inspected = state::decode(&session.docker(&inspection, 30.0)?)?;
    let available = profile::image_identity(&inspected, revision)? == image;
    require(available, "Selected application image is unavailable")?;
    Ok(value)
}

/// Whether the usage text of the installed project tool lists `option` on a
/// line of its own: after indentation, and before more blank space.
fn offered(usage: &str, option: &str) -> bool {
    usage.match_indices(option).any(|(at, _)| {
        let after = usage[at + option.len()..].chars().next();
        // The blank run before the option, nearest character first.
        let before = usage[..at].chars().rev();
        let indent: Vec<char> = before.take_while(|character| space(*character)).collect();
        let Some((_, earlier)) = indent.split_first() else {
            return false;
        };
        // The run must begin a line and leave at least one blank on it: it
        // reaches the start of the text, or holds a line break before its
        // last character.
        let starts = usage[..at].chars().count() == indent.len();
        after.is_some_and(space) && (starts || earlier.contains(&'\n'))
    })
}

/// Require readiness within `seconds`, on loopback or through the public
/// origin with its certificate verified.
fn ready(session: &mut Session<'_>, origin: Option<&str>, seconds: u64) -> Result<()> {
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
            value.object().ok_or(VALUE)?;
            let status = value.get("status").and_then(Json::string);
            require(status == Some("ready"), "Application is not ready")
        });
        match answered {
            Ok(()) => return Ok(()),
            Err(fault) if profile::retried(&fault) => {
                let late = clock.monotonic() >= deadline;
                require(!late, "Application readiness deadline exceeded")?;
                clock.sleep(Duration::from_millis(500));
            }
            Err(fault) => return Err(fault),
        }
    }
}

fn resolved(session: &mut Session<'_>) -> Result<Json> {
    let answer = session.compose(&["config", "--format", "json"], 10.0)?;
    let value = state::decode(&answer)?;
    value.object().ok_or(VALUE)?;
    Ok(value)
}

/// The digest the resolved project is compared by.
fn fingerprint(project: &Json) -> String {
    digest(python_dump(project, true).as_bytes())
}

/// Require each dependency's running image to be the pinned one selected.
fn dependencies(
    session: &mut Session<'_>,
    selected: &Json,
    running: &[(&str, Json)],
) -> Result<()> {
    for (service, key) in [("postgres", "postgresImage"), ("caddy", "caddyImage")] {
        let selector = selected.get(key).ok_or(VALUE).and_then(text)?;
        let pinned = selector.rsplit_once("@sha256:");
        let pinned = pinned.is_some_and(|(name, hash)| !name.is_empty() && hexadecimal(hash, 64))
            && !selector.chars().any(space);
        require(pinned, "Dependency images must be checksum-pinned")?;
        let inspection = ["image", "inspect", "--format", "{{.Id}}", selector];
        let image = utf8(session.docker(&inspection, 10.0)?)?;
        let current = running.iter().find(|(name, _)| *name == service);
        let current = field(&current.ok_or(KEY)?.1, "image")?;
        let same = current.string() == Some(image.trim());
        require(same, &format!("{service} image differs from selection"))?;
    }
    Ok(())
}

/// Require the resolved project to be the managed one and return its public
/// origin.
fn managed(host: &Host<'_>, project: &Json, image: &str) -> Result<String> {
    let services = field(project, "services")?;
    let environment = field(field(services, "simplestchat")?, "environment")?;
    environment.object().ok_or(VALUE)?;
    let disabled = field(environment, "RUN_MIGRATIONS")?.string() == Some("false");
    require(disabled, "Runtime migrations must remain disabled")?;
    let socket = host.paths.root.join("postgres-socket");
    profile::compose(project, socket.to_str().ok_or(VALUE)?, image)?;
    for service in SERVICES {
        let policy = field(field(services, service)?, "restart")?;
        let kept = policy.string() == Some("unless-stopped");
        require(
            kept,
            "Prepared reboot requires the existing unless-stopped policies",
        )?;
    }
    let origin = text(field(environment, "WEBAUTHN_ORIGIN")?)?;
    let canonical = origin.strip_prefix("https://").is_some_and(|name| {
        let plain = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
        !name.is_empty()
            && name
                .bytes()
                .all(|byte| plain(byte) || b".-".contains(&byte))
    });
    require(canonical, "Unexpected public origin")?;
    Ok(origin.to_owned())
}

/// Prove a healthy, unchanged selection before the public service goes down.
fn capture(session: &mut Session<'_>) -> Result<Json> {
    let host = session.host;
    let usage = utf8(session.compose(&["start", "--help"], 10.0)?)?;
    let waits = offered(&usage, "--wait") && offered(&usage, "--wait-timeout");
    require(waits, WAIT_SUPPORT)?;
    let hashes = configuration(host)?;
    let selected = selection(session)?;
    let mut containers = Vec::new();
    for service in SERVICES {
        containers.push((service, container(session, service)?));
    }
    for (service, value) in &containers {
        let running = flag(value, "Running") == Some(true);
        require(running, &format!("{service} must be running"))?;
        let hashed = utf8(session.compose(&["config", "--hash", service], 10.0)?)?;
        let fields: Vec<&str> = hashed.split_whitespace().collect();
        let same = fields == [*service, text(field(value, "configHash")?)?];
        require(same, "Running configuration differs from disk")?;
    }
    let selected_image = field(&selected, "serverImage")?;
    let ours = field(&containers[0].1, "image")?.semantic_eq(selected_image);
    require(ours, "Application image differs from selection")?;
    dependencies(session, &selected, &containers)?;
    let database = field(&containers[2].1, "state")?;
    database.object().ok_or(VALUE)?;
    let healthy = health(database)? == Some("healthy");
    require(healthy, "Database must be healthy")?;
    let migration = [
        "--profile",
        "maintenance",
        "ps",
        "--all",
        "--quiet",
        "migrate",
    ];
    let retained = session.compose(&migration, 10.0)?;
    let blank = retained
        .iter()
        .all(|byte| b" \t\n\r\x0b\x0c".contains(byte));
    require(blank, "Inspect retained migration container first")?;
    let project = resolved(session)?;
    let origin = managed(host, &project, text(selected_image)?)?;
    ready(session, None, 3)?;
    ready(session, Some(&origin), 3)?;
    let mut identities = Vec::new();
    for (service, value) in &containers {
        identities.push((*service, identity(value)?));
    }
    Ok(object(vec![
        ("schemaVersion", one()),
        ("bootId", Json::String(host.boot()?)),
        ("configuration", hashes),
        ("selection", selected),
        ("containers", object(identities)),
        ("companions", capture_companions(session)?),
        ("origin", Json::String(origin)),
        ("resolvedSha256", Json::String(fingerprint(&project))),
    ]))
}

/// Refuse changed files or containers instead of rebuilding or recreating
/// anything, and return the three containers as they are now.
fn validate(session: &mut Session<'_>, saved: &Json) -> Result<Vec<Json>> {
    let unchanged = configuration(session.host)?.semantic_eq(field(saved, "configuration")?);
    require(unchanged, "Configuration changed after reboot preparation")?;
    let selected = selection(session)?.semantic_eq(field(saved, "selection")?);
    require(selected, "Image selection changed after reboot preparation")?;
    let current = fingerprint(&resolved(session)?);
    let same = field(saved, "resolvedSha256")?.string() == Some(current.as_str());
    require(
        same,
        "Resolved configuration changed after reboot preparation",
    )?;
    let mut values = Vec::new();
    for service in SERVICES {
        let value = container(session, service)?;
        let recorded = field(field(saved, "containers")?, service)?;
        let kept = identity(&value)?.semantic_eq(recorded);
        require(
            kept,
            &format!("{service} container changed after preparation"),
        )?;
        values.push(value);
    }
    Ok(values)
}

/// Record that the reboot's preparation still owns the host.
fn write_state(host: &Host<'_>, attempt: &Path, saved: &Json, phase: &str) -> Result<()> {
    let evidence = digest(&std::fs::read(attempt.join("identity.json"))?);
    let record = object(vec![
        ("schemaVersion", one()),
        ("action", string("reboot")),
        ("attempt", string(attempt.to_str().ok_or(VALUE)?)),
        ("finalized", Json::Bool(false)),
        ("phase", string(phase)),
        ("bootId", field(saved, "bootId")?.clone()),
        ("identitySha256", Json::String(evidence)),
    ]);
    let path = host.paths.root.join("release-state.json");
    Ok(state::atomic_json(&path, &InOrder(&record))?)
}

/// Record a settled operation.
fn journal(host: &Host<'_>, attempt: &Path, phase: &str) -> Result<()> {
    let record = object(vec![
        ("schemaVersion", one()),
        ("attempt", string(attempt.to_str().ok_or(VALUE)?)),
        ("finalized", Json::Bool(true)),
        ("phase", string(phase)),
    ]);
    let path = host.paths.root.join("release-state.json");
    Ok(state::atomic_json(&path, &InOrder(&record))?)
}

/// Start the existing containers in dependency order, each behind a bounded
/// readiness gate.
fn start(session: &mut Session<'_>, saved: &Json) -> Result<()> {
    validate(session, saved)?;
    validate_companions(session, saved)?;
    let database = ["start", "--wait", "--wait-timeout", "180", "postgres"];
    session.compose(&database, 195.0)?;
    let database = container(session, "postgres")?;
    let healthy = flag(&database, "Running") == Some(true)
        && health(field(&database, "state")?)? == Some("healthy");
    require(healthy, "Database did not become healthy")?;
    session.compose(&["start", "simplestchat"], 30.0)?;
    ready(session, None, 45)?;
    session.compose(&["start", "caddy"], 30.0)?;
    ready(session, Some(text(field(saved, "origin")?)?), 45)?;
    let values = validate(session, saved)?;
    let running = |value: &Json| flag(value, "Running") == Some(true);
    require(
        values.iter().all(running),
        "A recovered service is not running",
    )?;
    restore_companions(session, saved)
}

/// The ordered graceful stops, with the evidence published before the
/// journal admits recovery after the boot.
fn stop(session: &mut Session<'_>, attempt: &Path, saved: &Json, report: &mut Json) -> Result<()> {
    let host = session.host;
    set(report, "phase", string("stop_for_reboot"));
    set(report, "interruptionStartedAt", timestamp(host));
    for service in SERVICES {
        let database = service == "postgres";
        let (grace, seconds) = if database { ("60", 75.0) } else { ("30", 45.0) };
        session.compose(&["stop", "--timeout", grace, service], seconds)?;
        let stopped = container(session, service)?;
        let recorded = field(field(saved, "containers")?, service)?;
        let code = field(&stopped, "state")?.get("ExitCode");
        let clean = identity(&stopped)?.semantic_eq(recorded)
            && flag(&stopped, "Running") == Some(false)
            && code.is_some_and(|code| code.semantic_eq(&Json::Integer("0".to_owned())));
        require(clean, &format!("{service} did not stop cleanly"))?;
    }
    set(report, "phase", string("await_reboot"));
    state::atomic_json(&attempt.join("outcome.json"), &InOrder(report))?;
    write_state(host, attempt, saved, "await_reboot")
}

/// Record exact identities, stop the existing services, and recover them at
/// once when a stop fails; the failed stop stays the failure.
fn prepare(session: &mut Session<'_>, report: &mut Json) -> Result<()> {
    let host = session.host;
    let attempt = session.commands.0.attempt.clone();
    let saved = capture(session)?;
    state::atomic_json(&attempt.join("identity.json"), &InOrder(&saved))?;
    set(report, "bootId", field(&saved, "bootId")?.clone());
    let revision = field(field(&saved, "selection")?, "revision")?;
    set(report, "revision", revision.clone());
    write_state(host, &attempt, &saved, "stop_for_reboot")?;
    let stopped = stop(session, &attempt, &saved, report);
    if stopped.is_err() {
        set(report, "recoveryAttempted", Json::Bool(true));
        (session.rearm)();
        let recovered = (|| {
            let same = field(&saved, "bootId")?.string() == Some(host.boot()?.as_str());
            require(same, "Boot changed during preparation")?;
            start(session, &saved)?;
            set(report, "recoveryPassed", Json::Bool(true));
            set(report, "phase", string("prepare_failed_recovered"));
            set(report, "interruptionFinishedAt", timestamp(host));
            journal(host, &attempt, "reboot_prepare_failed_recovered")
        })();
        if recovered.is_err() {
            set(report, "recoveryPassed", Json::Bool(false));
        }
    }
    stopped
}

/// The exact protected attempt the release record selects, with its recorded
/// identity and outcome.
fn pending(host: &Host<'_>, action: Action) -> Result<(PathBuf, Json, Json)> {
    let path = host.paths.root.join("release-state.json");
    protected(host.owner, &path, file(0o600, 16384))?;
    let record = state::decode(&std::fs::read(&path)?)?;
    record.object().ok_or(VALUE)?;
    let first =
        |value: &Json| matches!(value.get("schemaVersion"), Some(Json::Integer(one)) if one == "1");
    let word = |name: &str| record.get(name).and_then(Json::string);
    let prepared = first(&record)
        && record.get("finalized").and_then(Json::boolean) == Some(false)
        && word("action") == Some("reboot")
        && word("phase") == Some("await_reboot");
    require(prepared, "No unfinished prepared reboot is available")?;
    let recorded = record.get("bootId").ok_or(VALUE).and_then(text)?;
    require(boot_identity(recorded), "Invalid prepared boot identity")?;
    let changed = host.boot()? != recorded;
    require(
        changed == (action == Action::Resume),
        "Resume requires a changed boot; cancellation requires the original boot",
    )?;
    let selected = record.get("attempt").ok_or(VALUE).and_then(text)?;
    let results = host.paths.root.join("results");
    // Exactly one name below the results directory, spelled no other way.
    let name = selected.strip_prefix(results.to_str().ok_or(VALUE)?);
    let unique = name.and_then(|name| name.strip_prefix("/reboot."));
    let ours = unique.is_some_and(|unique| {
        let plain = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_';
        !unique.is_empty() && unique.bytes().all(plain)
    });
    require(ours, "Unexpected prepared attempt path")?;
    let attempt = PathBuf::from(selected);
    protected(host.owner, &attempt, DIRECTORY)?;
    let snapshot = attempt.join("identity.json");
    protected(host.owner, &snapshot, file(0o600, 65536))?;
    let bytes = std::fs::read(&snapshot)?;
    let evidence = record.get("identitySha256").ok_or(VALUE).and_then(text)?;
    let intact = hexadecimal(evidence, 64) && digest(&bytes) == evidence;
    require(intact, "Prepared identity evidence changed")?;
    let saved = state::decode(&bytes)?;
    saved.object().ok_or(VALUE)?;
    let matching = first(&saved) && saved.get("bootId").and_then(Json::string) == Some(recorded);
    require(matching, "Prepared identity does not match the journal")?;
    let outcome = attempt.join("outcome.json");
    protected(host.owner, &outcome, file(0o600, 65536))?;
    let report = state::decode(&std::fs::read(&outcome)?)?;
    report.object().ok_or(VALUE)?;
    let said = |name: &str| report.get(name).and_then(Json::string);
    let awaiting = said("action") == Some("reboot")
        && report.get("passed").and_then(Json::boolean) == Some(false)
        && said("phase") == Some("await_reboot")
        && said("bootId") == Some(recorded);
    require(awaiting, "Prepared outcome does not match the journal")?;
    Ok((attempt, saved, report))
}

/// Recover only the prepared containers; a cancellation stays a failed
/// reboot whose services were recovered.
fn recover(
    session: &mut Session<'_>,
    saved: &Json,
    report: &mut Json,
    attempt: &Path,
) -> Result<()> {
    let host = session.host;
    let resumed = attempt.join("resume") == session.commands.0.attempt;
    let phase = if resumed {
        "resume_after_reboot"
    } else {
        "cancel_before_reboot"
    };
    write_state(host, attempt, saved, phase)?;
    set(report, "phase", string(phase));
    set(report, "recoveryStartedAt", timestamp(host));
    set(report, "recoveryBootId", Json::String(host.boot()?));
    start(session, saved)?;
    set(report, "recoveryPassed", Json::Bool(true));
    set(report, "interruptionFinishedAt", timestamp(host));
    if resumed {
        set(report, "passed", Json::Bool(true));
        set(report, "phase", string("complete"));
        return journal(host, attempt, "reboot_complete");
    }
    set(report, "passed", Json::Bool(false));
    set(report, "phase", string("cancelled"));
    set(report, "failure", string(CANCELLED));
    journal(host, attempt, "reboot_cancelled")
}

/// What one transaction works on.
struct Opened {
    attempt: PathBuf,
    /// The recorded identity of a prepared reboot; nothing for a preparation.
    saved: Option<Json>,
    report: Json,
    /// Where this action's commands leave their output.
    evidence: PathBuf,
}

fn open(host: &Host<'_>, action: Action) -> Result<Opened> {
    let results = host.paths.root.join("results");
    protected(host.owner, &results, DIRECTORY)?;
    if action == Action::Prepare {
        let attempt = state::private_directory(&results, "reboot.")?;
        let report = object(vec![
            ("action", string("reboot")),
            ("startedAt", timestamp(host)),
            ("passed", Json::Bool(false)),
            ("phase", string("preflight")),
        ]);
        let evidence = attempt.clone();
        return Ok(Opened {
            attempt,
            saved: None,
            report,
            evidence,
        });
    }
    let (attempt, saved, report) = pending(host, action)?;
    // Recovery keeps its own evidence apart from the preparation's.
    let evidence = attempt.join(action.name());
    std::fs::DirBuilder::new().mode(0o700).create(&evidence)?;
    Ok(Opened {
        attempt,
        saved: Some(saved),
        report,
        evidence,
    })
}

/// What a transaction leaves the caller: the line to print, when it got far
/// enough to have an outcome, and how it ended.
type Settled = (Option<String>, Result<()>);

/// One transaction under the held workload lock.
fn transact(host: &Host<'_>, action: Action, rearm: &dyn Fn()) -> Settled {
    let opened = match open(host, action) {
        Ok(opened) => opened,
        Err(fault) => return (None, Err(fault)),
    };
    let (attempt, mut report) = (opened.attempt, opened.report);
    let mut session = Session {
        commands: Numbered(Evidence::new(host, opened.evidence)),
        host,
        rearm,
    };
    let outcome = match &opened.saved {
        None => prepare(&mut session, &mut report),
        Some(saved) => recover(&mut session, saved, &mut report, &attempt),
    };
    if let Err(fault) = &outcome {
        set(&mut report, "passed", Json::Bool(false));
        set(&mut report, "failure", Json::String(fault.text()));
    }
    set(&mut report, "updatedAt", timestamp(host));
    let written = state::atomic_json(&attempt.join("outcome.json"), &InOrder(&report));
    if let Err(failure) = written {
        return (None, Err(Fault::from(failure)));
    }
    let said = |name: &str| report.get(name).cloned().unwrap_or(Json::Null);
    let line = object(vec![
        ("action", string(action.name())),
        ("phase", said("phase")),
        ("passed", said("passed")),
        ("evidence", string(attempt.to_str().unwrap_or_default())),
    ]);
    (Some(format!("{}\n", python_dump(&line, false))), outcome)
}

/// Run one action: admit the caller, take the workload lock through `lock`
/// with the takeover the action is entitled to, and release it whatever
/// happens under it.
fn execute(
    host: &Host<'_>,
    action: Action,
    privileged: bool,
    rearm: &dyn Fn(),
    lock: impl FnOnce(Takeover<'_>) -> std::result::Result<Workload, assets::Failure>,
) -> Settled {
    if !privileged {
        let refusal = "Run as root on the prepared public host";
        return (None, Err(Fault::Refused(refusal.to_owned())));
    }
    let current = match action {
        Action::Prepare => Ok(String::new()),
        Action::Resume | Action::Cancel => host.boot().map_err(Fault::from),
    };
    let current = match current {
        Ok(current) => current,
        Err(fault) => return (None, Err(fault)),
    };
    let takeover = match action {
        Action::Prepare => Takeover::RebootPrepare,
        Action::Resume => Takeover::RebootResume { boot: &current },
        Action::Cancel => Takeover::RebootCancel { boot: &current },
    };
    let workload = match lock(takeover) {
        Ok(workload) => workload,
        Err(failure) if failure.busy() => return (None, Err(Fault::Kind("BlockingIOError"))),
        Err(_) => return (None, Err(Fault::Refused(UNFINISHED.to_owned()))),
    };
    let (line, outcome) = transact(host, action, rearm);
    let released = workload.finish().map_err(|_| Fault::Kind("OSError"));
    (line, outcome.and(released))
}

/// The one action named, or nothing for any other command line.
fn parse(arguments: &[OsString]) -> Option<Action> {
    let [action] = arguments else {
        return None;
    };
    let known = [Action::Prepare, Action::Resume, Action::Cancel];
    let named = |known: &Action| action.to_str() == Some(known.name());
    known.into_iter().find(named)
}

/// The one line a failed action leaves the caller: the kind of fault and
/// where to look, never private detail.
fn complaint(fault: &Fault) -> String {
    let kind = fault.kind();
    format!("Reboot operation failed ({kind}); inspect retained private evidence.\n")
}

/// Run `reboot prepare|resume|cancel` on the prepared host. A syntax error
/// returns 2 before any effect; the outcome line is printed whenever the
/// transaction got far enough to have one, also when it failed.
pub fn run(arguments: &[OsString]) -> ExitCode {
    let Some(action) = parse(arguments) else {
        return ExitCode::from(2);
    };
    rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o077));
    let stop = Arc::new(AtomicBool::new(false));
    let handled = [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT]
        .iter()
        .all(|signal| signal_hook::flag::register(*signal, Arc::clone(&stop)).is_ok());
    let commands = Interruptible {
        stop: &stop,
        refusal: "Reboot operation interrupted",
    };
    let clock = System::new();
    let host = Host {
        commands: &commands,
        clock: &clock,
        paths: Paths::fixed(),
        owner: 0,
    };
    let privileged = rustix::process::geteuid().is_root();
    let rearm = || stop.store(false, Ordering::SeqCst);
    let (line, outcome) = if handled {
        execute(&host, action, privileged, &rearm, Workload::acquire_for)
    } else {
        (None, Err(Fault::Kind("OSError")))
    };
    let printed = line.is_none_or(|line| emit(std::io::stdout(), &line));
    let outcome = if printed {
        outcome
    } else {
        Err(Fault::Kind("OSError"))
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(fault) => {
            let _ = emit(std::io::stderr(), &complaint(&fault));
            ExitCode::FAILURE
        }
    }
}
