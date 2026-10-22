//! Read-only by default, narrowly scoped GitHub cache maintenance.
//!
//! Apply requires successful exact-main CI, a fresh main head, complete bounded
//! inventories and an unchanged, still-disposable cache ID before each deletion.
//! No source, workflow run, artifact or repository deletion endpoint is allowed.

mod client;
mod policy;
mod process;

#[cfg(test)]
#[path = "ci_retention/retention_tests.rs"]
mod tests;

use anyhow::Result;
use serde_json::{Value, json};
use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "\
Usage: cargo run --locked -q -p xtask -- ci-retention [--apply --run-id ID --revision SHA]
Default: print a read-only plan for allowlisted main cache families.
Apply requires exact successful push CI and the current main revision.
";

struct Options {
    apply: bool,
    run_id: String,
    revision: String,
}

fn options(arguments: &[String]) -> Option<std::result::Result<Options, ()>> {
    let mut result = Options {
        apply: false,
        run_id: "0".into(),
        revision: String::new(),
    };
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--help" | "-h" => return None,
            "--apply" => result.apply = true,
            "--run-id" | "--revision" => {
                let Some(value) = arguments.next() else {
                    return Some(Err(()));
                };
                if argument == "--run-id" {
                    result.run_id = value.clone();
                } else {
                    result.revision = value.clone();
                }
            }
            "--" if arguments.len() == 0 => break,
            _ => {
                if let Some(value) = argument.strip_prefix("--run-id=") {
                    result.run_id = value.into();
                } else if let Some(value) = argument.strip_prefix("--revision=") {
                    result.revision = value.into();
                } else {
                    return Some(Err(()));
                }
            }
        }
    }
    if result.run_id.parse::<i128>().is_err() {
        return Some(Err(()));
    }
    Some(Ok(result))
}

async fn execute(client: &mut client::Github, options: &Options) -> Result<Value> {
    if options.apply {
        let run = options.run_id.parse::<u64>().unwrap_or(0);
        client.trusted_run(run, &options.revision).await?;
    }
    let entries = client.inventory(None).await?;
    let selected = policy::plan(&entries, (client.now)());
    let before = entries.iter().map(|entry| entry.size).sum::<u64>();
    let after = before - selected.iter().map(|entry| entry.size).sum::<u64>();
    let plan = json!({
        "repository": policy::REPOSITORY, "ref": policy::REFERENCE,
        "dryRun": !options.apply, "bytesBefore": before, "bytesAfterPlan": after,
        "softLimitBytes": policy::SOFT_BYTES,
        "protectedBytesExceedLimit": after > policy::SOFT_BYTES,
        "delete": selected.iter().map(policy::Entry::summary).collect::<Vec<_>>()
    });
    println!("{plan}");
    if options.apply {
        for entry in selected {
            client.remove(&entry, &options.revision).await?;
        }
    }
    Ok(plan)
}

pub(crate) fn run(root: &Path, arguments: &[String]) -> Result<ExitCode> {
    let Some(parsed) = options(arguments) else {
        print!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    };
    let Ok(options) = parsed else {
        eprint!("{USAGE}");
        return Ok(ExitCode::from(2));
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let signals = process::Signals::new()?;
        let mut client = client::Github::new(
            root.into(),
            crate::tooling::inherited_env(),
            options.apply,
            signals,
        );
        match execute(&mut client, &options).await {
            Ok(_) => {
                if options.apply {
                    println!("{}", json!({"deletedIds": client.deleted}));
                }
                Ok(ExitCode::SUCCESS)
            }
            Err(error) => {
                eprintln!("Cache retention stopped; no source, runs or artifacts were targeted.");
                println!("{}", json!({"deletedIds": client.deleted}));
                let code = error
                    .downcast_ref::<process::Interrupted>()
                    .map_or(1, |error| error.0);
                Ok(ExitCode::from(code))
            }
        }
    })
}
