//! The source machine's steps of a move. A freeze stops writers, keeps a
//! verified archive of the database and restores the services itself when
//! its preparation fails. A recovery starts only the recorded containers
//! and units again. Once the source is sealed its services are never started
//! by a move again: sealing and retiring only stop, disable and record.

use super::deployment::{
    DB_UNITS, Move, RETIRE_UNITS, bound, configuration, configuration_hashes, container_of, first,
    inspect, journal, no_clients, owned_container, pinned_postgres, ready, record, stop_units,
    stopped, unit_names, units,
};
use super::request::{MAX_PRIVATE, private, read_object};
use super::snapshot;
use super::target::verified_archive;
use super::value::{field, holds, integer, object_of, same, says, text, words};
use crate::backup::{dump_database, file_digest, headroom, publish};
use crate::host::failure::{Failure, Result, require};
use crate::host::fault::{object, set, string, timestamp};
use crate::host::state;
use crate::json::{InOrder, Json};

/// The most a move's archive of the database may hold.
pub(super) const MAX_DUMP: u64 = 512 * 1024 * 1024;
const SERVICES: [&str; 3] = ["postgres", "simplestchat", "caddy"];
/// The phases a source can still be recovered from.
const RECOVERABLE: [&str; 4] = ["freezing", "freeze-failed", "frozen", "source-recovered"];
/// Managed companions of a retired source, each with its project.
const COMPANIONS: [(&str, &str); 3] = [
    ("simplestchat-turn", "turn"),
    ("simplestchat-monitoring", "prometheus"),
    ("simplestchat-monitoring", "node-exporter"),
];

fn phase(record: &Json) -> Option<&str> {
    record.get("phase").and_then(Json::string)
}

/// Start again exactly the services and units a freeze recorded. A sealed
/// source is refused for good; a source already recovered is only proven
/// still running and ready.
pub(super) fn recover(session: &mut Move<'_>, prior: &Json) -> Result<Json> {
    let host = session.host;
    let request = session.request;
    let recoverable =
        |record: &Json| phase(record).is_some_and(|phase| RECOVERABLE.contains(&phase));
    require(recoverable(prior), "source_recovery_forbidden")?;
    let release = host.paths.root.join("release-state.json");
    let shared = read_object(host, &release, MAX_PRIVATE)?;
    let ours = first(&shared)
        && says(&shared, "operation", "server_migration")
        && bound(&shared, request)
        && recoverable(&shared);
    require(ours, "source_recovery_forbidden")?;
    let (images, environment) = configuration(host)?;
    let before = object_of(field(prior, "before")?)?;
    let origin = environment.get("ALLOWED_ORIGINS").ok_or(Failure::Key)?;
    let identified = *origin == request.source
        && same(
            Some(field(&images, "revision")?),
            Some(field(before, "revision")?),
        );
    require(identified, "source_recovery_identity")?;
    let unchanged = configuration_hashes(host)?.semantic_eq(field(before, "configurationSha256")?);
    require(unchanged, "source_configuration_changed")?;
    let recorded = object_of(field(before, "containers")?)?;
    let recovered = phase(prior) == Some("source-recovered");
    for service in SERVICES {
        let actual = owned_container(session, service, false)?.ok_or(Failure::Value)?;
        let expected = object_of(field(recorded, service)?)?;
        let kept = ["id", "image", "configHash"];
        let kept = kept
            .iter()
            .all(|key| same(actual.get(key), expected.get(key)));
        require(kept, "source_container_replaced")?;
        let hashed = session.compose(&["config", "--hash", service], 15.0)?;
        let hashed = String::from_utf8(hashed).map_err(|_| Failure::Value)?;
        let resolved = words(&hashed);
        let recorded = field(expected, "configHash")?.string();
        let current =
            resolved.len() == 2 && resolved[0] == service && Some(resolved[1]) == recorded;
        require(current, "source_configuration_changed")?;
        if recovered {
            let running = holds(&actual, "running", true);
            require(running, "recovered_source_no_longer_running")?;
        }
    }
    if recovered {
        ready(session, None, 30)?;
        ready(session, Some(&request.source), 30)?;
        return Ok(prior.clone());
    }
    let waited = ["start", "--wait", "--wait-timeout", "120"];
    session.compose(&[waited.as_slice(), &["postgres"]].concat(), 130.0)?;
    session.compose(&[waited.as_slice(), &["simplestchat"]].concat(), 130.0)?;
    ready(session, None, 30)?;
    session.compose(&["start", "caddy"], 30.0)?;
    ready(session, Some(&request.source), 30)?;
    let active = unit_names(object_of(field(prior, "units")?)?, true)?;
    if !active.is_empty() {
        let mut argv = vec!["/usr/bin/systemctl", "start"];
        argv.extend(active.iter().map(String::as_str));
        session.run(&argv, 120.0)?;
    }
    record(session, "source-recovered", prior, true)
}

/// Stop the writers, keep a snapshot and an archive of the quiet database
/// and prove that nothing changed while the archive was written.
fn capture(session: &mut Move<'_>, details: &Json, before: &Json, database: &Json) -> Result<Json> {
    let host = session.host;
    let directory = session.directory.clone();
    stop_units(session, field(details, "units")?)?;
    session.compose(&["stop", "--timeout", "30", "simplestchat"], 45.0)?;
    session.compose(&["stop", "--timeout", "3", "caddy"], 15.0)?;
    stopped(session)?;
    let identifier = text(field(database, "id")?)?.to_owned();
    no_clients(session, &identifier)?;
    let taken = snapshot::collect(host, &identifier)?;
    let described = directory.join("database-before.json");
    state::atomic_json(&described, &InOrder(&taken))?;
    let (partial, dump) = (
        directory.join("database.dump.partial"),
        directory.join("database.dump"),
    );
    require(
        !partial.exists() && !dump.exists(),
        "migration_archive_exists",
    )?;
    headroom(&mut session.runner, &identifier)?;
    dump_database(&mut session.runner, &identifier, &partial)?;
    let size = std::fs::metadata(&partial)?.len();
    let bounded = (1..=session.database.archive).contains(&size);
    require(bounded, "migration_archive_size")?;
    publish(&partial, &dump)?;
    no_clients(session, &identifier)?;
    let unchanged = snapshot::collect(host, &identifier)?.semantic_eq(&taken);
    require(unchanged, "source_changed_during_dump")?;
    let secrets = host.paths.config.join("secrets.json");
    private(host, &secrets, false, MAX_PRIVATE)?;
    let size = i64::try_from(std::fs::metadata(&dump)?.len()).map_err(|_| Failure::Value)?;
    let archive = object(vec![
        ("schemaVersion", integer(1)),
        ("request", session.request.json()),
        ("source", before.clone()),
        ("bytes", integer(size)),
        ("sha256", string(&file_digest(&dump)?)),
        ("snapshotSha256", string(&file_digest(&described)?)),
        ("secretsSha256", string(&file_digest(&secrets)?)),
        ("postgresImage", field(database, "image")?.clone()),
        (
            "postgresSelector",
            field(before, "postgresSelector")?.clone(),
        ),
        (
            "systemIdentifier",
            field(&taken, "systemIdentifier")?.clone(),
        ),
        ("completedAt", timestamp(host)),
    ]);
    state::atomic_json(&directory.join("archive.json"), &InOrder(&archive))?;
    let mut frozen = details.clone();
    set(&mut frozen, "archive", archive);
    record(session, "frozen", &frozen, false)
}

/// Freeze the source: record what runs, stop it, and keep a verified
/// archive. A preparation that fails restores the recorded services before
/// its own failure is reported; a restoration that fails too is recorded.
pub(super) fn freeze(session: &mut Move<'_>) -> Result<Json> {
    require(journal(session)?.is_none(), "migration_already_started")?;
    let before = inspect(session, true)?;
    let database = object_of(field(
        object_of(field(&before, "containers")?)?,
        "postgres",
    )?)?;
    let database = database.clone();
    let selector = text(field(&before, "postgresSelector")?)?.to_owned();
    pinned_postgres(session, &database, &selector)?;
    let writers = units(session, &DB_UNITS)?;
    let details = object(vec![
        ("role", string("source")),
        ("before", before.clone()),
        ("units", writers),
    ]);
    record(session, "freezing", &details, false)?;
    let frozen = capture(session, &details, &before, &database);
    if frozen.is_err() {
        // One termination request stops the freeze and still lets it put
        // the source back.
        (session.rearm)();
        let failed = record(session, "freeze-failed", &details, false)?;
        if recover(session, &failed).is_err() {
            let _ = record(session, "freeze-failed", &details, false);
        }
    }
    frozen
}

/// Disable the managed timers and every restart policy, then stop the three
/// containers and prove that nothing of the source can write again.
fn stop_writers(session: &mut Move<'_>) -> Result<()> {
    let selected = units(session, &RETIRE_UNITS)?;
    stop_units(session, &selected)?;
    let installed = unit_names(&selected, false)?;
    let timers: Vec<&str> = installed.iter().map(String::as_str).collect();
    let timers: Vec<&str> = timers
        .into_iter()
        .filter(|name| name.ends_with(".timer"))
        .collect();
    if !timers.is_empty() {
        session.run(
            &[["/usr/bin/systemctl", "disable"].as_slice(), &timers].concat(),
            30.0,
        )?;
    }
    for service in ["simplestchat", "caddy", "postgres"] {
        let value = owned_container(session, service, false)?.ok_or(Failure::Value)?;
        let identifier = text(field(&value, "id")?)?.to_owned();
        session.docker(&["update", "--restart=no", &identifier], 15.0)?;
        let grace = if service == "caddy" { "3" } else { "30" };
        session.docker(&["stop", "--time", grace, &identifier], 45.0)?;
        let actual = owned_container(session, service, false)?.ok_or(Failure::Value)?;
        let disabled = says(&actual, "id", &identifier)
            && holds(&actual, "running", false)
            && says(&actual, "restartPolicy", "no");
        require(disabled, "source_writer_not_disabled")?;
    }
    let after = units(session, &RETIRE_UNITS)?;
    for name in unit_names(&after, false)? {
        let value = object_of(field(&after, &name)?)?;
        require(holds(value, "active", false), "source_unit_still_active")?;
        if name.ends_with(".timer") {
            require(
                says(value, "enabled", "disabled"),
                "source_timer_still_enabled",
            )?;
        }
    }
    Ok(())
}

/// Seal the frozen source. The seal is recorded before anything is stopped
/// for good: from that record on, no failure can lead to a recovery.
pub(super) fn seal(session: &mut Move<'_>, prior: &Json) -> Result<Json> {
    let sealable = ["frozen", "sealing", "cutover-sealed"];
    let admitted = phase(prior).is_some_and(|phase| sealable.contains(&phase));
    require(admitted, "source_not_frozen")?;
    let mut prior = prior.clone();
    if phase(&prior) == Some("frozen") {
        stopped(session)?;
        let (_, taken) = verified_archive(session)?;
        let database = owned_container(session, "postgres", false)?.ok_or(Failure::Value)?;
        let identifier = text(field(&database, "id")?)?.to_owned();
        no_clients(session, &identifier)?;
        let unchanged = snapshot::collect(session.host, &identifier)?.semantic_eq(&taken);
        require(unchanged, "frozen_source_changed")?;
        prior = record(session, "sealing", &prior, false)?;
    }
    stop_writers(session)?;
    record(session, "cutover-sealed", &prior, false)
}

/// Retire the sealed source: its own services and companions stay stopped
/// and disabled, and every volume is kept.
pub(super) fn retire(session: &mut Move<'_>, prior: &Json) -> Result<Json> {
    let retirable = ["sealing", "cutover-sealed", "retiring", "retired"];
    let admitted = phase(prior).is_some_and(|phase| retirable.contains(&phase));
    require(admitted, "source_not_sealed")?;
    record(session, "retiring", prior, false)?;
    stop_writers(session)?;
    for (project, service) in COMPANIONS {
        if let Some(companion) = container_of(session, project, service, true)? {
            let identifier = text(field(&companion, "id")?)?.to_owned();
            session.docker(&["stop", "--time", "10", &identifier], 20.0)?;
            session.docker(&["update", "--restart=no", &identifier], 15.0)?;
        }
    }
    record(session, "retired", prior, false)
}
