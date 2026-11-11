//! The private operational report and the one database session every
//! monitoring statement uses: the database container's own client, over its
//! socket, with peer authentication and reviewed statements on standard input.

use super::alerts;
use crate::host::command::Request;
use crate::host::failure::{Failure, Result};
use crate::host::state;
use crate::host::{DOCKER, Host, POSTGRES};
use crate::json::Json;

const SESSION: [&str; 18] = [
    "exec",
    "-i",
    "--user",
    "postgres",
    POSTGRES,
    "psql",
    "-X",
    "-q",
    "-t",
    "-A",
    "--set",
    "ON_ERROR_STOP=1",
    "-h",
    "/run/simplestchat-postgres",
    "-U",
    "postgres",
    "-d",
    "simplestchat",
];
const ENVIRONMENT: [(&str, &str); 2] = [("PATH", "/usr/bin:/bin"), ("LC_ALL", "C")];
const SCOPE: &str = "Local spool health; a discarded snapshot means historical coverage was lost.";

/// Run one reviewed statement, with an optional JSON payload passed as a
/// client variable the statement quotes itself, and return its trimmed answer.
pub(super) fn database(host: &Host<'_>, source: &[u8], payload: Option<&str>) -> Result<Vec<u8>> {
    let mut argv: Vec<String> = DOCKER
        .iter()
        .chain(&SESSION)
        .map(|word| (*word).to_owned())
        .collect();
    if let Some(payload) = payload {
        argv.extend(["--set".to_owned(), format!("payload={payload}")]);
    }
    let oversized = "Oversized operational database response";
    let output = host.commands.run(&Request {
        environment: &ENVIRONMENT,
        input: Some(source),
        ..Request::fixed(argv, 8.0, 256 * 1024, oversized)
    })?;
    Ok(output.trim_ascii().to_vec())
}

/// Local spool health, visible even when discarded snapshots never reached
/// the database.
#[derive(serde::Serialize)]
struct Importer<'a> {
    #[serde(rename = "lastPollEpoch")]
    last_poll_epoch: Option<&'a Json>,
    #[serde(rename = "spoolSnapshots")]
    spool_snapshots: usize,
    #[serde(rename = "discardedSnapshots")]
    discarded_snapshots: i128,
    deployment: Option<&'a Json>,
    scope: &'static str,
}

/// The database's report with the importer's local state beside it.
#[derive(serde::Serialize)]
struct Report<'a> {
    #[serde(flatten)]
    database: std::collections::BTreeMap<&'a str, &'a Json>,
    importer: Importer<'a>,
}

/// One line of operational metadata: retained incidents and external-check
/// coverage from the database, and the importer's spool health.
pub(super) fn report(host: &Host<'_>) -> Result<String> {
    let statement = std::fs::read(host.paths.sql.join("monitoring-report.sql"))?;
    let answer = state::decode(&database(host, &statement, None)?)?;
    let fields = answer.object().ok_or(Failure::Value)?;
    let importer_state =
        state::object_or_empty(&host.paths.state.join("external.json"), 1024 * 1024)?;
    let discarded = match importer_state.get("dropped") {
        None => 0,
        Some(Json::Integer(text)) => text.parse().map_err(|_| Failure::Value)?,
        Some(_) => return Err(Failure::Value),
    };
    let report = Report {
        database: fields
            .iter()
            .filter(|(key, _)| key.as_str() != "importer")
            .map(|(key, value)| (key.as_str(), value))
            .collect(),
        importer: Importer {
            last_poll_epoch: importer_state.get("lastPollAt"),
            spool_snapshots: alerts::entries(&host.paths.state.join("external-spool"))?.len(),
            discarded_snapshots: discarded,
            deployment: importer_state.get("deployment"),
            scope: SCOPE,
        },
    };
    let mut line = serde_json::to_string(&report).map_err(|_| Failure::Value)?;
    line.push('\n');
    Ok(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::command::tests::Scripted;
    use crate::host::state::tests::Scratch;
    use crate::host::tests::{Fixed, paths};
    use serde_json::Value;

    /// Every recorded answer of the database and state of the importer gives
    /// the recorded report, or no report at all, through the recorded session.
    #[test]
    fn reports_and_the_database_session_are_the_recorded_ones() {
        let recorded: Value = serde_json::from_str(include_str!("recorded_prepare.json")).unwrap();
        let cases = recorded["report"].as_array().unwrap();
        assert_eq!(cases.len(), 9);
        for case in cases {
            let name = case["name"].as_str().unwrap();
            let scratch = Scratch::new();
            let paths = paths(scratch.path());
            std::fs::create_dir_all(paths.state.join("external-spool")).unwrap();
            std::fs::create_dir_all(&paths.sql).unwrap();
            std::fs::write(paths.sql.join("monitoring-report.sql"), b"-- report\n").unwrap();
            for number in 0..case["spool"].as_u64().unwrap() {
                let entry = paths
                    .state
                    .join(format!("external-spool/{number:020}.json"));
                std::fs::write(entry, "{}").unwrap();
            }
            if let Some(text) = case["externalState"].as_str() {
                std::fs::write(paths.state.join("external.json"), text).unwrap();
            }
            let answer = case["database"].as_str().unwrap();
            let fail = case["fail"] == true;
            let commands = Scripted::new(|_| {
                if fail {
                    Err(Failure::Command)
                } else {
                    Ok(answer.as_bytes().to_vec())
                }
            });
            let clock = Fixed::at("2026-11-11T12:00:00Z");
            let host = Host {
                commands: &commands,
                clock: &clock,
                paths,
                owner: 0,
            };
            let printed = report(&host);
            if case["status"] == 0 {
                let ours: Value = serde_json::from_str(&printed.unwrap()).unwrap();
                let theirs: Value = serde_json::from_str(case["stdout"].as_str().unwrap()).unwrap();
                assert_eq!(ours, theirs, "{name}");
            } else {
                assert!(printed.is_err(), "{name}");
            }
            let asked = commands.asked.borrow();
            assert_eq!(asked.len(), 1, "{name}");
            let call = &case["calls"][0];
            let argv = call["argv"].as_array().unwrap();
            let argv = argv.iter().map(|word| word.as_str().unwrap());
            assert_eq!(asked[0].argv, argv.collect::<Vec<_>>(), "{name}");
            assert_eq!(Some(asked[0].seconds), call["timeout"].as_f64(), "{name}");
            assert_eq!(
                asked[0].input.as_deref(),
                Some(&b"-- report\n"[..]),
                "{name}"
            );
            let environment: std::collections::BTreeMap<_, _> =
                asked[0].environment.iter().cloned().collect();
            assert_eq!(
                serde_json::to_value(environment).unwrap(),
                call["env"],
                "{name}"
            );
        }
    }

    /// A payload travels as one client variable after the fixed session.
    #[test]
    fn a_payload_is_one_client_variable_and_the_answer_is_trimmed() {
        let scratch = Scratch::new();
        let commands = Scripted::new(|_| Ok(b"  t \n".to_vec()));
        let clock = Fixed::at("2026-11-11T12:00:00Z");
        let host = Host {
            commands: &commands,
            clock: &clock,
            paths: paths(scratch.path()),
            owner: 0,
        };
        assert_eq!(
            database(&host, b"SELECT 1;", Some("{\"a\":1}")).unwrap(),
            b"t"
        );
        let asked = commands.asked.borrow();
        let tail = &asked[0].argv[asked[0].argv.len() - 2..];
        assert_eq!(tail, ["--set", "payload={\"a\":1}"]);
        assert_eq!(asked[0].input.as_deref(), Some(&b"SELECT 1;"[..]));
    }
}
