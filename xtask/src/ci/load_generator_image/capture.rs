//! All metadata reaches an anchored bounded file before decoding; no raw replay.

use super::{
    files::Owned,
    plan::{self, Context},
};
use crate::{
    ci::{
        bounded_log::Log,
        command::Command,
        process::{self, Executor},
    },
    tooling::Result,
};
use std::sync::Arc;

pub(super) struct Capture<'a, E> {
    pub inner: &'a mut E,
    pub files: &'a Owned,
    pub context: &'a Context,
    pub tools: &'a super::tools::Tools,
}
impl<E: Executor> Executor for Capture<'_, E> {
    async fn run(&mut self, mut request: Command) -> Result<Vec<u8>> {
        if request.load_image.is_some() {
            let _ = process::settle_unforwarded(request).await;
            return Err(plan::failure());
        }
        request.load_image = self.tools.invocation(request.program);
        let admitted = async {
            if !request.cleanup {
                self.inner.ready().await?;
            }
            self.files.check()?;
            request.timeout = request
                .timeout
                .min(plan::available(self.context, &request)?);
            Ok(())
        }
        .await;
        if let Err(error) = admitted {
            let _ = process::settle_unforwarded(request).await;
            return Err(error);
        }
        let record = if let Some(maximum) = request.capture.take() {
            if request.stdout.is_some()
                || request.combined_log.is_some()
                || request.separate_logs.is_some()
            {
                let _ = process::settle_unforwarded(request).await;
                return Err(plan::failure());
            }
            let (file, record) = match self.files.log(maximum) {
                Ok(value) => value,
                Err(error) => {
                    let _ = process::settle_unforwarded(request).await;
                    return Err(error);
                }
            };
            let log = match Log::start(file, maximum) {
                Ok(log) => log,
                Err(error) => {
                    let _ = process::settle_unforwarded(request).await;
                    return Err(error);
                }
            };
            request.combined_log = Some(Arc::new(log));
            Some(record)
        } else {
            None
        };
        match plan::available(self.context, &request) {
            Ok(left) => request.timeout = request.timeout.min(left),
            Err(error) => {
                let _ = process::settle_unforwarded(request).await;
                return Err(error);
            }
        }
        let result = self.inner.run(request).await;
        let read = record
            .as_ref()
            .map(|record| self.files.read(record))
            .transpose();
        result?;
        let bytes = read?.unwrap_or_default();
        self.files.check()?;
        Ok(bytes)
    }
    async fn ready(&mut self) -> Result<()> {
        self.inner.ready().await
    }
    fn cleanup_passed(&self) -> bool {
        self.inner.cleanup_passed()
    }
    async fn replay(
        &mut self,
        bytes: &[u8],
        stderr: bool,
        deadline: std::time::Instant,
    ) -> Result<()> {
        self.inner.replay(bytes, stderr, deadline).await
    }
}
