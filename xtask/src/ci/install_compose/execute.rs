//! Ordered installation and fixed diagnostic evidence. No received text escapes.

use super::{
    files::{self, Owned},
    plan::{self, Context},
};
use crate::ci::process::Executor;
use crate::tooling::Result;
use serde::Serialize;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Report {
    version: &'static str,
    architecture: &'static str,
    passed: bool,
    failure: Option<&'static str>,
    exit_code: u8,
    download_bytes: Option<u64>,
    verified: bool,
    installation_attempted: bool,
    installation_completed: bool,
    caller_version_passed: bool,
    root_version_passed: bool,
    cleanup_passed: bool,
    process_cleanup_passed: bool,
    staging_cleanup_passed: bool,
}

fn version(bytes: &[u8]) -> Result<()> {
    if std::str::from_utf8(bytes)
        .ok()
        .map(|value| value.trim_end_matches('\n'))
        != Some(plan::VERSION)
    {
        return Err(files::failure(
            "Compose version must match the pinned release.",
        ));
    }
    Ok(())
}

pub(super) async fn install(context: &Context, runner: &mut impl Executor) -> Result<()> {
    runner.ready().await?;
    let mut owned = Owned::new(context)?;
    let mut report = Report {
        version: plan::VERSION,
        architecture: context.asset.architecture,
        passed: false,
        failure: None,
        exit_code: 1,
        download_bytes: None,
        verified: false,
        installation_attempted: false,
        installation_completed: false,
        caller_version_passed: false,
        root_version_passed: false,
        cleanup_passed: false,
        process_cleanup_passed: false,
        staging_cleanup_passed: false,
    };
    let mut stage = "prepare";
    let primary: Result<()> = async {
        owned.prepare(context)?;
        stage = "download";
        runner
            .run(plan::download(context.asset, owned.download_sink()?))
            .await?;
        runner.ready().await?;
        stage = "authenticate";
        report.download_bytes = Some(owned.verify(context.asset.digest)?);
        report.verified = true;
        runner.ready().await?;
        owned.check_verified()?;
        stage = "install_directory";
        report.installation_attempted = true;
        runner.run(plan::install_directory()).await?;
        runner.ready().await?;
        owned.check_verified()?;
        stage = "install_file";
        runner
            .run(plan::install_file(&owned.path.join("verified")))
            .await?;
        report.installation_completed = true;
        stage = "caller_version";
        version(&runner.run(plan::caller_version()).await?)?;
        report.caller_version_passed = true;
        stage = "root_version";
        version(&runner.run(plan::root_version()).await?)?;
        report.root_version_passed = true;
        runner.ready().await
    }
    .await;
    // Always clean the owned staging namespace, even if cancellation prevents all
    // remaining process work. A diagnostic records attempted installation truth.
    let (reporter, cleanup) = owned.cleanup();
    report.staging_cleanup_passed = cleanup.is_ok();
    report.process_cleanup_passed = runner.cleanup_passed();
    let cleanup = cleanup.and(if report.process_cleanup_passed {
        Ok(())
    } else {
        Err(files::failure("Compose process cleanup unproven."))
    });
    report.cleanup_passed = cleanup.is_ok();
    if primary.is_ok() && cleanup.is_err() {
        stage = "cleanup";
    }
    let result = primary.and(cleanup);
    if let Err(error) = &result {
        report.failure = Some(stage);
        report.exit_code = error.code;
    } else if let Err(error) = runner.ready().await {
        report.failure = Some("finalize");
        report.exit_code = error.code;
        reporter.write(&report)?;
        return Err(error);
    } else {
        report.passed = true;
        report.exit_code = 0;
    }
    reporter.write(&report)?;
    result
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_matches_shell_trailing_newline_semantics_without_accepting_other_text() {
        for value in [b"5.6.0".as_slice(), b"5.6.0\n", b"5.6.0\n\n"] {
            super::version(value).unwrap();
        }
        for value in [
            b"5.6.0\r\n".as_slice(),
            b"5.6.0 private",
            b"5.6.0\nextra",
            b"\xff",
        ] {
            assert!(super::version(value).is_err());
        }
    }
}
