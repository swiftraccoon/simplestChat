//! Concrete source-tree mutations cannot widen the first-party execution closure.

use super::super::namespace::{Budget, Chain};
use super::{
    execute,
    files::Output,
    fixture::{Clock, Fake, Fixture, expect_error},
    plan::Plan,
    sources::{NAMES, Sources},
    worker::Checkpoint,
};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    path::Path,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

async fn run(fixture: &Fixture, fake: &mut Fake, budget: &Budget) -> crate::tooling::Result<()> {
    execute::transaction(
        execute::Inputs {
            path: fixture.request.clone(),
            selected: fixture.pin(),
            caller: fixture.caller.clone(),
            environment: fixture.acquisition.environment.clone(),
            run_id: "1".repeat(32),
        },
        budget.clone(),
        &Plan::new(fake.clock()),
        fake,
    )
    .await
}

fn mutate(root: &Path, profile: usize) {
    let build = root.join("build");
    match profile {
        0 => fs::write(build.join("argparse.pyc"), b"unmeasured legacy bytecode").unwrap(),
        1 => fs::write(build.join("argparse.py"), b"unmeasured source").unwrap(),
        2 => {
            fs::create_dir(build.join("argparse")).unwrap();
            fs::write(build.join("argparse/__init__.py"), b"unmeasured package").unwrap();
        }
        3 => {
            let path = root.join("ops/ansible/files/json.pyc");
            fs::write(&path, b"unmeasured transient import").unwrap();
            fs::remove_file(path).unwrap();
        }
        4 => {
            let path = build.join("security_vm_ops.py");
            let body = fs::read(&path).unwrap();
            fs::remove_file(&path).unwrap();
            fs::write(&path, body).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        5 => {
            let path = root.join("ops/ansible/files/release_json.py");
            let body = fs::read(&path).unwrap();
            fs::write(
                path,
                [body, b"altered original projected bytes".to_vec()].concat(),
            )
            .unwrap();
        }
        6 => symlink("/dev/null", build.join("argparse.pyc")).unwrap(),
        7 => {
            let displaced = root.parent().unwrap().join("displaced-projected-build");
            fs::rename(&build, &displaced).unwrap();
            fs::create_dir(&build).unwrap();
            fs::set_permissions(&build, fs::Permissions::from_mode(0o700)).unwrap();
            for name in fs::read_dir(displaced).unwrap() {
                let name = name.unwrap();
                fs::rename(name.path(), build.join(name.file_name())).unwrap();
            }
        }
        _ => {
            let path = root.parent().unwrap().join("python-cache/transient.pyc");
            fs::write(&path, b"transient bytecode cache").unwrap();
            fs::remove_file(path).unwrap();
        }
    }
}

fn settled(budget: &Budget) {
    assert_eq!(budget.count(), 0);
    let (attempted, fallback) = budget.close_counts();
    assert!(attempted > 0);
    assert_eq!(fallback, 0);
}

#[tokio::test]
async fn ignored_caller_imports_are_excluded_from_the_whole_execution_projection() {
    let fixture = Fixture::new();
    for profile in 0..3 {
        mutate(&fixture.caller, profile);
    }
    let mut fake = Fake::new(&fixture);
    let budget = Budget::default();
    run(&fixture, &mut fake, &budget).await.unwrap();
    let source = fixture.output.join("source");
    assert_eq!(
        fake.runtime_directories,
        [
            fixture.caller.clone(),
            fixture.caller.clone(),
            fixture.caller.clone(),
            source.clone(),
        ]
    );
    assert_eq!(
        fake.keeper_arguments[4],
        source.join("build/security_vm_ops.py")
    );
    assert!(!source.join("build/argparse.pyc").exists());
    assert!(!source.join("build/argparse.py").exists());
    assert!(!source.join("build/argparse").exists());
    for name in NAMES {
        let original = fixture.caller.join(name);
        let projected = source.join(name);
        assert_eq!(fs::read(&original).unwrap(), fs::read(&projected).unwrap());
        let stamp = fs::symlink_metadata(&projected).unwrap();
        assert_eq!(stamp.mode() & 0o7777, 0o600);
        assert_eq!(stamp.nlink(), 1);
        assert_ne!((stamp.dev(), stamp.ino()), {
            let original = fs::metadata(original).unwrap();
            (original.dev(), original.ino())
        });
    }
    assert_eq!(fake.keeper_calls, 1);
    settled(&budget);
}

#[tokio::test]
async fn projected_import_mutations_refuse_before_dispatch_after_dispatch_and_after_replay() {
    for timing in 0..3 {
        for profile in 0..9 {
            let fixture = Fixture::new();
            let mut fake = Fake::new(&fixture);
            let budget = Budget::default();
            let root = fixture.output.join("source");
            if timing == 2 {
                fake.acquire.replay_mutate = Some(Box::new(move || mutate(&root, profile)));
            } else {
                let at = if timing == 0 { 2 } else { 3 };
                fake.mutate = Some(Box::new(move |index| {
                    if index == at {
                        mutate(&root, profile);
                    }
                }));
            }
            expect_error(run(&fixture, &mut fake, &budget).await, 1);
            assert_eq!(fake.keeper_calls, usize::from(timing != 0));
            assert_eq!(fixture.output.join("summary.json").exists(), timing == 2);
            assert_eq!(fake.acquire.replays.len(), usize::from(timing == 2));
            settled(&budget);
        }
    }
}

#[tokio::test]
async fn failed_keeper_retains_its_status_after_projected_import_aftereffects() {
    let fixture = Fixture::new();
    let mut fake = Fake::new(&fixture);
    let root = fixture.output.join("source");
    fake.mutate = Some(Box::new(move |index| {
        if index == 3 {
            mutate(&root, 0);
        }
    }));
    fake.runtime_fail = Some((3, 7));
    let budget = Budget::default();
    expect_error(run(&fixture, &mut fake, &budget).await, 7);
    assert_eq!(fake.keeper_calls, 1);
    assert!(!fixture.output.join("summary.json").exists());
    assert!(fake.acquire.replays.is_empty());
    settled(&budget);
}

#[test]
fn projection_is_exclusive_and_its_exact_layout_is_derived_from_the_original_names() {
    let fixture = Fixture::new();
    let budget = Budget::default();
    let clock = Arc::new(Clock::new());
    let start = *clock.0.lock().unwrap();
    let checkpoint = Checkpoint {
        clock,
        start,
        end: start + Duration::from_secs(150),
        abort: Arc::new(AtomicBool::new(false)),
    };
    let output = Output::new(&fixture.output, &fixture.caller, &budget, &checkpoint).unwrap();
    let sources = Sources::open(&fixture.caller, &output, &budget, &checkpoint).unwrap();
    let root = sources.directory();
    let mut actual = Vec::new();
    let mut pending = vec![root.to_owned()];
    let mut directories = 0;
    while let Some(directory) = pending.pop() {
        directories += 1;
        assert_eq!(fs::metadata(&directory).unwrap().mode() & 0o7777, 0o700);
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                pending.push(entry.path());
            } else {
                actual.push(
                    entry
                        .path()
                        .strip_prefix(root)
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .to_owned(),
                );
            }
        }
    }
    actual.sort();
    let mut expected = NAMES.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    expected.sort();
    assert_eq!(actual, expected);
    assert_eq!(directories, 7);
    let chain = Chain::open(root, &budget).unwrap();
    chain.finish().unwrap();
    expect_error(
        Sources::open(&fixture.caller, &output, &budget, &checkpoint).map(|_| ()),
        1,
    );
    sources.check(&checkpoint).unwrap();
    settled(&budget);
}
