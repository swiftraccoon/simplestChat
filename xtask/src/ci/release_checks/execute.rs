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
    let node = Arc::new(files::node(&context.root, &context.environment)?);
    let mut chromium = None;
    let primary: Result<()> = async {
        let bytes = runner
            .run(plan::discover(Arc::clone(&node), deadline)?)
            .await?;
        node.validate()?;
        runner.ready().await?;
        chromium = Some(files::chromium(&bytes)?);
        runner
            .run(plan::coordinator(
                context,
                node.validate()?,
                chromium.as_ref().unwrap().validate()?,
                deadline,
            )?)
            .await?;
        node.validate()?;
        chromium.as_ref().unwrap().validate()?;
        runner.ready().await
    }
    .await;
    let closed_node = Arc::try_unwrap(node)
        .map_err(|_| files::failure("Release-check Node still borrowed."))
        .and_then(Executable::close);
    let closed_chromium = chromium.map_or(Ok(()), Executable::close);
    let cleaned = if runner.cleanup_passed() {
        Ok(())
    } else {
        Err(files::failure("Release-check CLI cleanup unproven."))
    };
    primary.and(closed_node).and(closed_chromium).and(cleaned)
}
