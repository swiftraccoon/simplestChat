//! The retained Python store and its browser command, replayed from `recorded_assets.json`
//! against this store in a second owned tree. Only filesystem semantics are compared; no
//! installed runtime or interpreter is invoked.
use super::{
    tests::{Fixture, capture, mode, revision},
    *,
};
use crate::test_support::{Recording, tally};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    path::Path,
};

/// What the retained store and browser command did in each state these tests put them in:
/// exit status, output and the tree left for the command, each operation's result and tree
/// for the store. `comment` in the recording says how it was made.
const RECORDED: &str = include_str!("recorded_assets.json");
const COMMAND: &str = "browser_command_receipts_and_trees_match_the_recorded_python_command";
const JOURNALS: &str =
    "journal_text_gates_refuse_bom_and_wide_encodings_for_both_journals_as_recorded";
const STORE: &str = "store_matches_the_recorded_python_lifecycle_and_hardlink_graph";
fn recording() -> Recording {
    Recording::new(RECORDED, &[COMMAND, JOURNALS, STORE])
}
/// The receipt the browser command prints when another process holds the workload lock.
const BUSY: &str = "{\"passed\":true,\"busy\":true,\"changed\":false}\n";

/// A tree without the two facts its filesystem decides rather than the store: a link's mode
/// and a directory's link count. Linux reports every link as 0777 and counts a directory's
/// subdirectories; macOS, where the recording was made, applies the umask to links and counts
/// every entry. The paths, kinds, bytes, other modes and the hard-link graph remain.
fn portable(tree: &Value) -> Value {
    let mut rows = tree.as_array().unwrap().clone();
    for row in &mut rows {
        match row[1].as_str().unwrap() {
            "link" => row[3] = Value::Null,
            "directory" => row[4] = Value::Null,
            _ => {}
        }
    }
    Value::Array(rows)
}
/// Writes the recorded store tree `rows` at `root`: directories, file bytes, links and hard
/// links, then every recorded file and directory mode, deepest first.
fn materialize(rows: &Value, root: &Path) {
    let mut first = BTreeMap::new();
    let mut modes = vec![];
    for row in rows.as_array().unwrap() {
        let name = row[0].as_str().unwrap();
        let path = if name.is_empty() {
            root.to_path_buf()
        } else {
            root.join(name)
        };
        let data = row[2].as_str().unwrap();
        match row[1].as_str().unwrap() {
            "directory" => fs::create_dir(&path).unwrap(),
            "link" => symlink(data, &path).unwrap(),
            _ => match first.get(&row[5].as_u64().unwrap()) {
                Some(original) => fs::hard_link(original, &path).unwrap(),
                None => {
                    let bytes = (0..data.len())
                        .step_by(2)
                        .map(|index| u8::from_str_radix(&data[index..index + 2], 16).unwrap())
                        .collect::<Vec<_>>();
                    fs::write(&path, bytes).unwrap();
                    first.insert(row[5].as_u64().unwrap(), path.clone());
                }
            },
        }
        if row[1] != "link" {
            modes.push((path, row[3].as_u64().unwrap()));
        }
    }
    for (path, value) in modes.into_iter().rev() {
        fs::set_permissions(path, fs::Permissions::from_mode(value as u32)).unwrap();
    }
}
/// The recorded step `step` of the command test, after checking that the fixture it was given
/// is the one this test has reached (`given`, the whole fixture tree).
fn step<'a>(
    steps: &'a [Value],
    step: usize,
    fixture: &Fixture,
    state: &str,
    action: &str,
) -> &'a Value {
    let recorded = &steps[step];
    assert_eq!(
        (recorded["state"].as_str(), recorded["action"].as_str()),
        (Some(state), Some(action))
    );
    assert_eq!(
        portable(&recorded["given"]),
        portable(&tree(&fixture.path)),
        "step {step}"
    );
    assert_eq!(recorded["stderr"], "", "step {step}");
    recorded
}
/// That this store reads the tree the recorded command left as it reads its own: written into
/// a fresh fixture, it opens with no generation and is left as it was.
fn readable(recorded: &Value) {
    let copy = Fixture::new();
    materialize(&recorded["tree"], &copy.root());
    assert_eq!(portable(&tree(&copy.root())), portable(&recorded["tree"]));
    let mut guard = copy.guard();
    assert_eq!(
        Store::open(&mut guard)
            .unwrap()
            .inspect()
            .unwrap()
            .generations,
        0
    );
    guard.finish().unwrap();
    assert_eq!(portable(&tree(&copy.root())), portable(&recorded["tree"]));
}
/// The recorded command found another process holding the workload lock: it reported busy
/// and wrote nothing. This store, in the same state, is refused as busy and writes nothing;
/// its guard holds an exclusive flock on the 0600 `run/simplestchat-bench/workload.lock`, which
/// the recorded command contends for. Then, once released, the command initialized, found the
/// store initialized, and pruned; each receipt and tree is this store's, and this store reads
/// each tree the command left. A prune of a fresh fixture leaves this store's tree.
///
/// The recording cannot show what the live comparison showed: that this implementation and
/// the Python command exclude each other on one lock file at run time. The lock's path, mode
/// and operation are held on this side here and in `assets_host_tests.rs`.
#[test]
fn browser_command_receipts_and_trees_match_the_recorded_python_command() {
    let recording = recording();
    let steps = recording.test(COMMAND)["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 5);
    let fixture = Fixture::new();
    let held = fixture.guard();
    let recorded = step(steps, 0, &fixture, "held", "initialize");
    let lock = fixture.path.join("run/simplestchat-bench/workload.lock");
    assert_eq!(fs::symlink_metadata(&lock).unwrap().mode() & 0o7777, 0o600);
    let probe = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock)
        .unwrap();
    assert_eq!(
        rustix::fs::flock(&probe, rustix::fs::FlockOperation::NonBlockingLockExclusive),
        Err(rustix::io::Errno::WOULDBLOCK)
    );
    drop(probe);
    let busy = match fixture.hook(None) {
        Ok(_) => panic!("a second guard acquired the held lock"),
        Err(failure) => failure,
    };
    assert!(busy.busy());
    assert_eq!(
        (recorded["status"].as_i64(), recorded["stdout"].as_str()),
        (Some(0), Some(BUSY))
    );
    assert_eq!(recorded["tree"], json!([]));
    assert!(!fixture.root().exists());
    held.finish().unwrap();
    for (index, action) in [(1, "initialize"), (2, "initialize"), (3, "prune")] {
        let recorded = step(steps, index, &fixture, "released", action);
        let mut guard = fixture.guard();
        let mut store = Store::open(&mut guard).unwrap();
        let changed = store.initialize().unwrap();
        let receipt = if action == "prune" {
            store.prune_at(100).unwrap();
            "{\"passed\":true}\n".to_string()
        } else {
            format!("{{\"passed\":true,\"changed\":{changed}}}\n")
        };
        assert_eq!(store.inspect().unwrap().generations, 0);
        guard.finish().unwrap();
        assert_eq!(recorded["status"], 0, "step {index}");
        assert_eq!(recorded["stdout"], receipt, "step {index}");
        assert_eq!(
            portable(&recorded["tree"]),
            portable(&tree(&fixture.root())),
            "step {index}"
        );
        readable(recorded);
    }
    let fresh = Fixture::new();
    let recorded = step(steps, 4, &fresh, "fresh", "prune");
    let mut guard = fresh.guard();
    let mut store = Store::open(&mut guard).unwrap();
    store.initialize().unwrap();
    store.prune_at(100).unwrap();
    guard.finish().unwrap();
    assert_eq!(
        (recorded["status"].as_i64(), recorded["stdout"].as_str()),
        (Some(0), Some("{\"passed\":true}\n"))
    );
    assert_eq!(portable(&recorded["tree"]), portable(&tree(&fresh.root())));
    readable(recorded);
}
/// Each journal is admitted or refused by this store's lock as the recorded command admitted
/// or refused it, each case bound to the SHA-256 of the journal's bytes.
#[test]
fn journal_text_gates_refuse_bom_and_wide_encodings_for_both_journals_as_recorded() {
    let text = r#"{"schemaVersion":1,"finalized":true,"note":"escaped\u0000private"}"#;
    let mut cases = vec![("utf8", text.as_bytes().to_vec(), true)];
    let mut bom = b"\xef\xbb\xbf".to_vec();
    bom.extend_from_slice(text.as_bytes());
    cases.push(("utf8-bom", bom, false));
    let mut raw = text.as_bytes().to_vec();
    raw.push(0);
    cases.push(("raw-nul", raw, false));
    let mut invalid = vec![0xff];
    invalid.extend_from_slice(text.as_bytes());
    cases.push(("invalid-utf8", invalid, false));
    for (label, little, bom) in [
        ("utf16le", true, false),
        ("utf16be", false, false),
        ("utf16le-bom", true, true),
        ("utf16be-bom", false, true),
    ] {
        let mut bytes = vec![];
        if bom {
            bytes.extend_from_slice(if little { b"\xff\xfe" } else { b"\xfe\xff" });
        }
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        cases.push((label, bytes, false));
    }
    for (label, little, bom) in [
        ("utf32le", true, false),
        ("utf32be", false, false),
        ("utf32le-bom", true, true),
        ("utf32be-bom", false, true),
    ] {
        let mut bytes = vec![];
        if bom {
            bytes.extend_from_slice(if little {
                b"\xff\xfe\0\0"
            } else {
                b"\0\0\xfe\xff"
            });
        }
        for unit in text.chars().map(u32::from) {
            bytes.extend_from_slice(&if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        cases.push((label, bytes, false));
    }
    let mut recorded = recording().cases(JOURNALS);
    let mut counted = BTreeMap::new();
    for journal in [
        "run/simplestchat-bench/current.json",
        "srv/simplestchat-public/release-state.json",
    ] {
        for (label, bytes, expected) in &cases {
            let fixture = Fixture::new();
            fixture.guard().finish().unwrap();
            let path = fixture.path.join(journal);
            fs::write(&path, bytes).unwrap();
            mode(&path, 0o600);
            let native = match fixture.hook(None) {
                Ok(guard) => guard.finish().map_err(Failure::code),
                Err(failure) => Err(failure.code()),
            };
            let case = recorded.case(bytes);
            assert_eq!(
                (case["label"].as_str(), case["journal"].as_str()),
                (Some(*label), Some(journal))
            );
            assert_eq!(case["mode"], 0o600);
            let original = &case["answer"];
            assert_eq!(native.is_ok(), *expected, "{journal}: {label}");
            assert_eq!(original["status"] == 0, *expected, "{journal}: {label}");
            assert!(!original["stdout"].as_str().unwrap().contains("private"));
            if !expected {
                assert_eq!(original["rootExists"], false, "{journal}: {label}");
                let refused: Value =
                    serde_json::from_str(original["stdout"].as_str().unwrap()).unwrap();
                assert_eq!(
                    refused["failure"].as_str(),
                    native.err(),
                    "{journal}: {label}"
                );
            }
            let outcome = native.err().unwrap_or("accepted");
            *counted.entry(outcome.to_string()).or_insert(0) += 1;
        }
    }
    assert_eq!(recorded.finish(), 24);
    // The recorder's tally: journals admitted, then refusals per code.
    assert_eq!(
        counted,
        tally(&[("accepted", 2), ("asset_operation_failed", 22)])
    );
}
fn tree(root: &Path) -> Value {
    fn visit(root: &Path, path: &Path, paths: &mut Vec<std::path::PathBuf>) {
        paths.push(path.into());
        if fs::symlink_metadata(path).unwrap().is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                visit(root, &entry.unwrap().path(), paths);
            }
        }
        let _ = root;
    }
    if !root.exists() {
        return json!([]);
    }
    let mut paths = vec![];
    visit(root, root, &mut paths);
    paths.sort();
    let mut inodes = std::collections::BTreeMap::new();
    let mut output = vec![];
    for path in paths {
        let stamp = fs::symlink_metadata(&path).unwrap();
        let kind = if stamp.is_dir() {
            "directory"
        } else if stamp.is_symlink() {
            "link"
        } else {
            "file"
        };
        let data = if stamp.is_symlink() {
            fs::read_link(&path).unwrap().display().to_string()
        } else if stamp.is_file() {
            fs::read(&path)
                .unwrap()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect()
        } else {
            String::new()
        };
        let next = inodes.len();
        let inode = *inodes.entry((stamp.dev(), stamp.ino())).or_insert(next);
        output.push(json!([
            path.strip_prefix(root).unwrap().display().to_string(),
            kind,
            data,
            stamp.mode() & 0o7777,
            stamp.nlink(),
            inode
        ]));
    }
    Value::Array(output)
}
pub(super) fn native(fixture: &Fixture, op: &Value) -> std::result::Result<Value, Failure> {
    if op["action"] == "mode" {
        mode(
            &fixture.root().join(op["path"].as_str().unwrap()),
            op["mode"].as_u64().unwrap() as u32,
        );
        return Ok(Value::Null);
    }
    if op["action"] == "version-link" {
        let version = fixture.root().join("public/app-version.json");
        fs::remove_file(&version).unwrap();
        fs::hard_link(fixture.root().join(op["path"].as_str().unwrap()), version).unwrap();
        return Ok(Value::Null);
    }
    if op["action"] == "schema" {
        let root = fixture.root();
        let path = root.join("index.json");
        let old = fs::read(&path).unwrap();
        let text = String::from_utf8(old.clone()).unwrap();
        let (_, tail) = text.split_once(',').unwrap();
        let new = format!(
            "{{\"schemaVersion\":{},{}",
            op["value"].as_str().unwrap(),
            tail
        )
        .replace(
            "\"createdAt\":0",
            &format!("\"createdAt\":{}", op["created"].as_str().unwrap_or("0")),
        );
        fs::rename(
            root.join(format!("union-{}", model::sha(&old))),
            root.join(format!("union-{}", model::sha(new.as_bytes()))),
        )
        .unwrap();
        fs::remove_file(root.join("public/assets")).unwrap();
        symlink(
            format!("../union-{}", model::sha(new.as_bytes())),
            root.join("public/assets"),
        )
        .unwrap();
        fs::write(path, new).unwrap();
        return Ok(Value::Null);
    }
    let mut guard = fixture.guard();
    let result = (|| {
        let mut store = Store::open(&mut guard)?;
        match op["action"].as_str().unwrap() {
            "initialize" => Ok(json!(store.initialize()?)),
            "publish" => {
                let captures = op["captures"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|entry| {
                        let pairs = entry[1]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|item| {
                                let text = item[1].as_str().unwrap();
                                let bytes = (0..text.len())
                                    .step_by(2)
                                    .map(|index| {
                                        u8::from_str_radix(&text[index..index + 2], 16).unwrap()
                                    })
                                    .collect();
                                (item[0].as_str().unwrap().to_string(), bytes)
                            })
                            .collect();
                        (entry[0].as_str().unwrap().into(), Capture(pairs))
                    })
                    .collect();
                Ok(
                    serde_json::to_value(store.publish_at(captures, op["now"].as_u64().unwrap())?)
                        .unwrap(),
                )
            }
            "prune" => {
                store.prune_at(op["now"].as_u64().unwrap())?;
                Ok(Value::Null)
            }
            "select" => Ok(json!(
                store.select_version(op["revision"].as_str().unwrap())?
            )),
            _ => unreachable!(),
        }
    })();
    let finished = guard.finish();
    result.and_then(|value| finished.map(|()| value))
}
fn publish_op(rev: char, now: u64, entries: &[(&str, &[u8])]) -> Value {
    let files = entries
        .iter()
        .map(|(name, bytes)| {
            json!([
                name,
                bytes
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            ])
        })
        .collect::<Vec<_>>();
    json!({"action":"publish","now":now,"captures":[[revision(rev),files]]})
}
/// Every store operation of the lifecycle leaves this store's result and tree, each bound to
/// the SHA-256 of the operation as this test serializes it.
#[test]
fn store_matches_the_recorded_python_lifecycle_and_hardlink_graph() {
    let mut operations = vec![
        json!({"action":"initialize"}),
        json!({"action":"initialize"}),
        json!({"action":"select","revision":revision('f')}),
        publish_op(
            'a',
            0,
            &[("z-abcdef.js", b"same"), ("a-abcdef.css", b"style")],
        ),
        publish_op(
            'a',
            0,
            &[("z-abcdef.js", b"same"), ("a-abcdef.css", b"style")],
        ),
        json!({"action":"schema","value":"true","created":"-0"}),
        json!({"action":"initialize"}),
        json!({"action":"schema","value":"1.0"}),
        json!({"action":"initialize"}),
        publish_op(
            'a',
            1,
            &[("a-abcdef.css", b"style"), ("z-abcdef.js", b"same")],
        ),
        publish_op('b', 2, &[("z-abcdef.js", b"same")]),
        publish_op('c', 2, &[("other-abcdef.js", b"third")]),
        publish_op('d', 3, &[("fourth-abcdef.js", b"fourth")]),
        publish_op('e', 4, &[("fifth-abcdef.js", b"fifth")]),
        publish_op('f', 5, &[("z-abcdef.js", b"different")]),
        json!({"action":"mode","path":"public/app-version.json","mode":0o600}),
        json!({"action":"select","revision":revision('f')}),
        json!({"action":"prune","now":MAX_AGE+4}),
    ];
    // Several single and two-required captures exercise capacity/order without
    // duplicating Python's retention implementation in the oracle driver.
    operations.push(json!({"action":"publish","now":MAX_AGE+5,"captures":[
        [revision('a'),[["left-abcdef.js","6c656674"]]],
        [revision('b'),[["right-abcdef.js","7269676874"]]]]}));
    let version = format!("{{\"revision\":\"{}\"}}\n", revision('f'));
    operations.push(publish_op(
        'c',
        MAX_AGE + 6,
        &[("version-abcdef.js", version.as_bytes())],
    ));
    operations.push(
        json!({"action":"version-link","path":format!("{}/version-abcdef.js",revision('c'))}),
    );
    operations.push(json!({"action":"select","revision":revision('e')}));
    let recording = recording();
    let native_fixture = Fixture::new();
    assert_eq!(
        portable(&recording.test(STORE)["given"]),
        portable(&tree(&native_fixture.path))
    );
    let mut recorded = recording.cases(STORE);
    let (mut passed, mut refused) = (0, 0);
    for (index, op) in operations.iter().enumerate() {
        let mut expected = recorded.answer(&serde_json::to_vec(op).unwrap());
        expected["tree"] = portable(&expected["tree"]);
        let result = native(&native_fixture, op);
        let left = portable(&tree(&native_fixture.root()));
        let actual = match result {
            Ok(value) => json!({"passed":true,"result":value,"tree":left}),
            Err(_) => json!({"passed":false,"tree":left}),
        };
        assert_eq!(actual, expected, "operation {index}: {op}");
        match actual["passed"].as_bool() {
            Some(true) => passed += 1,
            _ => refused += 1,
        }
    }
    assert_eq!(recorded.finish(), 22);
    // The recorder's tally: operations that passed and that were refused.
    assert_eq!((passed, refused), (22, 0));
    // Confirm the public capture wrapper itself agrees with the model used by
    // these fixtures; actual tar parity belongs to the separate capture suite.
    assert_eq!(capture(&[("left-abcdef.js", b"left")]).files(), 1);
}
