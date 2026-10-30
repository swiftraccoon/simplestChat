//! Real private ZIP copies and owned cancellation; signatures remain inert.
use super::*;
use crate::ci::ops_artifact::{
    acquire::{fixture::*, request},
    namespace::Budget,
};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::PathBuf,
    sync::{Condvar, Mutex, atomic::AtomicU8},
};

fn retained(fixture: &mut Fixture) -> PathBuf {
    let path = fixture.root.join("retained.zip");
    fs::write(&path, &fixture.outputs[4]).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    fixture.wire["artifactZip"] = json!(path);
    fs::write(&fixture.request, bytes(&fixture.wire)).unwrap();
    path
}
fn fake(fixture: &Fixture) -> Fake {
    let mut fake = Fake::new(fixture);
    fake.outputs.remove(4);
    fake
}
async fn run(fixture: &Fixture, fake: &mut Fake) -> Result<()> {
    let budget = Budget::default();
    let context = Context::new(fake.clock.clone());
    let result = super::super::execute::transaction(
        fixture.request.clone(),
        fixture.pin(),
        fixture.root.clone(),
        fixture.environment.clone(),
        budget.clone(),
        &context,
        fake,
    )
    .await;
    assert_eq!(budget.count(), 0);
    assert_eq!(budget.close_counts().1, 0, "no fallback descriptor closes");
    result
}

#[tokio::test]
async fn cached_zip_requires_fresh_metadata_and_the_complete_retained_verifier() {
    for count in [0, 16] {
        let mut fixture = Fixture::new(count);
        let path = retained(&mut fixture);
        let mut fake = fake(&fixture);
        run(&fixture, &mut fake).await.unwrap();
        assert_eq!(
            fake.calls, 8,
            "four acquisition and four retained verifier roles"
        );
        assert_eq!(fake.replays.len(), 1);
        assert_eq!(
            fs::read(path).unwrap(),
            fs::read(fixture.output.join("artifact.zip")).unwrap()
        );
        assert!(!fixture.output.join("gh-4.stderr").exists());
        let observed: Value = serde_json::from_slice(&fake.replays[0]).unwrap();
        assert_eq!(observed["retainedFiles"], count + 6);
        assert_eq!(observed["artifact"]["artifactId"], 456);
        assert!(fixture.output.join("verified/simplestchat-ops").is_file());
    }
}

#[test]
fn cached_zip_is_explicit_typed_private_data_not_a_saved_authority_marker() {
    let mut fixture = Fixture::new(0);
    retained(&mut fixture);
    let encoded = bytes(&fixture.wire);
    assert!(request::Request::decode(&encoded, &super::super::super::sha(&encoded)).is_ok());
    for value in [
        Value::Null,
        json!(true),
        json!(42),
        json!({"path":"/tmp/x"}),
        json!("relative.zip"),
        json!("/tmp/../zip"),
        json!("/tmp/unsafe\u{0085}zip"),
        json!(fixture.output.join("artifact.zip")),
        json!(fixture.root),
    ] {
        let mut wire = fixture.wire.clone();
        wire["artifactZip"] = value;
        let encoded = bytes(&wire);
        assert!(request::Request::decode(&encoded, &super::super::super::sha(&encoded)).is_err());
    }
}

#[tokio::test]
async fn stale_api_and_changed_zip_never_reach_archive_parsing_or_signatures() {
    for (index, pointer, value) in [
        (2, "/conclusion", json!("failure")),
        (2, "/run_attempt", json!(3)),
        (3, "/expired", json!(true)),
        (3, "/digest", json!(format!("sha256:{}", "0".repeat(64)))),
        (3, "/size_in_bytes", json!(1)),
    ] {
        let mut fixture = Fixture::new(0);
        retained(&mut fixture);
        let mut fake = fake(&fixture);
        let mut observed: Value = serde_json::from_slice(&fake.outputs[index]).unwrap();
        *observed.pointer_mut(pointer).unwrap() = value;
        fake.outputs[index] = bytes(&observed);
        assert!(run(&fixture, &mut fake).await.is_err());
        assert!(fake.calls <= 4);
        assert!(!fixture.output.join("verified").exists());
        assert_eq!(
            fs::read_dir(fixture.output.join("input")).unwrap().count(),
            0
        );
        assert!(fake.replays.is_empty());
    }
    let mut fixture = Fixture::new(0);
    let path = retained(&mut fixture);
    let mut data = fs::read(&path).unwrap();
    data[0] ^= 1;
    fs::write(path, data).unwrap();
    let mut fake = fake(&fixture);
    assert!(run(&fixture, &mut fake).await.is_err());
    assert_eq!(fake.calls, 4);
    assert_eq!(
        fs::read_dir(fixture.output.join("input")).unwrap().count(),
        0
    );
}

#[tokio::test]
async fn unsafe_source_kinds_modes_links_and_large_files_are_refused() {
    for case in 0..5 {
        let mut fixture = Fixture::new(0);
        let path = retained(&mut fixture);
        match case {
            0 => fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap(),
            1 => {
                fs::rename(&path, fixture.root.join("original.zip")).unwrap();
                symlink(fixture.root.join("original.zip"), &path).unwrap();
            }
            2 => fs::hard_link(&path, fixture.root.join("alias.zip")).unwrap(),
            3 => {
                fs::remove_file(&path).unwrap();
                fs::create_dir(&path).unwrap();
            }
            _ => fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .unwrap()
                .set_len(super::super::ZIP + 1)
                .unwrap(),
        }
        let mut fake = fake(&fixture);
        assert!(run(&fixture, &mut fake).await.is_err());
        assert_eq!(fake.calls, 0);
        assert!(!fixture.output.join("artifact.zip").exists());
    }
}

#[test]
fn original_inode_mode_bytes_and_explicit_source_close_remain_authoritative() {
    for case in 0..3 {
        let mut fixture = Fixture::new(0);
        let path = retained(&mut fixture);
        let budget = Budget::default();
        let checkpoint = Checkpoint::fixture();
        let source = Source::open(&path, &budget, &checkpoint).unwrap();
        match case {
            0 => {
                fs::rename(&path, fixture.root.join("original.zip")).unwrap();
                fs::write(&path, &fixture.outputs[4]).unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            }
            1 => fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap(),
            _ => {
                let mut data = fs::read(&path).unwrap();
                data[0] ^= 1;
                fs::write(&path, data).unwrap();
            }
        }
        let checked = source.check(&checkpoint);
        let closed = source.finish();
        assert!(checked.is_err() && closed.is_ok());
        assert_eq!(budget.count(), 0);
        assert_eq!(budget.close_counts().1, 0);
    }
}

#[test]
fn completed_matching_copy_cannot_outvote_a_late_source_close_failure() {
    let mut fixture = Fixture::new(0);
    let path = retained(&mut fixture);
    let budget = Budget::default();
    let checkpoint = Checkpoint::fixture();
    let mut source = Source::open(&path, &budget, &checkpoint).unwrap();
    source.refuse_close = true;
    let mut output = Output::new(&fixture.output, &budget).unwrap();
    assert!(
        copy(
            source,
            &mut output,
            &super::super::super::sha(&fixture.outputs[4]),
            fixture.outputs[4].len() as u64,
            &checkpoint
        )
        .is_err()
    );
    assert_eq!(
        fs::read(output.path("artifact.zip")).unwrap(),
        fixture.outputs[4]
    );
    output.finish().unwrap();
    assert_eq!(budget.count(), 0);
    assert_eq!(budget.close_counts().1, 0);
}

#[tokio::test]
async fn late_verifier_failure_and_final_replay_signal_preserve_failure() {
    for code in [1, 130, 143] {
        let mut fixture = Fixture::new(0);
        retained(&mut fixture);
        let mut fake = fake(&fixture);
        if code == 1 {
            fake.fail = Some((7, 1));
        } else {
            fake.replay_cancel = code;
        }
        assert_eq!(run(&fixture, &mut fake).await.unwrap_err().code, code);
        assert_eq!(fake.calls, 8);
    }
}

#[tokio::test]
async fn retained_transfer_signals_and_deadline_join_original_source_before_return() {
    for code in [124, 130, 143] {
        let mut fixture = Fixture::new(0);
        let path = retained(&mut fixture);
        let mut fake = fake(&fixture);
        let budget = Budget::default();
        let context = Context::new(fake.clock.clone());
        for index in 0..4 {
            context.command(index).unwrap();
            context.complete_command(index).unwrap();
        }
        let cancellation: Arc<AtomicU8> = fake.cancelled.clone();
        let clock = fake.clock.clone();
        let worker_budget = budget.clone();
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let worker_gate = gate.clone();
        let closed = Arc::new(AtomicBool::new(false));
        let worker_closed = closed.clone();
        let (started, ready) = tokio::sync::oneshot::channel();
        let mut operation = Box::pin(work(&context, &mut fake, move |checkpoint| {
            let source = Source::open(&path, &worker_budget, &checkpoint)?;
            let _ = started.send(());
            let (lock, condition) = &*worker_gate;
            let mut released = lock.lock().unwrap();
            while !*released {
                released = condition.wait(released).unwrap();
            }
            let checked = source.check(&checkpoint);
            let finished = source.finish();
            worker_closed.store(true, Ordering::SeqCst);
            checked.and(finished)
        }));
        tokio::time::timeout(Duration::from_secs(2),async {
            tokio::select! { result=ready=>result.unwrap(), _=&mut operation=>panic!("worker completed early") }
        }).await.unwrap();
        if code == 124 {
            clock.advance(Duration::from_secs(181));
        } else {
            cancellation.store(code, Ordering::SeqCst);
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(35), &mut operation)
                .await
                .is_err()
        );
        assert!(!closed.load(Ordering::SeqCst));
        let (lock, condition) = &*gate;
        *lock.lock().unwrap() = true;
        condition.notify_all();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), operation)
                .await
                .unwrap()
                .unwrap_err()
                .code,
            code
        );
        assert!(closed.load(Ordering::SeqCst));
        assert_eq!(budget.count(), 0);
        assert_eq!(budget.close_counts().1, 0);
    }
}
