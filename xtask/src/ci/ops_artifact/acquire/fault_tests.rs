use super::super::namespace::{self, Budget};
use super::{fixture::*, *};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

#[tokio::test]
async fn second_sink_refusal_awaits_first_pipe_and_file_without_removing_foreign_entry() {
    let fixture = Fixture::new(0);
    let budget = Budget::default();
    let output = files::Output::new(&fixture.output, &budget).unwrap();
    let baseline = budget.count();
    let foreign = fixture.output.join("gh-0.stderr");
    fs::write(&foreign, b"retain foreign bytes").unwrap();
    let mut pipes = Vec::new();
    let result = output
        .capture_observed(0, 4096, |log| {
            for descriptor in log.pipe_fds() {
                let path = std::path::PathBuf::from(format!("/dev/fd/{descriptor}"));
                pipes.push((path.clone(), fs::metadata(path).unwrap()));
            }
        })
        .await;
    assert!(result.is_err());
    assert_eq!(pipes.len(), 2);
    for (path, original) in pipes {
        assert!(!fs::metadata(path).is_ok_and(|stamp| namespace::binding(&original, &stamp)));
    }
    assert_eq!(fs::read(foreign).unwrap(), b"retain foreign bytes");
    assert_eq!(budget.count(), baseline);
    output.finish().unwrap();
    assert_eq!(budget.count(), 0);
}

#[test]
fn original_file_reopen_refuses_changed_bytes_and_mode_without_freezing_siblings() {
    let fixture = Fixture::new(0);
    let budget = Budget::default();
    let checkpoint = hash::Checkpoint::fixture();
    let (original, _) = files::original(&fixture.request, &budget, &checkpoint).unwrap();
    fs::write(fixture.root.join("new-sibling"), b"permitted sibling").unwrap();
    original.read(&budget, &checkpoint).unwrap();
    let mut data = fs::read(&fixture.request).unwrap();
    data[0] ^= 1;
    fs::write(&fixture.request, data).unwrap();
    assert!(original.read(&budget, &checkpoint).is_err());
    assert_eq!(budget.count(), 0);
}

#[test]
fn closed_output_witness_never_adopts_replacement_namespace_or_file_authority() {
    for directory in [false, true] {
        let fixture = Fixture::new(0);
        let budget = Budget::default();
        let checkpoint = hash::Checkpoint::fixture();
        let mut output = files::Output::new(&fixture.output, &budget).unwrap();
        output.write("retained.json", b"{}", &checkpoint).unwrap();
        let witness = output.finish().unwrap();
        witness.check(&budget, &checkpoint, false).unwrap();
        if directory {
            fs::rename(&fixture.output, fixture.root.join("original-output")).unwrap();
            fs::create_dir(&fixture.output).unwrap();
            fs::set_permissions(&fixture.output, fs::Permissions::from_mode(0o700)).unwrap();
        } else {
            fs::set_permissions(
                fixture.output.join("retained.json"),
                fs::Permissions::from_mode(0o4600),
            )
            .unwrap();
        }
        assert!(witness.check(&budget, &checkpoint, false).is_err());
        assert!(fixture.output.exists());
        assert_eq!(budget.count(), 0);
    }
}

#[test]
fn descriptor_admission_refuses_before_output_creation_and_releases_every_reservation() {
    let fixture = Fixture::new(0);
    let budget = Budget::default();
    let occupied = budget.reserve(128).unwrap();
    assert!(files::Output::new(&fixture.output, &budget).is_err());
    assert!(!fixture.output.exists());
    drop(occupied);
    let helper = budget.reserve_with_limit(64, 24).unwrap();
    assert!(budget.reserve_with_limit(64, 24).is_err());
    drop(helper);
    assert_eq!(budget.count(), 0);
}

#[tokio::test]
async fn cancellation_and_deadline_keep_the_same_file_worker_joined_until_checked_close() {
    for code in [130, 143, 124] {
        let fixture = Fixture::new(0);
        let mut fake = Fake::new(&fixture);
        let budget = Budget::default();
        let context = plan::Context::new(fake.clock.clone());
        let cancelled = fake.cancelled.clone();
        let clock = fake.clock.clone();
        let worker_budget = budget.clone();
        let path = fixture.root.join("worker-owned");
        fs::write(&path, b"owned worker bytes").unwrap();
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let worker_gate = gate.clone();
        let closed = Arc::new(AtomicBool::new(false));
        let worker_closed = closed.clone();
        let (started, ready) = tokio::sync::oneshot::channel();
        let mut operation = Box::pin(hash::work(&context, &mut fake, move |checkpoint| {
            let permit = worker_budget.permit()?;
            let file = std::fs::File::open(path).map_err(|_| failure(1))?;
            let _ = started.send(());
            let (lock, condition) = &*worker_gate;
            let mut released = lock.lock().unwrap();
            while !*released {
                released = condition.wait(released).unwrap();
            }
            let checked = checkpoint.check();
            let closed = nix::unistd::close(file).map_err(|_| failure(1));
            drop(permit);
            worker_closed.store(true, Ordering::SeqCst);
            checked.and(closed)
        }));
        let started = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::select! {
                result = ready => result.unwrap(),
                _ = &mut operation => panic!("file worker returned before release"),
            }
        })
        .await;
        let pending = started.is_ok();
        if code == 124 {
            clock.advance(Duration::from_secs(181));
        } else {
            cancelled.store(code, Ordering::SeqCst);
        }
        let still_owned = tokio::time::timeout(Duration::from_millis(35), &mut operation)
            .await
            .is_err();
        let not_closed_early = !closed.load(Ordering::SeqCst);
        // Release the owned gate before asserting, including setup failure paths.
        let (lock, condition) = &*gate;
        *lock.lock().unwrap() = true;
        condition.notify_all();
        let result = tokio::time::timeout(Duration::from_secs(2), &mut operation).await;
        assert!(pending);
        assert!(still_owned && not_closed_early);
        assert_eq!(result.unwrap().unwrap_err().code, code);
        assert!(closed.load(Ordering::SeqCst));
        assert_eq!(budget.count(), 0);
    }
}
