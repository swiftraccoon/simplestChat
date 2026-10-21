//! Executed local-CI matrix receipts bound to one launcher nonce and source pair.

use anyhow::{Result, ensure};
use receipt::Identity;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

mod files;
pub(crate) mod receipt;
#[cfg(test)]
mod tests;
mod workflows;

const USAGE: &str = "usage: cargo run --locked -q -p xtask -- ci-evidence \
record|complete|requirements [--id CHECK] [--root CHECKOUT]";

struct Context {
    root: PathBuf,
    environment: BTreeMap<String, String>,
}

impl Context {
    fn env(&self, name: &str) -> Result<&str> {
        self.environment
            .get(name)
            .map(String::as_str)
            .ok_or_else(|| anyhow::anyhow!("'{name}'"))
    }

    fn identity(&self) -> Result<Identity> {
        let event = files::document(Path::new(self.env("GITHUB_EVENT_PATH")?))?;
        let base = event["before"]
            .as_str()
            .filter(|value| !value.is_empty() && value.chars().count() <= 2048)
            .ok_or_else(|| anyhow::anyhow!("invalid_tool_string"))?;
        let identity = Identity {
            run_id: self.env("LOCAL_CI_RUN_ID")?.to_owned(),
            revision: self.env("GITHUB_SHA")?.to_owned(),
            base: base.to_owned(),
        };
        let hex = |value: &str, length| {
            value.len() == length
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        };
        ensure!(hex(&identity.run_id, 32), "local_ci_run_id");
        ensure!(
            hex(&identity.revision, 40) && hex(&identity.base, 40),
            "local_ci_revision"
        );
        Ok(identity)
    }

    fn record(&self, identifier: &str) -> Result<()> {
        ensure!(
            !identifier.is_empty()
                && identifier.len() <= 80
                && identifier.as_bytes()[0].is_ascii_lowercase()
                && identifier
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'),
            "local_ci_check_id"
        );
        let output = Path::new(self.env("LOCAL_CI_EVIDENCE")?)
            .join("receipts")
            .join(format!("{identifier}.json"));
        files::write_new(&output, &self.identity()?.matrix_receipt(identifier))
    }

    fn complete(&self) -> Result<()> {
        let inventory = workflows::Inventory::read(&self.root)?;
        let needs: Value = serde_json::from_str(self.env("GATE_RESULTS")?)?;
        let needs = workflows::object(&needs)?;
        ensure!(
            needs.keys().cloned().collect::<BTreeSet<_>>() == inventory.gates,
            "local_ci_gate_inventory"
        );
        for result in needs.values() {
            ensure!(
                workflows::object(result)?.get("result") == Some(&Value::from("success")),
                "local_ci_gate_failure"
            );
        }
        let identity = self.identity()?;
        let evidence = Path::new(self.env("LOCAL_CI_EVIDENCE")?);
        let receipts = evidence.join("receipts");
        let observed = match receipts.read_dir() {
            Ok(entries) => entries
                .map(|entry| -> Result<Option<String>> {
                    let path = entry?.path();
                    let Some(name) = path
                        .file_name()
                        .filter(|name| name.as_encoded_bytes().ends_with(b".json"))
                    else {
                        return Ok(None);
                    };
                    let name = name
                        .to_str()
                        .ok_or_else(|| anyhow::anyhow!("local_ci_incomplete_matrix_receipts"))?;
                    Ok(name.strip_suffix(".json").map(str::to_owned))
                })
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .flatten()
                .collect(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeSet::new(),
            Err(error) => return Err(error.into()),
        };
        ensure!(
            observed == inventory.checks,
            "local_ci_incomplete_matrix_receipts"
        );
        for identifier in &inventory.checks {
            let actual = files::document(&receipts.join(format!("{identifier}.json")))?;
            ensure!(
                identity.matches_matrix(&actual, identifier),
                "local_ci_receipt_identity"
            );
        }
        files::write_new(
            &evidence.join("required.json"),
            &json!({
                "schema": 1, "status": "passed", "runId": identity.run_id,
                "revision": identity.revision, "base": identity.base,
                "gates": inventory.gates, "checks": inventory.checks,
                "workflows": inventory.hashes,
            }),
        )
    }

    /// Retained only for callers comparing the old Python helper's operations.
    /// The Rust completion command needs no Python environment or YAML install.
    fn requirements(&self) -> Result<String> {
        let bytes = files::bounded(&self.root.join("ops/ansible/requirements.txt"))?;
        let source = String::from_utf8(bytes)?.replace("\\\n", " ");
        let entries = source
            .lines()
            .filter(|line| line.starts_with("pyyaml=="))
            .collect::<Vec<_>>();
        ensure!(
            entries.len() == 1 && entries[0].contains("--hash=sha256:"),
            "local_ci_yaml_requirement"
        );
        Ok(format!("{}\n", entries[0]))
    }

    fn execute(&self, operation: &str, identifier: &str) -> Result<()> {
        ensure!(
            self.environment.get("ACT").map(String::as_str) == Some("true")
                && self
                    .environment
                    .get("LOCAL_CI_DISPOSABLE")
                    .map(String::as_str)
                    == Some("1"),
            "local_ci_runner_only"
        );
        match operation {
            "record" => self.record(identifier),
            "complete" => self.complete(),
            "requirements" => {
                print!("{}", self.requirements()?);
                Ok(())
            }
            _ => unreachable!("operation validated at CLI boundary"),
        }
    }
}

pub(crate) fn run(root: &Path, arguments: &[String]) -> Result<ExitCode> {
    if arguments
        .iter()
        .any(|argument| argument == "--help" || argument == "-h")
    {
        println!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    }
    let mut operation = None;
    let mut identifier = String::new();
    let mut root = root.to_path_buf();
    let mut arguments = arguments.iter();
    let parsed = (|| {
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "record" | "complete" | "requirements" if operation.is_none() => {
                    operation = Some(argument.clone());
                }
                "--id" => identifier = arguments.next()?.clone(),
                "--root" => root = PathBuf::from(arguments.next()?),
                _ => return None,
            }
        }
        operation
    })();
    let Some(operation) = parsed else {
        eprintln!("{USAGE}");
        return Ok(ExitCode::from(2));
    };
    let work = || -> Result<()> {
        let context = Context {
            root: root.canonicalize()?,
            environment: std::env::vars_os()
                .map(|(name, value)| {
                    Ok((
                        name.into_string()
                            .map_err(|_| anyhow::anyhow!("local_ci_environment"))?,
                        value
                            .into_string()
                            .map_err(|_| anyhow::anyhow!("local_ci_environment"))?,
                    ))
                })
                .collect::<Result<_>>()?,
        };
        context.execute(&operation, &identifier)
    };
    match work() {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(error) => {
            eprintln!("Local CI evidence failed: {error}");
            Ok(ExitCode::FAILURE)
        }
    }
}
