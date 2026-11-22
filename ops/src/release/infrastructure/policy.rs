//! Every decision a guest action takes over what it read. Each function judges
//! decoded facts only; the guest reads them and runs the commands. Equality is
//! JSON's own (an integer equals the float of its value, as the deployment's
//! tools compare), and each type test is exact: a boolean is never an integer.

use super::model::{
    COUNT_NAMES, MAX_MEMORY, MAX_PIDS, PROJECT, SERVICES, bounded, exactly, field, is, object,
    strings, text,
};

use crate::host::failure::{Failure, Result, require};
use crate::host::hexadecimal;
use crate::json::Json;
use crate::text::python_lines;
use std::collections::{BTreeMap, BTreeSet};

const MAX_NANO_CPUS: i64 = 8_000_000_000;

fn string(value: &Json) -> Result<&str> {
    value.string().ok_or(Failure::Value)
}

fn utf8(bytes: &[u8]) -> Result<&str> {
    std::str::from_utf8(bytes).map_err(|_| Failure::Value)
}

/// The most bytes a protected file may have.
pub(crate) const MAX_FILE: u64 = 1024 * 1024;
/// The generated deployment secrets.
pub(crate) const SECRET_NAMES: [&str; 6] = [
    "postgres_admin_password",
    "migration_password",
    "database_password",
    "metrics_token",
    "proxy_secret",
    "owner_password",
];
/// How many of each the fixture adds.
pub(crate) const COUNTS: [i64; 6] = [1, 1, 0, 0, 1, 1];
/// The protected configuration files, each with its owner role and mode.
pub(crate) const FILES: [(&str, Owner, u32); 13] = [
    ("secrets.json", Owner::Root, 0o600),
    ("compose.base.yml", Owner::Root, 0o644),
    ("Caddyfile", Owner::Root, 0o644),
    ("compose.public.yml", Owner::Root, 0o600),
    ("app.env", Owner::Root, 0o600),
    ("migration.env", Owner::Root, 0o600),
    ("proxy.env", Owner::Root, 0o600),
    ("postgres-admin-password", Owner::Postgres, 0o400),
    ("pg_hba.conf", Owner::Root, 0o644),
    ("init-database.sql", Owner::Postgres, 0o400),
    ("runtime-grants.sql", Owner::Root, 0o600),
    ("images.json", Owner::Root, 0o600),
    ("browser-assets-installed.json", Owner::Root, 0o600),
];
/// The effective properties read of every fixture unit.
pub(crate) const UNIT_PROPERTIES: [&str; 10] = [
    "LoadState",
    "FragmentPath",
    "User",
    "DynamicUser",
    "UMask",
    "RuntimeDirectory",
    "RuntimeDirectoryMode",
    "RuntimeDirectoryPreserve",
    "ActiveState",
    "UnitFileState",
];
/// The sandbox the backup service must have, beyond its unit identity.
pub(crate) const BACKUP_SANDBOX: [(&str, &str); 11] = [
    ("NoNewPrivileges", "yes"),
    ("PrivateTmp", "yes"),
    ("ProtectSystem", "strict"),
    ("ProtectHome", "yes"),
    ("RestrictSUIDSGID", "yes"),
    ("LockPersonality", "yes"),
    ("MemoryMax", "268435456"),
    ("TasksMax", "64"),
    ("CPUQuotaPerSecUSec", "500ms"),
    ("TimeoutStartUSec", "5min"),
    (
        "ReadWritePaths",
        "/srv/simplestchat-public/backups /run/simplestchat-bench",
    ),
];
/// The backup timer's exact effective properties.
pub(crate) const BACKUP_TIMER: [(&str, &str); 7] = [
    ("LoadState", "loaded"),
    ("UnitFileState", "enabled"),
    ("ActiveState", "active"),
    ("Persistent", "yes"),
    ("RandomizedDelayUSec", "10min"),
    ("AccuracyUSec", "1min"),
    ("Triggers", "simplestchat-backup.service"),
];

pub(crate) const FILE_METADATA: &str = "Fixture file ownership, type, size or mode differs";
pub(crate) const FILE_CHANGED: &str = "Fixture file changed while reading";
pub(crate) const DIRECTORY_METADATA: &str = "Fixture directory ownership or mode differs";
const SELECTION: &str = "Fixture selection schema or owner differs";
const REVISION: &str = "Fixture revision is invalid";
pub(crate) const ROOT: &str = "Fixture requires root";
pub(crate) const PLATFORM: &str = "Fixture requires Linux x86_64";
pub(crate) const RUN_ID: &str = "Invalid fixture ownership token";
pub(crate) const DISTRIBUTION: &str = "Fixture requires Debian 13";
pub(crate) const HYPERVISOR: &str = "Fixture requires QEMU";
pub(crate) const MARKER: &str = "Fixture marker differs";
pub(crate) const HELPER_ORIGIN: &str = "Fixture helper origin differs";
pub(crate) const DOCKER_SOCKET: &str = "Fixture Docker socket differs";
const PROPERTY: &str = "Unexpected systemd property";
const PROPERTIES: &str = "Incomplete systemd properties";
const UNIT_IDENTITY: &str = "Fixture unit identity differs";
const UNIT_STATE: &str = "Fixture workload unit is enabled or active";
const RUNTIME_DIRECTORY: &str = "Fixture runtime directory differs";
const IMAGE_BUILD_DIRECTORY: &str = "Unexpected image build runtime directory";
const CONFIGURATION_FILES: &str = "Unexpected public configuration files";
const SECRET_SCHEMA: &str = "Fixture deployment secret schema differs";
const SECRET_INDEPENDENCE: &str = "Fixture secrets are not independent";
const APPLICATION: &str = "Fixture application configuration differs";
const MIGRATION: &str = "Fixture migration environment differs";
const PROXY: &str = "Fixture proxy environment differs";
const DATABASE_SECRET: &str = "Fixture database secret differs";
pub(crate) const PROJECT_PRESENT: &str = "Fixture public project is already present";
const CONFIGURATION_CHANGED: &str = "Public playbook changed protected configuration";
const DOTENV_LINE: &str = "Malformed app.env line";
const DOTENV_DUPLICATE: &str = "Duplicate app.env line";

/// The role a protected path belongs to; each has its own uid and gid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Owner {
    Root,
    Postgres,
    Caddy,
}

/// The protected deployment directories under the configuration (`None`) or
/// the data root (`Some(name)`), each with its owner role and mode.
pub(crate) const DIRECTORIES: [(Option<&str>, Owner, u32); 8] = [
    (None, Owner::Root, 0o700),
    (Some(""), Owner::Root, 0o700),
    (Some("results"), Owner::Root, 0o700),
    (Some("backups"), Owner::Root, 0o700),
    (Some("postgres"), Owner::Postgres, 0o700),
    (Some("postgres-socket"), Owner::Postgres, 0o755),
    (Some("caddy-data"), Owner::Caddy, 0o700),
    (Some("caddy-config"), Owner::Caddy, 0o700),
];

/// One run's binding: its ownership token and the release revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Selection {
    pub run: String,
    pub revision: String,
}

/// The run selection the staging playbook wrote: exactly a schema version 1,
/// this run's token and a revision.
pub(crate) fn selection(data: &[u8], run: &str) -> Result<Selection> {
    let value = object(data)?;
    let names = value
        .object()
        .map(|object| object.keys().cloned().collect::<Vec<_>>());
    let exact = hexadecimal(run, 32)
        && names.is_some_and(|names| names == ["revision", "runId", "schemaVersion"])
        && value.get("schemaVersion").and_then(Json::integer) == Some(1)
        && is(value.get("runId"), &text(run));
    require(exact, SELECTION)?;
    let revision = string(field(&value, "revision")?)?;
    require(hexadecimal(revision, 40), REVISION)?;
    Ok(Selection {
        run: run.to_owned(),
        revision: revision.to_owned(),
    })
}

/// The effective systemd properties `output` lists: each named property once
/// as `Name=value`, and every one of `names`.
pub(crate) fn properties(output: &[u8], names: &[&str]) -> Result<BTreeMap<String, String>> {
    let mut found = BTreeMap::new();
    for line in python_lines(utf8(output)?) {
        let (key, value) = line.split_once('=').unwrap_or((line, ""));
        let known = line.contains('=') && names.contains(&key) && !found.contains_key(key);
        require(known, PROPERTY)?;
        found.insert(key.to_owned(), value.to_owned());
    }
    require(found.len() == names.len(), PROPERTIES)?;
    Ok(found)
}

/// A fixture unit runs as root from its own unit file with a private umask;
/// a workload unit is inactive and never enabled; every unit but the image
/// build keeps the private benchmark runtime directory.
pub(crate) fn unit(value: &BTreeMap<String, String>, unit: &str, backup: bool) -> Result<()> {
    let get = |key: &str| value.get(key).map(String::as_str);
    let fragment = format!("/etc/systemd/system/{unit}");
    let identity = get("LoadState") == Some("loaded")
        && get("FragmentPath") == Some(fragment.as_str())
        && get("UMask") == Some("0077")
        && get("DynamicUser") == Some("no")
        && matches!(get("User"), Some("" | "root" | "0"));
    require(identity, UNIT_IDENTITY)?;
    if !backup {
        let inactive = get("ActiveState") == Some("inactive")
            && matches!(get("UnitFileState"), Some("static" | "disabled"));
        require(inactive, UNIT_STATE)?;
    }
    if unit == "simplestchat-image-build.service" {
        return require(get("RuntimeDirectory") == Some(""), IMAGE_BUILD_DIRECTORY);
    }
    let private = get("RuntimeDirectory") == Some("simplestchat-bench")
        && get("RuntimeDirectoryMode") == Some("0700")
        && get("RuntimeDirectoryPreserve") == Some("yes");
    require(private, RUNTIME_DIRECTORY)
}

/// The assignments of a dotenv text: every line but blanks and comments is a
/// `NAME=value` once, with an upper-case name.
pub(crate) fn dotenv(text: &str) -> Result<BTreeMap<String, String>> {
    let mut found = BTreeMap::new();
    for line in python_lines(text) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let assigned = line.split_once('=');
        let named = assigned.is_some_and(|(name, _)| {
            name.bytes()
                .next()
                .is_some_and(|byte| byte.is_ascii_uppercase())
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
        });
        require(named, DOTENV_LINE)?;
        let (name, value) = assigned.unwrap_or_default();
        require(!found.contains_key(name), DOTENV_DUPLICATE)?;
        found.insert(name.to_owned(), value.to_owned());
    }
    Ok(found)
}

/// The configuration directory holds exactly the protected files.
pub(crate) fn configuration_names(names: &BTreeSet<String>) -> Result<()> {
    let expected: BTreeSet<String> = FILES.iter().map(|(name, ..)| (*name).to_owned()).collect();
    require(*names == expected, CONFIGURATION_FILES)
}

/// The generated secrets have exactly their six names, each 64 lowercase
/// hexadecimal digits and all different; the environment files carry the
/// fixture's exact settings and the secrets they derive from; the database
/// administrator file holds its own secret.
pub(crate) fn configuration_values(
    secrets: &[u8],
    application: &[u8],
    migration: &[u8],
    proxy: &[u8],
    administrator: &[u8],
) -> Result<()> {
    let secrets = object(secrets)?;
    let named = secrets.object().is_some_and(|object| {
        object.len() == SECRET_NAMES.len()
            && SECRET_NAMES.iter().all(|name| {
                object
                    .get(*name)
                    .and_then(Json::string)
                    .is_some_and(|value| hexadecimal(value, 64))
            })
    });
    require(named, SECRET_SCHEMA)?;
    let secret = |name: &str| secrets.get(name).and_then(Json::string).unwrap_or_default();
    let distinct: BTreeSet<&str> = SECRET_NAMES.iter().map(|name| secret(name)).collect();
    require(distinct.len() == SECRET_NAMES.len(), SECRET_INDEPENDENCE)?;
    let url = |role: &str, password: &str| {
        format!(
            "postgres://simplestchat_{role}:{password}@localhost/simplestchat\
             ?host=/run/simplestchat-postgres&sslmode=disable"
        )
    };
    let environment = dotenv(utf8(application)?)?;
    let expected = [
        ("ALLOWED_ORIGINS", "https://vm-fixture.test".to_owned()),
        ("WEBAUTHN_RP_ID", "vm-fixture.test".to_owned()),
        ("WEBAUTHN_ORIGIN", "https://vm-fixture.test".to_owned()),
        ("ANNOUNCE_IP", "127.0.0.1".to_owned()),
        ("ANNOUNCE_IPV6", String::new()),
        ("RUN_MIGRATIONS", "false".to_owned()),
        ("REGISTRATION_ENABLED", "false".to_owned()),
        ("ALLOW_AD_HOC_ROOMS", "false".to_owned()),
        ("MEDIA_WORKERS", "1".to_owned()),
        ("BIND_ADDR", "0.0.0.0".to_owned()),
        ("PORT", "3000".to_owned()),
        ("METRICS_TOKEN", secret("metrics_token").to_owned()),
        ("TRUSTED_PROXY_SECRET", secret("proxy_secret").to_owned()),
        ("DATABASE_URL", url("app", secret("database_password"))),
    ];
    let application = expected
        .iter()
        .all(|(name, value)| environment.get(*name) == Some(value));
    require(application, APPLICATION)?;
    let migration = dotenv(utf8(migration)?)?;
    let expected: BTreeMap<String, String> = [
        ("DATABASE_URL", url("migrate", secret("migration_password"))),
        ("RUN_MIGRATIONS", "true".to_owned()),
        ("BIND_ADDR", "127.0.0.1".to_owned()),
        ("PORT", "3000".to_owned()),
        ("ANNOUNCE_IP", "127.0.0.1".to_owned()),
        ("MEDIA_WORKERS", "1".to_owned()),
        ("REGISTRATION_ENABLED", "false".to_owned()),
        ("ALLOW_AD_HOC_ROOMS", "false".to_owned()),
        ("RUST_LOG", "simplestchat=info,mediasoup=warn".to_owned()),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_owned(), value))
    .collect();
    require(migration == expected, MIGRATION)?;
    let proxy = dotenv(utf8(proxy)?)?;
    let expected: BTreeMap<String, String> = [
        ("CADDY_DOMAIN", "vm-fixture.test"),
        ("CADDY_UPSTREAM", "simplestchat:3000"),
        ("TRUSTED_PROXY_SECRET", secret("proxy_secret")),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_owned(), value.to_owned()))
    .collect();
    require(proxy == expected, PROXY)?;
    let stored = utf8(administrator)?.trim_matches(space);
    require(stored == secret("postgres_admin_password"), DATABASE_SECRET)
}

/// How many snapshots have now matched: the first records one; a later one
/// must find exactly one earlier pass of the same schema whose record equals
/// `record`, which makes two.
pub(crate) fn snapshot(previous: Option<&[u8]>, record: &Json) -> Result<u8> {
    let Some(previous) = previous else {
        return Ok(1);
    };
    let Json::Object(mut fields, mut order) = object(previous)? else {
        return Err(Failure::Value);
    };
    let passes = fields.remove("passes");
    order.retain(|name| name != "passes");
    let earlier = Json::Object(fields, order);
    let same = matches!(&passes, Some(Json::Integer(count)) if count == "1")
        && matches!(earlier.get("schemaVersion"), Some(Json::Integer(_)))
        && earlier.semantic_eq(record);
    require(same, CONFIGURATION_CHANGED)?;
    Ok(2)
}

/// The one container identity a service listing printed.
pub(crate) fn listed(listing: &[u8]) -> Result<String> {
    let identifier = utf8(listing)?.trim_matches(space);
    require(
        hexadecimal(identifier, 64),
        "Fixture service identity is ambiguous",
    )?;
    Ok(identifier.to_owned())
}

/// The container `inspection` describes is the one `identifier` of service
/// `service`: that full identity, the project's fixed name and both
/// ownership labels.
pub(crate) fn owned(identifier: &str, inspection: &[u8], service: &str) -> Result<Json> {
    let value = object(inspection)?;
    let labels = field(&value, "labels")?;
    labels.object().ok_or(Failure::Value)?;
    let mine = is(value.get("id"), &text(identifier))
        && is(value.get("name"), &text(&format!("/{PROJECT}-{service}-1")))
        && is(labels.get("com.docker.compose.project"), &text(PROJECT))
        && is(labels.get("com.docker.compose.service"), &text(service));
    require(mine, "Fixture container ownership differs")?;
    Ok(value)
}

/// The running container `value` of `service` uses `image` and its user,
/// runs normally, keeps its read-only unprivileged boundary and finite
/// limits, and the database and migrator have no network. Returns its id.
pub(crate) fn running(value: &Json, service: &str, image: &str) -> Result<String> {
    let state = field(value, "state")?;
    state.object().ok_or(Failure::Value)?;
    let host = field(value, "host")?;
    host.object().ok_or(Failure::Value)?;
    let user = || {
        let found = SERVICES.iter().find(|(name, _)| *name == service);
        found.map(|(_, user)| text(user)).ok_or(Failure::Key)
    };
    let identity = is(value.get("image"), &text(image)) && is(value.get("user"), &user()?);
    require(identity, "Fixture runtime identity differs")?;
    let normal = exactly(state.get("Running"), true) && exactly(state.get("OOMKilled"), false);
    require(normal, "Fixture service is not running normally")?;
    let added = host.get("CapAdd");
    let options = host.get("SecurityOpt");
    let isolated = exactly(host.get("ReadonlyRootfs"), true)
        && exactly(host.get("Privileged"), false)
        && is(host.get("CapDrop"), &strings(&["ALL"]))
        && added
            .is_none_or(|added| matches!(added, Json::Null) || added.semantic_eq(&strings(&[])))
        && (is(options, &strings(&["no-new-privileges"]))
            || is(options, &strings(&["no-new-privileges:true"])));
    require(isolated, "Fixture runtime isolation differs")?;
    let memory = i64::try_from(MAX_MEMORY).unwrap_or(i64::MAX);
    let limited = bounded(host.get("Memory"), 1, memory)
        && bounded(host.get("PidsLimit"), 1, MAX_PIDS)
        && bounded(host.get("NanoCpus"), 1, MAX_NANO_CPUS);
    require(limited, "Fixture runtime limits differ")?;
    if matches!(service, "postgres" | "migrate") {
        require(
            is(host.get("NetworkMode"), &text("none")),
            "Fixture runtime network differs",
        )?;
    }
    Ok(string(field(value, "id")?)?.to_owned())
}

/// The successful migration versions and checksums the database printed,
/// one `version t checksum` row per line, each version once.
pub(crate) fn ledger(output: &[u8]) -> Result<BTreeMap<String, String>> {
    let mut found = BTreeMap::new();
    for line in python_lines(utf8(output)?) {
        let fields: Vec<&str> = line.split(space).filter(|word| !word.is_empty()).collect();
        let valid = fields.len() == 3
            && fields[0]
                .bytes()
                .next()
                .is_some_and(|byte| (b'1'..=b'9').contains(&byte))
            && fields[0].bytes().all(|byte| byte.is_ascii_digit())
            && fields[1] == "t"
            && hexadecimal(fields[2], 96)
            && !found.contains_key(fields[0]);
        require(valid, "Invalid migration ledger")?;
        found.insert(fields[0].to_owned(), fields[2].to_owned());
    }
    require(!found.is_empty(), "Empty migration ledger")?;
    Ok(found)
}

/// The synthetic rows as an object of [`COUNT_NAMES`].
pub(crate) fn counts() -> Json {
    let entries: BTreeMap<String, Json> = COUNT_NAMES
        .iter()
        .zip(COUNTS)
        .map(|(name, count)| ((*name).to_owned(), Json::Integer(count.to_string())))
        .collect();
    let order = COUNT_NAMES.iter().map(|name| (*name).to_owned()).collect();
    Json::Object(entries, order)
}

/// Whether `value` are exactly the synthetic rows.
pub(crate) fn counted(value: &Json) -> bool {
    value.semantic_eq(&counts())
}

/// The migrations a backup receipt recorded, each a canonical version with a
/// SHA-384 the reviewed release packages for it.
pub(crate) fn recorded_migrations(
    receipt: &Json,
    packaged: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>> {
    let recorded = receipt
        .get("backupMigrations")
        .filter(|value| !matches!(value, Json::Null));
    let recorded = recorded.ok_or(Failure::Refused(
        "Backup lacks its recorded migration ledger",
    ))?;
    let entries = recorded.object().ok_or(Failure::Value)?;
    let mut migrations = BTreeMap::new();
    for (version, checksum) in entries {
        migrations.insert(version.clone(), string(checksum)?.to_owned());
    }
    let reviewed = !migrations.is_empty()
        && migrations.iter().all(|(version, checksum)| {
            version
                .bytes()
                .next()
                .is_some_and(|byte| (b'1'..=b'9').contains(&byte))
                && version.bytes().all(|byte| byte.is_ascii_digit())
                && hexadecimal(checksum, 96)
                && packaged.get(version) == Some(checksum)
        });
    require(
        reviewed,
        "Backup migration ledger differs from the reviewed release",
    )?;
    Ok(migrations)
}

/// The backup service has the unit identity and every sandbox setting, its
/// writable paths as a set; the timer has exactly its effective settings.
pub(crate) fn backup_units(
    service: &BTreeMap<String, String>,
    timer: &BTreeMap<String, String>,
) -> Result<()> {
    unit(service, "simplestchat-backup.service", true)?;
    let sandboxed = BACKUP_SANDBOX.iter().all(|(name, expected)| {
        let actual = service.get(*name).map(String::as_str);
        if *name == "ReadWritePaths" {
            actual.is_some_and(|actual| split(actual) == split(expected))
        } else {
            actual == Some(*expected)
        }
    });
    require(sandboxed, "Backup service sandbox differs")?;
    let expected: BTreeMap<String, String> = BACKUP_TIMER
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();
    require(*timer == expected, "Backup timer configuration differs")
}

/// Whether the backup service's run completed successfully.
pub(crate) fn backup_completed(value: &BTreeMap<String, String>) -> bool {
    let expected: BTreeMap<String, String> = [
        ("Result", "success"),
        ("ExecMainStatus", "0"),
        ("ActiveState", "inactive"),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_owned(), value.to_owned()))
    .collect();
    *value == expected
}

/// Whitespace as the deployment's tools strip and split text: Unicode white
/// space and the separators U+001C to U+001F.
fn space(character: char) -> bool {
    character.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&character)
}

/// The distinct whitespace-separated words of `text`.
fn split(text: &str) -> BTreeSet<&str> {
    text.split(space).filter(|word| !word.is_empty()).collect()
}

/// Whether a stopped timer's service has settled: inactive is settled, a
/// service still running or stopping is not yet, and any other state is a
/// failed earlier backup.
pub(crate) fn settled(state: &str) -> Result<bool> {
    if state == "inactive" {
        return Ok(true);
    }
    require(
        matches!(state, "active" | "activating" | "deactivating"),
        "A prior fixture backup failed",
    )?;
    Ok(false)
}

/// Whether the application's readiness answer says it is ready.
pub(crate) fn ready(answer: &[u8]) -> bool {
    object(answer).is_ok_and(|value| is(value.get("status"), &text("ready")))
}

/// Whether the hypervisor vendor file names QEMU.
pub(crate) fn qemu(vendor: &[u8]) -> bool {
    let head = &vendor[..vendor.len().min(256)];
    let white = |byte: &u8| matches!(*byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c);
    let start = head
        .iter()
        .position(|byte| !white(byte))
        .unwrap_or(head.len());
    let end = head
        .iter()
        .rposition(|byte| !white(byte))
        .map_or(start, |at| at + 1);
    &head[start..end] == b"QEMU"
}

/// The image identity `inspection` describes for `revision`: a local
/// content-addressed id, Linux amd64 as the unprivileged user, the release's
/// revision label, the server command with no entrypoint, the application
/// directory, no health check and only plain environment names.
pub(crate) fn image(inspection: &[u8], revision: &str) -> Result<String> {
    let value = object(inspection)?;
    let identifier = string(field(&value, "id")?)?;
    let content = identifier
        .strip_prefix("sha256:")
        .is_some_and(|hash| hexadecimal(hash, 64));
    require(content, "Missing local content-addressed image ID")?;
    let platform = is(Some(field(&value, "os")?), &text("linux"))
        && is(Some(field(&value, "architecture")?), &text("amd64"))
        && is(Some(field(&value, "user")?), &text("10001:10001"));
    require(platform, "Unexpected image platform or runtime user")?;
    let labels = field(&value, "labels")?;
    labels.object().ok_or(Failure::Value)?;
    require(
        is(
            labels.get("org.opencontainers.image.revision"),
            &text(revision),
        ),
        "Image revision mismatch",
    )?;
    let server = is(
        Some(field(&value, "cmd")?),
        &strings(&["/app/simplestChat"]),
    );
    let command = server && {
        let entry = field(&value, "entrypoint")?;
        matches!(entry, Json::Null) || entry.semantic_eq(&strings(&[]))
    };
    require(command, "Unexpected image entrypoint")?;
    let defaults = is(Some(field(&value, "workingDir")?), &text("/app"))
        && exactly(Some(field(&value, "hasHealthcheck")?), false);
    require(defaults, "Unexpected image execution defaults")?;
    let names = field(&value, "envNames")?.array().ok_or(Failure::Value)?;
    let names = names.iter().map(string).collect::<Result<Vec<_>>>()?;
    crate::release::profile::environment_names(&names).map_err(|_| Failure::Value)?;
    Ok(identifier.to_owned())
}

/// The server and database images the staged release selected, once the
/// manifest, the recorded images and the staging record all name `revision`
/// and the staged manifest digest is `manifest`'s, the staged image is the
/// recorded one and `identity` confirms it, and the database image is pinned
/// by digest.
pub(crate) fn selected(
    revision: &str,
    manifest_revision: &str,
    manifest: &str,
    images: &Json,
    staged: &Json,
    identity: impl FnOnce(&str) -> Result<String>,
) -> Result<(String, String)> {
    let joined = manifest_revision == revision
        && is(images.get("revision"), &text(revision))
        && is(staged.get("revision"), &text(revision))
        && is(staged.get("manifestSha256"), &text(manifest));
    require(joined, "Fixture release selection differs")?;
    let server = string(field(images, "serverImage")?)?;
    let same = is(staged.get("serverImage"), &text(server)) && identity(server)? == server;
    require(same, "Fixture server image differs")?;
    let postgres = string(field(images, "postgresImage")?)?;
    let pinned = postgres
        .strip_prefix("docker.io/library/postgres:")
        .and_then(|rest| rest.split_once("@sha256:"))
        .is_some_and(|(tag, hash)| {
            !tag.is_empty()
                && tag.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'.' | b'-')
                })
                && hexadecimal(hash, 64)
        });
    require(pinned, "Fixture PostgreSQL image is not immutable")?;
    Ok((server.to_owned(), postgres.to_owned()))
}

/// A backup belongs to the fixture's own database: the same running database
/// container, the receipt's database image, its recorded migrations and the
/// live ledger all equal to the release's.
pub(crate) fn backup_identity(
    running: &str,
    database: &str,
    receipt: &Json,
    image: &str,
    packaged: &BTreeMap<String, String>,
    ledger: impl FnOnce() -> Result<BTreeMap<String, String>>,
) -> Result<()> {
    let message = "Backup identity or complete ledger differs";
    require(
        running == database && is(receipt.get("postgresImage"), &text(image)),
        message,
    )?;
    require(
        recorded_migrations(receipt, packaged)? == *packaged,
        message,
    )?;
    require(ledger()? == *packaged, message)
}

/// A restore verified the archive, cleaned up its database and returned
/// exactly the synthetic rows.
pub(crate) fn restored(verified: bool, cleaned: bool, counts: Option<&Json>) -> Result<()> {
    require(
        verified && cleaned && counts.is_some_and(counted),
        "Fixture restore verification or cleanup differs",
    )
}

/// Whether a container listing printed nothing but whitespace.
pub(crate) fn empty(listing: &[u8]) -> bool {
    listing
        .iter()
        .all(|byte| matches!(*byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c))
}
