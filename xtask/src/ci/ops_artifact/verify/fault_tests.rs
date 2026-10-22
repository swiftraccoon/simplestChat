use super::super::namespace::{Budget, Chain};
use super::{fixture::*, *};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

#[test]
fn original_namespace_reopen_rejects_leaf_ancestor_replacement_and_authority_drift() {
    for change in 0..3 {
        let fixture = Fixture::new(0);
        let budget = Budget::default();
        let chain = Chain::open(&fixture.input, &budget).unwrap();
        let witness = chain.witness().unwrap();
        chain.finish().unwrap();
        match change {
            0 => {
                fs::rename(&fixture.input, fixture.root.join("old-input")).unwrap();
                fs::create_dir(&fixture.input).unwrap();
                fs::set_permissions(&fixture.input, fs::Permissions::from_mode(0o700)).unwrap();
            }
            1 => fs::set_permissions(&fixture.input, fs::Permissions::from_mode(0o777)).unwrap(),
            _ => {
                let moved = fixture.root.with_extension("replaced");
                fs::rename(&fixture.root, &moved).unwrap();
                fs::create_dir(&fixture.root).unwrap();
                assert!(witness.reopen(&budget).is_err());
                fs::remove_dir(&fixture.root).unwrap();
                fs::rename(moved, &fixture.root).unwrap();
            }
        }
        if change != 2 {
            assert!(witness.reopen(&budget).is_err());
        }
        assert_eq!(budget.count(), 0);
    }
}

#[test]
fn witness_ignores_sibling_timestamps_but_original_file_seal_rejects_same_length_changes() {
    let fixture = Fixture::new(0);
    let budget = Budget::default();
    let checkpoint = hash::Checkpoint::fixture();
    let (original, _) = files::original(&fixture.request, 16 * 1024, &budget, &checkpoint).unwrap();
    fs::write(fixture.root.join("sibling"), b"ordinary sibling").unwrap();
    original.read(&budget, &checkpoint).unwrap();
    let mut raw = fs::read(&fixture.request).unwrap();
    raw[0] ^= 1;
    fs::write(&fixture.request, raw).unwrap();
    assert!(original.read(&budget, &checkpoint).is_err());
    assert_eq!(budget.count(), 0);
}

#[tokio::test]
async fn changed_payload_or_staged_metadata_can_never_be_adopted_as_new_baseline() {
    for staged in [false, true] {
        let fixture = Fixture::new(1);
        let path = if staged {
            fixture.output.join("ops-preparation.json")
        } else {
            fixture.input.join("simplestchat-ops")
        };
        let mut fake = Fake::new(&fixture, Arc::new(Clock::new()));
        fake.mutate = Some(Box::new(move |index| {
            if index == 3 {
                let mut data = fs::read(&path).unwrap();
                let last = data.len() - 1;
                data[last] ^= 1;
                fs::write(&path, data).unwrap();
            }
        }));
        let context = plan::Context::new(fake.clock.clone());
        let budget = Budget::default();
        assert_eq!(
            execute::transaction(
                fixture.request.clone(),
                fixture.pin(),
                fixture.root.clone(),
                fixture.environment.clone(),
                budget.clone(),
                &context,
                &mut fake
            )
            .await
            .unwrap_err()
            .code,
            1
        );
        assert_eq!(budget.count(), 0);
        assert!(fake.replay.is_empty());
    }
}

#[tokio::test]
async fn extra_link_and_mode_refusals_keep_foreign_entries_and_unwind_descriptors() {
    for change in 0..4 {
        let fixture = Fixture::new(0);
        match change {
            0 => fs::write(fixture.input.join("foreign"), b"retain me").unwrap(),
            1 => {
                fs::remove_file(fixture.input.join(BUNDLE)).unwrap();
                symlink("ops-release.json", fixture.input.join(BUNDLE)).unwrap();
            }
            2 => fs::set_permissions(
                fixture.input.join(BUNDLE),
                fs::Permissions::from_mode(0o4600),
            )
            .unwrap(),
            _ => fs::hard_link(fixture.input.join(BUNDLE), fixture.root.join("alias")).unwrap(),
        }
        let mut fake = Fake::new(&fixture, Arc::new(Clock::new()));
        let budget = Budget::default();
        let context = plan::Context::new(fake.clock.clone());
        assert!(
            execute::transaction(
                fixture.request.clone(),
                fixture.pin(),
                fixture.root.clone(),
                fixture.environment.clone(),
                budget.clone(),
                &context,
                &mut fake
            )
            .await
            .is_err()
        );
        assert_eq!(budget.count(), 0);
        assert_eq!(fake.calls, 0);
        if change == 0 {
            assert!(fixture.input.join("foreign").exists());
        }
    }
}

struct JumpClock {
    start: Instant,
    reads: AtomicUsize,
}
impl plan::Clock for JumpClock {
    fn now(&self) -> Instant {
        self.start
            + Duration::from_secs(if self.reads.fetch_add(1, Ordering::SeqCst) == 0 {
                0
            } else {
                601
            })
    }
}
#[tokio::test]
async fn hash_admission_uses_one_current_sample_and_runs_nothing_after_entry() {
    let fixture = Fixture::new(0);
    let clock = Arc::new(JumpClock {
        start: Instant::now(),
        reads: AtomicUsize::new(0),
    });
    let context = plan::Context::new(clock);
    let mut executor = Fake::new(&fixture, Arc::new(Clock::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    assert_eq!(
        hash::work(&context, &mut executor, move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .await
        .unwrap_err()
        .code,
        124
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn enforced_descriptor_reservations_refuse_before_acquisition_and_release_all_tokens() {
    let budget = Budget::default();
    let held = budget.reserve(49).unwrap();
    assert!(budget.reserve_with_limit(16, 48).is_err());
    assert_eq!(budget.count(), 49);
    drop(held);
    let helper = budget.reserve_with_limit(64, 24).unwrap();
    assert_eq!(budget.count(), 64);
    assert!(budget.reserve_with_limit(64, 24).is_err());
    drop(helper);
    assert_eq!(budget.count(), 0);
}

#[tokio::test]
async fn second_capture_constructor_failure_awaits_the_first_pipe_and_output_owner() {
    let fixture = Fixture::new(0);
    let budget = Budget::default();
    let output = files::Output::new(&fixture.output, &budget).unwrap();
    let baseline = budget.count();
    let foreign = fixture.output.join("gh-0.stderr");
    fs::write(&foreign, b"retain this foreign file").unwrap();
    let mut pipes = Vec::new();
    let result = output
        .capture_observed(0, 4096, |log| {
            for fd in log.pipe_fds() {
                let path = std::path::PathBuf::from(format!("/dev/fd/{fd}"));
                pipes.push((path.clone(), fs::metadata(path).unwrap()));
            }
        })
        .await;
    assert!(result.is_err());
    assert_eq!(pipes.len(), 2);
    for (path, original) in pipes {
        assert!(
            !fs::metadata(path).is_ok_and(|now| super::super::namespace::binding(&original, &now))
        );
    }
    assert_eq!(budget.count(), baseline);
    assert_eq!(fs::read(foreign).unwrap(), b"retain this foreign file");
    output.finish().unwrap();
    assert_eq!(budget.count(), 0);
}

#[test]
fn original_leaf_identity_detects_physical_ancestor_overlap_on_every_filesystem() {
    let fixture = Fixture::new(0);
    let budget = Budget::default();
    let root = Chain::open(&fixture.root, &budget).unwrap();
    let input = Chain::open(&fixture.input, &budget).unwrap();
    let root_witness = root.witness().unwrap();
    let input_witness = input.witness().unwrap();
    assert!(root_witness.leaf_in(&input_witness));
    assert!(input_witness.leaf_in(&input_witness));
    assert!(!input_witness.leaf_in(&root_witness));
    let root_closed = root.finish();
    let input_closed = input.finish();
    root_closed.unwrap();
    input_closed.unwrap();
    assert_eq!(budget.count(), 0);
}

#[tokio::test]
async fn case_alias_inside_original_source_refuses_before_output_creation_or_dispatch() {
    use std::os::unix::fs::MetadataExt;
    let mut fixture = Fixture::new(0);
    let alias = fixture.root.join("INPUT");
    let Ok(alias_stamp) = fs::metadata(&alias) else {
        return;
    };
    let original = fs::metadata(&fixture.input).unwrap();
    if (alias_stamp.dev(), alias_stamp.ino()) != (original.dev(), original.ino()) {
        return;
    }
    let output = alias.join("forbidden-output");
    assert!(!output.starts_with(&fixture.input));
    fixture.wire["output"] = serde_json::json!(output);
    fixture.save_request();
    let mut fake = Fake::new(&fixture, Arc::new(Clock::new()));
    let context = plan::Context::new(fake.clock.clone());
    let budget = Budget::default();
    assert_eq!(
        execute::transaction(
            fixture.request.clone(),
            fixture.pin(),
            fixture.root.clone(),
            fixture.environment.clone(),
            budget.clone(),
            &context,
            &mut fake
        )
        .await
        .unwrap_err()
        .code,
        1
    );
    assert!(!output.exists());
    assert_eq!(fake.calls, 0);
    assert_eq!(budget.count(), 0);
}

#[test]
fn fresh_output_reopens_original_parent_before_mkdir_and_refuses_replacement() {
    let fixture = Fixture::new(0);
    let budget = Budget::default();
    let parent = Chain::open(&fixture.root, &budget).unwrap();
    let witness = parent.witness().unwrap();
    parent.finish().unwrap();
    let moved = fixture.root.with_extension("original-parent");
    fs::rename(&fixture.root, &moved).unwrap();
    fs::create_dir(&fixture.root).unwrap();
    fs::set_permissions(&fixture.root, fs::Permissions::from_mode(0o700)).unwrap();
    let admitted = files::Output::bound(&fixture.output, &budget, &witness);
    let created = fixture.output.exists();
    fs::remove_dir(&fixture.root).unwrap();
    fs::rename(moved, &fixture.root).unwrap();
    assert!(admitted.is_err());
    assert!(!created);
    assert_eq!(budget.count(), 0);
}
