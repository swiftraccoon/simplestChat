//! Real private generations and paired finalization refusals; no engine proof.

use super::super::{
    control::{Cause, Owner},
    files::Dir,
};
use super::{
    files,
    records::{Record, Records},
};
use crate::test_support::Fixture;
use std::{
    fs,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    os::unix::fs::PermissionsExt,
    time::{Duration, Instant},
};

fn seed(path: &std::path::Path, data: &[u8], mode: u32) -> std::io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    let written = file
        .write_all(data)
        .and_then(|()| file.flush())
        .and_then(|()| file.set_permissions(fs::Permissions::from_mode(mode)));
    let closed = nix::unistd::close(file).map_err(std::io::Error::from);
    written.and(closed)
}

fn owner() -> Owner {
    Owner::new(
        Instant::now(),
        Duration::from_secs(1800),
        Duration::from_secs(60),
    )
    .unwrap()
}

#[test]
fn replacement_keeps_both_original_generations_and_explicitly_closes_every_file() {
    let fixture = Fixture::new();
    let path = fixture.0.join("selection");
    seed(&path, b"before", 0o600).unwrap();
    let uid = rustix::process::geteuid().as_raw();
    let mut owner = owner();
    files::start_probe();
    let directory = Dir::open(&fixture.0, &mut owner).unwrap();
    let mut records = Records::default();
    let first = records.open(directory.clone(), "selection", uid, 64, &mut owner);
    let result = first
        .and_then(|_| records.atomic(&directory, "selection", b"after", &mut owner, false))
        .and_then(|()| records.verify(&mut owner, false));
    let bytes = fs::read(&path);
    records.finish(&mut owner);
    directory.finish(&mut owner);
    let closed = files::take_probe();
    let finished = owner.finish();
    assert_eq!(result, Ok(()));
    assert_eq!(bytes.unwrap(), b"after");
    assert!(closed.0 > 2);
    assert_eq!(closed.1, 0);
    assert_eq!(finished, Ok(()));
}

#[test]
fn equal_byte_foreign_log_is_not_adopted_and_every_original_closes() {
    let fixture = Fixture::new();
    let mut owner = owner();
    files::start_probe();
    let directory = Dir::open(&fixture.0, &mut owner).unwrap();
    let mut records = Records::default();
    let admitted = records.prepare_log(&directory, "001.stdout", &mut owner, false);
    let written = admitted.and_then(|at| records.finish_log(at, b"same", &mut owner, false));
    let foreign = fixture.0.join("foreign");
    let exchanged = (|| -> std::io::Result<()> {
        seed(&foreign, b"same", 0o600)?;
        fs::rename(&foreign, fixture.0.join("001.stdout"))
    })();
    let checked = if exchanged.is_ok() {
        records.verify(&mut owner, false)
    } else {
        owner.record(Cause::Io);
        Err(Cause::Io)
    };
    records.finish(&mut owner);
    directory.finish(&mut owner);
    let closed = files::take_probe();
    let primary = owner.error();
    let finished = owner.finish();
    assert_eq!(written, Ok(()));
    assert!(exchanged.is_ok());
    assert_eq!(checked, Err(Cause::Changed));
    assert_eq!(primary, Some(Cause::Changed));
    assert_eq!(finished, Err(Cause::Changed));
    assert!(closed.0 >= 2);
    assert_eq!(closed.1, 0);
}

#[test]
fn target_appearing_during_complete_temp_write_remains_foreign() {
    let fixture = Fixture::new();
    let mut owner = owner();
    files::start_probe();
    let directory = Dir::open(&fixture.0, &mut owner).unwrap();
    let mut records = Records::default();
    let result = records.atomic_fault(
        &directory,
        "selected",
        b"ours",
        &mut owner,
        false,
        Some(super::records::FOREIGN_AFTER_TEMP),
    );
    let visible = fs::read(fixture.0.join("selected"));
    records.finish(&mut owner);
    directory.finish(&mut owner);
    let closed = files::take_probe();
    let finished = owner.finish();
    assert_eq!(result, Err(Cause::Changed));
    assert_eq!(visible.unwrap(), b"foreign");
    assert_eq!(finished, Err(Cause::Changed));
    assert!(closed.0 >= 2);
    assert_eq!(closed.1, 0);
}

#[test]
fn rename_adopts_only_rename_ctime_and_refuses_an_equal_byte_mtime_change() {
    let fixture = Fixture::new();
    let mut owner = owner();
    files::start_probe();
    let directory = Dir::open(&fixture.0, &mut owner).unwrap();
    let mut record = Record::create(directory.clone(), "temporary", &mut owner, false).ok();
    let result = (|| {
        let record = record.as_mut().ok_or(Cause::Io)?;
        record.write(b"same", &mut owner, false)?;
        let changed = record.file().set_times(
            std::fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1)),
        );
        if changed.is_err() {
            owner.record(Cause::Io);
            return Err(Cause::Io);
        }
        if fs::rename(fixture.0.join("temporary"), fixture.0.join("selected")).is_err() {
            owner.record(Cause::Io);
            return Err(Cause::Io);
        }
        record.renamed("selected", &mut owner, false)
    })();
    if let Some(record) = record {
        record.finish(&mut owner);
    }
    directory.finish(&mut owner);
    let closed = files::take_probe();
    let finished = owner.finish();
    assert_eq!(result, Err(Cause::Changed));
    assert_eq!(finished, Err(Cause::Changed));
    assert!(closed.0 >= 1);
    assert_eq!(closed.1, 0);
}

#[test]
fn normal_log_write_cannot_borrow_rollback_reserve_and_still_closes_its_original() {
    let fixture = Fixture::new();
    let started = Instant::now();
    let mut owner =
        Owner::new(started, Duration::from_secs(1800), Duration::from_secs(60)).unwrap();
    owner.elapsed = Some(Duration::ZERO);
    files::start_probe();
    let directory = Dir::open(&fixture.0, &mut owner).unwrap();
    let mut records = Records::default();
    let admitted = records.prepare_log(&directory, "001.stdout", &mut owner, false);
    let candidate = super::budget::candidate_end(&owner).unwrap();
    let phase = owner.begin_deploy_candidate(candidate);
    owner.observe_at(candidate);
    let written = admitted.and_then(|at| records.finish_log(at, b"late", &mut owner, false));
    let bytes = fs::read(fixture.0.join("001.stdout"));
    owner.end_deploy_candidate();
    records.finish(&mut owner);
    directory.finish(&mut owner);
    let closed = files::take_probe();
    let finished = owner.finish();
    assert_eq!(phase, Ok(()));
    assert_eq!(written, Err(Cause::Deadline));
    assert_eq!(bytes.unwrap(), b"");
    assert!(closed.0 >= 1);
    assert_eq!(closed.1, 0);
    assert_eq!(finished, Err(Cause::Deadline));
}

#[test]
fn complete_temporary_sync_refusal_keeps_primary_through_actual_close_refusal() {
    let fixture = Fixture::new();
    let mut owner = owner();
    files::start_probe();
    let directory = Dir::open(&fixture.0, &mut owner).unwrap();
    let mut records = Records::default();
    let result = records.atomic_fault(
        &directory,
        "outcome.json",
        b"{\"passed\":true}\n",
        &mut owner,
        false,
        Some(1),
    );
    let failed = owner.error();
    files::fail_close(1);
    records.finish(&mut owner);
    directory.finish(&mut owner);
    let closed = files::take_probe();
    let final_cause = owner.error();
    let finished = owner.finish();
    let retained = fs::read_dir(&fixture.0).unwrap().count();
    assert_eq!(result, Err(Cause::Io));
    assert_eq!(failed, Some(Cause::Io));
    assert_eq!(final_cause, Some(Cause::Io));
    assert_eq!(finished, Err(Cause::Io));
    assert!(closed.0 >= 1);
    assert_eq!(closed.1, 0);
    assert_eq!(retained, 1);
}

#[test]
fn readonly_backup_capability_matches_the_original_without_full_consumption_ack() {
    let fixture = Fixture::new();
    let mut owner = owner();
    files::start_probe();
    let directory = Dir::open(&fixture.0, &mut owner).unwrap();
    let mut original = Record::create(directory.clone(), "backup", &mut owner, false).ok();
    let written = original
        .as_mut()
        .ok_or(Cause::Io)
        .and_then(|record| record.write(b"header and retained body", &mut owner, false));
    let readonly = Record::open(
        directory.clone(),
        "backup",
        rustix::process::geteuid().as_raw(),
        64,
        &mut owner,
    );
    let mut alias = false;
    if let Ok(readonly) = readonly {
        alias = original
            .as_ref()
            .is_some_and(|original| original.aliases(&readonly));
        readonly.finish(&mut owner);
    }
    if let Some(original) = original {
        original.finish(&mut owner);
    }
    directory.finish(&mut owner);
    let closed = files::take_probe();
    let finished = owner.finish();
    assert_eq!(written, Ok(()));
    assert!(alias);
    assert_eq!(finished, Ok(()));
    assert!(closed.0 >= 2);
    assert_eq!(closed.1, 0);
}

#[test]
fn actual_snapshot_read_uses_live_candidate_ceiling_and_keeps_original_recovery_body() {
    use crate::files::{Snapshot, SnapshotEvent};
    let fixture = Fixture::new();
    let path = fixture.0.join("config");
    seed(&path, b"configuration", 0o600).unwrap();
    let started = Instant::now();
    let mut owner =
        Owner::new(started, Duration::from_secs(1800), Duration::from_secs(60)).unwrap();
    owner.elapsed = Some(Duration::ZERO);
    let candidate = super::budget::candidate_end(&owner).unwrap();
    let mut snapshot = Snapshot::open_checked(&path, 64, |_, observed| {
        observed?;
        owner
            .check(super::super::control::Window::Body)
            .map_err(|_| crate::Error::Input)
    })
    .unwrap();
    let admitted = owner.begin_deploy_candidate(candidate);
    let mut buffer = [0; 5];
    let mut reads = 0;
    let result = snapshot.read_checked(&mut buffer, |event, observed| {
        observed?;
        if matches!(event, SnapshotEvent::Read) {
            reads += 1;
            if reads == 2 {
                owner.observe_at(candidate);
            }
        }
        owner
            .check(super::super::control::Window::Body)
            .map_err(|_| crate::Error::Input)
    });
    let primary = owner.error();
    owner.end_deploy_candidate();
    let recovery = owner.begin_deploy_recovery();
    let remaining = owner.body_end().saturating_duration_since(owner.now());
    let closed = snapshot.finish_checked(|_, observed| {
        observed?;
        owner.final_work().map_err(|_| crate::Error::Input)
    });
    owner.end_deploy_recovery();
    let finished = owner.finish();
    assert_eq!(admitted, Ok(()));
    assert!(result.is_err());
    assert_eq!(&buffer, b"confi");
    assert_eq!(primary, Some(Cause::Deadline));
    assert_eq!(recovery, Ok(()));
    assert_eq!(remaining, super::budget::RECOVERY);
    assert_eq!(closed, Ok(()));
    assert_eq!(finished, Err(Cause::Deadline));
}

#[test]
fn caddy_public_readonly_profile_keeps_private_modes_and_all_original_witnesses_strict() {
    let uid = rustix::process::geteuid().as_raw();
    for (mode, public, links, admitted) in [
        (0o644, true, false, true),
        (0o600, true, false, true),
        (0o644, false, false, false),
        (0o666, true, false, false),
        (0o640, true, false, false),
        (0o644, true, true, true),
        (0o600, false, true, false),
    ] {
        let fixture = Fixture::new();
        let path = fixture.0.join("Caddyfile");
        seed(&path, b"fixed proxy configuration", mode).unwrap();
        if links {
            fs::hard_link(&path, fixture.0.join("readonly-alias")).unwrap();
        }
        let mut owner = owner();
        files::start_probe();
        let directory = Dir::open(&fixture.0, &mut owner).unwrap();
        let result = if public {
            Record::caddy(directory.clone(), uid, &mut owner)
        } else {
            Record::open(directory.clone(), "Caddyfile", uid, 1024, &mut owner)
        };
        let positive = result.is_ok();
        if let Ok(record) = result {
            record.finish(&mut owner);
        }
        directory.finish(&mut owner);
        let closed = files::take_probe();
        let finished = owner.finish();
        assert_eq!(
            positive, admitted,
            "mode {mode:o}, public {public}, links {links}"
        );
        assert_eq!(finished.is_ok(), admitted);
        assert!(closed.0 > 0);
        assert_eq!(closed.1, 0);
    }
}

#[test]
fn caddy_readonly_owner_and_persistent_mode_change_refuse_without_adopting_the_inode() {
    let fixture = Fixture::new();
    let path = fixture.0.join("Caddyfile");
    seed(&path, b"fixed proxy configuration", 0o644).unwrap();
    let uid = rustix::process::geteuid().as_raw();
    let wrong_uid = uid.checked_add(1).unwrap();
    let mut first = owner();
    files::start_probe();
    let directory = Dir::open(&fixture.0, &mut first).unwrap();
    let refused = Record::caddy(directory.clone(), wrong_uid, &mut first);
    let wrong_owner = refused.is_err();
    if let Ok(record) = refused {
        record.finish(&mut first);
    }
    directory.finish(&mut first);
    let first_closed = files::take_probe();
    let first_finished = first.finish();
    let mut owner = owner();
    files::start_probe();
    let directory = Dir::open(&fixture.0, &mut owner).unwrap();
    let record = Record::caddy(directory.clone(), uid, &mut owner);
    let changed = fs::set_permissions(&path, fs::Permissions::from_mode(0o666));
    let checked = match &record {
        Ok(record) if changed.is_ok() => record.verify(&mut owner, false),
        _ => {
            owner.record(Cause::Io);
            Err(Cause::Io)
        }
    };
    if let Ok(record) = record {
        record.finish(&mut owner);
    }
    directory.finish(&mut owner);
    let second_closed = files::take_probe();
    let finished = owner.finish();
    assert!(wrong_owner);
    assert_eq!(first_finished, Err(Cause::Policy));
    assert!(changed.is_ok());
    assert_eq!(checked, Err(Cause::Changed));
    assert_eq!(finished, Err(Cause::Changed));
    assert!(first_closed.0 > 0 && second_closed.0 > 0);
    assert_eq!((first_closed.1, second_closed.1), (0, 0));
}
