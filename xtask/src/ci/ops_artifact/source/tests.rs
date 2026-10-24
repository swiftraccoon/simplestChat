//! Pure request and raw-catalogue boundary regressions.

use super::*;
use serde_json::{Value, json};

pub(super) fn wire() -> Value {
    json!({"schemaVersion":1,"selection":{"repository":"owner/repo",
        "revision":"c".repeat(40),"packageVersion":"0.1.0","protocolVersion":1,
        "target":"x86_64-unknown-linux-musl","buildRunId":1,"runAttempt":1},
        "repository":"/materials/repository","git":{"path":"/tools/git",
        "sha256":"a".repeat(64),"bytes":42},"output":"/output/fresh"})
}
fn decode(value: &Value) -> Result<Request> {
    let bytes = serde_json::to_vec(value).unwrap();
    Request::decode(&bytes, &sha(&bytes))
}
#[test]
fn request_original_bytes_strict_types_scope_and_private_debug() {
    let value = wire();
    let selected = decode(&value).unwrap();
    assert_eq!(selected.revision, "c".repeat(40));
    assert!(!format!("{selected:?}").contains("materials"));
    let pretty = serde_json::to_vec_pretty(&value).unwrap();
    assert!(Request::decode(&pretty, &selected.sha256).is_err());
    for (field, invalid) in [
        ("schemaVersion", json!(1.0)),
        ("selection", json!({})),
        ("PRIVATE_KEY", json!("PRIVATE_SECRET")),
        ("output", json!("/materials/repository/run")),
        ("output", json!("/materials")),
        ("output", json!("/tools")),
        ("repository", json!("/materials/../repo")),
        ("git", json!({})),
    ] {
        let mut value = wire();
        value[field] = invalid;
        assert!(decode(&value).is_err(), "{field}");
    }
    for invalid in [
        json!(true),
        json!(1.0),
        json!(0),
        json!(-1),
        json!(67108865u64),
    ] {
        let mut value = wire();
        value["git"]["bytes"] = invalid;
        assert!(decode(&value).is_err());
    }
    let original = serde_json::to_string(&wire()).unwrap();
    for invalid in [
        original.replacen('{', "{\"schemaVersion\":1,", 1),
        format!("{original}{}", " ".repeat(65536)),
    ] {
        assert!(Request::decode(invalid.as_bytes(), &sha(invalid.as_bytes())).is_err());
    }
}
fn row(mode: &str, kind: &str, size: &str, path: &str) -> Vec<u8> {
    format!("{mode} {kind} {} {size}\t{path}\0", "a".repeat(40)).into_bytes()
}
#[test]
fn catalogue_preserves_directories_empty_trees_modes_and_original_names() {
    let bytes = [
        row("040000", "tree", "-", "empty"),
        row("040000", "tree", "-", "nested"),
        row("100644", "blob", "0", "nested/space name"),
        row("100755", "blob", "1", "run"),
    ]
    .concat();
    let parsed = catalog::parse(&bytes).unwrap();
    assert!(parsed["empty"].directory);
    assert!(parsed["run"].executable);
    assert_eq!(parsed["nested/space name"].bytes, 0);
    assert!(catalog::parse(&[]).unwrap().is_empty());
}
#[test]
fn catalogue_refuses_links_submodules_unsafe_names_missing_parents_and_conflicts() {
    let mut cases = vec![
        row("120000", "blob", "1", "link"),
        row("160000", "commit", "-", "module"),
        row("100644", "blob", "1", "/absolute"),
        row("100644", "blob", "1", "../escape"),
        row("100644", "blob", "1", ".git/config"),
        row("100644", "blob", "1", "a//b"),
        row("100644", "blob", "1", "missing/child"),
        row("100644", "blob", "16777217", "big"),
        row("100644", "blob", "-1", "bad"),
    ];
    cases.push(
        [
            row("100644", "blob", "1", "same"),
            row("100644", "blob", "1", "same"),
        ]
        .concat(),
    );
    cases.push(
        [
            row("100644", "blob", "1", "a"),
            row("100644", "blob", "1", "a/b"),
        ]
        .concat(),
    );
    let mut truncated = row("100644", "blob", "1", "a");
    truncated.pop();
    cases.push(truncated);
    for value in cases {
        assert!(catalog::parse(&value).is_err());
    }
}
#[test]
fn catalogue_caps_actual_records_total_bytes_names_and_exact_ids() {
    let many = (0..=catalog::ENTRIES)
        .flat_map(|i| row("100644", "blob", "0", &format!("f{i}")))
        .collect::<Vec<_>>();
    assert!(catalog::parse(&many).is_err());
    let bytes = (0..17)
        .flat_map(|i| row("100644", "blob", "16777216", &format!("f{i}")))
        .collect::<Vec<_>>();
    assert!(catalog::parse(&bytes).is_err());
    assert!(catalog::parse(&vec![b' '; catalog::CATALOG as usize + 1]).is_err());
    for value in [
        "a".repeat(39),
        "A".repeat(40),
        format!("{}\n\n", "a".repeat(40)),
    ] {
        assert!(catalog::oid(value.as_bytes()).is_err());
    }
}

#[tokio::test]
async fn selected_git_hash_or_pending_refusal_checked_closes_original_file_and_parents() {
    struct Pending(Option<u8>);
    impl Executor for Pending {
        async fn run(&mut self, _: crate::ci::command::Command) -> Result<Vec<u8>> {
            panic!("selected Git admission cannot execute a command")
        }
        async fn ready(&mut self) -> Result<()> {
            match self.0 {
                Some(code) => Err(failure(code)),
                None => Ok(()),
            }
        }
    }
    for pending in [None, Some(124), Some(130), Some(143)] {
        let fixture = super::super::fixture::Fixture::new();
        let path = fixture.kit.join("host-tools/bin/cc");
        let original = std::fs::read(&path).unwrap();
        let selected = request::GitSelection {
            path: path.clone(),
            sha256: if pending.is_some() {
                sha(&original)
            } else {
                "0".repeat(64)
            },
            bytes: original.len() as u64,
        };
        let budget = Budget::default();
        let clock = NativeClock;
        let deadline = Deadline::new(&clock);
        let result = Git::open(&selected, &budget, &mut Pending(pending), &deadline).await;
        assert_eq!(result.err().unwrap().code, pending.unwrap_or(1));
        assert_eq!(budget.count(), 0);
        let (checked, fallback) = budget.close_counts();
        assert!(checked > 0);
        assert_eq!(fallback, 0);
        assert_eq!(std::fs::read(path).unwrap(), original);
    }
}

#[test]
fn incomplete_or_redirected_repository_checked_closes_both_original_chains() {
    for redirected in [false, true] {
        let fixture = super::super::fixture::Fixture::new();
        if redirected {
            std::fs::create_dir_all(fixture.source.join(".git/objects")).unwrap();
            std::fs::write(
                fixture.source.join(".git/commondir"),
                b"retain this redirect",
            )
            .unwrap();
        }
        let budget = Budget::default();
        assert_eq!(
            Repository::open(&fixture.source, &budget)
                .err()
                .unwrap()
                .code,
            1
        );
        assert_eq!(budget.count(), 0);
        let (checked, fallback) = budget.close_counts();
        assert!(checked > 0);
        assert_eq!(fallback, 0);
        assert!(fixture.source.join("Cargo.toml").is_file());
        if redirected {
            assert_eq!(
                std::fs::read(fixture.source.join(".git/commondir")).unwrap(),
                b"retain this redirect"
            );
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn selected_git_multi_chunk_hash_has_a_small_unboxed_yielding_future() {
    struct Yielding(usize);
    impl Executor for Yielding {
        async fn run(&mut self, _: crate::ci::command::Command) -> Result<Vec<u8>> {
            panic!("selected Git measurement must not execute a command");
        }
        async fn ready(&mut self) -> Result<()> {
            self.0 += 1;
            tokio::task::yield_now().await;
            Ok(())
        }
    }

    let fixture = super::super::fixture::Fixture::new();
    let path = fixture.kit.join("host-tools/bin/cc");
    let original = vec![b'G'; 2 * 65536 + 37];
    std::fs::write(&path, &original).unwrap();
    let selected = request::GitSelection {
        path: path.clone(),
        sha256: sha(&original),
        bytes: original.len() as u64,
    };
    let budget = Budget::default();
    let clock = NativeClock;
    let deadline = Deadline::new(&clock);
    let mut executor = Yielding(0);
    let opening = Git::open(&selected, &budget, &mut executor, &deadline);
    let compact = std::mem::size_of_val(&opening) < 32 * 1024;
    tokio::task::yield_now().await;
    let git = opening.await;
    let yielded = executor.0 >= 4;
    let valid = git.as_ref().is_ok_and(|git| git.validate().is_ok());
    let readback = std::fs::read(path);
    let closed = git.and_then(Git::finish);
    closed.unwrap();
    assert!(compact && yielded && valid);
    assert_eq!(budget.count(), 0);
    assert_eq!(budget.close_counts().1, 0);
    assert_eq!(readback.unwrap(), original);
}
