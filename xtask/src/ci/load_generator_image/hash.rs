//! Awaited filesystem-only workers keep the one async signal owner responsive.

use super::{
    files::{self, Owned, Record},
    plan::{self, Context},
};
use crate::{ci::process::Executor, tooling::Result};
use sha2::{Digest, Sha256};
use simplestchat_ops::release::archive::{
    ArchiveError, GeneratorIdentity, GeneratorSelection, verify_generator_archive_with_checkpoint,
};
use std::{
    fs::{File, Metadata},
    io::Read,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub(super) async fn work<T: Send + 'static, E: Executor>(
    context: &Context,
    runner: &mut E,
    operation: impl FnOnce(Arc<AtomicBool>, Instant) -> Result<T> + Send + 'static,
) -> Result<T> {
    runner.ready().await?;
    let slot = Duration::from_secs(120)
        .checked_sub(context.hash_used.get())
        .filter(|v| !v.is_zero())
        .ok_or_else(plan::expired)?;
    let remaining = plan::ENTRY
        .checked_sub(context.time.elapsed())
        .and_then(|v| v.checked_sub(Duration::from_secs(30)))
        .ok_or_else(plan::expired)?;
    let slot = slot.min(remaining);
    if slot.is_zero() {
        return Err(plan::expired());
    }
    let start = context.time.elapsed();
    let deadline = Instant::now() + slot;
    let abort = Arc::new(AtomicBool::new(false));
    let flag = abort.clone();
    let mut worker = tokio::task::spawn_blocking(move || operation(flag, deadline));
    let mut primary: Result<()> = Ok(());
    let result = loop {
        tokio::select! {
            result=&mut worker=>break result.map_err(|_|plan::failure()),
            _=tokio::time::sleep(Duration::from_millis(10))=>{
                let pending=runner.ready().await;
                let timed=if Instant::now()>=deadline||context.time.elapsed()>=plan::ENTRY{
                    Err(plan::expired())}else{Ok(())};
                primary=primary.and(pending).and(timed);
                if primary.is_err(){abort.store(true,Ordering::SeqCst);}
            }
        }
    };
    context
        .hash_used
        .set(context.hash_used.get() + context.time.elapsed().saturating_sub(start));
    let pending = runner.ready().await;
    let timely = if Instant::now() >= deadline || context.time.elapsed() >= plan::ENTRY {
        Err(plan::expired())
    } else {
        Ok(())
    };
    conclude(primary, result.and_then(|value| value), pending, timely)
}
pub(super) fn conclude<T>(
    primary: Result<()>,
    result: Result<T>,
    pending: Result<()>,
    timely: Result<()>,
) -> Result<T> {
    primary?;
    let value = result?;
    pending.and(timely)?;
    Ok(value)
}
fn allowed(abort: &AtomicBool, deadline: Instant) -> bool {
    !abort.load(Ordering::SeqCst) && Instant::now() < deadline
}
pub(super) async fn archive<E: Executor>(
    context: &Context,
    runner: &mut E,
    files: &Owned,
    record: &Record,
    selected: &str,
) -> Result<GeneratorIdentity> {
    let (file, stamp) = files.input(record)?;
    files::close(file)?;
    let selection =
        GeneratorSelection::new(selected, &context.revision).map_err(|_| plan::failure())?;
    let path = files.archive_path(record);
    let result = work(context, runner, move |abort, deadline| {
        verify_generator_archive_with_checkpoint(&path, &selection, || allowed(&abort, deadline))
            .map_err(|error| {
                if matches!(error, ArchiveError::Budget) {
                    plan::expired()
                } else {
                    plan::failure()
                }
            })
    })
    .await;
    let checked = files.check_input(record, &stamp);
    let identity = result?;
    checked?;
    Ok(identity)
}
fn stream(
    mut file: File,
    stamp: Metadata,
    maximum: u64,
    abort: Arc<AtomicBool>,
    deadline: Instant,
) -> Result<(String, u64, Metadata)> {
    let result = (|| {
        let mut hash = Sha256::new();
        let mut count = 0u64;
        let mut buffer = [0; 65536];
        while count < stamp.len() {
            if !allowed(&abort, deadline) {
                return Err(plan::expired());
            }
            let len = usize::try_from((stamp.len() - count).min(buffer.len() as u64))
                .map_err(|_| plan::failure())?;
            let size = file.read(&mut buffer[..len]).map_err(|_| plan::failure())?;
            if size == 0 {
                return Err(plan::failure());
            }
            count += size as u64;
            if count > maximum {
                return Err(plan::failure());
            }
            hash.update(&buffer[..size]);
        }
        let after = file.metadata().map_err(|_| plan::failure())?;
        if !files::same(&stamp, &after) {
            return Err(plan::failure());
        }
        Ok((
            hash.finalize().iter().map(|b| format!("{b:02x}")).collect(),
            count,
            stamp,
        ))
    })();
    let closed = files::close(file);
    let value = result?;
    closed?;
    Ok(value)
}
pub(super) async fn measure<E: Executor>(
    context: &Context,
    runner: &mut E,
    files: &Owned,
    record: &Record,
) -> Result<(String, u64, Metadata)> {
    let (file, stamp) = files.input(record)?;
    // Admission precedes ownership transfer: a rejected worker still closes the
    // supplied descriptor through the operation guard below.
    struct Input(Option<File>);
    impl Drop for Input {
        fn drop(&mut self) {
            if let Some(file) = self.0.take() {
                let _ = files::close(file);
            }
        }
    }
    let mut input = Input(Some(file));
    let maximum = record.maximum;
    let result = work(context, runner, move |abort, deadline| {
        stream(input.0.take().unwrap(), stamp, maximum, abort, deadline)
    })
    .await;
    if let Ok((_, _, stamp)) = &result {
        files.check_input(record, stamp)?;
    }
    result
}
