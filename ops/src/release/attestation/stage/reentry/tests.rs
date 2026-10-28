//! Actual local-file/flock handoff regressions; no daemon or host-runtime proof.
use super::*;
use std::{
    fs,
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::Path,
    time::{Duration, Instant},
};

fn write(path: &Path, bytes: &[u8], mode: u32) {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
        .unwrap();
    let result = file.write_all(bytes).and_then(|()| file.sync_all());
    let closed = nix::unistd::close(file);
    result.unwrap();
    closed.unwrap();
}
fn release(lease: workload::Lease) -> bool {
    let (result, file) = lease.release();
    let closed = file.is_some_and(|file| nix::unistd::close(file).is_ok());
    result.is_ok() && closed
}
struct Session {
    _fixture: crate::test_support::Fixture,
    access: Access,
    state: Option<State>,
    owner: Option<Owner>,
}
impl Session {
    fn new(current: bool) -> Self {
        let fixture = crate::test_support::Fixture::new();
        let cached = crate::release::attestation::verification_fixture::Fixture::new();
        let revision = "a".repeat(40);
        let root = fixture.0.join("public");
        let config = fixture.0.join("config");
        let work = fixture.0.join("work");
        let destination = root.join("releases").join(&revision);
        for path in [
            &root,
            &config,
            &work,
            &root.join("results"),
            &root.join("releases"),
            &destination,
        ] {
            fs::DirBuilder::new().mode(0o700).create(path).unwrap();
        }
        for name in ["image.tar", "release.json"] {
            let bytes = crate::release::attestation::verification_fixture::read(
                &cached.candidate.join(name),
            )
            .unwrap();
            write(&destination.join(name), &bytes, 0o600);
        }
        let docker = fixture.0.join("docker");
        write(&docker, b"#!/bin/sh\nexit 99\n", 0o700);
        if current {
            write(
                &work.join("current.json"),
                br#"{"schemaVersion":1,"finalized":true,"run":"first"}"#,
                0o600,
            );
        }
        let access = Access::for_host(
            root,
            config,
            work,
            docker,
            None,
            rustix::process::getuid().as_raw(),
        );
        let mut session = Self {
            _fixture: fixture,
            access,
            state: Some(State::for_host()),
            owner: Some(
                Owner::new(
                    Instant::now(),
                    Duration::from_secs(90),
                    Duration::from_secs(10),
                )
                .unwrap(),
            ),
        };
        session.setup();
        session
    }
    fn parts(&mut self) -> (&mut State, &mut Owner) {
        (
            self.state.as_mut().expect("state remains until finish"),
            self.owner.as_mut().expect("owner remains until finish"),
        )
    }
    fn setup(&mut self) {
        let state = self.state.as_mut().unwrap();
        let owner = self.owner.as_mut().unwrap();
        state.setup(&self.access, &"a".repeat(40), owner).unwrap();
        state
            .journal_in("maintenance_selected", true, owner, false)
            .unwrap();
    }
    fn unlock(&mut self) {
        let (state, owner) = self.parts();
        state.prepare_maintenance_unlock(owner).unwrap();
        state.unlock(owner);
        owner.check(Window::Body).unwrap();
    }
    fn reenter(&mut self) -> Result<()> {
        self.state
            .as_mut()
            .unwrap()
            .reacquire_maintenance(&self.access, self.owner.as_mut().unwrap())
    }
    fn contender(&self) -> std::result::Result<workload::Lease, workload::AcquisitionFailure> {
        workload::Lease::acquire(
            fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(self.access.work.join("workload.lock"))
                .unwrap(),
        )
    }
    fn finish(&mut self) -> Result<()> {
        let mut owner = self.owner.take().unwrap();
        self.state.take().unwrap().finish(&mut owner);
        owner.finish()
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        if self.owner.is_some() {
            let _ = self.finish();
        }
    }
}

fn busy(value: std::result::Result<workload::Lease, workload::AcquisitionFailure>) -> bool {
    match value {
        Err(failed) => failed.first == workload::Failure::Busy && !failed.close_failed,
        Ok(lease) => {
            let _ = release(lease);
            false
        }
    }
}

#[test]
fn maintenance_reentry_really_unlocks_and_takes_a_second_flock_without_a_new_attempt() {
    let mut session = Session::new(false);
    let before = session.state.as_ref().unwrap().attempt().unwrap().path();
    let first_busy = busy(session.contender());
    session.unlock();
    let intermediate = session.contender().ok().is_some_and(release);
    let resumed = session.reenter();
    let second_busy = busy(session.contender());
    let after = session.state.as_ref().unwrap().attempt().unwrap().path();
    let attempts = fs::read_dir(session.access.root.join("results"))
        .unwrap()
        .count();
    let journal: serde_json::Value =
        serde_json::from_slice(&fs::read(session.access.root.join("release-state.json")).unwrap())
            .unwrap();
    let finished = session.finish();
    assert!(first_busy && intermediate && second_busy);
    assert_eq!(resumed, Ok(()));
    assert_eq!(before, after);
    assert_eq!(attempts, 1);
    assert_eq!(journal["attempt"].as_str(), before.to_str());
    assert_eq!(journal["phase"], "maintenance_selected");
    assert_eq!(finished, Ok(()));
}

#[test]
fn finalized_benchmark_turnover_and_absence_are_readmitted_only_after_the_second_flock() {
    for (initial, remove) in [(true, false), (true, true), (false, false)] {
        let mut session = Session::new(initial);
        session.unlock();
        let current = session.access.work.join("current.json");
        if remove {
            fs::remove_file(&current).unwrap();
        } else {
            let temporary = session.access.work.join("next.json");
            write(
                &temporary,
                br#"{"schemaVersion":1,"finalized":true,"run":"next"}"#,
                0o600,
            );
            fs::rename(temporary, &current).unwrap();
        }
        let resumed = session.reenter();
        let second_busy = busy(session.contender());
        let admitted = session.state.as_ref().unwrap().benchmark_guard.is_some();
        let finished = session.finish();
        assert_eq!(resumed, Ok(()));
        assert!(second_busy);
        assert_eq!(admitted, !remove);
        assert_eq!(finished, Ok(()));
    }
}

#[test]
fn unfinished_or_ambiguous_benchmark_record_refuses_after_actual_reacquisition() {
    for record in [
        br#"{"schemaVersion":1,"finalized":false}"#.as_slice(),
        br#"{"schemaVersion":true,"finalized":true}"#,
        br#"{"schemaVersion":1,"finalized":true,"finalized":true}"#,
    ] {
        let mut session = Session::new(false);
        session.unlock();
        write(&session.access.work.join("current.json"), record, 0o600);
        let resumed = session.reenter();
        let second_busy = busy(session.contender());
        let completed_second_acquisition = session.state.as_ref().unwrap().maintenance_reacquired;
        let finished = session.finish();
        assert_eq!(resumed, Err(Cause::Policy));
        assert!(second_busy && completed_second_acquisition);
        assert_eq!(finished, Err(Cause::Policy));
    }
}

#[test]
fn changed_missing_or_contended_original_lock_never_resumes_maintenance() {
    for mode in ["replace", "missing", "contended"] {
        let mut session = Session::new(false);
        session.unlock();
        let lock = session.access.work.join("workload.lock");
        let contender = if mode == "contended" {
            session.contender().ok()
        } else {
            if mode == "replace" {
                let new = session.access.work.join("replacement.lock");
                write(&new, b"", 0o600);
                fs::rename(new, &lock).unwrap();
            } else {
                fs::remove_file(&lock).unwrap();
            }
            None
        };
        let resumed = session.reenter();
        let second_acquisition = session.state.as_ref().unwrap().maintenance_reacquired;
        let released = contender.map(release);
        let finished = session.finish();
        assert!(resumed.is_err());
        assert!(!second_acquisition);
        assert!(released.is_none_or(|v| v));
        assert!(finished.is_err());
    }
}

#[test]
fn retained_lease_borrower_refuses_unlock_and_prevents_second_acquisition() {
    let mut session = Session::new(false);
    let borrower = {
        let (state, owner) = session.parts();
        state.prepare_maintenance_unlock(owner).unwrap();
        state.borrowed_lease().unwrap()
    };
    let first = {
        let (state, owner) = session.parts();
        state.unlock(owner);
        owner.check(Window::Body)
    };
    let still_busy = busy(session.contender());
    let resumed = session.reenter();
    let no_new_handles = {
        let state = session.state.as_ref().unwrap();
        state.lock.is_none() && state.lease.is_none() && !state.maintenance_reacquired
    };
    let released = release(borrower);
    let finished = session.finish();
    assert_eq!(first, Err(Cause::Close));
    assert!(still_busy && no_new_handles && released);
    assert_eq!(resumed, Err(Cause::Close));
    assert_eq!(finished, Err(Cause::Close));
}

#[test]
fn owned_selection_journal_refuses_wrong_phase_or_replacement_with_equal_bytes() {
    let mut phase = Session::new(false);
    let prepared = {
        let (state, owner) = phase.parts();
        state.journal_in("staged", true, owner, false).unwrap();
        state.prepare_maintenance_unlock(owner)
    };
    let phase_finished = phase.finish();
    assert_eq!(prepared, Err(Cause::Changed));
    assert_eq!(phase_finished, Err(Cause::Changed));

    let mut replaced = Session::new(false);
    replaced.unlock();
    let path = replaced.access.root.join("release-state.json");
    let bytes = fs::read(&path).unwrap();
    let other = replaced.access.root.join("replacement.json");
    write(&other, &bytes, 0o600);
    fs::rename(other, path).unwrap();
    let resumed = replaced.reenter();
    let finished = replaced.finish();
    assert_eq!(resumed, Err(Cause::Changed));
    assert_eq!(finished, Err(Cause::Changed));
}

#[test]
fn owned_staged_marker_and_original_archive_must_survive_the_unlocked_interval() {
    for name in ["staged.json", "image.tar"] {
        let mut session = Session::new(false);
        let destination = session.access.root.join("releases").join("a".repeat(40));
        {
            let (state, owner) = session.parts();
            state
                .records
                .write(
                    state.destination.as_ref().unwrap(),
                    "staged.json",
                    b"{}",
                    owner,
                    false,
                )
                .unwrap();
        }
        session.unlock();
        fs::write(destination.join(name), b"changed").unwrap();
        let resumed = session.reenter();
        let finished = session.finish();
        assert_eq!(resumed, Err(Cause::Changed));
        assert_eq!(finished, Err(Cause::Changed));
    }
}

#[test]
fn candidate_inode_aliases_cover_sources_lock_benchmark_and_owned_stage_records() {
    let mut session = Session::new(true);
    let destination = session.access.root.join("releases").join("a".repeat(40));
    {
        let (state, owner) = session.parts();
        state
            .records
            .write(
                state.destination.as_ref().unwrap(),
                "staged.json",
                b"{}",
                owner,
                false,
            )
            .unwrap();
    }
    let paths = [
        destination.join("release.json"),
        destination.join("image.tar"),
        session.access.docker.clone(),
        session.access.work.join("workload.lock"),
        session.access.work.join("current.json"),
        session.access.root.join("release-state.json"),
        destination.join("staged.json"),
    ];
    let mut aliases = Vec::new();
    for path in paths {
        let file = File::open(path).unwrap();
        let result = {
            let (state, owner) = session.parts();
            state.input_aliases(&file, owner)
        };
        let closed = nix::unistd::close(file);
        aliases.push(result);
        closed.unwrap();
    }
    let foreign = session.access.config.join("candidate.env");
    write(&foreign, b"{}", 0o600);
    let file = File::open(foreign).unwrap();
    let distinct = {
        let (state, owner) = session.parts();
        state.input_aliases(&file, owner)
    };
    let closed = nix::unistd::close(file);
    let finished = session.finish();
    assert!(aliases.into_iter().all(|value| value == Ok(true)));
    assert_eq!(distinct, Ok(false));
    assert!(closed.is_ok());
    assert_eq!(finished, Ok(()));
}
