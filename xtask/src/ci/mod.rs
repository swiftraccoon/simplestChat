//! The release control plane's commands. The hosted CI steps and the bounded
//! command runner the plane drives live in `simplestchat-ci`.

use anyhow::Result;
use std::path::Path;
use std::process::ExitCode;

mod ops_artifact;

pub(crate) fn run_release_frontend(root: &Path, arguments: &[String]) -> Result<ExitCode> {
    Ok(ops_artifact::run_release_frontend(root, arguments))
}

pub(crate) fn run_ops_artifact(arguments: &[String]) -> Result<ExitCode> {
    ops_artifact::run(arguments)
}

pub(crate) fn run_vps_capacity(arguments: &[String]) -> Result<ExitCode> {
    if arguments.len() == 1 && matches!(arguments[0].as_str(), "--help" | "-h") {
        println!("{}", ops_artifact::capacity_vps::USAGE);
        return Ok(ExitCode::SUCCESS);
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    Ok(
        match runtime.block_on(ops_artifact::capacity_vps::native(arguments)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{}", error.message);
                ExitCode::from(error.code)
            }
        },
    )
}
