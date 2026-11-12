//! The read-only look at one machine before a migration changes anything.
//! It proves which machine this is, that a source is the configured
//! deployment of the stated origin, and that a target holds no database and
//! no public container. It prints only the machine identity, the domain, the
//! passkey identity and the deployed revision, or one fixed failure code:
//! no other configuration value and no operating-system detail leaves it.

use crate::host::command::{Commands, Local, Request};
use crate::host::failure::Failure;
use crate::host::fault::{object, string};
use crate::host::{DOCKER, emit, hexadecimal};
use crate::json::{Json, python_dump};
use crate::release::preflight::os_release;
use crate::text::python_lines;
use rustix::io::Errno;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::Metadata;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::ExitCode;

const MAX_CONFIGURATION: u64 = 65536;
/// The database's own account inside its image.
const DATABASE_OWNER: u32 = 999;
/// Where an administrator's shell finds commands on the supported
/// distribution, relative to the machine's root.
const SEARCH: [&str; 6] = [
    "usr/local/sbin",
    "usr/local/bin",
    "usr/sbin",
    "usr/bin",
    "sbin",
    "bin",
];
/// A failure whose cause stays on the machine.
const PRIVATE: &str = "inspection_failed";

type Result<T> = std::result::Result<T, &'static str>;

fn require(condition: bool, code: &'static str) -> Result<()> {
    if condition { Ok(()) } else { Err(code) }
}

fn private<T>(result: std::io::Result<T>) -> Result<T> {
    result.map_err(|_| PRIVATE)
}

/// What the process, the kernel and the filesystem say about this machine.
/// Tests substitute each.
pub(super) struct Machine<'a> {
    pub privileged: bool,
    pub system: &'a str,
    pub processor: &'a str,
    /// The machine's root, where its fixed files start.
    pub anchor: &'a Path,
    /// The public project's protected configuration.
    pub config: &'a Path,
    /// The public deployment's data root.
    pub root: &'a Path,
    pub commands: &'a dyn Commands,
    /// Who owns an entry.
    pub owner: &'a dyn Fn(&Path, &Metadata) -> u32,
}

/// Whether a path leads somewhere, following links. A path that cannot be
/// judged is a failure, not an absence.
fn exists(path: &Path) -> Result<bool> {
    let Err(error) = std::fs::metadata(path) else {
        return Ok(true);
    };
    let nowhere = [Errno::NOENT, Errno::NOTDIR, Errno::BADF, Errno::LOOP];
    let absent = Errno::from_io_error(&error).is_some_and(|errno| nowhere.contains(&errno));
    require(absent, PRIVATE).map(|()| false)
}

fn is_link(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_symlink())
}

/// Whether a name is taken at all, even by a link that leads nowhere.
fn present(path: &Path) -> Result<bool> {
    Ok(exists(path)? || is_link(path))
}

/// Require a private directory or a private file of bounded size that root
/// owns, without following a link.
fn protected(machine: &Machine<'_>, path: &Path, directory: bool) -> Result<()> {
    let metadata = private(std::fs::symlink_metadata(path))?;
    let (mode, kind) = match directory {
        true => (0o700, metadata.is_dir()),
        false => (0o600, metadata.is_file()),
    };
    let root = (machine.owner)(path, &metadata) == 0;
    let safe = root && metadata.mode() & 0o7777 == mode && kind;
    require(safe, "unsafe_migration_configuration")?;
    let sized = directory || (1..=MAX_CONFIGURATION).contains(&metadata.len());
    require(sized, "invalid_configuration_size")
}

/// The passkey identity and deployed revision of the deployment configured
/// for `origin`, or nothing for a machine without one. Only a target may
/// lack both configuration files; no other value of either is returned.
fn configured(machine: &Machine<'_>, origin: &str, target: bool) -> Result<Option<[String; 2]>> {
    let (config, root) = (machine.config, machine.root);
    if !exists(config)? {
        require(!is_link(config), "unsafe_migration_configuration")?;
        return Ok(None);
    }
    protected(machine, config, true)?;
    let (environment, manifest) = (config.join("app.env"), config.join("images.json"));
    let pair = [present(&environment)?, present(&manifest)?];
    if target && pair == [false, false] {
        if present(root)? {
            protected(machine, root, true)?;
        }
        let secrets = config.join("secrets.json");
        if present(&secrets)? {
            protected(machine, &secrets, false)?;
        }
        return Ok(None);
    }
    require(pair == [true, true], "configuration_pair_incomplete")?;
    protected(machine, root, true)?;
    protected(machine, &environment, false)?;
    protected(machine, &manifest, false)?;
    let text = String::from_utf8(private(std::fs::read(&environment))?).map_err(|_| PRIVATE)?;
    let assigned = |line: &&str| !line.is_empty() && !line.starts_with('#');
    let lines = python_lines(&text).filter(assigned);
    let pairs: Option<Vec<(&str, &str)>> = lines.map(|line| line.split_once('=')).collect();
    let pairs = pairs.ok_or("invalid_environment")?;
    let names: BTreeSet<&str> = pairs.iter().map(|(name, _)| *name).collect();
    require(names.len() == pairs.len(), "duplicate_environment_key")?;
    let value = |name: &str| {
        pairs
            .iter()
            .find(|(key, _)| *key == name)
            .map(|pair| pair.1)
    };
    let same = value("ALLOWED_ORIGINS") == Some(origin);
    require(same, "source_or_target_origin_mismatch")?;
    let identity = value("WEBAUTHN_RP_ID").unwrap_or_default();
    let plain =
        |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b".-".contains(&byte);
    let named = (1..=253).contains(&identity.len()) && identity.bytes().all(plain);
    require(named, "invalid_source_rp_id")?;
    let selected = private(std::fs::read(&manifest))?;
    let selected = crate::json::decode(&selected).map_err(|_| PRIVATE)?;
    require(selected.object().is_some(), "invalid_image_manifest")?;
    let revision = selected.get("revision").and_then(Json::string);
    let revision = revision.filter(|revision| hexadecimal(revision, 40));
    let revision = revision.ok_or("invalid_source_revision")?;
    Ok(Some([identity.to_owned(), revision.to_owned()]))
}

/// Whether a path is a program an administrator's shell would run.
fn program(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file() && metadata.mode() & 0o111 != 0)
}

/// Refuse a target that already holds database files or any container of
/// the public project.
fn fresh(machine: &Machine<'_>) -> Result<()> {
    let database = machine.root.join("postgres");
    if present(&database)? {
        let metadata = private(std::fs::symlink_metadata(&database))?;
        let ours = metadata.is_dir() && (machine.owner)(&database, &metadata) == DATABASE_OWNER;
        require(ours, "unsafe_target_database")?;
        let first = private(std::fs::read_dir(&database))?.next();
        private(first.transpose())
            .and_then(|entry| require(entry.is_none(), "destination_database_not_empty"))?;
    }
    let installed = |directory: &&str| program(&machine.anchor.join(directory).join("docker"));
    if !SEARCH.iter().any(installed) {
        return Ok(());
    }
    // The listing asks the packaged client; one found only elsewhere is not
    // a machine this inspection can vouch for.
    require(program(&machine.anchor.join("usr/bin/docker")), PRIVATE)?;
    let listing = ["ps", "--all", "--quiet", "--filter"];
    let project = "label=com.docker.compose.project=simplestchat-public";
    let argv = DOCKER.iter().chain(&listing).chain([&project]);
    let argv = argv.map(|word| (*word).to_owned()).collect();
    let occupied = "destination_has_public_containers";
    match machine
        .commands
        .run(&Request::fixed(argv, 20.0, 64 * 1024, occupied))
    {
        Ok(listed) => {
            let listed = String::from_utf8(listed).map_err(|_| PRIVATE)?;
            require(listed.trim().is_empty(), occupied)
        }
        Err(Failure::Command | Failure::Refused(_)) => Err(occupied),
        Err(_) => Err(PRIVATE),
    }
}

/// The inspected identities of one machine in its role, or the fixed code
/// of the first precondition it does not meet.
fn inspect(machine: &Machine<'_>, role: &str, origin: &str) -> Result<Json> {
    require(machine.privileged, "root_required")?;
    let target = match role {
        "source" => false,
        "target" => true,
        _ => return Err("invalid_migration_role"),
    };
    let plain =
        |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b".-".contains(&byte);
    let host = origin.strip_prefix("https://");
    let host = host.filter(|host| !host.is_empty() && host.bytes().all(plain));
    let host = host.ok_or("invalid_migration_origin")?;
    let supported = machine.system == "Linux" && machine.processor == "x86_64";
    require(supported, "unsupported_platform")?;
    let release = ["etc/os-release", "usr/lib/os-release"].map(|name| machine.anchor.join(name));
    let release = release.iter().find_map(|path| std::fs::read(path).ok());
    let (distribution, version) = os_release(&release.ok_or(PRIVATE)?).ok_or(PRIVATE)?;
    let debian = distribution == "debian" && version == "13";
    require(debian, "unsupported_distribution")?;
    let identity = private(std::fs::read(machine.anchor.join("etc/machine-id")))?;
    let identity = String::from_utf8(identity).map_err(|_| PRIVATE)?;
    let identity = identity.trim();
    require(hexadecimal(identity, 32), "invalid_machine_identity")?;
    let settings = configured(machine, origin, target)?;
    if target {
        fresh(machine)?;
    } else {
        require(settings.is_some(), "source_not_configured")?;
    }
    let field = |value: Option<&str>| value.map_or(Json::Null, string);
    let [passkeys, revision] = match &settings {
        Some([passkeys, revision]) => [Some(passkeys.as_str()), Some(revision.as_str())],
        None => [None, None],
    };
    Ok(object(vec![
        ("passed", Json::Bool(true)),
        ("machineId", string(identity)),
        ("domain", field(settings.as_ref().map(|_| host))),
        ("rpId", field(passkeys)),
        ("revision", field(revision)),
    ]))
}

/// The one line an inspection prints, and whether it passed.
pub(super) fn answer(machine: &Machine<'_>, role: &str, origin: &str) -> (String, bool) {
    let (record, passed) = match inspect(machine, role, origin) {
        Ok(record) => (record, true),
        Err(code) => {
            let failed = vec![("passed", Json::Bool(false)), ("failure", string(code))];
            (object(failed), false)
        }
    };
    (format!("{}\n", python_dump(&record, false)), passed)
}

/// Run `migrate inspect source|target ORIGIN`. Another shape of command
/// line returns 2; a machine that does not qualify prints its code and
/// returns 1.
pub(super) fn run(arguments: &[OsString]) -> ExitCode {
    let [Some(role), Some(origin)] = arguments
        .iter()
        .map(|word| word.to_str())
        .collect::<Vec<_>>()[..]
    else {
        return ExitCode::from(2);
    };
    let kernel = rustix::system::uname();
    let paths = crate::host::Paths::fixed();
    let machine = Machine {
        privileged: rustix::process::geteuid().is_root(),
        system: kernel.sysname().to_str().unwrap_or_default(),
        processor: kernel.machine().to_str().unwrap_or_default(),
        anchor: &paths.anchor,
        config: &paths.config,
        root: &paths.root,
        commands: &Local,
        owner: &|_, metadata| metadata.uid(),
    };
    let (line, passed) = answer(&machine, role, origin);
    match emit(std::io::stdout(), &line) && passed {
        true => ExitCode::SUCCESS,
        false => ExitCode::FAILURE,
    }
}

#[cfg(test)]
#[path = "inspect_tests.rs"]
mod tests;
