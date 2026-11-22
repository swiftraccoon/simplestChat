//! The Compose model a snapshot checks, the placement of a failed check in
//! its phase and slot, and the decoding rules every check shares. Equality is
//! JSON's own (an integer equals the float of its value, as the deployment's
//! tools compare), and each type test is exact: a boolean is never an integer.

use super::{CODES, Diagnostic};
use crate::host::failure::{Failure, Result, require};
use crate::json::Json;
use std::collections::BTreeSet;

/// The most memory, in bytes, a service may be given.
pub(crate) const MAX_MEMORY: u64 = 8 * 1024 * 1024 * 1024;
/// The most processes a service may run.
pub(crate) const MAX_PIDS: i64 = 1024;
const MAX_CPUS: f64 = 8.0;
/// The public project's Compose name.
pub(crate) const PROJECT: &str = "simplestchat-public";
/// The project's services and the user each runs as.
pub(crate) const SERVICES: [(&str, &str); 4] = [
    ("simplestchat", "10001:10001"),
    ("migrate", "10001:10001"),
    ("postgres", "999:999"),
    ("caddy", "10001:10001"),
];
/// The names of the synthetic rows the database action adds and a restore
/// must return.
pub(crate) const COUNT_NAMES: [&str; 6] = [
    "users",
    "rooms",
    "sessions",
    "credentials",
    "activeIncidents",
    "resolvedIncidents",
];
const COMPOSE_USER: &str = "Fixture Compose user differs";
const COMPOSE_ISOLATION: &str = "Fixture Compose isolation differs";
const COMPOSE_CAPABILITIES: &str = "Fixture Compose added capabilities differ";
const COMPOSE_PRIVILEGES: &str = "Fixture Compose privilege boundary differs";
const COMPOSE_RESOURCES: &str = "Fixture Compose resource limits differ";
const COMPOSE_CPUS: &str = "Fixture Compose CPU limit differs";
const COMPOSE_NETWORK: &str = "Fixture isolated network differs";
const COMPOSE_INIT: &str = "Fixture Compose init differs";
const COMPOSE_HTTP: &str = "Fixture HTTP publication is not loopback-only";
const COMPOSE_PROJECT: &str = "Fixture Compose project differs";
const COMPOSE_SERVICES: &str = "Fixture Compose services differ";

/// A failed check: bare, or already placed in the phase and slot it ran in.
/// The innermost placement wins, as nested checks are reported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Fault {
    Bare(Failure),
    Placed(Diagnostic),
}

impl From<Failure> for Fault {
    fn from(failure: Failure) -> Self {
        Self::Bare(failure)
    }
}

impl Fault {
    /// This fault's labels, placing a bare failure in `phase` and `context`.
    pub(crate) fn placed(self, phase: &str, context: &str) -> Diagnostic {
        match self {
            Self::Placed(diagnostic) => diagnostic,
            Self::Bare(failure) => diagnose(phase, context, &failure),
        }
    }
}

/// Place any bare failure of `result` in `phase` and `context`.
pub(crate) fn within<T>(
    phase: &str,
    context: &str,
    result: std::result::Result<T, Fault>,
) -> std::result::Result<T, Fault> {
    result.map_err(|fault| Fault::Placed(fault.placed(phase, context)))
}

/// The labels of `failure` in `phase` and `context`: a refusal is coded by its
/// message, a failed command is a release error and an expired one a
/// subprocess error, and unreviewed phases and contexts fall back.
pub(crate) fn diagnose(phase: &str, context: &str, failure: &Failure) -> Diagnostic {
    let (code, class) = match failure {
        Failure::Refused(message) => {
            let code = CODES.iter().find(|(text, _)| text == message);
            (
                code.map_or("unclassified", |(_, code)| *code),
                "release_error",
            )
        }
        Failure::Command => ("unclassified", "release_error"),
        Failure::Timeout => ("unclassified", "subprocess_error"),
        Failure::Os => ("unclassified", "os_error"),
        Failure::Value => ("unclassified", "value_error"),
        Failure::Key => ("unclassified", "key_error"),
    };
    Diagnostic::labelled(phase, context, code, class)
}

/// The lowercase hexadecimal SHA-256 of `bytes`.
pub(crate) fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(crate) fn text(value: &str) -> Json {
    Json::String(value.to_owned())
}

pub(crate) fn strings(values: &[&str]) -> Json {
    Json::Array(values.iter().map(|value| text(value)).collect())
}

/// Whether a field is present and equal to `expected`.
pub(crate) fn is(value: Option<&Json>, expected: &Json) -> bool {
    value.is_some_and(|value| value.semantic_eq(expected))
}

/// Whether a field is exactly the boolean `expected`.
pub(crate) fn exactly(value: Option<&Json>, expected: bool) -> bool {
    matches!(value, Some(Json::Bool(actual)) if *actual == expected)
}

/// Whether `value` is an integer within `low..=high`.
pub(crate) fn bounded(value: Option<&Json>, low: i64, high: i64) -> bool {
    value
        .and_then(Json::integer)
        .is_some_and(|number| (low..=high).contains(&number))
}

/// The object `bytes` decode to.
pub(crate) fn object(bytes: &[u8]) -> Result<Json> {
    let value = crate::json::decode(bytes).map_err(|_| Failure::Value)?;
    value.object().ok_or(Failure::Value)?;
    Ok(value)
}

pub(crate) fn field<'a>(value: &'a Json, name: &str) -> Result<&'a Json> {
    value.get(name).ok_or(Failure::Key)
}

/// A number as `float()` reads ASCII text: surrounding Unicode white space
/// (not the separators U+001C to U+001F), a sign, underscores between
/// digits, `inf` and `nan` are admitted; any other spelling, a non-ASCII
/// digit among them, is not a number.
fn number(text: &str) -> Result<f64> {
    let trimmed = text.trim();
    let bytes = trimmed.as_bytes();
    let mut plain = String::new();
    for (at, byte) in bytes.iter().enumerate() {
        if *byte == b'_' {
            let between = at > 0
                && bytes[at - 1].is_ascii_digit()
                && bytes.get(at + 1).is_some_and(u8::is_ascii_digit);
            if !between {
                return Err(Failure::Value);
            }
        } else {
            plain.push(char::from(*byte));
        }
    }
    plain.parse::<f64>().map_err(|_| Failure::Value)
}

/// The effective Compose settings of service `name`: its own user, a
/// read-only root, every capability dropped (only the proxy may bind low
/// ports), no privilege gain, bounded memory, processes and CPUs, no network
/// for the database and the migrator, an init process for all but the
/// database, and application HTTP published on loopback only.
pub(crate) fn service(value: &Json, name: &str) -> Result<()> {
    let user = SERVICES.iter().find(|(service, _)| *service == name);
    require(
        user.is_some_and(|(_, user)| is(value.get("user"), &text(user))),
        COMPOSE_USER,
    )?;
    let isolated = exactly(value.get("read_only"), true)
        && is(value.get("cap_drop"), &strings(&["ALL"]))
        && value
            .get("privileged")
            .is_none_or(|value| matches!(value, Json::Bool(false)));
    require(isolated, COMPOSE_ISOLATION)?;
    let added = if name == "caddy" {
        strings(&["NET_BIND_SERVICE"])
    } else {
        strings(&[])
    };
    let capabilities = value
        .get("cap_add")
        .unwrap_or(&strings(&[]))
        .semantic_eq(&added);
    require(capabilities, COMPOSE_CAPABILITIES)?;
    let boundary = is(
        value.get("security_opt"),
        &strings(&["no-new-privileges:true"]),
    );
    require(boundary, COMPOSE_PRIVILEGES)?;
    let memory = value.get("mem_limit").and_then(Json::string);
    let decimal = memory.is_some_and(|memory| {
        memory
            .bytes()
            .next()
            .is_some_and(|byte| (b'1'..=b'9').contains(&byte))
            && memory.parse::<u64>().is_ok_and(|bytes| bytes <= MAX_MEMORY)
    });
    let pids = bounded(value.get("pids_limit"), 1, MAX_PIDS);
    require(decimal && pids, COMPOSE_RESOURCES)?;
    let cpus = match value.get("cpus") {
        Some(Json::String(spelled)) => Some(number(spelled)?),
        Some(Json::Integer(spelled) | Json::Float(spelled)) => Some(number(spelled)?),
        _ => None,
    };
    require(
        cpus.is_some_and(|cpus| 0.0 < cpus && cpus <= MAX_CPUS),
        COMPOSE_CPUS,
    )?;
    if matches!(name, "postgres" | "migrate") {
        require(
            is(value.get("network_mode"), &text("none")),
            COMPOSE_NETWORK,
        )?;
    }
    if name != "postgres" {
        require(exactly(value.get("init"), true), COMPOSE_INIT)?;
    }
    if name == "simplestchat" {
        let ports = field(value, "ports")?.array().ok_or(Failure::Value)?;
        let mut http = Vec::new();
        for port in ports {
            port.object().ok_or(Failure::Value)?;
            if is(port.get("protocol"), &text("tcp")) {
                http.push(port);
            }
        }
        let loopback = http.len() == 1
            && is(http[0].get("target"), &Json::Integer("3000".into()))
            && is(http[0].get("published"), &text("3000"))
            && is(http[0].get("host_ip"), &text("127.0.0.1"));
        require(loopback, COMPOSE_HTTP)?;
    }
    Ok(())
}

/// The rendered Compose model of the public project: its name, exactly its
/// four services and each service's settings, in the model's own order. A
/// service's failure is placed in that service's slot. Returns the number of
/// services and the model's digest over its sorted spelling.
pub(crate) fn compose(document: &Json) -> std::result::Result<(usize, String), Fault> {
    document.object().ok_or(Failure::Value)?;
    require(is(document.get("name"), &text(PROJECT)), COMPOSE_PROJECT)?;

    let Some(Json::Object(services, order)) = document.get("services") else {
        return Err(Fault::Bare(if document.get("services").is_some() {
            Failure::Value
        } else {
            Failure::Key
        }));
    };
    let names: BTreeSet<&str> = services.keys().map(String::as_str).collect();
    let expected: BTreeSet<&str> = SERVICES.iter().map(|(name, _)| *name).collect();
    require(names == expected, COMPOSE_SERVICES)?;
    for name in order {
        let checked = services
            .get(name)
            .filter(|service| service.object().is_some())
            .ok_or(Failure::Value)
            .and_then(|service| self::service(service, name));
        within("snapshot_compose", name, checked.map_err(Fault::Bare))?;
    }
    Ok((
        services.len(),
        sha256(crate::json::python_dump(document, true).as_bytes()),
    ))
}
