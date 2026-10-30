//! Explicit local transport under the unchanged fifth acquisition allowance.
//! The supplied ZIP is data: fresh API metadata and the retained verifier remain
//! authoritative. Every opened file/ancestor is checked closed before parsing.

use super::super::namespace::{self, Budget, Chain, FLAGS, Held};
use super::{failure, files::Output, hash::Checkpoint, io, keep, plan::Context};
use crate::{ci::process::Executor, tooling::Result};
use std::{
    fs::Metadata,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

pub(super) struct Source {
    parent: Option<Chain>,
    file: Option<Held>,
    stamp: Option<Metadata>,
    name: std::ffi::OsString,
    #[cfg(test)]
    refuse_close: bool,
}
impl Source {
    pub(super) fn open(path: &Path, budget: &Budget, checkpoint: &Checkpoint) -> Result<Self> {
        checkpoint.check()?;
        if !super::request::path(path) {
            return Err(failure(1));
        }
        let name = path.file_name().ok_or_else(|| failure(1))?.into();
        let parent = Chain::open(path.parent().ok_or_else(|| failure(1))?, budget)?;
        let mut source = Self {
            parent: Some(parent),
            file: None,
            stamp: None,
            name,
            #[cfg(test)]
            refuse_close: false,
        };
        let admitted = (|| {
            let parent = source.parent.as_ref().ok_or_else(|| failure(1))?;
            let parent_stamp = io(parent.file().metadata())?;
            use std::os::unix::fs::MetadataExt;
            if parent_stamp.mode() & 0o7777 != 0o700
                || parent_stamp.uid() != rustix::process::getuid().as_raw()
            {
                return Err(failure(1));
            }
            source.file = Some(Held::open(parent.file(), &source.name, FLAGS, budget)?);
            let stamp =
                namespace::private_file(source.file.as_ref().ok_or_else(|| failure(1))?, false)?;
            if stamp.len() == 0 || stamp.len() > super::ZIP {
                return Err(failure(1));
            }
            source.stamp = Some(stamp);
            source.check(checkpoint)
        })();
        if let Err(error) = admitted {
            let _ = source.finish();
            return Err(error);
        }
        Ok(source)
    }
    fn check(&self, checkpoint: &Checkpoint) -> Result<()> {
        checkpoint.check()?;
        let parent = self.parent.as_ref().ok_or_else(|| failure(1))?;
        parent.verify()?;
        let stamp = self.stamp.as_ref().ok_or_else(|| failure(1))?;
        let file = self.file.as_ref().ok_or_else(|| failure(1))?;
        if !namespace::same(stamp, &namespace::private_file(file, false)?) {
            return Err(failure(1));
        }
        let visible = Held::open(parent.file(), &self.name, FLAGS, parent.budget())?;
        let checked = namespace::private_file(&visible, false).and_then(|now| {
            if namespace::same(stamp, &now) {
                Ok(())
            } else {
                Err(failure(1))
            }
        });
        let closed = visible.finish();
        checked.and(closed)?;
        checkpoint.check()
    }
    pub(super) fn finish(mut self) -> Result<()> {
        let closed = self.file.take().map(Held::finish).transpose().map(|_| ());
        let parent = self
            .parent
            .take()
            .map(Chain::finish)
            .transpose()
            .map(|_| ());
        let result = closed.and(parent);
        #[cfg(test)]
        if self.refuse_close {
            return result.and(Err(failure(1)));
        }
        result
    }
}

pub(super) fn copy(
    source: Source,
    output: &mut Output,
    expected_sha: &str,
    expected_bytes: u64,
    checkpoint: &Checkpoint,
) -> Result<()> {
    let copied = (|| {
        source.check(checkpoint)?;
        output.copy_zip(
            source.file.as_ref().ok_or_else(|| failure(1))?.file(),
            expected_sha,
            expected_bytes,
            checkpoint,
        )?;
        source.check(checkpoint)
    })();
    let closed = source.finish();
    copied.and(closed)?;
    checkpoint.check()
}

/// Local transfer is always joined, including cancellation and expiry. Its
/// slot does not spend the later archive/verification reserves or restart time.
pub(super) async fn work<T: Send + 'static, E: Executor>(
    context: &Context,
    executor: &mut E,
    operation: impl FnOnce(Checkpoint) -> Result<T> + Send + 'static,
) -> Result<T> {
    executor.ready().await?;
    let (start, end) = context.retained_transfer_window()?;
    let abort = Arc::new(AtomicBool::new(false));
    let checkpoint = Checkpoint::from_owner(context.clock.clone(), start, end, abort.clone());
    let mut worker = tokio::task::spawn_blocking(move || operation(checkpoint));
    let mut primary = None;
    let joined = loop {
        tokio::select! { biased;
            result = &mut worker => break result.map_err(|_| failure(1)),
            _ = tokio::time::sleep(Duration::from_millis(10)) => {
                if let Err(error) = executor.ready().await { keep(&mut primary, error); }
                match context.now() {
                    Ok(now) if now >= end => keep(&mut primary, failure(124)),
                    Err(error) => keep(&mut primary, error), _ => {}
                }
                if primary.is_some() { abort.store(true, Ordering::SeqCst); }
            }
        }
    };
    let advanced = context.complete_retained_transfer();
    if let Some(error) = primary {
        return Err(error);
    }
    let value = joined??;
    advanced?;
    executor.ready().await?;
    context.admit()?;
    Ok(value)
}

#[cfg(test)]
#[path = "retained_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "retained_deadline_tests.rs"]
mod deadline_tests;
