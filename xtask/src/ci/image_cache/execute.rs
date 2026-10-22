//! A metadata miss falls back only after cancellation and cleanup are checked.

use super::{
    failure,
    plan::{Context, Operation},
};
use std::io::Write;

use crate::{
    ci::{github_output::Target, process::Executor},
    tooling::Result,
};

const WARNING: &str =
    "Database cache hint unavailable; the online updater will check current upstream data.";

pub(super) async fn execute(
    context: &Context,
    operation: Operation,
    runner: &mut impl Executor,
    warnings: &mut impl Write,
) -> Result<()> {
    runner.ready().await?;
    if !runner.cleanup_passed() {
        return Err(failure("Image cache process cleanup unproven."));
    }
    let target = Target::open_environment(&context.environment)
        .map_err(|_| failure("Image cache output unavailable."))?;
    let primary = async {
        let prefix = context.prefix(&runner.run(context.reference(None)).await?)?;
        runner.ready().await?;
        if !runner.cleanup_passed() {
            return Err(failure("Image cache process cleanup unproven."));
        }
        let bytes = match runner.run(context.reference(Some(operation))).await {
            Ok(bytes) => bytes,
            Err(error) if operation == Operation::Hint => {
                runner.ready().await?;
                if matches!(error.code, 130 | 143) || !runner.cleanup_passed() {
                    return Err(error);
                }
                writeln!(warnings, "{WARNING}")
                    .map_err(|_| failure("Image cache warning publication failed."))?;
                prefix.as_bytes().into()
            }
            Err(error) => return Err(error),
        };
        runner.ready().await?;
        if !runner.cleanup_passed() {
            return Err(failure("Image cache process cleanup unproven."));
        }
        context.selection(&prefix, &bytes, operation == Operation::Hint)
    }
    .await;
    match primary {
        Ok(selection) => target
            .append_image(&selection)
            .map_err(|_| failure("Image cache output publication failed.")),
        Err(error) => {
            let _ = target.close();
            Err(error)
        }
    }
}
