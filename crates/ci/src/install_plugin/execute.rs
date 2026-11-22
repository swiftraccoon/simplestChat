//! Ordered installation and fixed diagnostic evidence. No received text escapes.

use super::{
    files::{self, Owned},
    plan::{self, Context},
};
use crate::process::Executor;
use serde::Serialize;
use simplestchat_toolkit::tooling::Result;

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

/// Accept only the release's exact version line, with any trailing newlines.
fn version(bytes: &[u8], release: &plan::Release) -> Result<()> {
    if std::str::from_utf8(bytes)
        .ok()
        .map(|value| value.trim_end_matches('\n'))
        != Some(release.answer)
    {
        return Err(files::failure(
            "Plugin version must match the pinned release.",
        ));
    }
    Ok(())
}

pub(super) async fn install(context: &Context, runner: &mut impl Executor) -> Result<()> {
    runner.ready().await?;
    let mut owned = Owned::new(context)?;
    let mut report = Report {
        version: context.release.version,
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
            .run(plan::download(
                context.release,
                context.asset,
                owned.download_sink()?,
            ))
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
            .run(plan::install_file(
                context.release,
                &owned.path.join("verified"),
            ))
            .await?;
        report.installation_completed = true;
        stage = "caller_version";
        version(
            &runner.run(plan::caller_version(context.release)).await?,
            context.release,
        )?;
        report.caller_version_passed = true;
        stage = "root_version";
        version(
            &runner.run(plan::root_version(context.release)).await?,
            context.release,
        )?;
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
        Err(files::failure("Plugin process cleanup unproven."))
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
    use crate::install_plugin::Plugin;

    #[test]
    fn version_matches_shell_trailing_newline_semantics_without_accepting_other_text() {
        let buildx = "github.com/docker/buildx v0.39.1 334b63fe2bb0f0e89a8fd067704a0d113f57ec25";
        for (plugin, answer, other) in [
            (Plugin::Compose, "5.6.0", buildx),
            (Plugin::Buildx, buildx, "5.6.0"),
        ] {
            let release = plugin.release();
            for suffix in ["", "\n", "\n\n"] {
                super::version(format!("{answer}{suffix}").as_bytes(), release).unwrap();
            }
            for value in [
                format!("{answer}\r\n").into_bytes(),
                format!("{answer} private").into_bytes(),
                format!("{answer}\nextra").into_bytes(),
                format!(" {answer}").into_bytes(),
                answer.as_bytes()[..answer.len() - 1].to_vec(),
                other.as_bytes().to_vec(),
                b"\xff".to_vec(),
                Vec::new(),
            ] {
                assert!(super::version(&value, release).is_err(), "{plugin:?}");
            }
        }
        let older = "github.com/docker/buildx v0.39.0 334b63fe2bb0f0e89a8fd067704a0d113f57ec25";
        assert!(super::version(older.as_bytes(), Plugin::Buildx.release()).is_err());
    }
}
