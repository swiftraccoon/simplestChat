//! Replays of the output budgets and readable trees of
//! `recorded_sandbox.json`: what the reference in Python made of each tree.

use super::{Usage, readable, usage};
use crate::{
    document::Document,
    image::{
        notices::tests::built,
        policy::tests::tallied,
        sandbox::{
            Limits,
            tests::{cases, expected, reopened, walked, whole},
        },
        secrets::tests::{here, text, there},
    },
    scratch::Scratch,
};
use std::{fs, os::unix::fs::PermissionsExt, path::Path};

/// Give the files of a recorded output below `root` the sizes it records:
/// the recorder kept an output's sizes, not its bytes.
fn sized(case: &Document, root: &Path) {
    for (name, entry) in case.get("tree").and_then(Document::fields).expect("a tree") {
        if entry.get("file").is_some() {
            let file = fs::OpenOptions::new().write(true).open(root.join(name));
            file.expect("a file")
                .set_len(whole(entry, "bytes"))
                .expect("a size");
        }
    }
}

#[test]
fn usages_match_the_recording() {
    let superuser = rustix::process::geteuid().is_root();
    let mut all = Vec::new();
    let mut stricter = 0;
    for case in cases("usages") {
        let name = text(&case, "name").to_owned();
        let scratch = Scratch::new("usage");
        let root = built(&case, &scratch.0);
        sized(&case, &root);
        let entries = case.get("files").and_then(Document::whole);
        let entries = entries.map_or(Limits::REVIEWED.entries, |entries| {
            u64::try_from(entries).expect("a count")
        });
        let came = usage(&root, whole(&case, "limit"), entries);
        reopened(&scratch.0);
        if name == "an unreadable directory" {
            // The reference passed over a directory it could not list, and
            // what it held; that is refused here, but by the superuser, who
            // can list it.
            let recorded = case.get("result").and_then(|result| result.get("held"));
            let passed = recorded.and_then(|held| held.get("withinLimit"));
            assert_eq!(passed, Some(&Document::Bool(true)));
            if !superuser {
                let refused = Usage {
                    within: false,
                    bytes: 0,
                    entries: 1,
                    rejected: true,
                };
                assert_eq!(came, refused);
            }
            stricter += 1;
            continue;
        }
        all.push((name, here(Ok(came.record())), there(&case)));
    }
    assert_eq!(stricter, 1);
    assert_eq!(tallied(all, &[]), [14, 0, 0, 0, 0]);
}

#[test]
fn readable_trees_match_the_recording() {
    let mut all = Vec::new();
    for case in cases("readables") {
        let name = text(&case, "name").to_owned();
        let scratch = Scratch::new("readable");
        let root = built(&case, &scratch.0);
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).expect("a mode");
        let came = readable(&root).map(|()| Document::Null);
        if here(came.clone()) == there(&case) {
            assert_eq!(
                walked(&root),
                expected(case.get("after").expect("a tree")),
                "{name}"
            );
            let mode = root
                .symlink_metadata()
                .expect("a root")
                .permissions()
                .mode()
                & 0o7777;
            assert_eq!(u64::from(mode), whole(&case, "rootMode"), "{name}");
        }
        reopened(&scratch.0);
        all.push((name, here(came), there(&case)));
    }
    assert_eq!(tallied(all, &[]), [4, 4, 0, 0, 0]);
}

#[test]
fn a_tree_that_is_not_there_is_refused() {
    let scratch = Scratch::new("absent");
    let absent = scratch.0.join("absent");
    let counted = usage(&absent, 10, 10);
    let refused = Usage {
        within: false,
        bytes: 0,
        entries: 0,
        rejected: true,
    };
    assert_eq!(counted, refused);
    assert_eq!(readable(&absent), Err(crate::Fault("tool_entry_missing")));
}

/// Twenty files of a byte below `root`, but for the eighth, a link to
/// nothing, so that what is counted and opened before it is told by the
/// order of the names.
fn planted(root: &Path) {
    fs::create_dir(root).expect("a root");
    for index in 0..20 {
        let path = root.join(format!("f{index:02}"));
        if index == 7 {
            std::os::unix::fs::symlink("missing", &path).expect("a link");
        } else {
            fs::write(&path, b"x").expect("a file");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("a mode");
        }
    }
}

#[test]
fn entries_are_counted_and_opened_in_the_order_of_their_names() {
    let scratch = Scratch::new("order");
    let root = scratch.0.join("root");
    planted(&root);
    let counted = Usage {
        within: false,
        bytes: 7,
        entries: 8,
        rejected: true,
    };
    assert_eq!(usage(&root, 100, 100), counted);
    assert_eq!(
        readable(&root),
        Err(crate::Fault("image_scanner_input_link"))
    );
    for index in (0..20).filter(|index| *index != 7) {
        let path = root.join(format!("f{index:02}"));
        let mode = path.metadata().expect("a file").permissions().mode() & 0o7777;
        assert_eq!(mode, if index < 7 { 0o444 } else { 0o600 }, "{index}");
    }
    reopened(&scratch.0);
}

#[test]
fn directories_are_walked_and_opened_in_the_order_of_their_names() {
    let scratch = Scratch::new("descent");
    let root = scratch.0.join("root");
    for (name, size) in [("a", 8), ("b", 1)] {
        fs::create_dir_all(root.join(name)).expect("a directory");
        fs::write(root.join(name).join("file"), vec![0; size]).expect("a file");
    }
    let counted = Usage {
        within: false,
        bytes: 8,
        entries: 3,
        rejected: false,
    };
    assert_eq!(usage(&root, 7, 100), counted);
    let other = scratch.0.join("other");
    fs::create_dir_all(other.join("a")).expect("a directory");
    fs::create_dir_all(other.join("b")).expect("a directory");
    let file = other.join("a/file");
    fs::write(&file, b"x").expect("a file");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).expect("a mode");
    std::os::unix::fs::symlink("missing", other.join("b/link")).expect("a link");
    assert_eq!(
        readable(&other),
        Err(crate::Fault("image_scanner_input_link"))
    );
    let mode = file.metadata().expect("a file").permissions().mode() & 0o7777;
    assert_eq!(mode, 0o444);
    for directory in ["a", "b"] {
        let mode = other
            .join(directory)
            .metadata()
            .expect("a directory")
            .permissions()
            .mode();
        assert_eq!(mode & 0o7777, 0o555, "{directory}");
    }
    reopened(&scratch.0);
}
