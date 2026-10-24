//! Actual retained-file metadata transitions, isolated to this fixture's inode.
use super::*;
use crate::test_support::Fixture;
use std::time::{Duration, Instant};

#[test]
fn seal_rejects_same_inode_mode_change_without_adopting_it_or_replacing_first_cause() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let fixture = Fixture::new();
    let mut facts = Vec::new();
    for (name, close_failure) in [("ordinary", None), ("paired-close", Some(1))] {
        let fact = runtime.block_on(async {
            let mut owner = Owner::new(
                Instant::now(),
                Duration::from_secs(30),
                Duration::from_secs(10),
            )
            .unwrap();
            let dir = Dir::open(&fixture.0, &mut owner).unwrap();
            let expected = fixture.0.components().count() + 1;
            let mut file = Retained::create(dir.clone(), name, &mut owner).unwrap();
            seal_mode_fault(name.into());
            let written = file.write(b"same original inode bytes", &mut owner);
            let observed = file.file().metadata();
            // Inject the independent close refusal AFTER the concrete metadata
            // observation. It may not replace Changed; the actual close runs.
            start_probe(close_failure);
            file.finish(&mut owner);
            dir.finish(&mut owner);
            (written, observed, owner.error(), take_probe(), expected)
        });
        facts.push(fact);
    }
    drop(runtime);
    drop(fixture);
    for (written, observed, primary, closes, expected) in facts {
        assert_eq!(written, Err(Cause::Changed));
        assert_eq!(primary, Some(Cause::Changed));
        assert_eq!(observed.unwrap().mode() & 0o7777, 0o644);
        assert_eq!(closes, (expected, 0));
    }
}

#[test]
fn sealing_accepts_only_the_intended_length_and_timestamp_transition() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let fixture = Fixture::new();
    let facts = runtime.block_on(async {
        let mut owner = Owner::new(
            Instant::now(),
            Duration::from_secs(30),
            Duration::from_secs(10),
        )
        .unwrap();
        let dir = Dir::open(&fixture.0, &mut owner).unwrap();
        let mut file = Retained::create(dir.clone(), "positive", &mut owner).unwrap();
        let before = file.file().metadata();
        let written = file.write(b"new bounded bytes", &mut owner);
        let after = file.file().metadata();
        let digest = if written.is_ok() {
            file.hash(&mut owner)
        } else {
            Err(Cause::Changed)
        };
        file.finish(&mut owner);
        dir.finish(&mut owner);
        (before, written, after, digest, owner.error())
    });
    drop(runtime);
    drop(fixture);
    let before = facts.0.unwrap();
    let after = facts.2.unwrap();
    assert_eq!(facts.1, Ok(()));
    assert_eq!(facts.4, None);
    assert!(binding(&before, &after));
    assert_eq!(before.nlink(), after.nlink());
    assert_eq!(after.len(), 17);
    assert_eq!(facts.3.unwrap(), hex(&Sha256::digest(b"new bounded bytes")));
}
