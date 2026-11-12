//! The managed server's execution profile: what its image must declare and
//! what the resolved project may ask of it. Configuration names and values
//! never enter a diagnostic; a mismatch is one fixed kind of fault.

use crate::host::fault::{Fault, Result, VALUE, field, require, text};
use crate::host::image_id;
use crate::json::Json;
use std::collections::BTreeSet;

const COMMAND: &str = "/app/simplestChat";
/// Fields the current canonical project model emits for a service. Any other
/// execution feature, lifecycle hooks included, needs review first.
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

/// An execution profile that is not the managed one.
const PROFILE: Fault = Fault::Kind("RuntimeProfileError");

fn profile(condition: bool) -> Result<()> {
    if condition { Ok(()) } else { Err(PROFILE) }
}

/// A field that may be absent or null; both read as nothing.
fn optional<'a>(value: &'a Json, name: &str) -> Option<&'a Json> {
    value.get(name).filter(|item| !matches!(item, Json::Null))
}

fn is_text(value: Option<&Json>, expected: &str) -> bool {
    value.and_then(Json::string) == Some(expected)
}

/// Whether `value` is a list of exactly these texts.
fn is_list(value: Option<&Json>, expected: &[&str]) -> bool {
    value.and_then(Json::array).is_some_and(|items| {
        items.len() == expected.len()
            && items
                .iter()
                .zip(expected)
                .all(|(item, text)| item.string() == Some(*text))
    })
}

fn empty_list(value: &Json) -> bool {
    value.array().is_some_and(<[Json]>::is_empty)
}

/// Reject repeated, malformed and loader, crypto-provider or locale-module
/// override names.
pub(super) fn environment_names(names: &[&str]) -> Result<()> {
    let distinct: BTreeSet<&&str> = names.iter().collect();
    profile(distinct.len() == names.len())?;
    let shaped = |name: &&str| {
        let mut bytes = name.bytes();
        bytes
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
            && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    };
    let forbidden = |name: &&str| {
        name.starts_with("LD_")
            || name.starts_with("OPENSSL_")
            || ["GLIBC_TUNABLES", "GCONV_PATH", "LOCPATH"].contains(name)
    };
    profile(names.iter().all(shaped) && !names.iter().any(forbidden))
}

/// Require the inspected image to be the reviewed runtime of `revision` and
/// return its identity.
pub(crate) fn image_identity(inspected: &Json, revision: &str) -> Result<String> {
    inspected.object().ok_or(VALUE)?;
    let identifier = text(field(inspected, "id")?)?;
    require(
        image_id(identifier),
        "Missing local content-addressed image ID",
    )?;
    for (name, expected) in [
        ("os", "linux"),
        ("architecture", "amd64"),
        ("user", "10001:10001"),
    ] {
        let matches = is_text(Some(field(inspected, name)?), expected);
        require(matches, "Unexpected image platform or runtime user")?;
    }
    let labels = field(inspected, "labels")?;
    labels.object().ok_or(VALUE)?;
    let label = labels.get("org.opencontainers.image.revision");
    require(is_text(label, revision), "Image revision mismatch")?;
    let command = is_list(Some(field(inspected, "cmd")?), &[COMMAND]);
    require(command, "Unexpected image entrypoint")?;
    let entrypoint = field(inspected, "entrypoint")?;
    let none = matches!(entrypoint, Json::Null) || empty_list(entrypoint);
    require(none, "Unexpected image entrypoint")?;
    let directory = is_text(Some(field(inspected, "workingDir")?), "/app");
    require(directory, "Unexpected image execution defaults")?;
    let unchecked = field(inspected, "hasHealthcheck")?.boolean() == Some(false);
    require(unchecked, "Unexpected image execution defaults")?;
    let names = field(inspected, "envNames")?.array().ok_or(VALUE)?;
    let names = names.iter().map(text).collect::<Result<Vec<&str>>>()?;
    environment_names(&names)?;
    Ok(identifier.to_owned())
}

/// Validate the resolved application service before it is stopped or
/// started: the selected image, only reviewed fields, the fixed user and
/// command, a read-only root with every capability dropped, an environment
/// of plain text without overrides, the one socket mount and the bounded
/// temporary filesystem.
pub(super) fn compose(resolved: &Json, socket: &str, image: &str) -> Result<()> {
    let services = field(resolved, "services")?;
    services.object().ok_or(VALUE)?;
    profile(image_id(image))?;
    let service = field(services, "simplestchat")?;
    let fields = service.object().ok_or(VALUE)?;
    profile(is_text(service.get("image"), image))?;
    profile(
        fields
            .keys()
            .all(|name| SERVICE_FIELDS.contains(&name.as_str())),
    )?;
    let absent_or =
        |name: &str, accept: &dyn Fn(&Json) -> bool| optional(service, name).is_none_or(accept);
    let execution = is_text(service.get("user"), "10001:10001")
        && absent_or("working_dir", &|value| value.string() == Some("/app"))
        && absent_or("command", &|value| is_list(Some(value), &[COMMAND]))
        && absent_or("entrypoint", &empty_list)
        && optional(service, "healthcheck").is_none();
    profile(execution)?;
    let isolation = service.get("read_only").and_then(Json::boolean) == Some(true)
        && service.get("init").and_then(Json::boolean) == Some(true)
        && is_list(service.get("cap_drop"), &["ALL"])
        && is_list(service.get("security_opt"), &["no-new-privileges:true"]);
    profile(isolation)?;
    let environment = field(service, "environment")?.object().ok_or(VALUE)?;
    let names: Vec<&str> = environment.keys().map(String::as_str).collect();
    environment_names(&names)?;
    let plain = environment.values().all(|value| value.string().is_some())
        && is_text(environment.get("RUN_MIGRATIONS"), "false");
    profile(plain)?;
    let mounts = field(service, "volumes")?.array().ok_or(VALUE)?;
    profile(mounts.len() == 1)?;
    let mount = mounts[0].object().ok_or(VALUE)?;
    let known = ["type", "source", "target", "read_only", "bind"];
    let bind = match mount.get("bind") {
        None => true,
        Some(Json::Object(options, _)) => {
            options.is_empty()
                || (options.len() == 1
                    && options.get("create_host_path").and_then(Json::boolean) == Some(true))
        }
        Some(_) => false,
    };
    let mounted = mount.keys().all(|name| known.contains(&name.as_str()))
        && is_text(mount.get("type"), "bind")
        && is_text(mount.get("source"), socket)
        && is_text(mount.get("target"), "/run/simplestchat-postgres")
        && mount.get("read_only").and_then(Json::boolean) == Some(true)
        && bind;
    profile(mounted)?;
    let temporary = ["/tmp:rw,nosuid,nodev,noexec,size=64m,mode=1777"];
    profile(is_list(service.get("tmpfs"), &temporary))
}

/// Whether a fault is one a readiness poll retries: a refusal or a value of
/// the wrong shape, but never a deadline of the probe itself.
pub(super) fn retried(fault: &Fault) -> bool {
    matches!(fault, Fault::Refused(_)) || *fault == VALUE
}
