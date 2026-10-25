//! One retained release backup, one joined disposable identity and one original clock.
use super::super::{
    control::{self, Cause, Owner, Result, Window},
    deploy::records::{Record, Records},
    files::Dir,
    stage::{python_byte_strip, python_strip},
};
use super::{Access, CommandOutput, Executor, Invocation, Options, Outcome, Role, policy};
use crate::{
    json::Json,
    release::{manifest::Manifest, workload::Lease},
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::Metadata,
    io::{Seek, SeekFrom},
    os::unix::fs::{FileExt, FileTypeExt, MetadataExt},
    path::Path,
    time::{Duration, Instant},
};

const MAX_BACKUP: u64 = 64 * 1024 * 1024;
const CLEANUP: Duration = Duration::from_secs(160);
const SETTLE: Duration = Duration::from_secs(10);
const MAX_COMMANDS: usize = 73;
const INSPECTION: u64 = 2 * 1024 * 1024;
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Report {
    container: String,
    container_id: String,
    postgres_image: String,
    cleanup_passed: bool,
    verified: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    counts: Option<Json>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FinalReport<'a> {
    #[serde(flatten)]
    report: &'a Report,
    backup_sha256: &'a str,
    release: &'a str,
    finished_at: &'a str,
}
fn encoded(value: &impl Serialize) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|_| Cause::Io)?;
    bytes.push(b'\n');
    Ok(bytes)
}
fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}
fn same(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.mode() == b.mode()
        && a.uid() == b.uid()
        && a.gid() == b.gid()
        && a.nlink() == b.nlink()
        && a.len() == b.len()
        && (a.mtime(), a.mtime_nsec(), a.ctime(), a.ctime_nsec())
            == (b.mtime(), b.mtime_nsec(), b.ctime(), b.ctime_nsec())
}
fn cause_name(cause: Cause) -> &'static str {
    match cause {
        Cause::Deadline => "native_deadline_refused",
        Cause::Interrupt | Cause::Terminate => "native_signal_refused",
        Cause::Close => "native_close_refused",
        Cause::Changed => "native_input_changed",
        Cause::Command => "native_command_refused",
        Cause::Bounds => "native_output_limit",
        Cause::Io => "OSError",
        _ => "native_restore_refused",
    }
}
/// The sole lease unlocks after stdout and all payload owners.
/// Lock aliases and readonly directory capabilities then settle before the final fence.
pub(super) struct Guard {
    lease: Option<Lease>,
    lock: Option<Record>,
    work: Option<Dir>,
    directories: Vec<Dir>,
}
impl Guard {
    pub fn finish(mut self, owner: &mut Owner) {
        if let Some(work) = &self.work {
            let _ = work.final_namespace(owner);
        }
        if let Some(lock) = &self.lock {
            let _ = lock.verify(owner, true);
        }
        if let Some(lease) = self.lease.take() {
            if lease.check().is_err() {
                owner.record(Cause::Changed);
            }
            let (released, file) = lease.release();
            if released.is_err() {
                owner.record(Cause::Close);
            }
            if let Some(file) = file
                && nix::unistd::close(file).is_err()
            {
                owner.record(Cause::Close);
            }
        }
        if let Some(lock) = self.lock.take() {
            lock.finish(owner);
        }
        self.work.take();
        for dir in self.directories.into_iter().rev() {
            dir.finish(owner);
        }
    }
}
#[derive(Clone, Copy)]
enum Input {
    Snapshot,
    Roles,
    Sql,
}
struct Finalized {
    guard: Guard,
    #[cfg(test)]
    fault_hits: usize,
    #[cfg(test)]
    snapshot_unlinks: usize,
}
struct State {
    directories: Vec<Dir>,
    records: Records,
    guard: Guard,
    attempt: Option<Dir>,
    state: Option<Dir>,
    tool: Option<usize>,
    socket: Option<Metadata>,
    snapshot: Option<Record>,
    snapshot_bytes: u64,
    snapshot_hash: Sha256,
    roles: Option<Record>,
    source: Option<usize>,
    sql: Option<usize>,
    failure: Option<String>,
    report: Report,
    revision: String,
    backup_sha256: String,
    migrations: BTreeMap<String, String>,
    commands: usize,
    candidate: bool,
    cleanup_phase: bool,
    cleanup_owed: bool,
    ready_end: Option<Instant>,
    #[cfg(test)]
    fault: Option<super::Fault>,
    #[cfg(test)]
    fault_hits: usize,
    #[cfg(test)]
    snapshot_unlinks: usize,
}
impl State {
    fn new() -> Self {
        Self {
            directories: Vec::new(),
            records: Records::default(),
            guard: Guard {
                lease: None,
                lock: None,
                work: None,
                directories: Vec::new(),
            },
            attempt: None,
            state: None,
            tool: None,
            socket: None,
            snapshot: None,
            snapshot_bytes: 0,
            snapshot_hash: Sha256::new(),
            roles: None,
            source: None,
            sql: None,
            failure: None,
            report: Report {
                container: String::new(),
                container_id: String::new(),
                postgres_image: String::new(),
                cleanup_passed: false,
                verified: false,
                counts: None,
            },
            revision: String::new(),
            backup_sha256: String::new(),
            migrations: BTreeMap::new(),
            commands: 0,
            candidate: false,
            cleanup_phase: false,
            cleanup_owed: false,
            ready_end: None,
            #[cfg(test)]
            fault: None,
            #[cfg(test)]
            fault_hits: 0,
            #[cfg(test)]
            snapshot_unlinks: 0,
        }
    }
    fn policy<T>(&mut self, owner: &mut Owner, result: policy::Result<T>) -> Result<T> {
        result.map_err(|message| {
            if self.failure.is_none() {
                self.failure = Some(message.into());
            }
            owner.record(Cause::Policy);
            Cause::Policy
        })
    }
    fn directory(&mut self, path: &Path, mode: u32, uid: u32, owner: &mut Owner) -> Result<Dir> {
        let dir = Dir::open(path, owner)?;
        self.directories.push(dir.clone());
        let stamp = control::io(owner, dir.file().metadata())?;
        if stamp.uid() != uid || stamp.mode() & 0o7777 != mode {
            return self.policy(owner, Err("unsafe_filesystem"));
        }
        owner.check(Window::Body)?;
        Ok(dir)
    }
    fn child(&mut self, parent: &Dir, name: &str, owner: &mut Owner) -> Result<Dir> {
        let dir = parent.child(name.as_ref(), owner)?;
        self.directories.push(dir.clone());
        let stamp = control::io(owner, dir.file().metadata())?;
        if stamp.uid() != rustix::process::geteuid().as_raw() || stamp.mode() & 0o7777 != 0o700 {
            return self.policy(owner, Err("unsafe_filesystem"));
        }
        Ok(dir)
    }
    fn input(
        &mut self,
        parent: &Dir,
        name: &str,
        uid: u32,
        maximum: u64,
        owner: &mut Owner,
    ) -> Result<usize> {
        let record = Record::restore_input(parent.clone(), name, uid, maximum, owner)?;
        self.records.files.push(record);
        Ok(self.records.files.len() - 1)
    }
    fn json(&mut self, at: usize, maximum: usize, owner: &mut Owner) -> Result<Json> {
        let bytes = self.records.files[at].bytes(maximum, owner, false)?;
        self.policy(owner, policy::decode(&bytes))
    }
    fn setup(&mut self, options: &Options, access: &Access, owner: &mut Owner) -> Result<()> {
        let root = self.directory(&access.root, 0o700, access.uid, owner)?;
        let _ = self.directory(&access.config, 0o700, access.uid, owner)?;
        let work = if access.work.exists() {
            self.directory(&access.work, 0o700, access.uid, owner)?
        } else {
            let parent = Dir::open(access.work.parent().ok_or(Cause::Input)?, owner)?;
            self.directories.push(parent.clone());
            self.child(
                &parent,
                access
                    .work
                    .file_name()
                    .and_then(|s| s.to_str())
                    .ok_or(Cause::Input)?,
                owner,
            )?
        };
        self.guard.work = Some(work.clone());
        self.guard.lock = Some(if work.exists("workload.lock".as_ref(), owner)? {
            Record::restore_input(work.clone(), "workload.lock", access.uid, u64::MAX, owner)?
        } else {
            Record::create(work.clone(), "workload.lock", owner, false)?
        });
        let file = control::io(owner, self.guard.lock.as_ref().unwrap().file().try_clone())?;
        match Lease::acquire(file) {
            Ok(lease) => self.guard.lease = Some(lease),
            Err(failed) => {
                let cause = match failed.first {
                    crate::release::workload::Failure::Busy => Cause::Policy,
                    crate::release::workload::Failure::Changed => Cause::Changed,
                    crate::release::workload::Failure::Close => Cause::Close,
                    crate::release::workload::Failure::Io => Cause::Io,
                };
                owner.record(cause);
                if failed.close_failed {
                    owner.record(Cause::Close);
                }
                return Err(cause);
            }
        }
        for (directory, name) in [(&work, "current.json"), (&root, "release-state.json")] {
            if directory.exists(name.as_ref(), owner)? {
                let at = self.input(directory, name, access.uid, 16384, owner)?;
                let value = self.json(at, 16384, owner)?;
                if !matches!(value.get("schemaVersion"),Some(Json::Integer(s)) if s=="1")
                    || value.get("finalized").and_then(Json::boolean) != Some(true)
                {
                    return self.policy(owner, Err("unfinished_operation"));
                }
            }
        }
        let state = self.directory(&access.state, 0o750, access.uid, owner)?;
        self.state = Some(state.clone());
        let results = self.directory(&access.root.join("results"), 0o700, access.uid, owner)?;
        let releases = self.directory(&access.root.join("releases"), 0o700, access.uid, owner)?;
        let source = self.directory(
            &results.path().join(&options.attempt),
            0o700,
            access.uid,
            owner,
        )?;
        let outcome_at = self.input(&source, "outcome.json", access.uid, 65536, owner)?;
        let outcome = self.json(outcome_at, 65536, owner)?;
        self.revision = self
            .policy(owner, policy::text(&outcome, "revision"))?
            .into();
        self.backup_sha256 = self
            .policy(owner, policy::text(&outcome, "backupSha256"))?
            .into();
        if !policy::hex(&self.revision, 40) || !policy::hex(&self.backup_sha256, 64) {
            return self.policy(owner, Err("Invalid backup identity"));
        }
        let manifest_dir = self.directory(
            &releases.path().join(&self.revision),
            0o700,
            access.uid,
            owner,
        )?;
        let manifest_at = self.input(
            &manifest_dir,
            "release.json",
            access.uid,
            1024 * 1024,
            owner,
        )?;
        let bytes = self.records.files[manifest_at].bytes(1024 * 1024, owner, false)?;
        let manifest = self.policy(
            owner,
            Manifest::decode(&bytes).map_err(|_| "invalid_release_manifest"),
        )?;
        if manifest.fields().revision != self.revision {
            return self.policy(owner, Err("Backup release identity differs"));
        }
        self.migrations = self.policy(
            owner,
            policy::migrations(&outcome, &manifest.fields().migrations),
        )?;
        let attempts = self.child(&state, "restores", owner)?;
        let mut random = [0; 16];
        owner.check(Window::Body)?;
        #[cfg(test)]
        if let Some(bytes) = access.nonce {
            random = bytes;
        }
        #[cfg(not(test))]
        getrandom::fill(&mut random).map_err(|_| {
            owner.record(Cause::Io);
            Cause::Io
        })?;
        #[cfg(test)]
        if access.nonce.is_none() {
            getrandom::fill(&mut random).map_err(|_| {
                owner.record(Cause::Io);
                Cause::Io
            })?;
        }
        owner.check(Window::Body)?;
        let attempt = attempts.create_child(
            format!("restore.{}", super::super::files::hex(&random)).as_ref(),
            owner,
        )?;
        self.directories.push(attempt.clone());
        self.attempt = Some(attempt.clone());
        self.source = Some(self.input(
            &source,
            "database-before.dump",
            access.uid,
            MAX_BACKUP,
            owner,
        )?);
        let snapshot = Record::create(attempt.clone(), "snapshot.dump", owner, false)?;
        self.snapshot = Some(snapshot);
        self.copy_backup(owner)?;
        let tool_dir = Dir::open(access.docker.parent().ok_or(Cause::Input)?, owner)?;
        self.directories.push(tool_dir.clone());
        let tool = Record::tool(
            tool_dir,
            access
                .docker
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or(Cause::Input)?,
            access.uid,
            owner,
        )?;
        self.records.files.push(tool);
        self.tool = Some(self.records.files.len() - 1);
        if let Some(path) = &access.socket {
            owner.check(Window::Body)?;
            let stamp = control::io(owner, path.symlink_metadata())?;
            if !stamp.file_type().is_socket() {
                return control::failed(owner, Cause::Policy);
            }
            self.socket = Some(stamp);
        }
        self.roles = Some(self.anonymous_input(
            ".restore-roles.sql",
            policy::ROLE_SQL,
            access.uid,
            owner,
        )?);
        random[6] = (random[6] & 0x0f) | 0x40;
        random[8] = (random[8] & 0x3f) | 0x80;
        self.report.container = format!("scpub-restore-{}", super::super::files::hex(&random));
        Ok(())
    }
    fn copy_backup(&mut self, owner: &mut Owner) -> Result<()> {
        let source = &self.records.files[self.source.unwrap()];
        if source.len() == 0 {
            return self.policy(owner, Err("Unsafe or oversized release backup"));
        }
        let mut buffer = vec![0; 1024 * 1024];
        let mut at = 0_u64;
        let mut hash = Sha256::new();
        loop {
            owner.check(Window::Body)?;
            let wanted = (MAX_BACKUP + 1 - at).min(buffer.len() as u64) as usize;
            #[cfg(test)]
            if self.fault == Some(super::Fault::SnapshotRead) && at == 0 {
                self.fault_hits += 1;
                return control::failed(owner, Cause::Io);
            }
            let count = match source.file().read_at(&mut buffer[..wanted], at) {
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                value => control::io(owner, value)?,
            };
            owner.check(Window::Body)?;
            if count == 0 {
                break;
            }
            let mut offset = 0;
            while offset < count {
                owner.check(Window::Body)?;
                #[cfg(test)]
                if self.fault == Some(super::Fault::SnapshotWrite) && self.snapshot_bytes == 0 {
                    self.fault_hits += 1;
                    return control::failed(owner, Cause::Io);
                }
                let written = match self
                    .snapshot
                    .as_ref()
                    .unwrap()
                    .file()
                    .write_at(&buffer[offset..count], at + offset as u64)
                {
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    value => control::io(owner, value)?,
                };
                if written == 0 {
                    return control::failed(owner, Cause::Io);
                }
                self.snapshot_hash.update(&buffer[offset..offset + written]);
                self.snapshot_bytes += written as u64;
                offset += written;
                owner.check(Window::Body)?;
            }
            hash.update(&buffer[..count]);
            at += count as u64;
            if at > MAX_BACKUP {
                return control::failed(owner, Cause::Bounds);
            }
        }
        #[cfg(test)]
        if matches!(
            self.fault,
            Some(super::Fault::SnapshotFlush | super::Fault::SnapshotSync)
        ) {
            self.fault_hits += 1;
            return control::failed(owner, Cause::Io);
        }
        self.snapshot.as_mut().unwrap().seal(owner, false)?;
        if at != source.len() || super::super::files::hex(&hash.finalize()) != self.backup_sha256 {
            return self.policy(owner, Err("Release backup digest differs"));
        }
        source.verify(owner, false)?;
        let writer = self.snapshot.take().unwrap();
        let reader = Record::restore_input(
            self.attempt.as_ref().unwrap().clone(),
            "snapshot.dump",
            rustix::process::geteuid().as_raw(),
            MAX_BACKUP,
            owner,
        );
        let reader = match reader {
            Ok(reader) => reader,
            Err(cause) => {
                self.snapshot = Some(writer);
                return Err(cause);
            }
        };
        let matched = writer.aliases(&reader);
        self.snapshot = Some(reader);
        if !matched {
            owner.record(Cause::Changed);
        }
        writer.finish(owner);
        owner.check(Window::Body)
    }
    fn anonymous_input(
        &self,
        name: &str,
        bytes: &[u8],
        uid: u32,
        owner: &mut Owner,
    ) -> Result<Record> {
        let parent = self.attempt.as_ref().unwrap().clone();
        let mut writer = Record::create(parent.clone(), name, owner, false)?;
        let opened = (|| {
            writer.write(bytes, owner, false)?;
            let reader = Record::restore_input(parent, name, uid, bytes.len() as u64, owner)?;
            if !writer.aliases(&reader) {
                owner.record(Cause::Changed);
                reader.finish(owner);
                return Err(Cause::Changed);
            }
            Ok(reader)
        })();
        writer.finish(owner);
        let mut reader = opened?;
        let accepted = owner
            .check(Window::Body)
            .and_then(|()| reader.unname(owner));
        if let Err(cause) = accepted {
            reader.finish(owner);
            return Err(cause);
        }
        Ok(reader)
    }
    fn witness(&self, access: &Access, owner: &mut Owner, cleanup: bool) -> Result<()> {
        if cleanup {
            owner.final_work()?;
        } else {
            owner.check(Window::Body)?;
        }
        if let Some(lease) = &self.guard.lease {
            if lease.check().is_err() {
                return control::failed(owner, Cause::Changed);
            }
        } else {
            return control::failed(owner, Cause::Changed);
        }
        if let Some(lock) = &self.guard.lock {
            lock.verify(owner, cleanup)?;
        }
        if cleanup {
            self.records.files[self.tool.ok_or(Cause::Changed)?].verify(owner, true)?;
        } else {
            self.records.verify(owner, false)?;
            if let Some(snapshot) = &self.snapshot {
                snapshot.verify(owner, false)?;
            }
            if let Some(roles) = &self.roles {
                roles.verify(owner, false)?;
            }
        }
        if let (Some(path), Some(first)) = (&access.socket, &self.socket) {
            let now = control::io(owner, path.symlink_metadata())?;
            if !now.file_type().is_socket() || !same(first, &now) {
                return control::failed(owner, Cause::Changed);
            }
        }
        Ok(())
    }
    fn input_record(&self, input: Input) -> &Record {
        match input {
            Input::Snapshot => self.snapshot.as_ref().unwrap(),
            Input::Roles => self.roles.as_ref().unwrap(),
            Input::Sql => &self.records.files[self.sql.unwrap()],
        }
    }
    fn enter_cleanup(&mut self, owner: &mut Owner) -> Result<()> {
        if self.candidate {
            owner.end_restore_candidate();
            self.candidate = false;
        }
        if !self.cleanup_phase {
            owner.begin_restore_cleanup()?;
            self.cleanup_phase = true;
        }
        Ok(())
    }
    fn record_report(
        &mut self,
        owner: &mut Owner,
        recovery: bool,
        target: super::AtomicTarget,
    ) -> Result<()> {
        let data = encoded(&self.report)?;
        let parent = self.attempt.as_ref().unwrap().clone();
        self.atomic(&parent, target, &data, owner, recovery)
    }
    fn atomic(
        &mut self,
        parent: &Dir,
        target: super::AtomicTarget,
        bytes: &[u8],
        owner: &mut Owner,
        recovery: bool,
    ) -> Result<()> {
        let name = match target {
            super::AtomicTarget::InitialRestore
            | super::AtomicTarget::CreatedRestore
            | super::AtomicTarget::FinalRestore => "restore.json",
            super::AtomicTarget::Outcome => "outcome.json",
            super::AtomicTarget::Freshness => "restore-verified.timestamp",
        };
        #[cfg(test)]
        let fault = self.atomic_fault(target);
        #[cfg(not(test))]
        let fault = None;
        self.records
            .atomic_fault(parent, name, bytes, owner, recovery, fault)
    }
    #[cfg(test)]
    fn atomic_fault(&mut self, target: super::AtomicTarget) -> Option<u8> {
        match self.fault {
            Some(super::Fault::Atomic(found, point)) if found == target => {
                self.fault_hits += 1;
                Some(match point {
                    super::AtomicPoint::Admission => 0,
                    super::AtomicPoint::FileSync => 1,
                    super::AtomicPoint::DirectorySync => 2,
                })
            }
            _ => None,
        }
    }
    async fn command(
        &mut self,
        access: &Access,
        role: Role,
        arguments: Vec<OsString>,
        input: Option<Input>,
        owner: &mut Owner,
        executor: &mut impl Executor,
    ) -> Result<(i32, Vec<u8>)> {
        let cleanup = role.cleanup();
        #[cfg(test)]
        if role == Role::Restore
            && matches!(
                self.fault,
                Some(super::Fault::InterruptBeforeRestore | super::Fault::TerminateBeforeRestore)
            )
        {
            self.fault_hits += 1;
            let signal = if self.fault == Some(super::Fault::InterruptBeforeRestore) {
                signal_hook::consts::SIGINT
            } else {
                signal_hook::consts::SIGTERM
            };
            if signal_hook::low_level::raise(signal).is_err() {
                return control::failed(owner, Cause::Io);
            }
            owner.check(Window::Body)?;
        }
        self.witness(access, owner, cleanup)?;
        let now = owner.now();
        let ceiling = if cleanup {
            owner.body_end()
        } else if self.candidate {
            owner
                .body_end()
                .checked_sub(CLEANUP)
                .ok_or(Cause::Deadline)?
        } else {
            owner.body_end()
        };
        let ceiling = if role == Role::Ready {
            ceiling.min(self.ready_end.ok_or(Cause::Changed)?)
        } else {
            ceiling
        };
        let body_end = (now + Duration::from_secs(role.seconds()))
            .min(ceiling.checked_sub(SETTLE).ok_or(Cause::Deadline)?);
        if body_end <= now {
            return control::failed(owner, Cause::Deadline);
        }
        let settlement_end = (body_end + SETTLE).min(ceiling);
        self.commands += 1;
        if self.commands > MAX_COMMANDS {
            return control::failed(owner, Cause::Bounds);
        }
        let attempt = self.attempt.as_ref().unwrap().clone();
        let out = self.records.prepare_log(
            &attempt,
            &format!("{:03}.stdout", self.commands),
            owner,
            cleanup,
        )?;
        let err = self.records.prepare_log(
            &attempt,
            &format!("{:03}.stderr", self.commands),
            owner,
            cleanup,
        )?;
        if let Some(input) = input {
            let input = self.input_record(input);
            input.verify(owner, cleanup)?;
            let mut original = input.file();
            control::io(owner, original.seek(SeekFrom::Start(0)))?;
        }
        let mut argv = args(&["--host", "unix:///var/run/docker.sock"]);
        argv.extend(arguments);
        let result = executor
            .run(
                owner,
                Invocation {
                    role,
                    program: access.docker.clone(),
                    arguments: argv,
                    input: input.map(|input| self.input_record(input).file()),
                    stdout: self.records.files[out].file(),
                    stderr: self.records.files[err].file(),
                    body_end,
                    settlement_end,
                },
            )
            .await;
        let CommandOutput {
            stdout: captured,
            status,
            settled,
            stdout_complete,
            stdout_bytes,
            stderr_bytes,
            error,
        } = result;
        // Only an actual raw0, naturally fenced complete stdout can identify
        // this create. Evidence seal/close failures below never erase that fact.
        if role == Role::Create
            && status == Some(0)
            && stdout_complete
            && let Ok(text) = std::str::from_utf8(&captured)
            && policy::hex(python_strip(text), 64)
        {
            self.report.container_id = python_strip(text).into();
        }
        if let Some(cause) = error {
            owner.record(cause);
            if self.failure.is_none() {
                self.failure = Some(cause_name(cause).into());
            }
        }
        let on_time = owner.now() < settlement_end;
        if !on_time {
            owner.record(Cause::Deadline);
        }
        let settled = settled && on_time;
        if !settled {
            owner.record(Cause::Close);
        }
        if owner.error().is_some() {
            let _ = self.enter_cleanup(owner);
        }
        let recovery = cleanup || owner.error().is_some();
        let expected = [(out, stdout_bytes), (err, stderr_bytes)];
        for (at, count) in expected {
            if let Err(cause) = self.records.files[at].observe_growth(owner, recovery) {
                owner.record(cause);
            } else if self.records.files[at].len() != count {
                owner.record(Cause::Changed);
            }
        }
        if stdout_bytes != captured.len() as u64 || stdout_bytes > INSPECTION {
            owner.record(Cause::Changed);
        }
        // The higher log index is retired first; all stored input positions remain unchanged.
        let error_close = self.records.finish_log(err, &[], owner, recovery);
        let output_close = self.records.finish_log(out, &[], owner, recovery);
        if let Some(input) = input {
            let _ = self.input_record(input).verify(owner, recovery);
        }
        if let Err(cause) = error_close {
            owner.record(cause);
        }
        if let Err(cause) = output_close {
            owner.record(cause);
        }
        let status = status.ok_or(Cause::Close)?;
        if !settled || !stdout_complete || owner.error().is_some() && !cleanup {
            return Err(owner.error().unwrap_or(Cause::Close));
        }
        if cleanup {
            self.witness(access, owner, true)?;
        } else {
            self.witness(access, owner, false)?;
        }
        Ok((status, captured))
    }

    fn checked(&mut self, owner: &mut Owner, output: (i32, Vec<u8>)) -> Result<Vec<u8>> {
        if output.0 != 0 {
            return self.policy_command(owner);
        }
        Ok(output.1)
    }
    fn policy_command<T>(&mut self, owner: &mut Owner) -> Result<T> {
        if self.failure.is_none() {
            self.failure = Some(format!(
                "Command {:03} failed; inspect private output",
                self.commands
            ));
        }
        control::failed(owner, Cause::Command)
    }
    async fn run_checked(
        &mut self,
        access: &Access,
        role: Role,
        arguments: Vec<OsString>,
        input: Option<Input>,
        owner: &mut Owner,
        executor: &mut impl Executor,
    ) -> Result<Vec<u8>> {
        let output = self
            .command(access, role, arguments, input, owner, executor)
            .await?;
        self.checked(owner, output)
    }
    async fn body(
        &mut self,
        options: &Options,
        access: &Access,
        owner: &mut Owner,
        executor: &mut impl Executor,
    ) -> Result<()> {
        self.setup(options, access, owner)?;
        let image = self
            .run_checked(
                access,
                Role::Inspect,
                args(&[
                    "inspect",
                    "--format",
                    "{{.Image}}",
                    "simplestchat-public-postgres-1",
                ]),
                None,
                owner,
                executor,
            )
            .await?;
        let image = self.policy(
            owner,
            std::str::from_utf8(&image).map_err(|_| "validation_failed"),
        )?;
        let image = python_strip(image);
        if !policy::image(image) {
            return self.policy(owner, Err("Invalid production PostgreSQL image"));
        }
        self.report.postgres_image = image.into();
        self.record_report(owner, false, super::AtomicTarget::InitialRestore)?;
        let prior = self
            .run_checked(
                access,
                Role::Prior,
                args(&[
                    "ps",
                    "--all",
                    "--quiet",
                    "--filter",
                    &format!("label={}", policy::LABEL),
                ]),
                None,
                owner,
                executor,
            )
            .await?;
        if !python_byte_strip(&prior).is_empty() {
            return self.policy(
                owner,
                Err("Inspect the retained restore container before starting another"),
            );
        }
        let candidate = owner
            .body_end()
            .checked_sub(CLEANUP)
            .ok_or(Cause::Deadline)?;
        owner.begin_restore_candidate(candidate)?;
        self.candidate = true;
        self.cleanup_owed = true;
        let body = self.verify(access, owner, executor).await;
        if let Err(cause) = body {
            owner.record(cause);
            if self.failure.is_none() {
                self.failure = Some(cause_name(cause).into());
            }
        }
        if body.is_ok() {
            let _ = owner.check(Window::Body);
        }
        let entered = self.enter_cleanup(owner);
        let cleanup = if entered.is_ok() {
            self.cleanup(access, owner, executor).await
        } else {
            entered
        };
        if cleanup.is_ok() {
            self.report.cleanup_passed = true;
        }
        if let Err(cause) = cleanup {
            owner.record(cause);
        }
        // The fixed final restore record is attempted even after a known body failure.
        let recorded = self.record_report(owner, true, super::AtomicTarget::FinalRestore);
        if let Err(cause) = recorded {
            owner.record(cause);
        }
        if body.is_err() || cleanup.is_err() || recorded.is_err() || owner.error().is_some() {
            return Err(owner.error().unwrap_or(Cause::Close));
        }
        owner.end_restore_cleanup();
        self.cleanup_phase = false;
        if let Some(snapshot) = self.snapshot.take() {
            self.unlink_snapshot(snapshot, owner, false)?;
        }
        if !self.report.verified || !self.report.cleanup_passed {
            return self.policy(
                owner,
                Err("Restore verification or cleanup was not successful"),
            );
        }
        #[cfg(test)]
        let timestamp = access.timestamp.clone().unwrap_or_else(timestamp);
        #[cfg(not(test))]
        let timestamp = timestamp();
        let bytes = encoded(&FinalReport {
            report: &self.report,
            backup_sha256: &self.backup_sha256,
            release: &self.revision,
            finished_at: &timestamp,
        })?;
        self.atomic(
            &self.attempt.as_ref().unwrap().clone(),
            super::AtomicTarget::Outcome,
            &bytes,
            owner,
            false,
        )?;
        #[derive(Serialize)]
        struct Freshness<'a> {
            evidence: &'a str,
            #[serde(flatten)]
            report: FinalReport<'a>,
        }
        let path = self.attempt.as_ref().unwrap().path();
        let path = path.to_str().ok_or(Cause::Input)?;
        let bytes = encoded(&Freshness {
            evidence: path,
            report: FinalReport {
                report: &self.report,
                backup_sha256: &self.backup_sha256,
                release: &self.revision,
                finished_at: &timestamp,
            },
        })?;
        let state = self.state.as_ref().unwrap().clone();
        if state.exists("restore-verified.timestamp".as_ref(), owner)? {
            let prior = Record::open(
                state,
                "restore-verified.timestamp",
                access.uid,
                u64::MAX,
                owner,
            )?;
            self.records.files.push(prior);
        }
        self.atomic(
            &self.state.as_ref().unwrap().clone(),
            super::AtomicTarget::Freshness,
            &bytes,
            owner,
            false,
        )?;
        owner.check(Window::Body)
    }
    async fn verify(
        &mut self,
        access: &Access,
        owner: &mut Owner,
        executor: &mut impl Executor,
    ) -> Result<()> {
        let name = self.report.container.clone();
        let image = self.report.postgres_image.clone();
        let created = self
            .run_checked(
                access,
                Role::Create,
                policy::create(&name, &image),
                None,
                owner,
                executor,
            )
            .await?;
        let created = self.policy(
            owner,
            std::str::from_utf8(&created).map_err(|_| "validation_failed"),
        )?;
        let created = python_strip(created);
        if !policy::hex(created, 64) {
            return self.policy(owner, Err("Invalid restore container identity"));
        }
        self.report.container_id = created.into();
        self.record_report(owner, false, super::AtomicTarget::CreatedRestore)?;
        self.run_checked(
            access,
            Role::Start,
            args(&["start", created]),
            None,
            owner,
            executor,
        )
        .await?;
        let ready_end = owner.now() + Duration::from_secs(30);
        self.ready_end = Some(ready_end);
        loop {
            // No attempt is admitted without its existing five+five settlement
            // debt inside this original live readiness phase and candidate B-R.
            owner.check(Window::Body)?;
            if owner.now() + SETTLE >= ready_end {
                return control::failed(owner, Cause::Deadline);
            }
            let id = self.report.container_id.clone();
            let output = self
                .command(
                    access,
                    Role::Ready,
                    args(&[
                        "exec",
                        "--user",
                        "999:999",
                        &id,
                        "/bin/sh",
                        "-c",
                        policy::READY_TEST,
                    ]),
                    None,
                    owner,
                    executor,
                )
                .await?;
            owner.check(Window::Body)?;
            if owner.now() >= ready_end {
                return control::failed(owner, Cause::Deadline);
            }
            if output.0 == 0 {
                break;
            }
            let until = owner.now() + Duration::from_millis(500);
            if until >= ready_end {
                return control::failed(owner, Cause::Deadline);
            }
            #[cfg(test)]
            if access.virtual_poll {
                owner.observe_at(until);
            } else {
                tokio::time::sleep_until(until.into()).await;
            }
            #[cfg(not(test))]
            tokio::time::sleep_until(until.into()).await;
            owner.check(Window::Body)?;
        }
        self.ready_end = None;
        let id = self.report.container_id.clone();
        self.run_checked(
            access,
            Role::Sql,
            policy::sql(&id),
            Some(Input::Roles),
            owner,
            executor,
        )
        .await?;
        self.run_checked(
            access,
            Role::Restore,
            args(&[
                "exec",
                "-i",
                "--user",
                "999:999",
                &id,
                "timeout",
                "--signal=TERM",
                "--kill-after=2s",
                "120s",
                "pg_restore",
                "--exit-on-error",
                "--single-transaction",
                "-h",
                policy::SOCKET,
                "-U",
                "postgres",
                "-d",
                policy::DATABASE,
            ]),
            Some(Input::Snapshot),
            owner,
            executor,
        )
        .await?;
        // Ledger SQL uses an original anonymous fixed input, not an arbitrary argument.
        if let Some(roles) = self.roles.take() {
            roles.finish(owner);
        }
        self.roles = Some(self.anonymous_input(
            ".restore-ledger.sql",
            policy::LEDGER.as_bytes(),
            access.uid,
            owner,
        )?);
        let ledger = self
            .run_checked(
                access,
                Role::Ledger,
                policy::sql(&id),
                Some(Input::Roles),
                owner,
                executor,
            )
            .await?;
        let ledger = self.policy(
            owner,
            std::str::from_utf8(&ledger).map_err(|_| "validation_failed"),
        )?;
        if python_strip(ledger) != policy::ledger(&self.migrations) {
            return self.policy(owner, Err("Restored migration ledger differs from release"));
        }
        let library = Dir::open(&access.library, owner)?;
        self.directories.push(library.clone());
        let sql = Record::restore_sql(library, access.uid, owner)?;
        self.records.files.push(sql);
        self.sql = Some(self.records.files.len() - 1);
        let counts = self
            .run_checked(
                access,
                Role::Verify,
                policy::sql(&id),
                Some(Input::Sql),
                owner,
                executor,
            )
            .await?;
        let counts = self.policy(owner, policy::decode(&counts))?;
        if counts.object().is_none() {
            return self.policy(owner, Err("validation_failed"));
        }
        self.report.counts = Some(counts);
        self.run_checked(
            access,
            Role::Amcheck,
            args(&[
                "exec",
                "--user",
                "999:999",
                &id,
                "timeout",
                "--signal=TERM",
                "--kill-after=2s",
                "120s",
                "pg_amcheck",
                "--install-missing",
                "--heapallindexed",
                "--parent-check",
                "-h",
                policy::SOCKET,
                "-U",
                "postgres",
                "-d",
                policy::DATABASE,
            ]),
            None,
            owner,
            executor,
        )
        .await?;
        self.report.verified = true;
        Ok(())
    }
    fn selectors(&self) -> Vec<OsString> {
        args(&[
            "ps",
            "--all",
            "--quiet",
            "--no-trunc",
            "--filter",
            &format!("label={}={}", policy::LABEL, self.report.container),
            "--filter",
            &format!("name=^/{}$", self.report.container),
        ])
    }
    async fn cleanup(
        &mut self,
        access: &Access,
        owner: &mut Owner,
        executor: &mut impl Executor,
    ) -> Result<()> {
        if !self.cleanup_owed
            || self.report.container.len() != 46
            || !self.report.container.starts_with("scpub-restore-")
            || !policy::hex(&self.report.container[14..], 32)
            || !policy::image(&self.report.postgres_image)
        {
            return self.policy(owner, Err("Invalid owned restore identity"));
        }
        let selectors = self.selectors();
        let selected = self
            .run_checked(
                access,
                Role::CleanupSelect,
                selectors.clone(),
                None,
                owner,
                executor,
            )
            .await?;
        let selected = self.policy(
            owner,
            std::str::from_utf8(&selected).map_err(|_| "validation_failed"),
        )?;
        let id = python_strip(selected).to_owned();
        if !id.is_empty() {
            if !policy::hex(&id, 64) {
                return self.policy(owner, Err("Ambiguous restore container"));
            }
            let format = concat!(
                "{\"id\":{{json .Id}},\"name\":{{json .Name}},",
                "\"image\":{{json .Image}},\"owner\":{{json (index .Config.Labels \"",
                "clinic.research.simplestchat.restore\")}}}"
            );
            let metadata = self
                .run_checked(
                    access,
                    Role::CleanupInspect,
                    args(&["inspect", "--format", format, &id]),
                    None,
                    owner,
                    executor,
                )
                .await?;
            let metadata = self.policy(owner, policy::decode(&metadata))?;
            let valid = self.report.container_id.is_empty() || self.report.container_id == id;
            if !valid
                || policy::text(&metadata, "id").ok() != Some(id.as_str())
                || policy::text(&metadata, "name").ok()
                    != Some(format!("/{}", self.report.container).as_str())
                || policy::text(&metadata, "image").ok()
                    != Some(self.report.postgres_image.as_str())
                || policy::text(&metadata, "owner").ok() != Some(self.report.container.as_str())
            {
                return self.policy(owner, Err("Restore container ownership changed"));
            }
            self.run_checked(
                access,
                Role::CleanupRemove,
                args(&["rm", "--force", "--volumes", &id]),
                None,
                owner,
                executor,
            )
            .await?;
        }
        let absent = self
            .run_checked(
                access,
                Role::CleanupConfirm,
                selectors,
                None,
                owner,
                executor,
            )
            .await?;
        if !python_byte_strip(&absent).is_empty() {
            return self.policy(owner, Err("Restore container cleanup incomplete"));
        }
        self.cleanup_owed = false;
        Ok(())
    }
    fn unlink_snapshot(
        &mut self,
        snapshot: Record,
        owner: &mut Owner,
        recovery: bool,
    ) -> Result<()> {
        #[cfg(test)]
        {
            self.snapshot_unlinks += 1;
        }
        #[cfg(test)]
        if self.fault == Some(super::Fault::SnapshotUnlink) {
            self.fault_hits += 1;
            owner.record(Cause::Io);
            snapshot.finish(owner);
            return Err(Cause::Io);
        }
        snapshot.retire(owner, recovery)
    }
    fn finish(mut self, owner: &mut Owner) -> Finalized {
        // The archive snapshot is always retired when its exact known written
        // prefix can be established under B; a refusal still closes its original.
        if let Some(mut snapshot) = self.snapshot.take() {
            let entered = self.enter_cleanup(owner);
            let checked = entered.and_then(|()| snapshot.observe_growth(owner, true));
            let known = checked.is_ok()
                && snapshot.len() == self.snapshot_bytes
                && snapshot.sha256()
                    == Some(
                        super::super::files::hex(&self.snapshot_hash.clone().finalize()).as_str(),
                    );
            if known {
                let _ = self.unlink_snapshot(snapshot, owner, true);
            } else {
                if checked.is_ok() {
                    owner.record(Cause::Changed);
                }
                snapshot.finish(owner);
            }
        }
        if let Some(roles) = self.roles.take() {
            roles.finish(owner);
        }
        if self.candidate {
            owner.end_restore_candidate();
            self.candidate = false;
        }
        if self.cleanup_phase {
            owner.end_restore_cleanup();
            self.cleanup_phase = false;
        }
        self.records.finish(owner);
        self.attempt.take();
        self.state.take();
        self.guard.directories = std::mem::take(&mut self.directories);
        Finalized {
            guard: self.guard,
            #[cfg(test)]
            fault_hits: self.fault_hits,
            #[cfg(test)]
            snapshot_unlinks: self.snapshot_unlinks,
        }
    }
}
fn timestamp() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}
pub(super) async fn execute(
    options: &Options,
    access: &Access,
    owner: &mut Owner,
    executor: &mut impl Executor,
) -> Outcome {
    let mut state = State::new();
    #[cfg(test)]
    {
        state.fault = access.fault;
    }
    let result = state.body(options, access, owner, executor).await;
    if let Err(cause) = result {
        owner.record(cause);
        if state.failure.is_none() {
            state.failure = Some(cause_name(cause).into());
        }
    }
    // Every parent-owned file settles before the sole lock owner can unlock.
    let mut passed = result.is_ok() && owner.check(Window::Body).is_ok();
    let mut failure = state.failure.clone();
    let finalized = state.finish(owner);
    if failure.is_none() {
        failure = owner.error().map(|cause| cause_name(cause).into());
    }
    passed &= owner.error().is_none();
    Outcome {
        passed,
        failure,
        guard: Some(finalized.guard),
        #[cfg(test)]
        fault_hits: finalized.fault_hits,
        #[cfg(test)]
        snapshot_unlinks: finalized.snapshot_unlinks,
    }
}
