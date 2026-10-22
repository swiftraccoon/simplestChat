//! Independently bounded binary stdout and private diagnostics, without a pipe shell.

use super::{
    files::{Area, Owned, Record},
    plan,
};
use crate::{
    ci::{
        bounded_log::Log,
        command::{Command, SeparateLogs},
        process,
    },
    tooling::Result,
};
use std::sync::Arc;

pub(super) async fn output(
    files: &Owned,
    area: Area,
    name: &str,
    maximum: u64,
    mut request: Command,
) -> Result<(Command, Record, Record)> {
    if request.stdout.is_some() || request.combined_log.is_some() || request.separate_logs.is_some()
    {
        let _ = process::settle_unforwarded(request).await;
        return Err(plan::failure());
    }
    let result = (|| {
        let (file, record) = files.create(area, name, maximum)?;
        let stdout = Log::start(file, maximum)?;
        Ok((stdout, record))
    })();
    let (stdout, record) = match result {
        Ok(value) => value,
        Err(error) => {
            let _ = process::settle_unforwarded(request).await;
            return Err(error);
        }
    };
    // Move the first admitted owner into the request before admitting its peer,
    // so every later failure uses the same all-attempt Native finalizer.
    request.capture = None;
    request.combined_log = Some(Arc::new(stdout));
    let second = (|| {
        let (file, record) = files.log(plan::LOG_LIMIT)?;
        Ok((Log::start(file, plan::LOG_LIMIT)?, record))
    })();
    let (stderr, diagnostic) = match second {
        Ok(value) => value,
        Err(error) => {
            let _ = process::settle_unforwarded(request).await;
            return Err(error);
        }
    };
    let stdout = match Arc::try_unwrap(request.combined_log.take().unwrap()) {
        Ok(value) => value,
        Err(value) => {
            request.combined_log = Some(value);
            let _ = stderr.finish().await;
            let _ = process::settle_unforwarded(request).await;
            return Err(plan::failure());
        }
    };
    request.separate_logs = Some(Arc::new(SeparateLogs { stdout, stderr }));
    Ok((request, record, diagnostic))
}
