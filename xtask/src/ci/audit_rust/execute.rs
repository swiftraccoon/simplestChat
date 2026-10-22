//! Installer and audit failures stay distinct; all owned cleanup is attempted.

use super::{
    files::{self, Owned},
    plan::{self, Context},
};
use crate::{
    ci::{process::Executor, selected_executable::Executable},
    tooling::Result,
};
use serde::Serialize;
use std::{sync::Arc, time::Instant};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Report {
    passed: bool,
    failure: Option<&'static str>,
    exit_code: u8,
    installer_completed: bool,
    executable_verified: bool,
    audit_completed: bool,
    executable_closed: bool,
    process_cleanup_passed: bool,
    namespace_cleanup_passed: bool,
    cleanup_passed: bool,
}
pub(super) async fn execute(context: &Context, runner: &mut impl Executor) -> Result<()> {
    let deadline = Instant::now() + plan::BUDGET;
    runner.ready().await?;
    let owned = Owned::new(context)?;
    let mut selected = None;
    let mut admission_closed = true;
    let mut stage = "prepare";
    let mut report = Report {
        passed: false,
        failure: None,
        exit_code: 1,
        installer_completed: false,
        executable_verified: false,
        audit_completed: false,
        executable_closed: true,
        process_cleanup_passed: false,
        namespace_cleanup_passed: false,
        cleanup_passed: false,
    };
    let primary: Result<()> = async {
        owned.prepare()?;
        let tools = owned.path.join("tools");
        stage = "install";
        runner.run(plan::reference(&tools, true, deadline)?).await?;
        report.installer_completed = true;
        owned.check()?;
        stage = "resolve";
        let returned = runner
            .run(plan::reference(&tools, false, deadline)?)
            .await?;
        owned.check()?;
        stage = "validate";
        selected = Some(Arc::new(Executable::open(
            &owned.path,
            owned.descriptor()?,
            &returned,
            &mut admission_closed,
        )?));
        report.executable_verified = true;
        report.executable_closed = false;
        stage = "audit";
        runner
            .run(plan::audit(
                Arc::clone(selected.as_ref().unwrap()),
                deadline,
            )?)
            .await?;
        report.audit_completed = true;
        selected.as_ref().unwrap().validate()?;
        runner.ready().await
    }
    .await;
    let closed = match selected {
        Some(executable) => Arc::try_unwrap(executable)
            .map_err(|_| files::failure("Rust audit executable still borrowed."))
            .and_then(Executable::close),
        None if admission_closed => Ok(()),
        None => Err(files::failure("Rust audit admission close unproven.")),
    };
    report.executable_closed = closed.is_ok();
    let (reporter, cleanup) = owned.cleanup();
    report.namespace_cleanup_passed = cleanup.is_ok();
    report.process_cleanup_passed = runner.cleanup_passed();
    let cleanup = closed.and(cleanup).and(if report.process_cleanup_passed {
        Ok(())
    } else {
        Err(files::failure("Rust audit process cleanup unproven."))
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
