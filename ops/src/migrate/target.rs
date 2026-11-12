//! The destination machine's steps of a move. A restore fills an empty
//! database from the verified archive, proves it equal to the source's
//! snapshot and renames only the managed owner when the site's name changes.
//! A verification proves that restored state unchanged before the cutover,
//! and an abort keeps a failed copy inside the move's private directory.
//! No step starts the destination's application.

use super::deployment::{
    Move, bound, configuration, configuration_hashes, database_identity, inspect, journal,
    no_clients, owned_container, pinned_postgres, record, sql, stopped,
};
use super::request::{MAX_PRIVATE, private, read_object, site, taken};
use super::snapshot::{self, SOCKET};
use super::value::{counts, field, holds, object_of, same, says, text};
use crate::backup::file_digest;
use crate::host::evidence::Streams;
use crate::host::failure::{Failure, Result, require};
use crate::host::fault::{Fault, object, set, string};
use crate::host::{hexadecimal, state};
use crate::json::{InOrder, Json};
use crate::reboot::{IMAGE_FORMAT, image_identity};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::Path;

fn phase(record: &Json) -> Option<&str> {
    record.get("phase").and_then(Json::string)
}

/// The archive and the snapshot it was taken with, proven to be the exact
/// private bytes the source recorded, beside the same secrets.
pub(super) fn verified_archive(session: &Move<'_>) -> Result<(Json, Json)> {
    let host = session.host;
    let directory = &session.directory;
    let archive = read_object(host, &directory.join("archive.json"), MAX_PRIVATE)?;
    let ours = counts(&archive, "schemaVersion", 1) && bound(&archive, session.request);
    require(ours, "archive_identity")?;
    let dump = directory.join("database.dump");
    private(host, &dump, false, session.database.archive)?;
    let size = std::fs::metadata(&dump)?.len();
    let intact = size > 0 && {
        let recorded = field(&archive, "bytes")?;
        let recorded = match recorded {
            Json::Integer(_) => recorded.integer(),
            _ => return Err(Failure::Value),
        };
        recorded == i64::try_from(size).ok()
            && archive.get("sha256").and_then(Json::string) == Some(file_digest(&dump)?.as_str())
    };
    require(intact, "archive_digest_mismatch")?;
    let described = directory.join("database-before.json");
    let taken = read_object(host, &described, MAX_PRIVATE)?;
    let recorded = archive.get("snapshotSha256").and_then(Json::string);
    let matching = recorded == Some(file_digest(&described)?.as_str())
        && holds(&taken, "passed", true)
        && same(
            taken.get("systemIdentifier"),
            archive.get("systemIdentifier"),
        );
    require(matching, "snapshot_digest_mismatch")?;
    let carried = host.paths.config.join("secrets.json");
    private(host, &carried, false, MAX_PRIVATE)?;
    let recorded = archive.get("secretsSha256").and_then(Json::string);
    let shared = recorded == Some(file_digest(&carried)?.as_str());
    require(shared, "destination_secrets_differ")?;
    Ok((archive, taken))
}

/// One statement that renames the managed owner inside the given cluster
/// only, and fails unless every other field of that account is unchanged.
pub(super) fn owner_sql(source: &str, destination: &str, cluster: &str) -> Result<String> {
    let (old, new) = (site(source)?, site(destination)?);
    let numbered = !cluster.is_empty() && cluster.bytes().all(|byte| byte.is_ascii_digit());
    require(numbered, "invalid_target_system_identity")?;
    // A canonical origin holds no quote, backslash or statement syntax.
    Ok(format!(
        "DO $$ DECLARE original public.users%ROWTYPE; affected bigint; BEGIN
IF current_database()<>'simplestchat' OR current_user<>'postgres'
 OR (pg_control_system()).system_identifier::text<>'{cluster}' THEN
 RAISE EXCEPTION 'Destination database identity mismatch'; END IF;
SELECT * INTO STRICT original FROM public.users WHERE email='owner@{old}' FOR UPDATE;
IF EXISTS(SELECT FROM public.users WHERE email='owner@{new}')
 OR NOT EXISTS(SELECT FROM public.rooms WHERE id='lobby' AND owner_id=original.id) THEN
 RAISE EXCEPTION 'Managed owner precondition failed'; END IF;
UPDATE public.users SET email='owner@{new}' WHERE id=original.id AND email='owner@{old}';
GET DIAGNOSTICS affected=ROW_COUNT;
IF affected<>1 OR (SELECT to_jsonb(u)-'email' FROM public.users u WHERE id=original.id)
 IS DISTINCT FROM (to_jsonb(original)-'email') THEN
 RAISE EXCEPTION 'Managed owner preservation failed'; END IF;
END $$;"
    ))
}

/// Whether two secrets are equal, without telling where they differ.
fn equal(one: &str, other: &str) -> bool {
    let (one, other) = (one.as_bytes(), other.as_bytes());
    let mut difference = u8::from(one.len() != other.len());
    for (index, byte) in one.iter().enumerate() {
        difference |= byte ^ other.get(index).copied().unwrap_or_default();
    }
    difference == 0
}

/// Keep the seeding credential in step with the owner the destination now
/// has. An existing credential is never replaced, only confirmed.
fn retain_owner(session: &Move<'_>) -> Result<()> {
    let host = session.host;
    let config = &host.paths.config;
    let seeding = read_object(host, &config.join("secrets.json"), MAX_PRIVATE)?;
    let password = text(field(&seeding, "owner_password")?)?;
    require(hexadecimal(password, 64), "invalid_owner_secret")?;
    let address = format!("owner@{}", site(&session.request.destination)?);
    let path = config.join("owner.json");
    if !taken(&path) {
        let owner = object(vec![
            ("email", string(&address)),
            ("password", string(password)),
        ]);
        return state::atomic_json(&path, &InOrder(&owner));
    }
    let current = read_object(host, &path, MAX_PRIVATE)?;
    let fields = current.object().ok_or(Failure::Value)?;
    let exact =
        fields.len() == 2 && fields.contains_key("email") && fields.contains_key("password");
    let kept = exact
        && says(&current, "email", &address)
        && equal(text(field(&current, "password")?)?, password);
    require(kept, "destination_owner_file_conflict")
}

/// The restored snapshot with the one difference a renamed owner makes:
/// the digest of the users table, computed from the stream itself.
fn renamed_snapshot(session: &Move<'_>, database: &str, restored: &Json) -> Result<Json> {
    let request = session.request;
    let mut result = restored.clone();
    let mut matches = 0;
    let Json::Object(entries, _) = &mut result else {
        return Err(Failure::Value);
    };
    let Some(Json::Array(tables)) = entries.get_mut("tables") else {
        return Err(if entries.contains_key("tables") {
            Failure::Value
        } else {
            Failure::Key
        });
    };
    for table in tables {
        object_of(table)?;
        if says(table, "schema", "public") && says(table, "name", "users") {
            matches += 1;
            let old = format!("owner@{}", site(&request.source)?);
            let new = format!("owner@{}", site(&request.destination)?);
            let digest = snapshot::renamed_users(session.host, database, &old, &new)?;
            if let Json::Object(fields, order) = &digest {
                for name in order {
                    set(table, name, fields[name].clone());
                }
            }
        }
    }
    require(matches == 1, "users_snapshot_missing")?;
    Ok(result)
}

/// Fill the destination's empty database from the archive and prove the
/// result, before and after the owner's rename.
pub(super) fn restore(session: &mut Move<'_>) -> Result<Json> {
    let host = session.host;
    let request = session.request;
    let directory = session.directory.clone();
    require(journal(session)?.is_none(), "migration_already_started")?;
    let (archive, taken) = verified_archive(session)?;
    let before = inspect(session, false)?;
    let source = object_of(field(&archive, "source")?)?;
    let other = !same(
        Some(field(&before, "machineId")?),
        Some(field(source, "machineId")?),
    );
    require(other, "identical_migration_hosts")?;
    let party = same(Some(field(&before, "rpId")?), Some(field(source, "rpId")?));
    require(party, "passkey_rp_id_changed")?;
    let details = object(vec![
        ("role", string("destination")),
        ("before", before.clone()),
        ("archive", archive.clone()),
    ]);
    record(session, "restoring", &details, false)?;
    let start = [
        "up",
        "--detach",
        "--no-build",
        "--pull",
        "never",
        "--wait",
        "--wait-timeout",
        "120",
        "postgres",
    ];
    session.compose(&start, 135.0)?;
    let database = owned_container(session, "postgres", false)?.ok_or(Failure::Value)?;
    let identifier = text(field(&database, "id")?)?.to_owned();
    let identity = database_identity(session, &identifier)?;
    let empty = matches!(field(&identity, "emptyDatabase")?, Json::Bool(true));
    require(empty, "target_database_not_empty")?;
    let cluster = field(&identity, "systemIdentifier")?;
    let another = !same(Some(cluster), Some(field(&archive, "systemIdentifier")?));
    require(another, "same_database_cluster")?;
    let version = same(
        Some(field(&identity, "serverVersion")?),
        Some(field(&taken, "postgresVersion")?),
    );
    require(version, "postgres_version_mismatch")?;
    let selector = field(&archive, "postgresSelector")?;
    let image = same(Some(field(&before, "postgresSelector")?), Some(selector))
        && same(
            Some(field(&database, "image")?),
            Some(field(&archive, "postgresImage")?),
        );
    require(image, "postgres_image_mismatch")?;
    pinned_postgres(session, &database, text(selector)?)?;
    stopped(session)?;
    no_clients(session, &identifier)?;
    let restoring = [
        "exec",
        "--interactive",
        "--user",
        "999:999",
        &identifier,
        "timeout",
        "--signal=TERM",
        "--kill-after=2s",
        "120s",
        "pg_restore",
        "--exit-on-error",
        "--single-transaction",
        "--host",
        SOCKET,
        "--username",
        "postgres",
        "--dbname",
        "simplestchat",
    ];
    let dump = directory.join("database.dump");
    let source = Streams {
        source: Some(&dump),
        ..Streams::default()
    };
    session.runner.docker(&restoring, 130.0, source)?;
    let restored = snapshot::collect(host, &identifier)?;
    state::atomic_json(
        &directory.join("database-restored.json"),
        &InOrder(&restored),
    )?;
    snapshot::equivalent(&taken, &restored)?;
    let renamed = request.source != request.destination;
    let mut expected = restored;
    if renamed {
        expected = renamed_snapshot(session, &identifier, &expected)?;
        let statement = owner_sql(&request.source, &request.destination, text(cluster)?)?;
        sql(session, &identifier, &statement, false)?;
    }
    retain_owner(session)?;
    stopped(session)?;
    let after = snapshot::collect(host, &identifier)?;
    require(
        after.semantic_eq(&expected),
        "unexpected_owner_rename_changes",
    )?;
    let described = directory.join("database-renamed.json");
    state::atomic_json(&described, &InOrder(&after))?;
    let mut finished = details;
    set(&mut finished, "databaseSystemIdentifier", cluster.clone());
    set(
        &mut finished,
        "databaseEquivalentBeforeOwnerRename",
        Json::Bool(true),
    );
    set(&mut finished, "ownerEmailChanged", Json::Bool(renamed));
    set(&mut finished, "ownerFieldsPreserved", Json::Bool(true));
    set(
        &mut finished,
        "renamedSnapshotSha256",
        string(&file_digest(&described)?),
    );
    record(session, "restored", &finished, true)
}

/// Prove, before the cutover, that the destination is still exactly what
/// the restore left: the same release, a stopped application and unchanged
/// data. This records nothing.
pub(super) fn verify(session: &mut Move<'_>, prior: &Json) -> Result<Json> {
    let host = session.host;
    let request = session.request;
    let restored = says(prior, "role", "destination") && phase(prior) == Some("restored");
    require(restored, "destination_not_restored")?;
    let (archive, _) = verified_archive(session)?;
    let (images, environment) = configuration(host)?;
    let source = field(&archive, "source");
    let identified = same(
        Some(field(&images, "revision")?),
        Some(&string(&request.revision)),
    ) && environment.get("ALLOWED_ORIGINS") == Some(&request.destination)
        && {
            let party = environment.get("WEBAUTHN_RP_ID").ok_or(Failure::Key)?;
            field(object_of(source?)?, "rpId")?.string() == Some(party.as_str())
        };
    require(identified, "destination_identity_changed")?;
    let selected = text(field(&images, "serverImage")?)?;
    let inspection = ["image", "inspect", "--format", IMAGE_FORMAT, selected];
    let inspected = state::decode(&session.docker(&inspection, 30.0)?)?;
    // Which review the image failed stays with the command's kept output.
    let reviewed = image_identity(&inspected, &request.revision);
    let reviewed = reviewed.map_err(|fault| match fault {
        Fault::Kind("KeyError") => Failure::Key,
        Fault::Kind("ValueError") => Failure::Value,
        _ => Failure::Refused("destination_image_unreviewed"),
    })?;
    require(reviewed == selected, "destination_image_revision_mismatch")?;
    stopped(session)?;
    let database = owned_container(session, "postgres", false)?.ok_or(Failure::Value)?;
    let image = same(
        Some(field(&database, "image")?),
        Some(field(&archive, "postgresImage")?),
    );
    require(image, "postgres_image_mismatch")?;
    let described = session.directory.join("database-renamed.json");
    let expected = read_object(host, &described, MAX_PRIVATE)?;
    let recorded = prior.get("renamedSnapshotSha256").and_then(Json::string);
    let kept = recorded == Some(file_digest(&described)?.as_str());
    require(kept, "renamed_snapshot_changed")?;
    let identifier = text(field(&database, "id")?)?.to_owned();
    no_clients(session, &identifier)?;
    let unchanged = snapshot::collect(host, &identifier)?.semantic_eq(&expected);
    require(unchanged, "restored_database_changed")?;
    Ok(object(vec![
        ("phase", string("target-verified")),
        (
            "databaseSystemIdentifier",
            field(&expected, "systemIdentifier")?.clone(),
        ),
    ]))
}

/// Move the stopped, failed copy of the database into the move's private
/// directory and leave an empty one in its place. Nothing is deleted.
fn retain_failed_database(session: &Move<'_>) -> Result<()> {
    let database = session.host.paths.root.join("postgres");
    let retained = session.directory.join("retained-postgres");
    let (owner, group) = (session.database.owner, session.database.group);
    for path in [&database, &retained] {
        if taken(path) {
            let metadata = std::fs::symlink_metadata(path)?;
            let safe = metadata.is_dir()
                && metadata.uid() == owner
                && metadata.mode() & 0o7777 == 0o700
                && std::fs::canonicalize(path)? == **path;
            require(safe, "unsafe_failed_database_directory")?;
        }
    }
    if retained.exists() {
        let replaced = !database.exists() || std::fs::read_dir(&database)?.next().is_none();
        require(replaced, "replacement_database_not_empty")?;
    } else {
        require(database.is_dir(), "failed_database_missing")?;
        std::fs::rename(&database, &retained)?;
    }
    if !database.exists() {
        std::fs::DirBuilder::new().mode(0o700).create(&database)?;
        std::os::unix::fs::chown(&database, Some(owner), Some(group))?;
    }
    Ok(())
}

/// Whether a container mounts the deployment's database directory where the
/// database keeps its files.
fn mounts_database(mounts: &Json, directory: &Path) -> Result<bool> {
    let directory = directory.to_str().ok_or(Failure::Value)?;
    for mount in mounts.array().ok_or(Failure::Value)? {
        object_of(mount)?;
        if says(mount, "Type", "bind")
            && says(mount, "Source", directory)
            && says(mount, "Destination", "/var/lib/postgresql")
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Give up a restore that did not finish: only while the destination's
/// application has never existed, and keeping the failed copy.
pub(super) fn abort(session: &mut Move<'_>, prior: &Json) -> Result<Json> {
    let host = session.host;
    let abortable = ["restoring", "aborting"];
    let admitted = says(prior, "role", "destination")
        && phase(prior).is_some_and(|phase| abortable.contains(&phase));
    require(admitted, "destination_abort_phase")?;
    let release = host.paths.root.join("release-state.json");
    let shared = read_object(host, &release, MAX_PRIVATE)?;
    let ours = says(&shared, "operation", "server_migration")
        && bound(&shared, session.request)
        && same(shared.get("phase"), prior.get("phase"))
        && holds(&shared, "finalized", false);
    require(ours, "destination_abort_journal")?;
    let before = object_of(field(prior, "before")?)?;
    let unchanged = configuration_hashes(host)?.semantic_eq(field(before, "configurationSha256")?);
    require(unchanged, "destination_abort_configuration_changed")?;
    for service in ["simplestchat", "caddy"] {
        let created = owned_container(session, service, true)?.is_some();
        require(!created, "destination_application_was_created")?;
    }
    let database = owned_container(session, "postgres", true)?;
    let restoring = phase(prior) == Some("restoring");
    if restoring {
        require(database.is_some(), "failed_database_container_missing")?;
    }
    let mut details = prior.clone();
    let container = match &database {
        Some(database) => Some(text(field(database, "id")?)?.to_owned()),
        None => None,
    };
    if let Some(container) = &container {
        let same = restoring || says(prior, "abortedContainer", container);
        require(same, "destination_abort_container_changed")?;
        let inspection = ["inspect", "--format", "{{json .Mounts}}", container];
        let mounts = state::decode(&session.docker(&inspection, 10.0)?)?;
        let mounted = mounts_database(&mounts, &host.paths.root.join("postgres"))?;
        require(mounted, "destination_abort_database_mount")?;
        set(&mut details, "abortedContainer", string(container));
    }
    record(session, "aborting", &details, false)?;
    if let Some(container) = &container {
        session.docker(&["stop", "--time", "30", container], 45.0)?;
        session.docker(&["update", "--restart=no", container], 15.0)?;
        session.docker(&["rm", container], 15.0)?;
    }
    retain_failed_database(session)?;
    record(session, "aborted", &details, true)
}
