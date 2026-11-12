//! `simplestchat-ops release profile`: require the managed server's
//! execution profile before a launcher starts or replaces anything. One
//! document arrives on standard input: the image's own configuration, or the
//! resolved Compose model of a deployment and the image it must run. The
//! answer is one fixed line; no name or value of the configuration is ever
//! printed.
//!
//! The checks describe the shipped application and its migration service.
//! They make no claim about other services or about what an image contains.
//!
//! The same rules judge the project before a deployment, a maintenance and
//! a reboot change anything: those callers take a refusal as it is, a fixed
//! code, a field that is not there or a value of the wrong shape, and name
//! it in their own evidence.

#[cfg(test)]
#[path = "profile_tests.rs"]
mod tests;

use crate::host::{emit, image_id};
use crate::json::{self, Json};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Read;
use std::process::ExitCode;

/// The most bytes a document may take.
const MAX_CONFIGURATION: usize = 2 * 1024 * 1024;
const BROKEN: &str = "runtime_configuration";
const USER: &str = "10001:10001";
const SERVER: &str = "/app/simplestChat";
/// Where the launcher's deployment keeps the database's socket.
const SOCKETS: &str = "/srv/simplestchat-public/postgres-socket";
/// Loader and crypto-provider overrides, by the start of their names.
const FORBIDDEN_PREFIXES: [&str; 2] = ["LD_", "OPENSSL_"];
/// Allocator, conversion-module and locale overrides.
const FORBIDDEN_NAMES: [&str; 3] = ["GLIBC_TUNABLES", "GCONV_PATH", "LOCPATH"];
/// What the current Compose model says of a service. Anything else that can
/// execute, lifecycle hooks included, needs review before it is admitted.
const SERVICE_FIELDS: [&str; 27] = [
    "build",
    "cap_drop",
    "command",
    "cpus",
    "depends_on",
    "entrypoint",
    "environment",
    "healthcheck",
    "image",
    "init",
    "labels",
    "logging",
    "mem_limit",
    "network_mode",
    "networks",
    "pids_limit",
    "ports",
    "profiles",
    "read_only",
    "restart",
    "security_opt",
    "stop_grace_period",
    "tmpfs",
    "ulimits",
    "user",
    "volumes",
    "working_dir",
];
const MOUNT_FIELDS: [&str; 5] = ["type", "source", "target", "read_only", "bind"];

/// Why a profile does not hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// A requirement that is not met, by its fixed code.
    Code(&'static str),
    /// A field that must be there and is not.
    Key,
    /// A value of the wrong shape.
    Value,
}

/// Nothing, or the first requirement that is not met.
pub(crate) type Judged = Result<(), Refusal>;

/// Nothing, or the fixed code the command answers with.
type Verdict = Result<(), &'static str>;

fn require(condition: bool, code: &'static str) -> Judged {
    match condition {
        true => Ok(()),
        false => Err(Refusal::Code(code)),
    }
}

fn text(value: &str) -> Json {
    Json::String(value.to_owned())
}

fn list(values: &[&str]) -> Json {
    Json::Array(values.iter().map(|value| text(value)).collect())
}

/// Whether a field is exactly `expected`.
fn is(value: Option<&Json>, expected: &Json) -> bool {
    value.is_some_and(|value| value.semantic_eq(expected))
}

/// Whether a field is left out, null or exactly `expected`.
fn unset_or(value: Option<&Json>, expected: &Json) -> bool {
    value.is_none_or(|value| matches!(value, Json::Null) || value.semantic_eq(expected))
}

/// Whether a field is left out or null.
fn unset(value: Option<&Json>) -> bool {
    value.is_none_or(|value| matches!(value, Json::Null))
}

/// Refuse a repeated name, a name that is not a plain identifier, and every
/// loader, crypto-provider and locale-module override, empty ones included.
/// A repeated name is refused before any other name is looked at.
pub(crate) fn environment_names(names: &[&str]) -> Judged {
    let repeated = names
        .iter()
        .enumerate()
        .any(|(index, name)| names[..index].contains(name));
    require(!repeated, "runtime_environment_duplicate")?;
    let plain = |name: &str| {
        let start = |byte: u8| byte.is_ascii_alphabetic() || byte == b'_';
        name.bytes().next().is_some_and(start)
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    };
    let forbidden = |name: &str| {
        FORBIDDEN_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
            || FORBIDDEN_NAMES.contains(&name)
    };
    let admitted = names.iter().all(|name| plain(name) && !forbidden(name));
    require(admitted, "runtime_environment_override")
}

/// Require the image's own execution defaults.
fn image(config: &Json) -> Judged {
    let defaults = is(config.get("User"), &text(USER))
        && is(config.get("WorkingDir"), &text("/app"))
        && is(config.get("Cmd"), &list(&[SERVER]))
        && unset_or(config.get("Entrypoint"), &list(&[]))
        && unset(config.get("Healthcheck"));
    require(defaults, "runtime_image_execution")?;
    let assignments = config.get("Env").ok_or(Refusal::Key)?;
    let mut names = Vec::new();
    for assignment in assignments.array().ok_or(Refusal::Value)? {
        let assignment = assignment.string().ok_or(Refusal::Value)?;
        let assigned = assignment.split_once('=');
        let (name, _) = assigned.ok_or(Refusal::Code("runtime_environment_assignment"))?;
        names.push(name);
    }
    environment_names(&names)
}

/// Require one managed service to run `selected` as shipped: its user,
/// command and isolation, no forbidden environment name, the migration
/// switch of its role, the one read-only mount of the socket directory and
/// its bounded temporary filesystem.
fn service(service: &Json, migration: bool, selected: &str, sockets: &str) -> Judged {
    let fields = service.object().ok_or(Refusal::Value)?;
    let field = |name: &str| fields.get(name);
    require(is(field("image"), &text(selected)), "runtime_compose_image")?;
    let known = fields
        .keys()
        .all(|name| SERVICE_FIELDS.contains(&name.as_str()));
    require(known, "runtime_compose_fields")?;
    let execution = is(field("user"), &text(USER))
        && unset_or(field("working_dir"), &text("/app"))
        && unset_or(field("command"), &list(&[SERVER]))
        && unset_or(field("entrypoint"), &list(&[]))
        && unset(field("healthcheck"));
    require(execution, "runtime_compose_execution")?;
    let isolation = matches!(field("read_only"), Some(Json::Bool(true)))
        && matches!(field("init"), Some(Json::Bool(true)))
        && is(field("cap_drop"), &list(&["ALL"]))
        && is(field("security_opt"), &list(&["no-new-privileges:true"]));
    require(isolation, "runtime_compose_isolation")?;
    let environment = field("environment").ok_or(Refusal::Key)?;
    let environment = environment.object().ok_or(Refusal::Value)?;
    let names: Vec<&str> = environment.keys().map(String::as_str).collect();
    environment_names(&names)?;
    let switch = if migration { "true" } else { "false" };
    let values = environment.values().all(|value| value.string().is_some())
        && environment.get("RUN_MIGRATIONS").and_then(Json::string) == Some(switch);
    require(values, "runtime_compose_environment")?;
    let mounts = field("volumes").ok_or(Refusal::Key)?;
    let mounts = mounts.array().ok_or(Refusal::Value)?;
    require(mounts.len() == 1, "runtime_compose_mounts")?;
    let mount = mounts[0].object().ok_or(Refusal::Value)?;
    let created = Json::Object(
        BTreeMap::from([("create_host_path".to_owned(), Json::Bool(true))]),
        Vec::new(),
    );
    let nothing = Json::Object(BTreeMap::new(), Vec::new());
    let bind = mount.get("bind");
    let socket = mount
        .keys()
        .all(|name| MOUNT_FIELDS.contains(&name.as_str()))
        && is(mount.get("type"), &text("bind"))
        && is(mount.get("source"), &text(sockets))
        && is(mount.get("target"), &text("/run/simplestchat-postgres"))
        && matches!(mount.get("read_only"), Some(Json::Bool(true)))
        && (bind.is_none() || is(bind, &nothing) || is(bind, &created));
    require(socket, "runtime_compose_mounts")?;
    // The container's own bounded mount without execution, not a host path.
    let size = if migration { "32m" } else { "64m" };
    let temporary = format!("/tmp:rw,nosuid,nodev,noexec,size={size},mode=1777");
    require(
        is(field("tmpfs"), &list(&[&temporary])),
        "runtime_compose_tmpfs",
    )
}

/// Require the application of a resolved model, and with `migration` its
/// migration service, to run `selected` with the database's socket
/// directory `sockets` as its one mount.
pub(crate) fn compose(config: &Json, sockets: &str, selected: &str, migration: bool) -> Judged {
    let services = config.get("services").ok_or(Refusal::Key)?;
    let services = services.object().ok_or(Refusal::Value)?;
    require(image_id(selected), "runtime_image_selection")?;
    let roles = [("simplestchat", false), ("migrate", true)];
    for (name, role) in roles.into_iter().take(1 + usize::from(migration)) {
        let judged = services.get(name).ok_or(Refusal::Key)?;
        service(judged, role, selected, sockets)?;
    }
    Ok(())
}

/// What is asked about.
#[derive(Debug, PartialEq, Eq)]
enum Kind {
    /// An image's own configuration.
    Image,
    /// A resolved Compose model and the identity of the image it must run.
    Compose(String),
}

/// Judge one document. A field that is not there and a value of the wrong
/// shape are one fixed code for the command.
fn verdict(kind: &Kind, document: &[u8]) -> Verdict {
    let bounded = (1..=MAX_CONFIGURATION).contains(&document.len());
    if !bounded {
        return Err("runtime_configuration_size");
    }
    let value = json::decode_limited(document, MAX_CONFIGURATION).map_err(|_| BROKEN)?;
    let judged = match kind {
        _ if value.object().is_none() => Err(Refusal::Value),
        Kind::Image => image(&value),
        Kind::Compose(selected) => compose(&value, SOCKETS, selected, true),
    };
    judged.map_err(|refusal| match refusal {
        Refusal::Code(code) => code,
        Refusal::Key | Refusal::Value => BROKEN,
    })
}

/// The one line that answers, and whether it says the profile holds.
fn line(verdict: Verdict) -> (String, bool) {
    match verdict {
        Ok(()) => ("{\"passed\": true}\n".to_owned(), true),
        Err(code) => (
            format!("{{\"passed\": false, \"failure\": \"{code}\"}}\n"),
            false,
        ),
    }
}

/// `image`, `compose --image-id ID` or `compose --image-id=ID`, or nothing
/// for any other command line.
fn parse(arguments: &[OsString]) -> Option<Kind> {
    let words: Option<Vec<&str>> = arguments.iter().map(|word| word.to_str()).collect();
    match words?[..] {
        ["image"] => Some(Kind::Image),
        ["compose", "--image-id", selected] => Some(Kind::Compose(selected.to_owned())),
        ["compose", joined] => {
            let selected = joined.strip_prefix("--image-id=")?;
            Some(Kind::Compose(selected.to_owned()))
        }
        _ => None,
    }
}

/// Run `release profile image` or `release profile compose --image-id ID`
/// on the document given on standard input. A syntax error returns 2 before
/// anything is read; otherwise one line is printed, and only a profile that
/// holds returns success.
pub fn run(arguments: &[OsString]) -> ExitCode {
    let Some(kind) = parse(arguments) else {
        return ExitCode::from(2);
    };
    let mut document = Vec::new();
    let limit = u64::try_from(MAX_CONFIGURATION)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let read = std::io::stdin()
        .lock()
        .take(limit)
        .read_to_end(&mut document);
    let judged = read
        .map_err(|_| BROKEN)
        .and_then(|_| verdict(&kind, &document));
    let (line, holds) = line(judged);
    match emit(std::io::stdout(), &line) && holds {
        true => ExitCode::SUCCESS,
        false => ExitCode::FAILURE,
    }
}
