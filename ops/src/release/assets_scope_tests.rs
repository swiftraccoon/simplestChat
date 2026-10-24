//! Real retained-file I/O and exact original-owner refusal/close ordering.
use super::*;
use std::{
    fs::File,
    io::Read,
    time::{Duration, Instant},
};

fn owner() -> Owner {
    Owner::new(
        Instant::now(),
        Duration::from_secs(300),
        Duration::from_secs(30),
    )
    .unwrap()
}

#[test]
fn actual_read_completed_after_original_body_is_not_a_positive_observation() {
    let fixture = crate::test_support::Fixture::new();
    let path = fixture.0.join("owned-read");
    std::fs::write(&path, b"retained").unwrap();
    let mut file = File::open(path).unwrap();
    let mut owner = owner();
    let mut bytes = [0; 8];
    let (read, closed) = {
        let scope = Scope::owned(&mut owner, Mode::Body);
        let read = scope.io(|| {
            let read = file.read(&mut bytes);
            scope.owner.as_ref().unwrap().borrow_mut().expire_body();
            read
        });
        let closed = scope.closed(super::super::files::Access::default().close(file));
        (read, closed)
    };
    let cause = owner.error();
    let finished = owner.finish();
    assert!(read.is_err());
    assert_eq!(bytes, *b"retained");
    assert!(closed.is_ok());
    assert_eq!(cause, Some(Cause::Deadline));
    assert_eq!(finished, Err(Cause::Deadline));
}

#[test]
fn observed_io_failure_precedes_later_clock_and_checked_close_fault() {
    let fixture = crate::test_support::Fixture::new();
    let path = fixture.0.join("owned-close");
    std::fs::write(&path, b"owned").unwrap();
    let file = File::open(path).unwrap();
    let mut owner = owner();
    let (body, actual_close, close) = {
        let scope = Scope::owned(&mut owner, Mode::Body);
        let body = scope.io::<()>(|| {
            scope.owner.as_ref().unwrap().borrow_mut().expire_body();
            Err(std::io::Error::other("inert read fault"))
        });
        let actual_close = nix::unistd::close(file);
        let close = scope.closed(
            actual_close
                .map_err(|_| fail("asset_operation_failed"))
                .and(Err(fail("asset_operation_failed"))),
        );
        (body, actual_close.is_ok(), close)
    };
    let cause = owner.error();
    let finished = owner.finish();
    assert!(body.is_err());
    assert!(actual_close);
    assert!(close.is_err());
    assert_eq!(cause, Some(Cause::Io));
    assert_eq!(finished, Err(Cause::Io));
}

#[test]
fn known_recovery_retains_primary_but_cannot_use_local_reserve() {
    let mut owner = owner();
    owner.record(Cause::Policy);
    let (body, recovery, expired) = {
        let scope = Scope::owned(&mut owner, Mode::Body);
        let body = scope.check();
        let recovery = {
            let _phase = scope.recovery();
            scope.check()
        };
        scope.owner.as_ref().unwrap().borrow_mut().expire_body();
        let expired = {
            let _phase = scope.recovery();
            scope.check()
        };
        (body, recovery, expired)
    };
    let cause = owner.error();
    let finished = owner.finish();
    assert!(body.is_err());
    assert!(recovery.is_ok());
    assert!(expired.is_err());
    assert_eq!(cause, Some(Cause::Policy));
    assert_eq!(finished, Err(Cause::Policy));
}

#[test]
fn snapshot_changed_original_wins_over_later_actual_close_faults() {
    let fixture = crate::test_support::Fixture::new();
    let path = fixture.0.join("snapshot-close");
    std::fs::write(&path, b"original").unwrap();
    let expected_closes = path
        .components()
        .filter(|value| matches!(value, std::path::Component::Normal(_)))
        .count()
        + 1;
    let mut owner = owner();
    let mut closed = 0;
    let mut all_closed = true;
    let result = {
        let scope = Scope::owned(&mut owner, Mode::Body);
        let snapshot = crate::files::Snapshot::open_checked(&path, 64, |event, value| {
            scope.snapshot(event, value)
        });
        match snapshot {
            Ok(snapshot) => {
                let mutation = std::fs::write(&path, b"modified");
                let finished = snapshot.finish_checked_fault(
                    |file| {
                        let result = nix::unistd::close(file);
                        all_closed &= result.is_ok();
                        closed += 1;
                        result
                            .map_err(|_| crate::Error::Input)
                            .and(Err(crate::Error::Input))
                    },
                    |event, value| scope.snapshot(event, value),
                );
                (mutation.is_ok(), finished)
            }
            Err(error) => (false, Err(error)),
        }
    };
    let cause = owner.error();
    let finished = owner.finish();
    assert!(result.0);
    assert_eq!(result.1, Err(crate::Error::Changed));
    assert_eq!(closed, expected_closes);
    assert!(all_closed);
    assert_eq!(cause, Some(Cause::Changed));
    assert_eq!(finished, Err(Cause::Changed));
}

#[test]
fn nonce_short_reads_and_interrupted_retry_keep_the_original_physical_fence() {
    struct Reads<'a, 'owner> {
        file: File,
        scope: &'a Scope<'owner>,
        calls: usize,
        expire: bool,
    }
    impl Read for Reads<'_, '_> {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            self.calls += 1;
            if self.calls == 1 {
                return Err(std::io::ErrorKind::Interrupted.into());
            }
            let end = bytes.len().min(3);
            let read = self.file.read(&mut bytes[..end]);
            if self.expire {
                self.scope
                    .owner
                    .as_ref()
                    .unwrap()
                    .borrow_mut()
                    .expire_body();
            }
            read
        }
    }
    for expire in [false, true] {
        let fixture = crate::test_support::Fixture::new();
        let path = fixture.0.join("nonce-original");
        std::fs::write(&path, b"0123456789abcdef").unwrap();
        let file = File::open(path).unwrap();
        let mut owner = owner();
        let (result, calls, closed) = {
            let scope = Scope::owned(&mut owner, Mode::Body);
            let mut reads = Reads {
                file,
                scope: &scope,
                calls: 0,
                expire,
            };
            let result = super::super::commit::nonce_bytes(&mut reads, &scope);
            let calls = reads.calls;
            let closed = scope.closed(super::super::files::Access::default().close(reads.file));
            (result, calls, closed)
        };
        let cause = owner.error();
        let finished = owner.finish();
        assert!(closed.is_ok());
        if expire {
            assert!(result.is_err());
            assert_eq!(calls, 2);
            assert_eq!(cause, Some(Cause::Deadline));
            assert_eq!(finished, Err(Cause::Deadline));
        } else {
            assert_eq!(result, Ok(*b"0123456789abcdef"));
            assert_eq!(calls, 7);
            assert_eq!(cause, None);
            assert!(finished.is_ok());
        }
    }
}

#[test]
fn successful_remnant_cleanup_cannot_borrow_the_candidate_recovery_debt() {
    use super::super::files::{Access, Fs, ScopedFs};
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    for failed in [false, true] {
        let fixture = crate::test_support::Fixture::new();
        std::fs::set_permissions(&fixture.0, std::fs::Permissions::from_mode(0o700)).unwrap();
        let root = File::open(&fixture.0).unwrap();
        let uid = root.metadata().unwrap().uid();
        let mut fs = Fs::open(
            root,
            Access {
                owner: Some(uid),
                hook: None,
            },
        )
        .unwrap();
        let mut owner = owner();
        let candidate = owner.now() + Duration::from_secs(60);
        let admitted = owner.begin_deploy_candidate(candidate);
        let cleanup = {
            let scope = Scope::owned(&mut owner, Mode::Body);
            let mut scoped = ScopedFs::new(&mut fs, &scope);
            let result = (|| {
                let entry = scoped.write(0, "known-remnant", b"owned", 0o600)?;
                if failed {
                    scope.record(Cause::Policy);
                }
                scope
                    .owner
                    .as_ref()
                    .unwrap()
                    .borrow_mut()
                    .observe_at(candidate);
                let _recovery = scope.recovery_if(failed);
                scoped.unlink(entry)
            })();
            scope.observed(&result);
            result
        };
        let files_closed = {
            let scope = Scope::owned(&mut owner, Mode::Final);
            fs.finish_scoped(&scope)
        };
        owner.end_deploy_candidate();
        let cause = owner.error();
        let owner_finished = owner.finish();
        let retained = fixture.0.join("known-remnant").exists();
        assert!(admitted.is_ok());
        assert!(files_closed.is_ok());
        if failed {
            assert!(cleanup.is_ok());
            assert!(!retained);
            assert_eq!(cause, Some(Cause::Policy));
            assert_eq!(owner_finished, Err(Cause::Policy));
        } else {
            assert!(cleanup.is_err());
            assert!(retained);
            assert_eq!(cause, Some(Cause::Deadline));
            assert_eq!(owner_finished, Err(Cause::Deadline));
        }
    }
}
