//! Complete-tree copy boundaries using owned inert source trees.

use super::{fixture::*, materials, namespace::Budget, *};
use std::fs;

#[tokio::test]
async fn bounded_copy_preserves_tree_frame_and_refuses_reuse() {
    let fixture = Fixture::new();
    let budget = Budget::default();
    let clock = NativeClock;
    let deadline = Deadline::new(&clock);
    let mut executor = Noop;
    let original = materials::measure(&fixture.source, &budget, &mut executor, &deadline)
        .await
        .unwrap();
    let copied = materials::copy_bounded(
        &fixture.source,
        &fixture.output,
        &original.sha256,
        original.bytes,
        &budget,
        &mut executor,
        &deadline,
    )
    .await
    .unwrap();
    assert_eq!(copied.sha256, original.sha256);
    assert_eq!(copied.files, original.files);
    assert_eq!(copied.bytes, original.bytes);
    let path = fixture.output.join("ops/src/main.rs");
    let bytes = fs::read(&path).unwrap();
    assert!(
        materials::copy_bounded(
            &fixture.source,
            &fixture.output,
            &original.sha256,
            original.bytes,
            &budget,
            &mut executor,
            &deadline,
        )
        .await
        .is_err()
    );
    assert_eq!(fs::read(path).unwrap(), bytes);
    assert_eq!(budget.count(), 0);
}

#[tokio::test]
async fn wrong_frame_or_byte_cap_refuses_before_creating_destination() {
    let fixture = Fixture::new();
    let budget = Budget::default();
    let clock = NativeClock;
    let deadline = Deadline::new(&clock);
    let mut executor = Noop;
    let original = materials::measure(&fixture.source, &budget, &mut executor, &deadline)
        .await
        .unwrap();
    for (selected, maximum) in [
        ("0".repeat(64), original.bytes),
        (original.sha256, original.bytes - 1),
    ] {
        assert!(
            materials::copy_bounded(
                &fixture.source,
                &fixture.output,
                &selected,
                maximum,
                &budget,
                &mut executor,
                &deadline,
            )
            .await
            .is_err()
        );
        assert!(!fixture.output.exists());
        assert_eq!(budget.count(), 0);
    }
}
