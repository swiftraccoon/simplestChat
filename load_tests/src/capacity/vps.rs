//! Pure private-VPS request and evidence policy. No function starts a runtime,
//! process, SSH connection, engine, service or workload. Python remains the
//! operational reference until host-binary distribution and Tier 2 ownership are ported.

use super::json::{self, Json};
use anyhow::{Result, ensure};
use std::collections::{BTreeMap, BTreeSet};

const ROOT: &str = "/srv/simplestchat-bench";
const MAX_REQUEST: usize = 512 * 1024;
const MAX_JSON: usize = 8 * 1024 * 1024;

fn object(value: &Json) -> Result<&BTreeMap<String, Json>> {
    if let Json::Object(value) = value {
        Ok(value)
    } else {
        anyhow::bail!("expected_object")
    }
}
fn array(value: &Json) -> Result<&[Json]> {
    if let Json::Array(value) = value {
        Ok(value)
    } else {
        anyhow::bail!("expected_array")
    }
}
fn text(value: &Json) -> Result<&str> {
    value
        .string()
        .ok_or_else(|| anyhow::anyhow!("expected_string"))
}
fn decode(bytes: &[u8], maximum: usize) -> Result<Json> {
    ensure!(bytes.len() <= maximum, "json_too_large");
    json::parse(bytes).map_err(|_| anyhow::anyhow!("invalid_json"))
}
fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn integer_token(value: &str) -> bool {
    !value.contains(['.', 'e', 'E']) && !matches!(value, "NaN" | "Infinity" | "-Infinity")
}
fn number(value: &Json, minimum: f64, maximum: f64) -> Result<f64> {
    let Json::Number(_) = value else {
        anyhow::bail!("invalid_number");
    };
    let value = value
        .number("number")
        .map_err(|_| anyhow::anyhow!("invalid_number"))?;
    ensure!(
        value.is_finite() && (minimum..=maximum).contains(&value),
        "invalid_number"
    );
    Ok(value)
}
fn integer(value: &Json, minimum: u64, maximum: u64) -> Result<u64> {
    ensure!(
        matches!(value,Json::Number(token) if integer_token(token)),
        "invalid_integer"
    );
    Ok(number(value, minimum as f64, maximum as f64)? as u64)
}

/// Validation retains the original object and numeric tokens, including unknown
/// fields in the bounded adapter's Unicode-scalar and standard Serde nesting
/// domain. Unpaired surrogates and deeper JSON are refused as `invalid_json`;
/// Python's broader decoder domain is not claimed. Booleans and integral float
/// tokens never become integer selectors.
#[derive(Clone, Debug)]
pub struct Request {
    value: Json,
    original: Vec<u8>,
}
impl Request {
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        ensure!(bytes.len() <= MAX_REQUEST, "remote_request_too_large");
        let value = decode(bytes, MAX_REQUEST)?;
        object(&value)?;
        ensure!(hex(text(&value["revision"])?, 40), "invalid_revision");
        ensure!(hex(text(&value["run"])?, 32), "invalid_run_id");
        ensure!(
            matches!(
                value["workload"].string(),
                Some("meetings" | "large-meeting" | "webinar")
            ),
            "invalid_workload"
        );
        let label = text(&value["label"])?;
        ensure!(
            !label.is_empty()
                && label.len() <= 64
                && label.as_bytes()[0].is_ascii_alphanumeric()
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b)),
            "invalid_label"
        );
        for (name, low, high) in [
            ("firstSize", 2, 10000),
            ("steps", 1, 12),
            ("meetingSize", 2, 64),
            ("speakers", 1, 64),
            ("runtimeSeconds", 120, 21600),
        ] {
            integer(&value[name], low, high)?;
        }
        ensure!(matches!(value["quick"], Json::Bool(_)), "invalid_quick");
        ensure!(
            matches!(value["audioOnly"], Json::Bool(_)),
            "invalid_audio_only"
        );
        let interval = integer(&value["chatIntervalMs"], 0, 600000)?;
        let end = if value["quick"].is_true() {
            30000
        } else {
            60000
        };
        ensure!(
            interval == 0 || (1000..=end - 2000).contains(&interval),
            "chat_window_too_short"
        );
        for name in ["serverCpus", "generatorCpus", "appCpus"] {
            if !matches!(value[name], Json::Null) {
                number(&value[name], 0.1, 256.0)?;
            }
        }
        if !matches!(value["monthlyPrice"], Json::Null) {
            number(&value["monthlyPrice"], 0.01, 1e6)?;
        }
        number(
            value
                .get("portMbps")
                .unwrap_or(&Json::Number("1000".into())),
            1.0,
            1e6,
        )?;
        Ok(Self {
            value,
            original: bytes.to_vec(),
        })
    }
    pub fn revision(&self) -> &str {
        self.value["revision"].string().unwrap()
    }
    pub fn run_id(&self) -> &str {
        self.value["run"].string().unwrap()
    }
    pub fn workload(&self) -> &str {
        self.value["workload"].string().unwrap()
    }
    /// Return the validated request unchanged for the existing transport envelope.
    pub fn as_json(&self) -> &[u8] {
        &self.original
    }
}

/// Produce arguments only. The operation owner chooses a verified host-compatible
/// executable and its `capacity` prefix; this library never assumes one is installed.
pub fn capacity_arguments(request: &Request, images: &[u8]) -> Result<Vec<String>> {
    let images = decode(images, MAX_JSON)?;
    object(&images)?;
    let mut args = vec![
        "run".into(),
        "--engine".into(),
        "docker".into(),
        "--output".into(),
        format!(
            "{ROOT}/results/capacity-controller.{}/workload",
            request.run_id()
        ),
    ];
    for (name, flag) in [
        ("serverImage", "--server-image"),
        ("generatorImage", "--generator-image"),
    ] {
        args.extend([flag.into(), text(&images[name])?.into()]);
    }
    for (name, flag) in [
        ("label", "--label"),
        ("workload", "--workloads"),
        ("firstSize", "--first-size"),
        ("steps", "--steps"),
        ("meetingSize", "--meeting-size"),
        ("speakers", "--speakers"),
        ("chatIntervalMs", "--chat-interval-ms"),
        ("serverCpus", "--server-cpus"),
        ("generatorCpus", "--generator-cpus"),
        ("appCpus", "--app-cpus"),
        ("portMbps", "--port-mbps"),
        ("monthlyPrice", "--monthly-price"),
    ] {
        let value = &request.value[name];
        if !matches!(value, Json::Null) {
            args.extend([flag.into(), value.display()]);
        }
    }
    for (name, flag) in [("audioOnly", "--audio-only"), ("quick", "--quick")] {
        if request.value[name].is_true() {
            args.push(flag.into());
        }
    }
    if request.workload() == "meetings" {
        for name in ["MAX_PARTICIPANTS_PER_ROOM", "MAX_BROADCASTERS_PER_ROOM"] {
            args.extend([
                "--server-env".into(),
                format!("{name}={}", request.value["meetingSize"].display()),
            ]);
        }
    }
    Ok(args)
}

fn numeric_equal(a: &str, b: &str) -> bool {
    if integer_token(a) && integer_token(b) {
        return if a == "-0" { "0" } else { a } == if b == "-0" { "0" } else { b };
    }
    if integer_token(a) || integer_token(b) {
        let (integer, float) = if integer_token(a) { (a, b) } else { (b, a) };
        let value = float.parse::<f64>().unwrap_or(f64::NAN);
        return value.is_finite()
            && value.fract() == 0.0
            && (if value == 0.0 {
                "0".into()
            } else {
                format!("{value:.0}")
            }) == if integer == "-0" { "0" } else { integer };
    }
    a.parse::<f64>().unwrap_or(f64::NAN) == b.parse::<f64>().unwrap_or(f64::NAN)
}
fn equal(a: &Json, b: &Json) -> bool {
    match (a, b) {
        (Json::Null, Json::Null) => true,
        (Json::String(a), Json::String(b)) => a == b,
        (Json::Number(a), Json::Number(b)) => numeric_equal(a, b),
        (Json::Bool(a), Json::Bool(b)) => a == b,
        (Json::Bool(a), Json::Number(b)) => numeric_equal(if *a { "1" } else { "0" }, b),
        (Json::Number(a), Json::Bool(b)) => numeric_equal(a, if *b { "1" } else { "0" }),
        (Json::Array(a), Json::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| equal(a, b))
        }
        (Json::Object(a), Json::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, value)| b.get(key).is_some_and(|b| equal(value, b)))
        }
        _ => false,
    }
}

/// A workload lower bound remains distinct from failed and invalid observations.
#[derive(Debug)]
pub struct Assessment {
    pub passed: bool,
    passing: Vec<Json>,
    failed: Vec<Json>,
    ceilings: Json,
    projection: Json,
}
impl Assessment {
    pub fn passing_steps(&self) -> usize {
        self.passing.len()
    }
    pub fn failed_steps(&self) -> usize {
        self.failed.len()
    }
    pub fn to_json(&self) -> Vec<u8> {
        encode(&record([
            ("passed", Json::Bool(self.passed)),
            ("passingSteps", Json::Array(self.passing.clone())),
            ("failedSteps", Json::Array(self.failed.clone())),
            ("ceilings", self.ceilings.clone()),
            ("projection", self.projection.clone()),
        ]))
        .into_bytes()
    }
}

pub fn assess(bytes: &[u8], request: &Request) -> Result<Assessment> {
    let report = decode(bytes, MAX_JSON)?;
    object(&report)?;
    ensure!(
        equal(&report["schemaVersion"], &Json::Number("1".into())),
        "invalid_calibration_schema"
    );
    let measurement = &report["measurement"];
    object(measurement)?;
    let browser = &measurement["browser"];
    object(browser)?;
    ensure!(
        equal(&measurement["meetingSize"], &request.value["meetingSize"]),
        "measurement_shape_mismatch"
    );
    ensure!(
        ["audioOnly", "speakers", "chatIntervalMs"]
            .iter()
            .all(|name| equal(&browser[name], &request.value[name])),
        "measurement_shape_mismatch"
    );
    for role in ["server", "generator"] {
        let images = &report["images"];
        object(images)?;
        let image = &images[role];
        object(image)?;
        ensure!(
            image["revision"].string() == Some(request.revision())
                && text(&image["id"])?
                    .strip_prefix("sha256:")
                    .is_some_and(|id| hex(id, 64)),
            "measurement_image_mismatch"
        );
    }
    let steps = array(&report["steps"])?;
    for step in steps {
        object(step)?;
    }
    ensure!(
        !steps.is_empty()
            && steps
                .iter()
                .all(|step| step["workload"].string() == Some(request.workload())),
        "missing_requested_workload"
    );
    ensure!(
        steps
            .iter()
            .all(|step| matches!(step["valid"], Json::Bool(_))
                && matches!(step["passed"], Json::Bool(_))),
        "invalid_step_verdict"
    );
    let (passing, failed): (Vec<_>, Vec<_>) = steps
        .iter()
        .cloned()
        .partition(|step| step["valid"].is_true() && step["passed"].is_true());
    Ok(Assessment {
        passed: failed.is_empty(),
        passing,
        failed,
        ceilings: report["ceilings"].clone(),
        projection: report["projection"].clone(),
    })
}

pub fn parse_unit_state(output: &str) -> Result<Vec<u8>> {
    // The reference command captures stdout and stderr separately at8MiB each.
    ensure!(output.len() <= 2 * MAX_JSON, "unit_response_too_large");
    let mut fields = BTreeMap::new();
    // Python splitlines also recognizes these internal separators. Splitting
    // CRLF twice only creates an empty line, ignored by the same key rule.
    for line in output.split([
        '\n', '\r', '\u{b}', '\u{c}', '\u{1c}', '\u{1d}', '\u{1e}', '\u{85}', '\u{2028}',
        '\u{2029}',
    ]) {
        let (key, value) = line.split_once('=').unwrap_or((line, ""));
        if !key.is_empty() {
            fields.insert(key.into(), Json::String(value.into()));
        }
    }
    let value = Json::Object(fields);
    ensure!(
        matches!(value["LoadState"].string(), Some("loaded" | "not-found"))
            && value.get("ActiveState").is_some()
            && value.get("MainPID").is_some(),
        "invalid_unit_state"
    );
    Ok(encode(&value).into_bytes())
}

/// Retained success is terminal only after its exact process has exited.
pub fn unit_finished(bytes: &[u8]) -> Result<bool> {
    let value = decode(bytes, MAX_JSON)?;
    object(&value)?;
    Ok(
        matches!(value["ActiveState"].string(), Some("inactive" | "failed"))
            || value["ActiveState"].string() == Some("active")
                && value["SubState"].string() == Some("exited")
                && value["MainPID"].string() == Some("0"),
    )
}

/// Command observations supplied by an operation owner. `owned` is the exact
/// label-filtered immutable-ID observation, not authority derived from a name.
/// Each encoded record is bounded here. The operation owner must bound aggregate
/// observation count and bytes; this is not a standalone untrusted snapshot decoder.
#[derive(Default)]
pub struct Snapshot {
    pub before: Vec<String>,
    pub after: Vec<String>,
    pub owned: Vec<String>,
    pub journal: Option<Vec<u8>>,
    pub inspections: BTreeMap<String, Vec<u8>>,
}
#[derive(Debug)]
pub struct Residuals {
    pub clean: bool,
    value: Json,
}
impl Residuals {
    pub fn to_json(&self) -> Vec<u8> {
        encode(&self.value).into_bytes()
    }
}

/// Projects observations without granting cleanup authority. Ownership parity
/// covers the engine's string-valued image identities. Malformed nested NaN image
/// values remain unconfirmed rather than inheriting CPython container-identity
/// equality for its shared NaN decoder constant.
pub fn residuals(snapshot: &Snapshot) -> Result<Residuals> {
    ensure!(
        snapshot.after.iter().all(|id| hex(id, 64)),
        "invalid_container_id"
    );
    let mut expected = BTreeMap::new();
    let owned = if let Some(journal) = &snapshot.journal {
        let journal = decode(journal, MAX_JSON)?;
        object(&journal)?;
        ensure!(hex(text(&journal["runId"])?, 16), "invalid_ownership_run");
        for entry in array(&journal["containers"])? {
            object(entry)?;
            expected.insert(text(&entry["name"])?.to_owned(), entry.clone());
        }
        ensure!(
            snapshot.owned.iter().all(|id| hex(id, 64)),
            "invalid_container_id"
        );
        snapshot.owned.as_slice()
    } else {
        &[]
    };
    let before: BTreeSet<_> = snapshot.before.iter().collect();
    let discovered: BTreeSet<_> = snapshot
        .after
        .iter()
        .filter(|id| !before.contains(id))
        .chain(owned)
        .collect();
    let mut details = Vec::new();
    for id in discovered {
        let bytes = snapshot
            .inspections
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("missing_inspection"))?;
        let inspection = decode(bytes, MAX_JSON)?;
        let values = array(&inspection)?;
        ensure!(values.len() == 1, "container_inspection_mismatch");
        let value = &values[0];
        object(value)?;
        ensure!(
            value["Id"].string() == Some(id),
            "container_inspection_mismatch"
        );
        let name = text(&value["Name"])?;
        let name = name.strip_prefix('/').unwrap_or(name);
        let config = &value["Config"];
        object(config)?;
        let confirmed = owned.contains(id)
            && expected.get(name).is_some_and(|entry| {
                equal(&config["Image"], &entry["image"])
                    && (matches!(entry["id"], Json::Null) || equal(&entry["id"], &value["Id"]))
            });
        let state = &value["State"];
        object(state)?;
        let state = record(
            [
                "Status",
                "Running",
                "ExitCode",
                "OOMKilled",
                "StartedAt",
                "FinishedAt",
            ]
            .map(|key| (key, state[key].clone())),
        );
        details.push(record([
            ("id", Json::String(id.clone())),
            ("confirmedOwned", Json::Bool(confirmed)),
            (
                "inspection",
                record([
                    ("Id", Json::String(id.clone())),
                    ("Name", Json::String(name.into())),
                    ("Image", value["Image"].clone()),
                    ("RequestedImage", config["Image"].clone()),
                    ("State", state),
                ]),
            ),
        ]));
    }
    let clean = details.is_empty();
    let strings =
        |values: &[String]| Json::Array(values.iter().cloned().map(Json::String).collect());
    Ok(Residuals {
        clean,
        value: record([
            ("containersBefore", strings(&snapshot.before)),
            ("containersAfter", strings(&snapshot.after)),
            ("residuals", Json::Array(details)),
            ("clean", Json::Bool(clean)),
        ]),
    })
}

fn record<const N: usize>(entries: [(&str, Json); N]) -> Json {
    Json::Object(
        entries
            .into_iter()
            .map(|(key, value)| (key.into(), value))
            .collect(),
    )
}
fn encode(value: &Json) -> String {
    match value {
        Json::Null => "null".into(),
        Json::Bool(value) => value.to_string(),
        Json::Number(value) => value.clone(),
        Json::String(value) => serde_json::to_string(value).unwrap(),
        Json::Array(values) => format!(
            "[{}]",
            values.iter().map(encode).collect::<Vec<_>>().join(",")
        ),
        Json::Object(values) => format!(
            "{{{}}}",
            values
                .iter()
                .map(|(key, value)| format!(
                    "{}:{}",
                    serde_json::to_string(key).unwrap(),
                    encode(value)
                ))
                .collect::<Vec<_>>()
                .join(",")
        ),
    }
}

#[cfg(test)]
#[path = "vps_tests.rs"]
mod tests;
