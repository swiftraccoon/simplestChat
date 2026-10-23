//! Rust owns its CLI groups; Python retains ownership of isolated fixture work.

use super::{
    files::{self, Executable},
    plan::{self, Context},
};
use crate::{ci::process::Executor, tooling::Result};
use std::{sync::Arc, time::Instant};

pub(super) async fn execute(context: &Context, runner: &mut impl Executor) -> Result<()> {
    let deadline = Instant::now() + plan::BUDGET;
    runner.ready().await?;
    let http_policy = files::Executable::open(&context.http_policy)?;
    let node = match files::node(&context.root, &context.environment) {
        Ok(node) => Arc::new(node),
        Err(error) => {
            let _ = http_policy.close();
            return Err(error);
        }
    };
    let mut chromium = None;
    let mut python = None;
    let primary: Result<()> = async {
        http_policy.validate()?;
        let bytes = runner
            .run(plan::discover(Arc::clone(&node), deadline)?)
            .await?;
        node.validate()?;
        http_policy.validate()?;
        runner.ready().await?;
        chromium = Some(files::chromium(&bytes)?);
        python = Some(Arc::new(files::select(
            &context.root,
            &context.environment,
            std::ffi::OsStr::new("python3"),
        )?));
        runner
            .run(plan::coordinator(
                context,
                node.validate()?,
                chromium.as_ref().unwrap().validate()?,
                http_policy.validate()?,
                python.as_ref().unwrap().clone(),
                deadline,
            )?)
            .await?;
        node.validate()?;
        http_policy.validate()?;
        chromium.as_ref().unwrap().validate()?;
        runner.ready().await?;
        plan::fence(deadline)
    }
    .await;
    let closed_node = Arc::try_unwrap(node)
        .map_err(|_| files::failure("Release-check Node still borrowed."))
        .and_then(Executable::close);
    let closed_chromium = chromium.map_or(Ok(()), Executable::close);
    let closed_http = http_policy.close();
    let closed_python = python.map_or(Ok(()), |proof| {
        let checked = proof.validate().map(|_| ());
        let closed = Arc::try_unwrap(proof)
            .map_err(|_| files::failure("Release-check Python still borrowed."))
            .and_then(Executable::close);
        checked.and(closed)
    });
    let final_clock = plan::fence(deadline);
    let cleaned = if runner.cleanup_passed() {
        Ok(())
    } else {
        Err(files::failure("Release-check CLI cleanup unproven."))
    };
    primary
        .and(closed_node)
        .and(closed_chromium)
        .and(closed_http)
        .and(closed_python)
        .and(cleaned)
        .and(final_clock)
}
