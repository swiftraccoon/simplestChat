//! What the fixture accepts from the engine, the Ops executable and the
//! browser helper. Each check reads already bounded bytes or decoded JSON and
//! answers with a fixed refusal; none runs anything.

use super::{
    host::{Fault, Result, require},
    json::{self, integer, object},
    plan::{LABEL, PROJECT},
};
use serde_json::{Map, Value, json};

/// The browser helper's fixed failure stages; any other text is unavailable.
pub(super) const ROLLOVER_STAGES: [&str; 36] = [
    "backend-release",
    "backend-request",
    "backend-status",
    "browser-context",
    "browser-launch",
    "browser-module",
    "capture-guard",
    "cleanup",
    "configuration",
    "deferred-import",
    "deferred-revision",
    "deferred-status",
    "deploy-candidate",
    "deploy-command",
    "deploy-result",
    "document-cache",
    "document-capture",
    "document-chunk",
    "document-identity",
    "document-navigation",
    "document-page",
    "document-revision",
    "document-state",
    "document-status",
    "identity",
    "load-new",
    "load-old",
    "open-candidate",
    "open-original",
    "report-write",
    "rollback",
    "setup-or-cleanup",
    "version-cache",
    "version-request",
    "version-state",
    "version-status",
];
/// The longest helper stderr a stage is read from.
pub(super) const MAX_BROWSER_ERROR: usize = 1024;
/// The longest browser report accepted.
pub(super) const MAX_BROWSER_REPORT: u64 = 4096;
const MAX_VALIDATOR_ERROR: usize = 2048;
const MAX_VALIDATOR_STDERR: usize = 4096;
const OWNERSHIP: &str = "Container ownership does not match this fixture";
const MALFORMED: &str = "Container inspection is malformed";

fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// A 64-digit engine identity, as a container or network ID is listed.
pub(super) fn identity(value: &str) -> bool {
    hex(value, 64)
}

/// An immutable `sha256:` image identity.
pub(super) fn image(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|digest| hex(digest, 64))
}

/// Python's truth value of a decoded JSON value.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(number) => number.as_f64().is_some_and(|value| value != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(values) => !values.is_empty(),
        Value::Object(values) => !values.is_empty(),
    }
}

/// Python's `value == number` for a decoded JSON value.
fn equals(value: &Value, number: i64) -> bool {
    match value {
        Value::Bool(value) => i64::from(*value) == number,
        Value::Number(value) => value.as_f64() == Some(number as f64),
        _ => false,
    }
}

/// The inspection of `identity`, once it is exactly that container of this
/// run's project with one of the fixture's service labels.
pub(super) fn owned(identity: &str, inspection: &str, token: &str) -> Result<Value> {
    require(self::identity(identity), "Container identity must be exact")?;
    let value = json::decode(inspection.as_bytes()).ok_or(Fault::Check(MALFORMED))?;
    let labels = object(&value)
        .and_then(|value| value.get("labels"))
        .and_then(object)
        .ok_or(Fault::Check(MALFORMED))?;
    let text = |key: &str| labels.get(key).and_then(Value::as_str);
    require(
        value.get("id").and_then(Value::as_str) == Some(identity)
            && text(LABEL) == Some(token)
            && text("com.docker.compose.project") == Some(PROJECT)
            && text("com.docker.compose.service")
                .is_some_and(|service| super::plan::SERVICES.contains(&service)),
        OWNERSHIP,
    )?;
    Ok(value)
}

/// The compose service label of an owned inspection.
pub(super) fn service(value: &Value) -> Option<&str> {
    value
        .get("labels")?
        .get("com.docker.compose.service")?
        .as_str()
}

/// Refuse anything but a clean stop, or the deliberately failed candidate's
/// fixed exit 42 when `deliberate`.
pub(super) fn healthy_exit(value: &Value, deliberate: bool) -> Result<()> {
    let state = value
        .get("state")
        .and_then(object)
        .ok_or(Fault::Check(MALFORMED))?;
    let field = |key: &str| state.get(key).ok_or(Fault::Check(MALFORMED));
    let (running, killed, error, code) = (
        field("Running")?,
        field("OOMKilled")?,
        field("Error")?,
        field("ExitCode")?,
    );
    require(
        !truthy(running)
            && !truthy(killed)
            && !truthy(error)
            && equals(code, if deliberate { 42 } else { 0 }),
        "Owned container did not exit as expected",
    )
}

/// What proves a retained dependency was not restarted: its identity, image,
/// start time and restart count.
pub(super) fn stable(value: &Value) -> Result<[Value; 4]> {
    let field = |value: Option<&Value>| value.cloned().ok_or(Fault::Check(MALFORMED));
    Ok([
        field(value.get("id"))?,
        field(value.get("image"))?,
        field(value.get("state").and_then(|state| state.get("StartedAt")))?,
        field(value.get("restarts"))?,
    ])
}

/// The local engine's checks before the fixture creates anything.
pub(super) mod engine {
    use super::*;

    pub(in super::super) fn empty_containers(listing: &str) -> Result<()> {
        require(
            listing.is_empty(),
            "Existing containers; use an empty disposable engine",
        )
    }
    pub(in super::super) fn empty_networks(listing: &str) -> Result<()> {
        require(listing.is_empty(), "Existing public Compose network")
    }
    /// `docker image save` must offer `--platform` as an option of its own.
    pub(in super::super) fn exports_platform(help: &str) -> Result<()> {
        let option = regex::Regex::new(r"(?m)^\s+--platform(?:\s|$)").expect("a fixed expression");
        require(
            option.is_match(help),
            "Docker image export requires --platform support",
        )
    }
    /// The server API version, at least 1.48.
    pub(in super::super) fn api(version: &str) -> Result<()> {
        let parts: Vec<&str> = version.split('.').collect();
        let numbers: Vec<u32> = parts
            .iter()
            .filter(|part| {
                (1..=3).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_digit())
            })
            .filter_map(|part| part.parse().ok())
            .collect();
        require(
            parts.len() == 2 && numbers.len() == 2 && (numbers[0], numbers[1]) >= (1, 48),
            "Docker server API 1.48 or newer is required",
        )
    }
    /// The default builder must be the engine's own driver, exactly once.
    pub(in super::super) fn builder(inspection: &str) -> Result<()> {
        let driver = regex::Regex::new(r"(?m)^Driver:\s+(\S+)\s*$").expect("a fixed expression");
        let drivers: Vec<&str> = driver
            .captures_iter(inspection)
            .filter_map(|found| found.get(1).map(|value| value.as_str()))
            .collect();
        require(
            drivers == ["docker"],
            "Fixture builds require the default local Docker builder",
        )
    }
    /// The selected production image: Linux/AMD64, the application user and
    /// command, no entrypoint, and an immutable identity, which is returned.
    pub(in super::super) fn production(inspection: &str) -> Result<String> {
        let refused = Fault::Check("Select the already-built Linux/amd64 production image");
        let value = json::decode(inspection.as_bytes()).ok_or(refused.clone())?;
        let field = |key: &str| value.get(key);
        let identity = field("id").and_then(Value::as_str).filter(|id| image(id));
        let entrypoint = field("entrypoint");
        let accepted = field("os") == Some(&json!("linux"))
            && field("architecture") == Some(&json!("amd64"))
            && field("user") == Some(&json!("10001:10001"))
            && field("cmd") == Some(&json!(["/app/simplestChat"]))
            && (entrypoint == Some(&Value::Null) || entrypoint == Some(&json!([])));
        match identity {
            Some(identity) if accepted => Ok(identity.to_owned()),
            _ => Err(refused),
        }
    }
}

/// The response checker's exact success receipt; no field more or less,
/// and no number or boolean of another JSON type.
pub(super) fn receipt() -> Value {
    json!({
        "passed": true,
        "requests": 5,
        "scope": "owned-loopback-release-fixture",
        "securityHeaders": true,
        "contentSecurityPolicy": true,
        "jsonNoStore": true,
        "anonymousCookiesAbsent": true,
        "missingAuthorizationRejected": true,
        "privateEndpointsOmitted": true,
        "responseLimits": true,
    })
}

/// The browser helper's exact acceptance report.
pub(super) fn rollover() -> Value {
    json!({
        "passed": true,
        "editions": 2,
        "oldTabAfterUpgrade": true,
        "newTabAfterRollback": true,
        "backendMissesVerified": 2,
        "documentsUnchanged": true,
        "captureCalls": 0,
        "htmlUncached": true,
        "selectedVersionVerified": true,
    })
}

/// The fixed stage named by the browser helper's one-line failure, or
/// `unavailable` for anything else.
pub(super) fn browser_stage(stderr: &[u8]) -> &'static str {
    let Some(rest) = stderr.strip_prefix(b"Release asset rollover browser check failed: ") else {
        return "unavailable";
    };
    let stage = rest.strip_suffix(b"\n").unwrap_or(rest);
    if stderr.len() > MAX_BROWSER_ERROR
        || stage.is_empty()
        || !stage
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || *byte == b'-')
    {
        return "unavailable";
    }
    ROLLOVER_STAGES
        .iter()
        .find(|known| known.as_bytes() == stage)
        .copied()
        .unwrap_or("unavailable")
}

/// The fixed identity and phase fields of one release attempt's outcome.
pub(super) fn sanitized(outcome: &Map<String, Value>) -> Value {
    let kept = [
        "action",
        "revision",
        "phase",
        "passed",
        "failure",
        "rollbackAttempted",
        "rollbackPassed",
    ];
    Value::Object(
        kept.iter()
            .filter_map(|key| {
                outcome
                    .get(*key)
                    .map(|value| ((*key).to_owned(), value.clone()))
            })
            .collect(),
    )
}

/// One release attempt's verdict: its outcome names `action` and
/// `revision`, and its exit status and `passed` agree with whether the
/// attempt was meant to fail.
pub(super) fn release(
    action: &str,
    revision: &str,
    status: Option<i32>,
    failure: bool,
    outcome: &Map<String, Value>,
) -> Result<()> {
    let malformed = Fault::Check("Release evidence is malformed");
    let (named, released) = (outcome.get("action"), outcome.get("revision"));
    require(
        named.ok_or(malformed.clone())?.as_str() == Some(action)
            && released.ok_or(malformed)?.as_str() == Some(revision),
        "Release evidence identity mismatch",
    )?;
    let exited = if failure {
        status != Some(0)
    } else {
        status == Some(0)
    };
    require(
        exited && outcome.get("passed") == Some(&Value::Bool(!failure)),
        "Release exit status and original outcome disagree",
    )
}

/// The failed candidate's retained evidence among the attempt's stdout
/// texts: the fixed startup marker, and at least one inspection of the
/// candidate image whose state has an integer exit code and no OOM kill.
pub(super) fn failed_start(outputs: &[String], image: &str) -> Result<()> {
    require(
        outputs
            .iter()
            .any(|text| text.contains(super::plan::FAILURE_MARKER)),
        "Failed startup log was not retained",
    )?;
    let captured = outputs.iter().filter_map(|text| {
        let value = json::decode(text.as_bytes())?;
        let value = object(&value)?.clone();
        (value.get("image").and_then(Value::as_str) == Some(image))
            .then(|| value.get("state").and_then(object).cloned())
            .flatten()
    });
    let mut captured = captured;
    require(
        captured.any(|state| {
            integer(state.get("ExitCode")).is_some()
                && matches!(state.get("ExitCode"), Some(Value::Number(_)))
                && state.get("OOMKilled") == Some(&Value::Bool(false))
        }),
        "Failed candidate image/state evidence was not retained",
    )
}

/// Why the validator of a failed release is not described.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Unavailable {
    Name,
    Inspection,
    Ownership,
    State,
}
impl Unavailable {
    pub(super) fn reason(self) -> &'static str {
        match self {
            Self::Name => "ValidatorNameUnavailable",
            Self::Inspection => "ValidatorInspectionUnavailable",
            Self::Ownership => "ValidatorOwnershipMismatch",
            Self::State => "ValidatorStateUnavailable",
        }
    }
}

/// The validator container's fixed name, read from its bounded name file.
pub(super) fn validator_name(bytes: &[u8]) -> std::result::Result<String, Unavailable> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| Unavailable::Name)?
        .trim();
    let valid = text
        .strip_prefix("scpub-release-validate-")
        .is_some_and(|suffix| hex(suffix, 32));
    if valid {
        Ok(text.to_owned())
    } else {
        Err(Unavailable::Name)
    }
}

/// The verified startup state of the validator `name` of the failed release
/// of `image`: its exit code, status, OOM flag and bounded error text, and
/// the container identity whose logs may be read.
pub(super) fn validator(
    name: &str,
    image: &str,
    inspection: &str,
) -> std::result::Result<(String, Value), Unavailable> {
    let value = json::decode(inspection.as_bytes()).ok_or(Unavailable::Inspection)?;
    let value = object(&value).ok_or(Unavailable::Inspection)?;
    let text = |key: &str| value.get(key).and_then(Value::as_str);
    let id = text("id")
        .filter(|id| identity(id))
        .ok_or(Unavailable::Ownership)?;
    let named = format!("/{name}");
    let owned = text("name") == Some(named.as_str())
        && text("image") == Some(image)
        && text("user") == Some("10001:10001")
        && value.get("entrypoint") == Some(&json!(["/usr/bin/timeout"]))
        && text("network") == Some("none")
        && value.get("readOnly") == Some(&Value::Bool(true));
    if !owned {
        return Err(Unavailable::Ownership);
    }
    let state = value
        .get("state")
        .and_then(object)
        .ok_or(Unavailable::State)?;
    let code = match state.get("ExitCode") {
        Some(Value::Number(number)) if !number.is_f64() => number.as_i64(),
        _ => None,
    };
    let killed = state.get("OOMKilled").and_then(Value::as_bool);
    let error = state.get("Error").and_then(Value::as_str);
    let status = state.get("Status").and_then(Value::as_str);
    let statuses = [
        "created",
        "running",
        "paused",
        "restarting",
        "removing",
        "exited",
        "dead",
    ];
    let (Some(code), Some(killed), Some(error), Some(status)) = (code, killed, error, status)
    else {
        return Err(Unavailable::State);
    };
    if !(0..=255).contains(&code) || !statuses.contains(&status) {
        return Err(Unavailable::State);
    }
    let shown: String = error.chars().take(MAX_VALIDATOR_ERROR).collect();
    Ok((
        id.to_owned(),
        json!({
            "diagnosticUnavailable": false,
            "exitCode": code,
            "status": status,
            "oomKilled": killed,
            "error": shown,
            "errorTruncated": error.chars().count() > MAX_VALIDATOR_ERROR,
        }),
    ))
}

/// The validator's bounded runtime stderr, as the report may show it.
pub(super) fn validator_logs(stderr: &[u8]) -> Value {
    let shown = &stderr[..stderr.len().min(MAX_VALIDATOR_STDERR)];
    json!({
        "diagnosticUnavailable": false,
        "stderr": String::from_utf8_lossy(shown),
        "stderrTruncated": stderr.len() > MAX_VALIDATOR_STDERR,
    })
}

/// The number of validator stderr bytes read: one more than is shown.
pub(super) const VALIDATOR_STDERR_READ: u64 = MAX_VALIDATOR_STDERR as u64 + 1;
