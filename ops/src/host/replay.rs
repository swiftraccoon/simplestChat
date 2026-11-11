//! What the recorded-answer replays share: the recorder's tree under a
//! scratch root, its listing, and the placeholders for names that are random
//! on a host.

use serde_json::Value;
use std::collections::BTreeMap;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub(crate) const TOPS: [(&str, &str); 6] = [
    ("srv", "srv/simplestchat-public"),
    ("etc", "etc/simplestchat-public"),
    ("run", "run/simplestchat-bench"),
    ("offsite", "offsite"),
    ("state", "state"),
    ("turn", "turn"),
];

/// Where a recorded path lives under the scratch root: the recorder's
/// three directories are the fixed ones the workload lock owns.
pub(crate) fn placed(base: &Path, recorded: &str) -> PathBuf {
    let (top, rest) = recorded.split_once('/').unwrap_or((recorded, ""));
    let fixed = TOPS.iter().find(|(name, _)| *name == top).unwrap().1;
    base.join(fixed).join(rest)
}

pub(crate) fn chmod(path: &Path, mode: u32) {
    std::fs::set_permissions(path, PermissionsExt::from_mode(mode)).unwrap();
}

pub(crate) fn mode(entry: &Value) -> u32 {
    let text = entry["mode"].as_str().unwrap().strip_prefix("0o").unwrap();
    u32::from_str_radix(text, 8).unwrap()
}

/// A file's recorded content: its text, or one character repeated.
pub(crate) fn content(entry: &Value) -> String {
    let spelled = &entry["text"];
    match spelled.as_str() {
        Some(text) => text.to_owned(),
        None => {
            let count = usize::try_from(spelled["count"].as_u64().unwrap()).unwrap();
            spelled["repeat"].as_str().unwrap().repeat(count)
        }
    }
}

/// Where a recorded link points: into the recorded tree, or, for a target
/// that names no recorded directory, wherever it says.
pub(crate) fn target(base: &Path, link: &str) -> PathBuf {
    let top = link.split('/').next().unwrap();
    if TOPS.iter().any(|(name, _)| *name == top) {
        placed(base, link)
    } else {
        PathBuf::from(link)
    }
}

/// Build the recorded tree, then give every entry its recorded age.
pub(crate) fn build(base: &Path, tree: &serde_json::Map<String, Value>, epoch: i64) {
    chmod(base, 0o700);
    // The lock's three directories have a parent of their own.
    for (top, _) in TOPS.iter().filter(|(_, fixed)| fixed.contains('/')) {
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(base.join(top))
            .unwrap();
    }
    for (name, entry) in tree {
        let path = placed(base, name);
        if let Some(link) = entry["link"].as_str() {
            std::os::unix::fs::symlink(target(base, link), &path).unwrap();
        } else if entry["directory"] == true {
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&path)
                .unwrap();
        } else {
            std::fs::write(&path, content(entry)).unwrap();
        }
    }
    for (name, entry) in tree.iter().rev() {
        let path = placed(base, name);
        if entry["link"].is_string() {
            continue;
        }
        chmod(&path, mode(entry));
        // An entry without a recorded age keeps the one it just got.
        let Some(age) = entry["mtime"].as_i64() else {
            continue;
        };
        let stamp = rustix::fs::Timespec {
            tv_sec: epoch + age,
            tv_nsec: 0,
        };
        let times = rustix::fs::Timestamps {
            last_access: stamp,
            last_modification: stamp,
        };
        rustix::fs::utimensat(rustix::fs::CWD, &path, &times, rustix::fs::AtFlags::empty())
            .unwrap();
    }
}

/// The workload lock as another operation that created it holds it.
pub(crate) fn held_lock(base: &Path) -> std::fs::File {
    let work = base.join("run/simplestchat-bench");
    if !work.exists() {
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&work)
            .unwrap();
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    let lock = options.open(work.join("workload.lock")).unwrap();
    let exclusive = rustix::fs::FlockOperation::NonBlockingLockExclusive;
    rustix::fs::flock(&lock, exclusive).unwrap();
    lock
}

/// The tree as the recorder listed it: recorded name to kind and content.
pub(crate) fn listed(base: &Path) -> BTreeMap<String, (u32, String)> {
    fn walk(base: &Path, at: &Path, name: &str, found: &mut BTreeMap<String, (u32, String)>) {
        let metadata = std::fs::symlink_metadata(at).unwrap();
        let content = if metadata.is_symlink() {
            let target = std::fs::read_link(at).unwrap();
            let within = TOPS
                .iter()
                .find(|(_, fixed)| target.starts_with(base.join(fixed)));
            match within {
                Some((top, fixed)) => {
                    let rest = target.strip_prefix(base.join(fixed)).unwrap();
                    let rest = rest.to_str().unwrap();
                    let separator = if rest.is_empty() { "" } else { "/" };
                    format!("link:{top}{separator}{rest}")
                }
                // A target outside the recorded directories stands as it is.
                None => format!("link:{}", target.to_str().unwrap()),
            }
        } else if metadata.is_dir() {
            "directory".to_owned()
        } else {
            format!("text:{}", std::fs::read_to_string(at).unwrap())
        };
        // A link's own mode means nothing and follows the creator's mask.
        let mode = if metadata.is_symlink() {
            0
        } else {
            metadata.mode() & 0o7777
        };
        found.insert(name.to_owned(), (mode, content));
        if metadata.is_dir() {
            for entry in std::fs::read_dir(at).unwrap() {
                let entry = entry.unwrap();
                let child = format!("{name}/{}", entry.file_name().to_str().unwrap());
                walk(base, &entry.path(), &child, found);
            }
        }
    }
    let mut found = BTreeMap::new();
    for (top, fixed) in TOPS {
        if base.join(fixed).exists() {
            walk(base, &base.join(fixed), top, &mut found);
        }
    }
    found
}

pub(crate) fn expected(tree: &serde_json::Map<String, Value>) -> BTreeMap<String, (u32, String)> {
    let entries = tree.iter().map(|(name, entry)| {
        let content = if let Some(link) = entry["link"].as_str() {
            format!("link:{link}")
        } else if entry["directory"] == true {
            "directory".to_owned()
        } else {
            format!("text:{}", content(entry))
        };
        let mode = if entry["link"].is_string() {
            0
        } else {
            mode(entry)
        };
        (name.clone(), (mode, content))
    });
    entries.collect()
}

/// The recorder's placeholders for names that are random on a host.
pub(crate) fn normalized(text: &str, base: &Path) -> String {
    let mut text = text.replace(base.to_str().unwrap(), "<base>");
    // The lock's fixed directories stand where the recorder had one each.
    for (top, fixed) in TOPS {
        text = text.replace(fixed, top);
    }
    for prefix in [
        "upload.",
        "restore.",
        "download.",
        "recovery.",
        "scpub-restore-",
    ] {
        let length = if prefix.ends_with('-') { 32 } else { 8 };
        let placeholder = if prefix.ends_with('-') {
            "<id>"
        } else {
            "<name>"
        };
        let mut rebuilt = String::new();
        let mut rest = text.as_str();
        while let Some(at) = rest.find(prefix) {
            let after = &rest[at + prefix.len()..];
            let unique = after.get(..length).filter(|unique| {
                unique
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
            });
            rebuilt.push_str(&rest[..at + prefix.len()]);
            match unique {
                Some(_) => {
                    rebuilt.push_str(placeholder);
                    rest = &after[length..];
                }
                None => rest = after,
            }
        }
        rebuilt.push_str(rest);
        text = rebuilt;
    }
    text
}

/// A recorded text with its placeholders made concrete for this run.
pub(crate) fn concrete(text: &str, base: &Path) -> String {
    let mut text = text.to_owned();
    for (top, fixed) in TOPS {
        text = text.replace(&format!("<base>/{top}"), &format!("<base>/{fixed}"));
    }
    text.replace("<base>", base.to_str().unwrap())
        .replace(".<name>", ".abcd1234")
        .replace("<id>", &"1".repeat(32))
}
