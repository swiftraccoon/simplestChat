//! Create the separately owned operational schema, after retaining one
//! private verified archive of the database as it was. An existing schema is
//! kept only when its version, shape, ownership and role privileges are the
//! reviewed ones.

use super::collect::docker;
use super::command::Request;
use super::failure::{Failure, Result, require};
use super::host::{DOCKER, Host, POSTGRES};
use super::report::database;

const CONNECTION: [&str; 6] = [
    "-h",
    "/run/simplestchat-postgres",
    "-U",
    "postgres",
    "-d",
    "simplestchat",
];
const INCIDENT_SHAPE: &str = "SELECT incident_key,rule,severity,resource,first_seen,last_seen,\
resolved_at, release_revision,observations FROM operations.alerts LIMIT 0; \
SELECT singleton,observed_at FROM operations.alert_cursor LIMIT 0;";
const INCIDENT_OWNERSHIP: &str = "SELECT nspowner=\
(SELECT oid FROM pg_roles WHERE rolname='postgres') \
AND NOT has_schema_privilege('simplestchat_app','operations','USAGE') \
AND NOT has_schema_privilege('simplestchat_migrate','operations','USAGE') \
FROM pg_namespace WHERE nspname='operations';";
const EXTERNAL_SHAPE: &str = "SELECT workflow,run_id,attempt,source_revision,\
deployed_revision_at_import, started_at,completed_at,result,conclusion,checks,imported_at \
FROM operations.external_runs LIMIT 0; \
SELECT workflow,observed_at,last_complete_at,window_start,api_ok,complete,pending, \
truncated,failure_since,history_gap_since FROM operations.external_status LIMIT 0;";
const EXTERNAL_OWNERSHIP: &str = "SELECT count(*)=3 AND bool_and( \
c.relowner=(SELECT oid FROM pg_roles WHERE rolname='postgres') \
AND NOT has_table_privilege('simplestchat_app',c.oid,'SELECT,INSERT,UPDATE,DELETE,TRUNCATE') \
AND NOT has_table_privilege('simplestchat_migrate',c.oid, \
'SELECT,INSERT,UPDATE,DELETE,TRUNCATE')) \
FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace \
WHERE n.nspname='operations' AND c.relkind='r' \
AND c.relname IN ('external_schema_version','external_runs','external_status');";

/// The database container, with standard input attached.
fn container(tail: &[&str]) -> Vec<String> {
    let head = ["exec", "-i", "--user", "postgres", POSTGRES];
    let words = DOCKER.iter().chain(&head).chain(tail);
    words.map(|word| (*word).to_owned()).collect()
}

/// Retain a private custom-format archive and prove it lists.
fn backup(host: &Host<'_>) -> Result<()> {
    let sequence = host
        .clock
        .now()
        .timestamp_nanos_opt()
        .ok_or(Failure::Value)?;
    let archive = host
        .paths
        .state
        .join(format!("schema-before-{sequence}.dump"));
    let dump = [&["pg_dump", "--format=custom"][..], &CONNECTION].concat();
    let oversized = "Oversized schema archive response";
    host.commands.run(&Request {
        archive: Some(&archive),
        ..Request::fixed(container(&dump), 60.0, 0, oversized)
    })?;
    require(
        std::fs::metadata(&archive)?.len() > 0,
        "Empty schema backup",
    )?;
    let listed = Request {
        source: Some(&archive),
        ..Request::fixed(
            container(&["pg_restore", "--list"]),
            30.0,
            16 * 1024 * 1024,
            oversized,
        )
    };
    host.commands.run(&listed).map(|_| ())
}

/// Add the separately versioned external-check tables, unless they exist, and
/// require their version, shape, ownership and privileges. Returns whether
/// they were created.
fn external(host: &Host<'_>, backed_up: bool) -> Result<bool> {
    let existing = database(
        host,
        b"SELECT to_regclass('operations.external_schema_version');",
        None,
    )?;
    if existing.is_empty() {
        if !backed_up {
            backup(host)?;
        }
        let schema = std::fs::read(host.paths.sql.join("monitoring-external-schema.sql"))?;
        database(host, &schema, None)?;
    }
    let version = database(
        host,
        b"SELECT version FROM operations.external_schema_version;",
        None,
    )?;
    require(version == b"1", "Unsupported external operations schema")?;
    database(host, EXTERNAL_SHAPE.as_bytes(), None)?;
    let owned = database(host, EXTERNAL_OWNERSHIP.as_bytes(), None)?;
    require(
        owned == b"t",
        "External operations ownership or privileges changed",
    )?;
    Ok(existing.is_empty())
}

/// Prepare the schema and say whether anything was created.
pub(super) fn prepare(host: &Host<'_>) -> Result<&'static str> {
    let probe = |statement: &str, stop: bool| {
        let mut words = vec![
            "exec", "--user", "postgres", POSTGRES, "psql", "-X", "-q", "-t", "-A",
        ];
        words.extend(CONNECTION);
        if stop {
            words.extend(["--set", "ON_ERROR_STOP=1"]);
        }
        words.extend(["-c", statement]);
        docker(host, &words)
    };
    let existing = probe("SELECT to_regclass('operations.schema_version');", false)?;
    if !existing.is_empty() {
        let version = probe("SELECT version FROM operations.schema_version;", false)?;
        require(version == "1", "Unsupported operations schema version")?;
        // Required columns and the singleton shape, before declaring it retained.
        probe(INCIDENT_SHAPE, true)?;
        let owned = probe(INCIDENT_OWNERSHIP, false)?;
        require(
            owned == "t",
            "Operations ownership or role privileges changed",
        )?;
        return Ok(if external(host, false)? {
            "created\n"
        } else {
            "retained\n"
        });
    }
    backup(host)?;
    let schema = std::fs::read(host.paths.sql.join("monitoring-schema.sql"))?;
    let apply = [&["psql", "-X", "-q"][..], &CONNECTION].concat();
    let oversized = "Oversized schema statement response";
    host.commands.run(&Request {
        input: Some(&schema),
        ..Request::fixed(container(&apply), 15.0, 16 * 1024 * 1024, oversized)
    })?;
    external(host, true)?;
    Ok("created\n")
}

#[cfg(test)]
mod tests {
    use super::super::command::tests::Scripted;
    use super::super::host::tests::{Fixed, paths};
    use super::super::state::tests::Scratch;
    use super::*;
    use serde_json::Value;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    /// Statements compare by their words: the earlier source's indentation is
    /// not part of a statement.
    fn words(text: &str) -> Vec<&str> {
        text.split_whitespace().collect()
    }

    /// Answer as the recorded scripted database did for `facts`.
    fn answer(request: &Request<'_>, facts: &Value) -> Result<Vec<u8>> {
        let argv = &request.argv;
        let has = |word: &str| argv.iter().any(|argument| argument == word);
        let input = request
            .input
            .map(String::from_utf8_lossy)
            .unwrap_or_default();
        let statement = if has("-c") {
            argv.last().unwrap().clone()
        } else {
            input.into_owned()
        };
        let fact = |name: &str, default: &str| facts[name].as_str().unwrap_or(default).to_owned();
        // As on a host, the archive exists from before the command starts.
        let archive = request.archive.map(|path| {
            let mut options = std::fs::OpenOptions::new();
            options
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)
                .unwrap()
        });
        if let Some(fail) = facts["fail"].as_str()
            && (has(fail) || statement.contains(fail))
        {
            return Err(Failure::Command);
        }
        let text = if let Some(mut archive) = archive {
            std::io::Write::write_all(&mut archive, fact("dump", "archive").as_bytes()).unwrap();
            String::new()
        } else if statement.contains("to_regclass('operations.schema_version')") {
            fact("schema", "")
        } else if statement.contains("to_regclass('operations.external_schema_version')") {
            fact("external", "")
        } else if statement.trim() == "SELECT version FROM operations.schema_version;" {
            fact("version", "1")
        } else if statement.trim() == "SELECT version FROM operations.external_schema_version;" {
            fact("externalVersion", "1")
        } else if statement.contains("nspowner") {
            fact("permissions", "t")
        } else if statement.contains("count(*)=3") {
            fact("externalPermissions", "t")
        } else {
            String::new()
        };
        Ok(format!("{text}\n").into_bytes())
    }

    /// Twelve recorded databases: a fresh one, a retained one, one gaining the
    /// external tables, each refusal and each failing command give the
    /// recorded outcome through the recorded commands, and the archive is
    /// retained exactly when the recorded one was.
    #[test]
    fn preparation_follows_the_recorded_commands_and_outcomes() {
        let recorded: Value = serde_json::from_str(include_str!("recorded_prepare.json")).unwrap();
        let cases = recorded["prepare"].as_array().unwrap();
        assert_eq!(cases.len(), 12);
        for case in cases {
            let name = case["name"].as_str().unwrap();
            let scratch = Scratch::new();
            let paths = paths(scratch.path());
            std::fs::create_dir_all(&paths.state).unwrap();
            std::fs::create_dir_all(&paths.sql).unwrap();
            for schema in ["monitoring-schema.sql", "monitoring-external-schema.sql"] {
                std::fs::write(paths.sql.join(schema), format!("<{schema}>")).unwrap();
            }
            let commands = Scripted::new(|request| answer(request, &case["facts"]));
            let clock = Fixed::at("2026-11-11T12:00:00.5Z");
            let host = Host {
                commands: &commands,
                clock: &clock,
                paths,
                owner: 0,
            };
            let outcome = prepare(&host);
            let expected = &case["result"];
            match &outcome {
                Ok(printed) => assert_eq!(Some(*printed), expected["stdout"].as_str(), "{name}"),
                Err(Failure::Refused(message)) => {
                    assert_eq!(Some(*message), expected["refused"].as_str(), "{name}");
                }
                Err(_) => assert_eq!(expected["error"], "CalledProcessError", "{name}"),
            }
            let asked = commands.asked.borrow();
            let calls = case["calls"].as_array().unwrap();
            assert_eq!(asked.len(), calls.len(), "{name}");
            for (ours, theirs) in asked.iter().zip(calls) {
                let argv = theirs["argv"].as_array().unwrap();
                let argv: Vec<&str> = argv.iter().map(|word| word.as_str().unwrap()).collect();
                assert_eq!(ours.argv.len(), argv.len(), "{name}");
                for (word, recorded) in ours.argv.iter().zip(argv) {
                    assert_eq!(words(word), words(recorded), "{name}");
                }
                assert_eq!(Some(ours.seconds), theirs["timeout"].as_f64(), "{name}");
                assert_eq!(ours.archive.is_some(), theirs["archive"] == true, "{name}");
                let input = match (&ours.input, &ours.source) {
                    (_, Some(_)) => Some("<archive>".to_owned()),
                    (Some(bytes), None) => Some(String::from_utf8(bytes.clone()).unwrap()),
                    (None, None) => None,
                };
                let recorded = theirs["input"].as_str().map(words);
                assert_eq!(input.as_deref().map(words), recorded, "{name}");
            }
            drop(asked);
            let mut archives = Vec::new();
            for entry in std::fs::read_dir(&host.paths.state).unwrap() {
                let entry = entry.unwrap();
                let metadata = entry.metadata().unwrap();
                let mode = format!("0o{:o}", metadata.permissions().mode() & 0o777);
                let name = entry.file_name().into_string().unwrap();
                archives.push(serde_json::json!([name, metadata.len(), mode]));
            }
            assert_eq!(Value::Array(archives), case["archives"], "{name}");
        }
    }
}
