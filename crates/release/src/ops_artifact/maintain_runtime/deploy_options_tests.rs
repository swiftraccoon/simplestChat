use super::*;
fn arguments() -> Vec<String> {
    [
        "--run-id",
        "1",
        "--run-attempt",
        "2",
        "--artifact-id",
        "3",
        "--server-artifact-id",
        "4",
        "--previous-revision",
        &"b".repeat(40),
        "--previous-run-id",
        "5",
        "--previous-run-attempt",
        "6",
        "--previous-artifact-id",
        "7",
        "--output",
        "/tmp/deploy-fixture",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}
#[test]
fn explicit_selections_keep_old_and_current_attempts_separate() {
    let parsed = DeployOptions::parse_for(&arguments(), &"a".repeat(40)).unwrap();
    assert_eq!(
        (
            parsed.common.run,
            parsed.common.attempt,
            parsed.current_server
        ),
        (1, 2, 4)
    );
    assert_eq!(
        (
            parsed.previous.run,
            parsed.previous.attempt,
            parsed.previous.artifact
        ),
        (5, 6, 7)
    );
    assert_eq!(parsed.previous_options().revision, "b".repeat(40));
    assert_eq!(parsed.common.revision, "a".repeat(40));
    assert_eq!(parsed.common.scenario, SCENARIO);
}
#[test]
fn malformed_unknown_replayed_and_unbounded_selectors_refuse() {
    let original = arguments();
    for at in [1, 3, 5, 7, 11, 13, 15] {
        for invalid in [
            "0",
            "-1",
            "1.0",
            "true",
            "9007199254740992",
            "18446744073709551616",
        ] {
            let mut changed = original.clone();
            changed[at] = invalid.into();
            assert!(DeployOptions::parse_for(&changed, &"a".repeat(40)).is_err());
        }
    }
    for (at, value) in [
        (8, "--unknown".into()),
        (9, "a".repeat(40)),
        (9, "B".repeat(40)),
        (17, "relative/path".into()),
    ] {
        let mut changed = original.clone();
        changed[at] = value;
        assert!(DeployOptions::parse_for(&changed, &"a".repeat(40)).is_err());
    }
    let mut duplicate = original.clone();
    duplicate.extend(["--run-id".into(), "1".into()]);
    assert!(DeployOptions::parse_for(&duplicate, &"a".repeat(40)).is_err());
    assert!(DeployOptions::parse_for(&original[..17], &"a".repeat(40)).is_err());
    assert!(DeployOptions::parse_for(&original, &"A".repeat(40)).is_err());
}
