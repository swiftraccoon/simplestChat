//! One maintenance handoff between two actual acquisitions of the original
//! workload-lock inode. Source and attempt owners survive the unlocked interval.

use super::{Access, Cause, Owner, Result, State, Window, control, json_value, text};
use crate::release::{attestation::files::Retained, workload};
use std::{fs::File, os::unix::fs::MetadataExt};

fn same_lock(before: &std::fs::Metadata, now: &std::fs::Metadata) -> bool {
    before.is_file()
        && now.is_file()
        && before.dev() == now.dev()
        && before.ino() == now.ino()
        && before.mode() == now.mode()
        && before.uid() == now.uid()
        && before.gid() == now.gid()
        && before.nlink() == now.nlink()
        && before.len() == now.len()
        && (
            before.mtime(),
            before.mtime_nsec(),
            before.ctime(),
            before.ctime_nsec(),
        ) == (now.mtime(), now.mtime_nsec(), now.ctime(), now.ctime_nsec())
}

impl State {
    pub(super) fn acquire_workload(&mut self, owner: &mut Owner) -> Result<()> {
        let file = control::io(
            owner,
            self.lock.as_ref().ok_or(Cause::Changed)?.file().try_clone(),
        )?;
        self.lease = Some(workload::Lease::acquire(file).map_err(|failed| {
            let cause = match failed.first {
                workload::Failure::Busy => {
                    self.failure = Some("BlockingIOError".into());
                    Cause::Io
                }
                workload::Failure::Io => Cause::Io,
                workload::Failure::Changed => Cause::Changed,
                workload::Failure::Close => Cause::Close,
            };
            owner.record(cause);
            if failed.close_failed {
                owner.record(Cause::Close);
            }
            cause
        })?);
        Ok(())
    }

    fn maintenance_journal(&self, uid: u32, owner: &mut Owner) -> Result<()> {
        let root = self.root.as_ref().ok_or(Cause::Changed)?;
        let attempt = self.attempt.as_ref().ok_or(Cause::Changed)?;
        self.records
            .check_owned(root, "release-state.json", owner)?;
        let journal = Retained::open(root.clone(), "release-state.json", owner)?;
        let result = (|| {
            journal.protected(uid, owner)?;
            let record = json_value(&journal.bytes(16384, owner)?)?;
            let schema = record.get("schemaVersion");
            if !matches!(schema, Some(crate::json::Json::Integer(v)) if v == "1")
                || record.get("finalized").and_then(crate::json::Json::boolean) != Some(true)
                || text(&record, "phase") != Some("maintenance_selected")
                || text(&record, "attempt") != attempt.path().to_str()
            {
                return control::failed(owner, Cause::Changed);
            }
            owner.check(Window::Body)
        })();
        if let Err(first) = result {
            owner.record(first);
        }
        journal.finish(owner);
        result?;
        owner.check(Window::Body)
    }

    /// Retire only the optional first-hold benchmark record while its namespace
    /// is still valid and the real first flock remains held. A later finalized
    /// benchmark record may then be admitted after a fresh actual acquisition.
    pub fn prepare_maintenance_unlock(&mut self, owner: &mut Owner) -> Result<()> {
        owner.check(Window::Body)?;
        if !self.host
            || self.lock.is_none()
            || self.lease.is_none()
            || self.maintenance_handoff_prepared
            || self.maintenance_reacquired
        {
            return control::failed(owner, Cause::Policy);
        }
        self.originals(owner)?;
        self.records.check_all(owner)?;
        let uid = control::io(
            owner,
            self.root.as_ref().ok_or(Cause::Changed)?.file().metadata(),
        )?
        .uid();
        self.maintenance_journal(uid, owner)?;
        if let Some(guard) = self.benchmark_guard.take() {
            guard.finish(owner);
        }
        owner.check(Window::Body)?;
        self.maintenance_handoff_prepared = true;
        Ok(())
    }

    /// Reopen the retained lock pathname and take a fresh nonblocking flock.
    /// The original attempt, selected journal and all source owners are retained.
    pub fn reacquire_maintenance(&mut self, access: &Access, owner: &mut Owner) -> Result<()> {
        let result = self.reacquire(access, owner);
        if let Err(first) = result {
            owner.record(first);
        }
        result
    }

    fn reacquire(&mut self, access: &Access, owner: &mut Owner) -> Result<()> {
        owner.check(Window::Body)?;
        if !self.host
            || self.lock.is_some()
            || self.lease.is_some()
            || !self.maintenance_handoff_prepared
            || self.maintenance_reacquired
        {
            return control::failed(owner, Cause::Policy);
        }
        let work = self.work.as_ref().ok_or(Cause::Changed)?.clone();
        let root = self.root.as_ref().ok_or(Cause::Changed)?;
        if work.path() != access.work || root.path() != access.root {
            return control::failed(owner, Cause::Changed);
        }
        root.protected(access.uid, owner)?;
        work.protected(access.uid, owner)?;
        // openat creates a new open-file description. try_clone only gives the
        // newly acquired Lease its own explicitly closed descriptor afterward.
        self.lock = Some(Retained::open(work.clone(), "workload.lock", owner)?);
        let lock = self.lock.as_ref().ok_or(Cause::Changed)?;
        lock.protected(access.uid, owner)?;
        if !same_lock(
            self.lock_stamp.as_ref().ok_or(Cause::Changed)?,
            &control::io(owner, lock.file().metadata())?,
        ) {
            return control::failed(owner, Cause::Changed);
        }
        self.acquire_workload(owner)?;
        self.maintenance_reacquired = true;
        if work.exists("current.json".as_ref(), owner)? {
            self.benchmark_guard = Some(Retained::open(work, "current.json", owner)?);
            let current = self.benchmark_guard.as_ref().ok_or(Cause::Changed)?;
            current.protected(access.uid, owner)?;
            let record = json_value(&current.bytes(16384, owner)?)?;
            let schema = record.get("schemaVersion");
            if !matches!(schema, Some(crate::json::Json::Integer(v)) if v == "1")
                || record.get("finalized").and_then(crate::json::Json::boolean) != Some(true)
            {
                return self.policy(
                    owner,
                    "Resolve unfinished benchmark cleanup before releasing",
                );
            }
        }
        self.maintenance_journal(access.uid, owner)?;
        self.records.check_all(owner)?;
        self.originals(owner)?;
        owner.check(Window::Body)
    }

    /// Compare the candidate's held inode with every input and atomic stage
    /// record, including the lock. Equal bytes never make an alias safe to edit.
    pub fn input_aliases(&self, input: &File, owner: &mut Owner) -> Result<bool> {
        owner.check(Window::Body)?;
        let selected = control::io(owner, input.metadata())?;
        for original in self
            .originals
            .iter()
            .chain(self.benchmark_guard.iter())
            .chain(self.manifest.iter())
            .chain(self.archive.iter())
            .chain(self.tool.iter())
            .chain(self.lock.iter())
        {
            owner.check(Window::Body)?;
            let stamp = control::io(owner, original.file().metadata())?;
            if selected.dev() == stamp.dev() && selected.ino() == stamp.ino() {
                return Ok(true);
            }
        }
        self.records.input_aliases(input, owner)
    }
}

#[cfg(test)]
mod tests;
