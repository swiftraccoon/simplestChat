//! The act builder's cache, publication and source extraction against owned
//! fixtures and shell tool doubles. No compiler or network is used.

use super::super::process::{Runner, Signals};
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

static NEXT: AtomicUsize = AtomicUsize::new(0);
const REVISION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const IDENTITY: &str = "go1.27.1\ndarwin\narm64\n";

/// An owned disposable directory.
struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "simplestchat-local-act-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn source_root() -> String {
    format!("act-{REVISION}")
}

/// One archive member below the source root, written without the builder's
/// own path checks so that malformed names reach the extractor.
fn member(name: &str, kind: tar::EntryType, mode: u32, link: &str) -> (tar::Header, &'static [u8]) {
    let mut header = tar::Header::new_ustar();
    let path = if name.is_empty() {
        source_root()
    } else {
        format!("{}/{name}", source_root())
    };
    let old = header.as_old_mut();
    old.name[..path.len()].copy_from_slice(path.as_bytes());
    old.linkname[..link.len()].copy_from_slice(link.as_bytes());
    header.set_entry_type(kind);
    header.set_mode(mode);
    let data: &[u8] = if kind == tar::EntryType::Regular {
        b"ok"
    } else {
        b""
    };
    header.set_size(data.len() as u64);
    header.set_cksum();
    (header, data)
}

fn file(name: &str) -> (tar::Header, &'static [u8]) {
    member(name, tar::EntryType::Regular, 0o644, "")
}

/// A small owned source archive: the root directory, then `members`.
fn archive(directory: &Path, members: &[(tar::Header, &'static [u8])]) -> PathBuf {
    let path = directory.join("source.tar.gz");
    let output = fs::File::create(&path).unwrap();
    let encoder = flate2::write::GzEncoder::new(output, flate2::Compression::fast());
    let mut builder = tar::Builder::new(encoder);
    let root = member("", tar::EntryType::Directory, 0o755, "");
    for (header, data) in std::iter::once(&root).chain(members) {
        builder.append(header, *data).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap();
    path
}

/// A checkout with a pin and patch, and tool doubles that record their calls.
struct Checkout {
    directory: Directory,
    log: PathBuf,
}

impl Checkout {
    fn new(patch: &str, recorded_patch_digest: Option<&str>) -> Self {
        let directory = Directory::new();
        let root = &directory.0;
        fs::create_dir(root.join("build")).unwrap();
        fs::create_dir(root.join("tools")).unwrap();
        fs::write(root.join(PATCH), patch).unwrap();
        let recorded = match recorded_patch_digest {
            Some(recorded) => recorded.to_owned(),
            None => digest(&root.join(PATCH)).unwrap(),
        };
        fs::write(root.join(PIN), json!({"patchSha256": recorded}).to_string()).unwrap();
        let log = root.join("calls.log");
        for (tool, body) in [
            ("go", format!("printf '{}'", IDENTITY.replace('\n', "\\n"))),
            ("curl", "exit 22".to_owned()),
            ("patch", "exit 1".to_owned()),
            ("slow", "/bin/sleep 30".to_owned()),
        ] {
            let path = root.join("tools").join(tool);
            let script = format!("#!/bin/sh\necho \"{tool} $*\" >> \"$CALLS\"\n{body}\n");
            fs::write(&path, script).unwrap();
            fs::set_permissions(path, Permissions::from_mode(0o755)).unwrap();
        }
        Self { directory, log }
    }

    fn root(&self) -> &Path {
        &self.directory.0
    }

    fn runner(&self) -> Runner {
        let environment = [
            ("PATH", self.root().join("tools").into_os_string()),
            ("CALLS", self.log.clone().into_os_string()),
        ]
        .into_iter()
        .map(|(name, value)| (name.into(), value))
        .collect();
        let (_, receiver) = tokio::sync::mpsc::unbounded_channel();
        Runner::new(self.root(), environment, Signals::injected(receiver))
    }

    fn calls(&self) -> String {
        fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// The cache key for this checkout's pin and the doubles' Go identity.
    fn key(&self) -> String {
        let mut hash = Sha256::new();
        hash.update(fs::read(self.root().join(PIN)).unwrap());
        hash.update(IDENTITY);
        let bytes = hash.finalize();
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

/// From `test_publish_exposes_binary_and_receipt_together`: the final cache is
/// absent until both required artifacts are complete.
#[test]
fn publish_exposes_binary_and_receipt_together() {
    let directory = Directory::new();
    let (staging, target) = (directory.0.join("staging"), directory.0.join("target"));
    fs::create_dir(&staging).unwrap();
    fs::write(staging.join("act"), b"completed compiler output").unwrap();
    let key = "a".repeat(64);
    publish(&staging, &target, &key).unwrap();
    assert!(!staging.exists());
    let binary = digest(&target.join("act")).unwrap();
    let receipt = target.join("receipt.json");
    assert_eq!(
        fs::read_to_string(&receipt).unwrap(),
        format!("{{\"inputSha256\": \"{key}\", \"binarySha256\": \"{binary}\"}}\n")
    );
    let mode = fs::metadata(receipt).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}

/// From `test_interrupted_publish_leaves_no_reusable_cache`: a receipt that
/// could not be written cannot make a partial build look installed.
#[test]
fn interrupted_publish_leaves_no_reusable_cache() {
    let directory = Directory::new();
    let (staging, target) = (directory.0.join("staging"), directory.0.join("target"));
    fs::create_dir(&staging).unwrap();
    fs::write(staging.join("act"), b"completed compiler output").unwrap();
    // A directory in the receipt's place makes the receipt write fail.
    fs::create_dir(staging.join("receipt.json")).unwrap();
    assert!(publish(&staging, &target, &"a".repeat(64)).is_err());
    assert!(!target.exists());
    fs::remove_dir(staging.join("receipt.json")).unwrap();
    publish(&staging, &target, &"a".repeat(64)).unwrap();
    assert!(target.join("receipt.json").is_file());
}

/// From `test_changed_patch_fails_before_compiler_or_network`: a maintained
/// patch must match its recorded checksum before any tool runs.
#[tokio::test]
async fn changed_patch_fails_before_compiler_or_network() {
    let checkout = Checkout::new("different patch", Some(&"0".repeat(64)));
    let mut runner = checkout.runner();
    let failure = prepare(&mut runner).await.unwrap_err();
    assert!(
        failure.message.contains("local_act_patch_digest"),
        "{failure:?}"
    );
    assert_eq!(checkout.calls(), "");
}

/// From `test_cache_reuse_requires_exact_binary_hash`: a warm start accepts a
/// complete matching build and refuses modified bytes, without the network.
#[tokio::test]
async fn cache_reuse_requires_exact_binary_hash() {
    let checkout = Checkout::new("reviewed patch", None);
    let key = checkout.key();
    let target = checkout.root().join(".cache/ci-act").join(&key);
    fs::create_dir_all(&target).unwrap();
    let binary = target.join("act");
    fs::write(&binary, b"reviewed build").unwrap();
    fs::set_permissions(&binary, Permissions::from_mode(0o700)).unwrap();
    let receipt = json!({"inputSha256": key, "binarySha256": digest(&binary).unwrap()});
    fs::write(target.join("receipt.json"), receipt.to_string()).unwrap();
    let mut runner = checkout.runner();
    let environment = runner.environment.clone();
    assert_eq!(prepare(&mut runner).await.unwrap(), binary);
    // The checkout's own settings are back in place for the next command.
    assert_eq!(runner.environment, environment);
    assert_eq!(runner.root, checkout.root());
    assert!(runner.limit.is_none());
    fs::write(&binary, b"modified build").unwrap();
    let failure = prepare(&mut runner).await.unwrap_err();
    assert!(
        failure.message.contains("local_act_cache_integrity"),
        "{failure:?}"
    );
    fs::write(&binary, b"reviewed build").unwrap();
    fs::set_permissions(&binary, Permissions::from_mode(0o600)).unwrap();
    assert!(prepare(&mut runner).await.is_err());
    let identity = "go env GOVERSION GOOS GOARCH\n";
    assert_eq!(checkout.calls(), identity.repeat(3));
}

/// A cold start with an unreachable archive fails closed and leaves neither
/// a published build nor its workspace behind.
#[tokio::test]
async fn failed_download_publishes_nothing_and_removes_its_workspace() {
    let checkout = Checkout::new("reviewed patch", None);
    let pin = json!({
        "patchSha256": digest(&checkout.root().join(PATCH)).unwrap(),
        "revision": REVISION, "archiveBytes": 7, "archiveSha256": "0".repeat(64), "version": "0",
    });
    fs::write(checkout.root().join(PIN), pin.to_string()).unwrap();
    let mut runner = checkout.runner();
    let failure = prepare(&mut runner).await.unwrap_err();
    assert!(
        failure.message.contains("local_act_archive_download"),
        "{failure:?}"
    );
    let calls = checkout.calls();
    let download = calls.lines().nth(1).unwrap();
    assert!(download.starts_with("curl --fail --silent --show-error --location --proto =https"));
    assert!(download.contains(" --max-time 120 --max-filesize 7 --output "));
    let url = format!("https://codeload.github.com/nektos/act/tar.gz/{REVISION}");
    assert!(download.ends_with(&url));
    assert_eq!(calls.lines().count(), 2);
    let cache = checkout.root().join(".cache/ci-act");
    let mut left = fs::read_dir(cache)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    left.sort();
    assert_eq!(left, ["go-build", "go-modules", "go-path", "tmp"]);
}

/// A command still running at the limit is stopped and reported as a timeout.
#[tokio::test]
async fn a_command_over_its_limit_is_stopped() {
    let checkout = Checkout::new("reviewed patch", None);
    let mut runner = checkout.runner();
    runner.limit = Some(Duration::from_millis(200));
    let started = Instant::now();
    let failure = runner.capture("slow", [""; 0]).await.unwrap_err();
    assert_eq!(failure.code, 124);
    assert!(started.elapsed() < Duration::from_secs(10));
}

/// From `test_source_extraction_preserves_internal_links_and_executable_modes`:
/// pinned fixture links work without writable or special file modes.
#[test]
fn source_extraction_preserves_internal_links_and_executable_modes() {
    let directory = Directory::new();
    let members = [
        member("tool", tar::EntryType::Regular, 0o6775, ""),
        member("tool-link", tar::EntryType::Symlink, 0o777, "tool"),
        file("plain"),
    ];
    let archive = archive(&directory.0, &members);
    let source = extract_source(&archive, &directory.0, REVISION, &LIMITS).unwrap();
    assert_eq!(source, directory.0.join(source_root()));
    assert_eq!(fs::read(source.join("tool-link")).unwrap(), b"ok");
    assert!(source.join("tool-link").is_symlink());
    let mode = |name: &str| {
        fs::metadata(source.join(name))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777
    };
    assert_eq!(mode("tool"), 0o755);
    assert_eq!(mode("plain"), 0o644);
}

/// From `test_source_extraction_rejects_escape_links_types_and_collisions_before_writes`:
/// no malformed member can redirect or overwrite a source-tree member.
#[test]
fn source_extraction_rejects_escape_links_types_and_collisions_before_writes() {
    let fixtures = [
        (vec![file("../escape")], "local_act_source_path"),
        (
            vec![member(
                "escape",
                tar::EntryType::Symlink,
                0o777,
                "../../outside",
            )],
            "local_act_source_link",
        ),
        (
            vec![member("device", tar::EntryType::Char, 0o644, "")],
            "local_act_source_type",
        ),
        (
            vec![file("same"), file("same")],
            "local_act_source_duplicate",
        ),
        (
            vec![file("parent"), file("parent/child")],
            "local_act_source_parent",
        ),
        (vec![file("a/./b")], "local_act_source_path"),
        (vec![file("back\\slash")], "local_act_source_path"),
    ];
    for (members, code) in fixtures {
        let directory = Directory::new();
        let archive = archive(&directory.0, &members);
        let failure = extract_source(&archive, &directory.0, REVISION, &LIMITS).unwrap_err();
        assert!(failure.message.contains(code), "{code}: {failure:?}");
        assert!(!directory.0.join(source_root()).exists(), "{code}");
    }
    let directory = Directory::new();
    let archive = archive(&directory.0, &[file("plain")]);
    let failure = extract_source(&archive, &directory.0, "main", &LIMITS).unwrap_err();
    assert!(failure.message.contains("local_act_source_revision"));
}

/// From `test_source_extraction_bounds_expansion_count_and_individual_files`:
/// budgets apply before any file or link reaches the filesystem.
#[test]
fn source_extraction_bounds_expansion_count_and_individual_files() {
    let directory = Directory::new();
    let archive = archive(&directory.0, &[file("file")]);
    let limits = |bytes, member, members| Limits {
        bytes,
        member,
        members,
    };
    for (limits, code) in [
        (
            limits(1, LIMITS.member, LIMITS.members),
            "local_act_source_expansion",
        ),
        (
            limits(LIMITS.bytes, LIMITS.member, 1),
            "local_act_source_path",
        ),
        (
            limits(LIMITS.bytes, 1, LIMITS.members),
            "local_act_source_size",
        ),
    ] {
        let failure = extract_source(&archive, &directory.0, REVISION, &limits).unwrap_err();
        assert!(failure.message.contains(code), "{code}: {failure:?}");
        assert!(!directory.0.join(source_root()).exists(), "{code}");
    }
}

/// GitHub's source archives begin with archive-wide extended metadata, which
/// is not a member of the tree.
#[test]
fn archive_wide_metadata_is_not_a_tree_member() {
    let directory = Directory::new();
    let mut global = tar::Header::new_ustar();
    global.as_old_mut().name[..17].copy_from_slice(b"pax_global_header");
    global.set_entry_type(tar::EntryType::XGlobalHeader);
    global.set_mode(0o666);
    let comment: &[u8] = b"52 comment=4f411281417e88660bea1c1a1749aa71ae0bd60f\n";
    global.set_size(comment.len() as u64);
    global.set_cksum();
    let path = directory.0.join("source.tar.gz");
    let output = fs::File::create(&path).unwrap();
    let encoder = flate2::write::GzEncoder::new(output, flate2::Compression::fast());
    let mut builder = tar::Builder::new(encoder);
    builder.append(&global, comment).unwrap();
    let root = member("", tar::EntryType::Directory, 0o755, "");
    for (header, data) in [root, file("plain")] {
        builder.append(&header, data).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap();
    let source = extract_source(&path, &directory.0, REVISION, &LIMITS).unwrap();
    assert_eq!(fs::read(source.join("plain")).unwrap(), b"ok");
}
