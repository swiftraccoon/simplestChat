//! Disable PostgreSQL bound-parameter logging without restarting anything.
//!
//! Only the two fixed catalog entries are read. The remediation addresses the
//! current database by its full container identity and rechecks that identity
//! around every step. The two separate statements are safe to repeat after an
//! interruption, and a reload counts only once new sessions see its effect.

use crate::host::evidence::{Evidence, Streams};
use crate::host::failure::{Failure, Result, require};
use crate::host::state;
use crate::host::{Clock, Host};
use crate::json::Json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Each setting with the context its catalog row must report.
const SETTINGS: [(&str, &str); 2] = [
    ("log_parameter_max_length", "superuser"),
    ("log_parameter_max_length_on_error", "user"),
];
const QUERY: &str = "SELECT json_agg(row_to_json(s) ORDER BY name) FROM (\
SELECT name, setting, source, context, pending_restart FROM pg_settings \
WHERE name IN ('log_parameter_max_length','log_parameter_max_length_on_error')) AS s";
const ALTER: [&str; 2] = [
    "ALTER SYSTEM SET log_parameter_max_length = '0'",
    "ALTER SYSTEM SET log_parameter_max_length_on_error = '0'",
];
const RELOAD: &str = "SELECT pg_reload_conf()";
const VERIFY: Duration = Duration::from_secs(15);
const POLL: Duration = Duration::from_millis(250);
const CONTAINER_FORMAT: &str = concat!(
    r#"{"id":{{json .Id}},"image":{{json .Image}},"state":{{json .State}},"#,
    r#""restarts":{{.RestartCount}},"#,
    r#""configHash":{{json (index .Config.Labels "com.docker.compose.config-hash")}}}"#
);

/// Current container inspection and bounded fixed daemon commands.
pub(super) trait Runner {
    /// The one running database container of the public project.
    fn container(&mut self) -> Result<Json>;
    /// Run the daemon with fixed arguments and keep its output privately.
    fn docker(&mut self, arguments: &[&str], seconds: f64) -> Result<Vec<u8>>;
}

/// One allowlisted statement in a fresh non-interactive socket session.
fn query(runner: &mut dyn Runner, database: &str, statement: &str) -> Result<Vec<u8>> {
    let full = database.len() == 64
        && database
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
    require(full, "Invalid database container identity")?;
    let known = statement == QUERY || statement == RELOAD || ALTER.contains(&statement);
    require(known, "Unsupported database logging statement")?;
    let arguments = [
        "exec",
        "--user",
        "999:999",
        "--env",
        "PGOPTIONS=-c statement_timeout=5000 -c lock_timeout=3000",
        database,
        "psql",
        "--no-psqlrc",
        "--no-password",
        "--set",
        "ON_ERROR_STOP=on",
        "--host",
        "/run/simplestchat-postgres",
        "--username",
        "postgres",
        "--dbname",
        "simplestchat",
        "--tuples-only",
        "--no-align",
        "--command",
        statement,
    ];
    runner.docker(&arguments, 10.0)
}

/// Validate exactly the two reloadable settings and refuse any stronger policy.
pub(super) fn settings(raw: &[u8]) -> Result<BTreeMap<String, Json>> {
    let decoded = state::decode(raw)?;
    let rows = decoded.array().ok_or(Failure::Value)?;
    require(
        rows.len() == SETTINGS.len(),
        "Missing database logging settings",
    )?;
    let mut result = BTreeMap::new();
    for row in rows {
        row.object().ok_or(Failure::Value)?;
        let text = |name: &str| row.get(name).and_then(Json::string).ok_or(Failure::Value);
        let (name, setting) = (text("name")?, text("setting")?);
        let context = SETTINGS
            .iter()
            .find(|(known, _)| *known == name)
            .map(|(_, context)| *context);
        let fresh = context.is_some() && !result.contains_key(name);
        require(fresh, "Unexpected logging setting")?;
        let value = setting == "-1"
            || ((1..=10).contains(&setting.len())
                && setting.bytes().all(|byte| byte.is_ascii_digit()));
        require(value, "Invalid logging setting value")?;
        let reported = row.get("context").and_then(Json::string);
        require(reported == context, "Unexpected logging setting context")?;
        let settled = row.get("pending_restart").and_then(Json::boolean) == Some(false);
        require(settled, "Database setting has pending restart")?;
        let source = row.get("source").and_then(Json::string).unwrap_or_default();
        let plain = ["default", "configuration file", "command line"].contains(&source);
        require(
            plain,
            "Database logging has an overriding session, role or database policy",
        )?;
        let consistent = source != "command line" || setting == "0";
        let contradictory =
            "Contradictory command-line logging setting requires full configuration maintenance";
        require(consistent, contradictory)?;
        result.insert(name.to_owned(), row.clone());
    }
    Ok(result)
}

/// Whether both limits are persisted zeros, not zeros inherited from defaults.
pub(super) fn explicit_zero(snapshot: &BTreeMap<String, Json>) -> bool {
    SETTINGS.iter().all(|(name, _)| {
        let field = |key: &str| {
            snapshot
                .get(*name)
                .and_then(|row| row.get(key))
                .and_then(Json::string)
        };
        field("setting") == Some("0")
            && ["configuration file", "command line"].contains(&field("source").unwrap_or_default())
    })
}

/// The fields that prove a retained container was not replaced or restarted.
fn stable(container: &Json) -> Result<[Json; 4]> {
    let field = |name: &str| container.get(name).cloned().ok_or(Failure::Key);
    let started = container.get("state").ok_or(Failure::Key)?;
    started.object().ok_or(Failure::Value)?;
    let started = started.get("StartedAt").cloned().ok_or(Failure::Key)?;
    Ok([field("id")?, field("image")?, started, field("restarts")?])
}

/// Require the same database identity, image, start time and restart count.
fn unchanged(runner: &mut dyn Runner, original: &Json) -> Result<()> {
    let (now, then) = (stable(&runner.container()?)?, stable(original)?);
    let same = now.iter().zip(&then).all(|(a, b)| a.semantic_eq(b));
    require(
        same,
        "Database container changed during logging remediation",
    )
}

/// What the remediation observed and did.
#[derive(Default, serde::Serialize)]
pub(super) struct Report {
    #[serde(rename = "schemaVersion")]
    schema_version: u8,
    operation: &'static str,
    evidence: String,
    #[serde(rename = "startedAt")]
    started_at: String,
    passed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    changed: Option<bool>,
    #[serde(rename = "verificationConnection")]
    verification: BTreeMap<&'static str, &'static str>,
    #[serde(rename = "databaseContainer", skip_serializing_if = "Option::is_none")]
    database: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    before: Option<BTreeMap<String, Json>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    after: Option<BTreeMap<String, Json>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure: Option<String>,
    #[serde(rename = "finishedAt", skip_serializing_if = "Option::is_none")]
    finished_at: Option<String>,
}

/// Apply only the two fixed zero values and verify the effective policy
/// after the asynchronous reload.
pub(super) fn apply(runner: &mut dyn Runner, clock: &dyn Clock, report: &mut Report) -> Result<()> {
    let original = runner.container()?;
    let database = original
        .get("id")
        .and_then(Json::string)
        .ok_or(Failure::Value)?
        .to_owned();
    report.database = Some(database.clone());
    let before = settings(&query(runner, &database, QUERY)?)?;
    report.before = Some(before.clone());
    report.changed = Some(false);
    if explicit_zero(&before) {
        unchanged(runner, &original)?;
        report.after = Some(before);
        return Ok(());
    }
    for statement in ALTER {
        unchanged(runner, &original)?;
        report.changed = Some(true);
        query(runner, &database, statement)?;
    }
    unchanged(runner, &original)?;
    let reloaded = query(runner, &database, RELOAD)?.trim_ascii() == b"t";
    require(reloaded, "Database reload was refused")?;
    let deadline = clock.monotonic() + VERIFY;
    loop {
        let after = settings(&query(runner, &database, QUERY)?)?;
        let effective = explicit_zero(&after);
        report.after = Some(after);
        if effective {
            return unchanged(runner, &original);
        }
        require(
            clock.monotonic() < deadline,
            "Database logging reload was not effective",
        )?;
        clock.sleep(POLL);
    }
}

/// The daemon through the host, with every command's output kept in the
/// attempt directory as numbered private files.
struct Compose<'a> {
    evidence: Evidence<'a>,
    configuration: &'a Path,
}

impl Runner for Compose<'_> {
    fn container(&mut self) -> Result<Json> {
        let configuration = self.configuration.to_str().ok_or(Failure::Value)?;
        let environment = format!("{configuration}/app.env");
        let project = format!("{configuration}/compose.public.yml");
        let listed = [
            "compose",
            "--project-name",
            "simplestchat-public",
            "--project-directory",
            configuration,
            "--env-file",
            &environment,
            "-f",
            &project,
            "ps",
            "--status",
            "running",
            "--quiet",
            "postgres",
        ];
        let listed = String::from_utf8(self.docker(&listed, 30.0)?).map_err(|_| Failure::Value)?;
        let identity = listed.trim();
        let one = identity.len() == 64
            && identity
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
        require(one, "Exactly one running postgres container is required")?;
        let inspected = self.docker(&["inspect", "--format", CONTAINER_FORMAT, identity], 30.0)?;
        let result = state::decode(&inspected)?;
        result.object().ok_or(Failure::Value)?;
        let running = result.get("state").ok_or(Failure::Key)?;
        running.object().ok_or(Failure::Value)?;
        let flag = |name: &str| running.get(name).ok_or(Failure::Key).map(Json::truthiness);
        require(
            flag("Running")? && !flag("OOMKilled")?,
            "postgres is not running normally",
        )?;
        Ok(result)
    }

    fn docker(&mut self, arguments: &[&str], seconds: f64) -> Result<Vec<u8>> {
        self.evidence.docker(arguments, seconds, Streams::default())
    }
}

/// What the evidence and the journal say about a failure: a refusal's own
/// text, or the class of fault.
fn described(failure: &Failure, number: u32) -> (String, &'static str) {
    match failure {
        Failure::Refused(message) => ((*message).to_owned(), "ReleaseError"),
        Failure::Command => (
            format!("Command {number:03} failed; inspect private output"),
            "ReleaseError",
        ),
        Failure::Timeout => ("TimeoutExpired".to_owned(), "TimeoutExpired"),
        Failure::Value => ("ValueError".to_owned(), "ValueError"),
        Failure::Key => ("KeyError".to_owned(), "KeyError"),
        Failure::Os => ("OSError".to_owned(), "OSError"),
    }
}

/// Run the remediation in a new private attempt directory and keep its
/// outcome on success and on partial failure. The caller holds the workload
/// lock. Returns the outcome line and, after a failure, the line for the
/// journal.
pub(super) fn disable(host: &Host<'_>, configuration: &Path) -> (Option<String>, Option<String>) {
    let journal = |class: &str| {
        format!("Database logging remediation failed ({class}); inspect private evidence.\n")
    };
    let stamp = |host: &Host<'_>| host.clock.now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let prepared = (|| -> Result<PathBuf> {
        let parent = host.paths.root.join("results");
        state::protected(&parent, host.owner, true, &[0o700], None)?;
        let mut random = [0_u8; 4];
        getrandom::fill(&mut random).map_err(|_| Failure::Os)?;
        let suffix: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let attempt = parent.join(format!("database-logging.{suffix}"));
        std::os::unix::fs::DirBuilderExt::mode(&mut std::fs::DirBuilder::new(), 0o700)
            .create(&attempt)?;
        Ok(attempt)
    })();
    let attempt = match prepared {
        Ok(attempt) => attempt,
        Err(failure) => return (None, Some(journal(described(&failure, 0).1))),
    };
    let mut report = Report {
        schema_version: 1,
        operation: "database_logging",
        evidence: attempt.display().to_string(),
        started_at: stamp(host),
        verification: BTreeMap::from([("role", "postgres"), ("database", "simplestchat")]),
        changed: Some(false),
        ..Report::default()
    };
    let evidence = Evidence::new(host, attempt.clone());
    let mut runner = Compose {
        evidence,
        configuration,
    };
    let outcome = apply(&mut runner, host.clock, &mut report);
    report.passed = outcome.is_ok();
    let failed = outcome
        .err()
        .map(|failure| described(&failure, runner.evidence.number));
    report.failure = failed.as_ref().map(|(text, _)| text.clone());
    report.finished_at = Some(stamp(host));
    let written = state::atomic_json(&attempt.join("outcome.json"), &report);
    let line = serde_json::to_string(&report).ok().map(|line| line + "\n");
    let journal = match (failed, written) {
        (Some((_, class)), _) => Some(journal(class)),
        (None, Err(failure)) => Some(journal(described(&failure, 0).1)),
        (None, Ok(())) => None,
    };
    (line, journal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::command::Request;
    use crate::host::tests::Fixed;
    use serde_json::Value;

    /// The scripted database of one recorded case.
    struct Scripted<'a> {
        case: &'a Value,
        snapshots: Vec<String>,
        inspections: u64,
        commands: Vec<Value>,
    }

    impl Runner for Scripted<'_> {
        fn container(&mut self) -> Result<Json> {
            self.inspections += 1;
            self.commands
                .push(serde_json::json!({"container": "postgres"}));
            let replaced = self.case["replacementAt"].as_u64() == Some(self.inspections);
            let id = if replaced {
                "b".repeat(64)
            } else {
                "a".repeat(64)
            };
            let text = format!(
                r#"{{"id":"{id}","image":"sha256:{}","state":{{"StartedAt":"2026-11-11T10:00:00Z","Running":true,"OOMKilled":false}},"restarts":0,"configHash":"{}"}}"#,
                "c".repeat(64),
                "d".repeat(64)
            );
            state::decode(text.as_bytes())
        }

        fn docker(&mut self, arguments: &[&str], seconds: f64) -> Result<Vec<u8>> {
            self.commands
                .push(serde_json::json!({"argv": arguments, "timeout": seconds}));
            let statement = *arguments.last().unwrap();
            if self.case["fail"].as_str() == Some(statement) {
                return Err(Failure::Command);
            }
            if self.case["timeout"].as_str() == Some(statement) {
                return Err(Failure::Timeout);
            }
            if statement == QUERY {
                let answer = if self.snapshots.len() > 1 {
                    self.snapshots.remove(0)
                } else {
                    self.snapshots[0].clone()
                };
                return Ok(answer.into_bytes());
            }
            if statement == RELOAD {
                return Ok(self.case["reloadResult"]
                    .as_str()
                    .unwrap()
                    .as_bytes()
                    .to_vec());
            }
            Ok(b"ALTER SYSTEM\n".to_vec())
        }
    }

    fn words(text: &str) -> Vec<&str> {
        text.split_whitespace().collect()
    }

    /// The statements are the recorded ones, and every recorded catalog answer
    /// is validated or refused as recorded.
    #[test]
    fn statements_and_catalog_validation_are_the_recorded_ones() {
        let recorded = super::super::recorded::reference();
        let logging = &recorded["databaseLogging"];
        assert_eq!(words(QUERY), words(logging["query"].as_str().unwrap()));
        assert_eq!(serde_json::json!(ALTER), logging["alter"]);
        assert_eq!(RELOAD, logging["reload"].as_str().unwrap());
        let cases = logging["settings"].as_array().unwrap();
        assert_eq!(cases.len(), 18);
        for case in cases {
            let label = case["label"].as_str().unwrap();
            match settings(case["raw"].as_str().unwrap().as_bytes()) {
                Ok(snapshot) => {
                    assert_eq!(
                        Some(explicit_zero(&snapshot)),
                        case["explicitZero"].as_bool(),
                        "{label}"
                    );
                    assert_eq!(
                        serde_json::to_value(&snapshot).unwrap(),
                        case["value"],
                        "{label}"
                    );
                }
                Err(Failure::Refused(message)) => {
                    assert_eq!(Some(message), case["refused"].as_str(), "{label}");
                }
                Err(_) => assert_eq!(case["error"], "ValueError", "{label}"),
            }
        }
    }

    /// Seventeen recorded databases: each succeeds, refuses or fails as
    /// recorded, through the recorded sequence of identity checks and
    /// sessions, and reports the recorded change.
    #[test]
    fn remediation_follows_the_recorded_sessions_and_outcomes() {
        let recorded: Value = serde_json::from_str(include_str!("recorded_logging.json")).unwrap();
        assert_eq!(
            CONTAINER_FORMAT,
            recorded["containerFormat"].as_str().unwrap()
        );
        let cases = recorded["cases"].as_array().unwrap();
        assert_eq!(cases.len(), 17);
        for case in cases {
            let name = case["name"].as_str().unwrap();
            let snapshots = case["snapshots"].as_array().unwrap().iter();
            let snapshots = snapshots.map(|item| {
                item.as_str()
                    .map_or_else(|| item.to_string(), str::to_owned)
            });
            let mut runner = Scripted {
                case,
                snapshots: snapshots.collect(),
                inspections: 0,
                commands: Vec::new(),
            };
            let clock = Fixed::at("2026-11-11T12:00:00Z");
            let mut report = Report::default();
            let outcome = apply(&mut runner, &clock, &mut report);
            let expected = &case["result"];
            match &outcome {
                Ok(()) => assert_eq!(expected["passed"], true, "{name}"),
                Err(failure) => {
                    let (text, class) = described(failure, 4);
                    match expected["refused"].as_str() {
                        Some(refusal) => assert_eq!(text, refusal, "{name}"),
                        None => assert_eq!(class, expected["error"].as_str().unwrap(), "{name}"),
                    }
                }
            }
            let theirs = &case["report"];
            assert_eq!(report.changed, theirs["changed"].as_bool(), "{name}");
            assert_eq!(
                report.database.as_deref(),
                theirs["databaseContainer"].as_str(),
                "{name}"
            );
            let rows = |snapshot: &Option<BTreeMap<String, Json>>| {
                snapshot
                    .as_ref()
                    .map_or(Value::Null, |rows| serde_json::to_value(rows).unwrap())
            };
            assert_eq!(rows(&report.before), theirs["before"], "{name}");
            assert_eq!(rows(&report.after), theirs["after"], "{name}");
            let commands = case["commands"].as_array().unwrap();
            if commands
                .iter()
                .any(|command| command.get("elided").is_some())
            {
                // The wait is bounded by the clock, not by a count of looks.
                assert!(
                    (55..=65).contains(&runner.commands.len().saturating_sub(10)),
                    "{name}"
                );
                continue;
            }
            assert_eq!(runner.commands.len(), commands.len(), "{name}");
            for (ours, recorded) in runner.commands.iter().zip(commands) {
                assert_eq!(ours.get("container"), recorded.get("container"), "{name}");
                assert_eq!(
                    ours["timeout"].as_f64(),
                    recorded["timeout"].as_f64(),
                    "{name}"
                );
                let argv = |command: &Value| {
                    let argv = command["argv"].as_array().cloned().unwrap_or_default();
                    argv.iter()
                        .map(|word| word.as_str().unwrap().to_owned())
                        .collect::<Vec<_>>()
                };
                let (ours, recorded) = (argv(ours), argv(recorded));
                assert_eq!(ours.len(), recorded.len(), "{name}");
                for (word, expected) in ours.iter().zip(&recorded) {
                    assert_eq!(words(word), words(expected), "{name}");
                }
            }
        }
    }

    /// From `test_full_id_and_fixed_sql_are_required`: neither a shortened
    /// identity nor any other statement reaches the daemon.
    #[test]
    fn a_partial_identity_or_another_statement_issues_no_command() {
        let case = serde_json::json!({});
        let mut runner = Scripted {
            case: &case,
            snapshots: Vec::new(),
            inspections: 0,
            commands: Vec::new(),
        };
        let full = "a".repeat(64);
        for (identity, statement, refusal) in [
            (&full[..12], QUERY, "Invalid database container identity"),
            (
                &"A".repeat(64),
                QUERY,
                "Invalid database container identity",
            ),
            (&full, "SELECT 1", "Unsupported database logging statement"),
            (
                &full,
                "ALTER SYSTEM RESET ALL",
                "Unsupported database logging statement",
            ),
        ] {
            let refused = query(&mut runner, identity, statement);
            assert_eq!(refused, Err(Failure::Refused(refusal)), "{statement}");
        }
        assert!(runner.commands.is_empty());
    }

    /// From `test_locked_execution_retains_private_success_and_partial_failure_evidence`:
    /// a success, and a failure after the first change, each leave one
    /// private attempt with every command's numbered private output and an
    /// outcome that says what happened; the printed line is that outcome.
    #[test]
    fn an_attempt_keeps_private_numbered_evidence_and_its_outcome_either_way() {
        use crate::host::command::tests::Scripted as Commands;
        use crate::host::state::tests::Scratch;
        use crate::host::tests::paths;
        use std::io::Write as _;
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let recorded: Value = serde_json::from_str(include_str!("recorded_logging.json")).unwrap();
        let snapshots = &recorded["cases"][0]["snapshots"];
        let (defaults, explicit) = (snapshots[0].to_string(), snapshots[2].to_string());
        let mode = |path: &Path| std::fs::symlink_metadata(path).unwrap().mode() & 0o7777;
        for failing in [None, Some(ALTER[1])] {
            let scratch = Scratch::new();
            let place = paths(scratch.path());
            let results = place.root.join("results");
            let mut builder = std::fs::DirBuilder::new();
            std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
            builder.recursive(true).create(&results).unwrap();
            let looks = std::cell::Cell::new(0);
            let commands = Commands::new(|request: &Request<'_>| {
                // As on a host, both files exist from before the command starts.
                let private = |path: &Path| {
                    let mut options = std::fs::OpenOptions::new();
                    options
                        .write(true)
                        .create_new(true)
                        .mode(0o600)
                        .open(path)
                        .unwrap()
                };
                let mut output = private(request.archive.unwrap());
                private(request.errors.unwrap());
                let has = |word: &str| request.argv.iter().any(|argument| argument == word);
                let last = request.argv.last().unwrap().as_str();
                let answer = if has("ps") {
                    format!("{}\n", "a".repeat(64))
                } else if has("inspect") {
                    let state = r#"{"StartedAt":"s","Running":true,"OOMKilled":false}"#;
                    let id = "a".repeat(64);
                    format!(r#"{{"id":"{id}","image":"i","state":{state},"restarts":0}}"#)
                } else if Some(last) == failing {
                    return Err(Failure::Command);
                } else if last == QUERY {
                    looks.set(looks.get() + 1);
                    if looks.get() == 1 {
                        defaults.clone()
                    } else {
                        explicit.clone()
                    }
                } else if last == RELOAD {
                    "t\n".to_owned()
                } else {
                    "ALTER SYSTEM\n".to_owned()
                };
                output.write_all(answer.as_bytes()).unwrap();
                Ok(Vec::new())
            });
            let clock = Fixed::at("2026-11-11T12:00:00Z");
            let owner = rustix::process::geteuid().as_raw();
            let host = Host {
                commands: &commands,
                clock: &clock,
                paths: place,
                owner,
            };
            let configuration = scratch.path().join("configuration");
            let (line, journal) = disable(&host, &configuration);
            let attempts = std::fs::read_dir(&results).unwrap();
            let attempts: Vec<PathBuf> = attempts.map(|entry| entry.unwrap().path()).collect();
            assert_eq!(attempts.len(), 1);
            let attempt = &attempts[0];
            let name = attempt.file_name().unwrap().to_str().unwrap();
            let suffix = name.strip_prefix("database-logging.").unwrap();
            assert!(suffix.len() == 8 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit()));
            assert_eq!(mode(attempt), 0o700);
            let outcome = attempt.join("outcome.json");
            assert_eq!(mode(&outcome), 0o600);
            let saved: Value = serde_json::from_slice(&std::fs::read(&outcome).unwrap()).unwrap();
            let printed: Value = serde_json::from_str(line.as_deref().unwrap()).unwrap();
            assert!(line.unwrap().ends_with("}\n"));
            assert_eq!(printed, saved);
            let asked = commands.asked.borrow();
            let failure = failing
                .map(|_| format!("Command {:03} failed; inspect private output", asked.len()));
            let mut expected = serde_json::json!({
                "schemaVersion": 1,
                "operation": "database_logging",
                "evidence": attempt.to_str().unwrap(),
                "startedAt": "2026-11-11T12:00:00Z",
                "finishedAt": "2026-11-11T12:00:00Z",
                "passed": failing.is_none(),
                "changed": true,
                "verificationConnection": {"role": "postgres", "database": "simplestchat"},
                "databaseContainer": "a".repeat(64),
                "before": snapshots[0].as_array().unwrap().iter().map(|row| {
                    (row["name"].as_str().unwrap().to_owned(), row.clone())
                }).collect::<serde_json::Map<_, _>>(),
            });
            match &failure {
                Some(text) => expected["failure"] = serde_json::json!(text),
                None => {
                    expected["after"] = snapshots[2]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|row| (row["name"].as_str().unwrap().to_owned(), row.clone()))
                        .collect::<serde_json::Map<_, _>>()
                        .into();
                }
            }
            assert_eq!(saved, expected);
            let refusal =
                "Database logging remediation failed (ReleaseError); inspect private evidence.\n";
            assert_eq!(journal.as_deref(), failing.map(|_| refusal));
            // Every command left its two private files, numbered in order.
            assert_eq!(asked.len(), if failing.is_some() { 9 } else { 15 });
            for number in 1..=asked.len() {
                for kind in ["stdout", "stderr"] {
                    assert_eq!(mode(&attempt.join(format!("{number:03}.{kind}"))), 0o600);
                }
            }
            assert_eq!(
                std::fs::read_dir(attempt).unwrap().count(),
                asked.len() * 2 + 1
            );
            let listing = &asked[0];
            assert_eq!(listing.argv[..3], crate::host::DOCKER.map(str::to_owned));
            let project = configuration.to_str().unwrap();
            let rest: Vec<&str> = listing.argv[3..].iter().map(String::as_str).collect();
            let (environment, compose) = (
                format!("{project}/app.env"),
                format!("{project}/compose.public.yml"),
            );
            assert_eq!(
                rest,
                [
                    "compose",
                    "--project-name",
                    "simplestchat-public",
                    "--project-directory",
                    project,
                    "--env-file",
                    &environment,
                    "-f",
                    &compose,
                    "ps",
                    "--status",
                    "running",
                    "--quiet",
                    "postgres",
                ]
            );
            let inspect: Vec<&str> = asked[1].argv[3..].iter().map(String::as_str).collect();
            assert_eq!(
                inspect,
                ["inspect", "--format", CONTAINER_FORMAT, &"a".repeat(64)]
            );
            for command in asked.iter() {
                let fixed = [("PATH", "/usr/sbin:/usr/bin:/sbin:/bin"), ("LC_ALL", "C")];
                let pairs = fixed.map(|(key, value)| (key.to_owned(), value.to_owned()));
                assert_eq!(command.environment, pairs);
                assert!(command.input.is_none() && command.source.is_none());
            }
        }
    }
}
