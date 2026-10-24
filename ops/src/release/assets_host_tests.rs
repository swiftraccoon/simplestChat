//! Whole borrowed-store transactions under a real original flock.
use super::super::{
    files,
    tests::{Fixture, capture, revision},
};
use super::*;
use crate::release::attestation::control::Cause;
use std::{
    fs::{self, File, OpenOptions},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::Path,
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
fn lease(fixture: &Fixture) -> Lease {
    let directory = fixture.path.join("run/simplestchat-bench");
    fs::create_dir(&directory).unwrap();
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(directory.join("workload.lock"))
        .unwrap();
    Lease::acquire(file).unwrap()
}
fn borrowed(fixture: &Fixture, lease: &Lease, owner: &mut Owner) -> Result<HostStore> {
    let path = fixture.path.join("srv/simplestchat-public");
    let uid = fs::metadata(&path)
        .map_err(|_| super::super::fail("asset_operation_failed"))?
        .uid();
    let file = File::from(files::sys(rustix::fs::open(
        &path,
        files::FLAGS | rustix::fs::OFlags::DIRECTORY,
        rustix::fs::Mode::empty(),
    ))?);
    HostStore::borrow(file, lease.clone(), uid, owner)
}
fn held_elsewhere(path: &Path) -> Result<bool> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|_| super::super::fail("asset_operation_failed"))?;
    let locked = rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive);
    let busy = locked == Err(rustix::io::Errno::WOULDBLOCK);
    let unlocked = if locked.is_ok() {
        files::sys(rustix::fs::flock(&file, rustix::fs::FlockOperation::Unlock))
    } else {
        Ok(())
    };
    let closed = files::Access::default().close(file);
    unlocked.and(closed).map(|()| busy)
}
fn release(lease: Lease) -> bool {
    let (unlocked, original) = lease.release();
    let closed = original
        .map(|file| nix::unistd::close(file).is_ok())
        .unwrap_or(false);
    unlocked.is_ok() && closed
}

#[test]
fn publish_select_and_close_never_reacquire_or_unlock_the_borrowed_original() {
    let fixture = Fixture::new();
    let mut owner = owner();
    let lease = lease(&fixture);
    let mut host = borrowed(&fixture, &lease, &mut owner).ok();
    let result = (|| {
        let store = host
            .as_mut()
            .ok_or_else(|| super::super::fail("asset_operation_failed"))?;
        let published = store.publish(
            vec![(revision('a'), capture(&[("app-abcdef.js", b"old")]))],
            1,
            &mut owner,
        )?;
        let changed = store.select(&revision('a'), &mut owner, false)?;
        let repeat = store.select(&revision('a'), &mut owner, false)?;
        Ok::<_, super::super::Failure>((published.generations, published.bytes, changed, repeat))
    })();
    let lock = fixture.path.join("run/simplestchat-bench/workload.lock");
    let during = held_elsewhere(&lock);
    let finished = host.take().map(|host| host.finish(&mut owner));
    let after_borrower = held_elsewhere(&lock);
    let released = release(lease);
    let after_owner = held_elsewhere(&lock);
    let owner_finished = owner.finish();
    assert_eq!(result, Ok((1, 3, true, false)));
    assert_eq!(during, Ok(true));
    assert_eq!(finished, Some(Ok(())));
    assert_eq!(after_borrower, Ok(true));
    assert!(released);
    assert_eq!(after_owner, Ok(false));
    assert!(owner_finished.is_ok());
}

#[test]
fn recovery_selects_known_generation_with_original_primary_and_retires_every_borrower() {
    let fixture = Fixture::new();
    let mut owner = owner();
    let lease = lease(&fixture);
    let mut host = borrowed(&fixture, &lease, &mut owner).ok();
    let result = (|| {
        let store = host
            .as_mut()
            .ok_or_else(|| super::super::fail("asset_operation_failed"))?;
        store.publish(
            vec![
                (revision('a'), capture(&[("app-abcdef.js", b"old")])),
                (revision('b'), capture(&[("app-ghijkl.js", b"new")])),
            ],
            1,
            &mut owner,
        )?;
        store.select(&revision('a'), &mut owner, false)?;
        owner.record(Cause::Command);
        let ordinary = store.select(&revision('b'), &mut owner, false);
        let recovery = store.select(&revision('b'), &mut owner, true);
        Ok::<_, super::super::Failure>((ordinary.is_err(), recovery?))
    })();
    let finished = host.take().map(|host| host.finish(&mut owner));
    let released = release(lease);
    let bytes = fs::read(fixture.root().join("public/app-version.json"));
    let cause = owner.error();
    let owner_finished = owner.finish();
    assert_eq!(result, Ok((true, true)));
    assert_eq!(finished, Some(Ok(())));
    assert!(released);
    assert_eq!(
        bytes.unwrap(),
        format!("{{\"revision\":\"{}\"}}\n", revision('b')).as_bytes()
    );
    assert_eq!(cause, Some(Cause::Command));
    assert_eq!(owner_finished, Err(Cause::Command));
}

#[test]
fn expired_body_select_cannot_replace_the_retained_selected_generation() {
    let fixture = Fixture::new();
    let mut owner = owner();
    let lease = lease(&fixture);
    let mut host = borrowed(&fixture, &lease, &mut owner).ok();
    let result = (|| {
        let store = host
            .as_mut()
            .ok_or_else(|| super::super::fail("asset_operation_failed"))?;
        store.publish(
            vec![(revision('a'), capture(&[("app-abcdef.js", b"old")]))],
            1,
            &mut owner,
        )?;
        store.select(&revision('a'), &mut owner, false)?;
        let path = fixture.root().join("public/app-version.json");
        let before = fs::read(&path).map_err(|_| super::super::fail("asset_operation_failed"))?;
        let before_inode = fs::metadata(&path)
            .map_err(|_| super::super::fail("asset_operation_failed"))?
            .ino();
        owner.expire_body();
        let refused = store.select(&revision('b'), &mut owner, false).is_err();
        Ok::<_, super::super::Failure>((path, before, before_inode, refused))
    })();
    let finished = host.take().map(|host| host.finish(&mut owner));
    let released = release(lease);
    let cause = owner.error();
    let owner_finished = owner.finish();
    let (path, before, inode, refused) = result.unwrap();
    assert!(refused);
    assert_eq!(finished, Some(Ok(())));
    assert!(released);
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(fs::metadata(path).unwrap().ino(), inode);
    assert_eq!(cause, Some(Cause::Deadline));
    assert_eq!(owner_finished, Err(Cause::Deadline));
}

#[test]
fn changed_original_lock_refuses_store_mutation_and_still_closes_the_borrower() {
    let fixture = Fixture::new();
    let mut owner = owner();
    let lease = lease(&fixture);
    let mut host = borrowed(&fixture, &lease, &mut owner).ok();
    let lock = fixture.path.join("run/simplestchat-bench/workload.lock");
    fs::set_permissions(&lock, fs::Permissions::from_mode(0o644)).unwrap();
    let result = host.as_mut().map(|store| {
        store.publish(
            vec![(revision('a'), capture(&[("app-abcdef.js", b"old")]))],
            1,
            &mut owner,
        )
    });
    let finished = host.take().map(|host| host.finish(&mut owner));
    let released = release(lease);
    let cause = owner.error();
    let owner_finished = owner.finish();
    assert!(matches!(result, Some(Err(_))));
    assert!(matches!(finished, Some(Err(_))));
    assert!(released);
    assert!(!fixture.root().exists());
    assert_eq!(cause, Some(Cause::Changed));
    assert_eq!(owner_finished, Err(Cause::Changed));
}
