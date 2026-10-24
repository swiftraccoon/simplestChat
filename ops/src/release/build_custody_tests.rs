//! Direct custody regressions over original handles and private filesystem data.
//!
//! No Git, Docker, image, network request or signal is executed. Unix listeners
//! only create inert socket inodes in this test's private directory.

use crate::release::build::{
    Socket,
    control::{Failure, Owner},
    files::{self, CloseObservation, Dir, Held, Output},
    source::{Environment, Tool, Tree},
};
use crate::test_support::Fixture;
use serde_json::json;
use std::{
    collections::BTreeSet,
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{Cursor, Write},
    os::{
        fd::{AsRawFd, OwnedFd},
        unix::{
            fs::{MetadataExt, OpenOptionsExt, PermissionsExt, symlink},
            net::UnixListener,
        },
    },
    path::Path,
    time::{Duration, Instant},
};

const SOURCE_REVISION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn owner() -> Owner {
    let mut owner = Owner::new(
        Instant::now(),
        crate::release::build::control::Mode::Build,
        120,
    )
    .unwrap();
    owner.advance(Duration::ZERO);
    owner
}

fn directory(path: &Path) {
    fs::create_dir(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn private(path: &Path, bytes: &[u8], mode: u32) {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(mode)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
    files::close(file).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

fn listener_close(listener: UnixListener) {
    let descriptor: OwnedFd = listener.into();
    files::close(File::from(descriptor)).unwrap();
}

fn source_bytes(bytes: &[u8]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_mode(0o644);
    header.set_size(bytes.len() as u64);
    header.set_cksum();
    builder
        .append_data(&mut header, "partial.bin", Cursor::new(bytes))
        .unwrap();
    builder.into_inner().unwrap()
}

fn archive(fixture: &Fixture, bytes: &[u8], owner: &mut Owner) -> (Dir, Held) {
    let parent = Dir::open(&fixture.0).unwrap();
    let mut archive = parent.create(OsStr::new("source.tar"), 0o600).unwrap();
    let end = owner.end();
    archive.write(bytes, owner, end).unwrap();
    (parent, archive)
}

fn failed_extract(archive: &mut Held, parent: &Path, owner: &mut Owner) -> Failure {
    let end = owner.end();
    match Tree::extract(archive, parent, SOURCE_REVISION, owner, end) {
        Err(error) => error,
        Ok(tree) => {
            tree.remove(owner, end).unwrap();
            panic!("invalid source unexpectedly completed")
        }
    }
}

fn output(fixture: &Fixture, name: &str) -> Output {
    Output::create(Dir::open(&fixture.0).unwrap(), OsStr::new(name)).unwrap()
}

fn every_original_closed_once(expected: &[(i32, u64, u64)], observations: &[CloseObservation]) {
    // Numeric descriptors are observed only while their original File is live.
    // The close owner supplies completed observations; nothing borrows or probes
    // a descriptor after close, when the number could already have been reused.
    let expected: BTreeSet<_> = expected.iter().copied().collect();
    assert!(!expected.is_empty());
    for (descriptor, device, inode) in expected {
        let attempts: Vec<_> = observations
            .iter()
            .filter(|observation| {
                observation.descriptor == descriptor
                    && observation.identity == Some((device, inode))
            })
            .collect();
        assert_eq!(attempts.len(), 1, "original descriptor close count");
        assert!(
            attempts[0].succeeded,
            "the real close must precede any injected error"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn truncated_member_retires_the_created_partial_leaf_and_context() {
    let fixture = Fixture::new();
    let mut owner = owner();
    // Preserve the mature builder's real 4096-byte declaration, then retain
    // only three member bytes. The complete header reaches file creation.
    let mut bytes = source_bytes(&vec![b'x'; 4096]);
    bytes.truncate(512 + 3);
    let (parent, mut archive) = archive(&fixture, &bytes, &mut owner);
    files::fixture_observe_closes(None);
    assert_eq!(
        failed_extract(&mut archive, &fixture.0, &mut owner),
        Failure::Archive
    );
    assert!(!fixture.0.join("context/partial.bin").exists());
    assert!(!fixture.0.join("context").exists());
    let observations = files::fixture_take_closes();
    assert!(
        observations.iter().any(|observation| {
            observation.regular && observation.bytes == Some(3) && observation.succeeded
        }),
        "the test must copy and explicitly close a real partial leaf before refusal"
    );
    archive.finish().unwrap();
    parent.finish().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn deadline_cleanup_retains_original_files_and_explicitly_closes_every_admitted_peer() {
    for inject_close in [false, true] {
        let fixture = Fixture::new();
        let mut owner = owner();
        let mut output = output(&fixture, "late");
        let end = owner.end();
        output
            .json("first.json", &json!({"member":1}), &mut owner, end)
            .unwrap();
        output
            .json("second.json", &json!({"member":2}), &mut owner, end)
            .unwrap();
        let path = output.path().to_owned();
        let before: Vec<_> = ["first.json", "second.json"]
            .iter()
            .map(|name| {
                let path = path.join(name);
                let metadata = fs::metadata(&path).unwrap();
                let bytes = fs::read(&path).unwrap();
                (path, metadata.dev(), metadata.ino(), bytes)
            })
            .collect();
        let originals = output.fixture_descriptors();
        let fail = inject_close.then(|| output.get("first.json").unwrap().file().as_raw_fd());
        owner.advance(end - owner.now());
        assert_eq!(owner.check(), Err(Failure::Deadline));
        files::fixture_observe_closes(fail);
        let failure = output.remove(&owner, end).unwrap_err();
        if inject_close {
            assert_eq!(failure, Failure::Close);
        } else {
            assert_eq!(failure, Failure::Deadline);
        }
        owner.record(failure);
        assert_eq!(owner.primary(), Some(Failure::Deadline));
        let observations = files::fixture_take_closes();
        every_original_closed_once(&originals, &observations);
        assert!(path.is_dir());
        for (path, device, inode, bytes) in before {
            let metadata = fs::metadata(&path).unwrap();
            assert_eq!((metadata.dev(), metadata.ino()), (device, inode));
            assert_eq!(fs::read(path).unwrap(), bytes);
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn negative_after_real_close_still_attempts_all_other_original_closes() {
    let fixture = Fixture::new();
    let mut owner = owner();
    let mut output = output(&fixture, "close-fault");
    let end = owner.end();
    for name in ["a.json", "b.json", "c.json"] {
        output
            .json(name, &json!({"member":name}), &mut owner, end)
            .unwrap();
    }
    let originals = output.fixture_descriptors();
    let failed = output.get("a.json").unwrap().file().as_raw_fd();
    files::fixture_observe_closes(Some(failed));
    assert_eq!(output.finish(), Err(Failure::Close));
    every_original_closed_once(&originals, &files::fixture_take_closes());
    for name in ["a.json", "b.json", "c.json"] {
        assert!(fixture.0.join("close-fault").join(name).is_file());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn equal_target_docker_aliases_are_supported_but_alias_inode_exchange_is_refused() {
    let fixture = Fixture::new();
    let engine = fixture.0.join("engine");
    directory(&engine);
    let target = engine.join("socket");
    let listener = UnixListener::bind(&target).unwrap();
    symlink("engine", fixture.0.join("current")).unwrap();
    symlink("current/socket", fixture.0.join("docker.sock")).unwrap();
    symlink(&target, fixture.0.join("second.sock")).unwrap();
    let mut owner = owner();
    let end = owner.end();
    let first = Socket::open(
        &format!("unix://{}", fixture.0.join("docker.sock").display()),
        &mut owner,
        end,
    )
    .unwrap();
    let second = Socket::open(
        &format!("unix://{}", fixture.0.join("second.sock").display()),
        &mut owner,
        end,
    )
    .unwrap();
    first.check().unwrap();
    second.check().unwrap();
    symlink("current/socket", fixture.0.join("replacement")).unwrap();
    fs::rename(fixture.0.join("replacement"), fixture.0.join("docker.sock")).unwrap();
    assert_eq!(first.check(), Err(Failure::Changed));
    assert_eq!(first.finish(), Err(Failure::Changed));
    second.check().unwrap();
    second.finish().unwrap();
    assert_eq!(
        fs::read_link(fixture.0.join("docker.sock")).unwrap(),
        Path::new("current/socket")
    );
    listener_close(listener);
}

#[tokio::test(flavor = "current_thread")]
async fn changed_docker_alias_target_and_socket_inode_are_independently_refused() {
    for change_target in [false, true] {
        let fixture = Fixture::new();
        let target = fixture.0.join("engine.sock");
        let original = UnixListener::bind(&target).unwrap();
        let alias = fixture.0.join("docker.sock");
        symlink("engine.sock", &alias).unwrap();
        let mut owner = owner();
        let end = owner.end();
        let socket = Socket::open(&format!("unix://{}", alias.display()), &mut owner, end).unwrap();
        let replacement = if change_target {
            let replacement = UnixListener::bind(fixture.0.join("other.sock")).unwrap();
            symlink("other.sock", fixture.0.join("new-alias")).unwrap();
            fs::rename(fixture.0.join("new-alias"), &alias).unwrap();
            replacement
        } else {
            fs::rename(&target, fixture.0.join("retained-original.sock")).unwrap();
            UnixListener::bind(&target).unwrap()
        };
        assert_eq!(socket.check(), Err(Failure::Changed));
        assert_eq!(socket.finish(), Err(Failure::Changed));
        listener_close(original);
        listener_close(replacement);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn path_lookup_skips_nonexecutable_shadow_and_keeps_its_original_selected_tool() {
    let fixture = Fixture::new();
    let shadow = fixture.0.join("shadow");
    let selected = fixture.0.join("selected");
    directory(&shadow);
    directory(&selected);
    private(&shadow.join("docker"), b"nonexecutable shadow\n", 0o600);
    private(&selected.join("docker"), b"inert selected tool\n", 0o700);
    let environment = Environment::from([(
        "PATH".into(),
        std::env::join_paths([&shadow, &selected]).unwrap(),
    )]);
    let mut owner = owner();
    let end = owner.end();
    let tool = Tool::resolve("docker", &fixture.0, &environment, &mut owner, end).unwrap();
    assert_eq!(tool.path(), selected.join("docker"));
    tool.check().unwrap();
    // A new earlier executable does not transfer the already admitted selection.
    fs::set_permissions(shadow.join("docker"), fs::Permissions::from_mode(0o700)).unwrap();
    tool.check().unwrap();
    assert_eq!(tool.path(), selected.join("docker"));
    tool.finish().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn equal_byte_release_alias_exchange_is_negative_and_all_original_handles_close() {
    let fixture = Fixture::new();
    let mut owner = owner();
    let end = owner.end();
    let mut output = output(&fixture, "publication");
    output
        .json(
            "release.candidate.json",
            &json!({"revision":"fixture"}),
            &mut owner,
            end,
        )
        .unwrap();
    output
        .get_mut("release.candidate.json")
        .unwrap()
        .linked(OsStr::new("release.json"), &mut owner, end)
        .unwrap();
    let candidate = output.path().join("release.candidate.json");
    let published = output.path().join("release.json");
    let original = fs::metadata(&candidate).unwrap();
    let bytes = fs::read(&candidate).unwrap();
    assert_eq!(fs::metadata(&published).unwrap().ino(), original.ino());
    let foreign = output.path().join("foreign.json");
    private(&foreign, &bytes, 0o600);
    fs::rename(&foreign, &published).unwrap();
    let replacement = fs::metadata(&published).unwrap();
    assert_ne!(
        (replacement.dev(), replacement.ino()),
        (original.dev(), original.ino())
    );
    assert_eq!(fs::read(&published).unwrap(), bytes);
    assert_eq!(output.check(), Err(Failure::Changed));
    let originals = output.fixture_descriptors();
    files::fixture_observe_closes(None);
    assert_eq!(output.finish(), Err(Failure::Changed));
    every_original_closed_once(&originals, &files::fixture_take_closes());
    let after = fs::metadata(&published).unwrap();
    assert_eq!(
        (after.dev(), after.ino()),
        (replacement.dev(), replacement.ino())
    );
    assert_eq!(fs::read(published).unwrap(), bytes);
}

#[tokio::test(flavor = "current_thread")]
async fn copied_source_requires_its_intended_bytes_to_equal_the_original_readback() {
    let fixture = Fixture::new();
    let mut owner = owner();
    let bytes = source_bytes(b"intended source bytes\n");
    let (parent, mut archive) = archive(&fixture, &bytes, &mut owner);
    files::fixture_after_write_flip_once();
    assert_eq!(
        failed_extract(&mut archive, &fixture.0, &mut owner),
        Failure::Changed
    );
    assert!(!fixture.0.join("context").exists());
    let end = owner.end();
    assert_eq!(archive.bytes(bytes.len(), &mut owner, end).unwrap(), bytes);
    archive.finish().unwrap();
    parent.finish().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn json_and_cleanup_writers_reject_different_readback_without_replacing_first_cause() {
    for cleanup in [false, true] {
        let fixture = Fixture::new();
        let mut owner = owner();
        let end = owner.end();
        let mut output = output(&fixture, "intended-json");
        let value = json!({"member":"intended", "passed":false});
        let mut intended = serde_json::to_vec_pretty(&value).unwrap();
        intended.push(b'\n');
        if cleanup {
            owner.record(Failure::Command);
        }
        files::fixture_after_write_flip_once();
        let error = if cleanup {
            output.json_cleanup("result.json", &value, &owner, end)
        } else {
            output.json("result.json", &value, &mut owner, end)
        }
        .unwrap_err();
        assert_eq!(error, Failure::Changed);
        owner.record(error);
        assert_eq!(
            owner.primary(),
            Some(if cleanup {
                Failure::Command
            } else {
                Failure::Changed
            })
        );
        assert_ne!(
            fs::read(output.path().join("result.json")).unwrap(),
            intended
        );
        output.finish().unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn admitted_migration_pair_closes_after_original_deadline_and_tree_cleanup_retains_files() {
    fn member(builder: &mut tar::Builder<Vec<u8>>, name: &str, bytes: &[u8], directory: bool) {
        let mut header = tar::Header::new_gnu();
        header.set_mode(if directory { 0o755 } else { 0o644 });
        header.set_size(bytes.len() as u64);
        header.set_entry_type(if directory {
            tar::EntryType::Directory
        } else {
            tar::EntryType::Regular
        });
        header.set_cksum();
        builder
            .append_data(&mut header, name, Cursor::new(bytes))
            .unwrap();
    }

    let fixture = Fixture::new();
    let mut owner = owner();
    let end = owner.end();
    let inputs = [
        "Dockerfile",
        ".dockerignore",
        "Cargo.lock",
        "web/package-lock.json",
        "build/pip-constraints.txt",
    ];
    let mut builder = tar::Builder::new(Vec::new());
    for directory in ["build", "web", "migrations"] {
        member(&mut builder, directory, &[], true);
    }
    for name in inputs {
        member(&mut builder, name, b"pinned input", false);
    }
    member(
        &mut builder,
        "migrations/001_fixture.sql",
        b"SELECT 1;\n",
        false,
    );
    let (parent, mut archive) = archive(&fixture, &builder.into_inner().unwrap(), &mut owner);
    let tree = Tree::extract(&mut archive, &fixture.0, SOURCE_REVISION, &mut owner, end).unwrap();
    let tree_path = tree.path().to_owned();
    let tree_originals = tree.fixture_descriptors();
    let migration = tree_path.join("migrations/001_fixture.sql");
    // These pathname observations are comparison data, not admitted handle authority.
    let metadata = fs::metadata(&migration).unwrap();
    let identity = (metadata.dev(), metadata.ino());
    assert_eq!(metadata.len(), 10);
    let parent_identities: Vec<_> = migration
        .parent()
        .unwrap()
        .ancestors()
        .map(|path| {
            let metadata = fs::metadata(path).unwrap();
            (metadata.dev(), metadata.ino())
        })
        .collect();

    files::fixture_observe_closes(None);
    crate::release::build::source::fixture_expire_migration_hash_once();
    assert_eq!(
        tree.metadata(&mut owner, end).unwrap_err(),
        Failure::Deadline
    );
    assert_eq!(owner.primary(), Some(Failure::Deadline));
    assert_eq!(owner.now(), end);
    let observations = files::fixture_take_closes();
    let regular: Vec<_> = observations
        .iter()
        .enumerate()
        .filter(|(_, observation)| {
            observation.regular
                && observation.identity == Some(identity)
                && observation.bytes == Some(10)
        })
        .collect();
    assert!(
        regular.len() >= 2,
        "visible comparison and the original admitted file must close"
    );
    assert!(regular.iter().all(|(_, observation)| observation.succeeded));
    let (last, original) = *regular.last().unwrap();
    assert!(
        regular[..regular.len() - 1]
            .iter()
            .all(|(_, observation)| { observation.descriptor != original.descriptor }),
        "the final regular close must retire its distinct original owned descriptor"
    );
    for identity in parent_identities {
        assert!(
            observations[last + 1..].iter().any(|observation| {
                !observation.regular
                    && observation.identity == Some(identity)
                    && observation.succeeded
            }),
            "the admitted parent chain must explicitly close after the migration file"
        );
    }

    files::fixture_observe_closes(None);
    assert_eq!(tree.remove(&owner, end), Err(Failure::Deadline));
    every_original_closed_once(&tree_originals, &files::fixture_take_closes());
    assert_eq!(owner.primary(), Some(Failure::Deadline));
    assert!(tree_path.is_dir());
    assert_eq!(fs::read(&migration).unwrap(), b"SELECT 1;\n");
    let retained = fs::metadata(&migration).unwrap();
    assert_eq!((retained.dev(), retained.ino()), identity);
    for name in inputs {
        assert_eq!(fs::read(tree_path.join(name)).unwrap(), b"pinned input");
    }
    archive.finish().unwrap();
    parent.finish().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn exact_source_membership_refuses_extra_file_even_when_all_directory_seals_are_current() {
    let fixture = Fixture::new();
    let mut owner = owner();
    let end = owner.end();
    let bytes = source_bytes(b"known source member\n");
    let (parent, mut archive) = archive(&fixture, &bytes, &mut owner);
    let mut tree =
        Tree::extract(&mut archive, &fixture.0, SOURCE_REVISION, &mut owner, end).unwrap();
    let path = tree.path().to_owned();
    let originals = tree.fixture_descriptors();
    let known = path.join("partial.bin");
    let known_stamp = fs::metadata(&known).unwrap();
    let extra = path.join("unrecorded.txt");
    private(&extra, b"unrecorded context member\n", 0o644);
    let foreign = fs::metadata(&extra).unwrap();
    assert!(files::same(&known_stamp, &fs::metadata(&known).unwrap()));
    assert_eq!(fs::read(&known).unwrap(), b"known source member\n");

    // Refresh every directory growth observation explicitly. Only the extraction
    // journal's exact expected names can distinguish this state from valid input.
    tree.fixture_refresh_directory_metadata(&mut owner, end)
        .unwrap();
    assert_eq!(owner.primary(), None);
    assert_eq!(tree.check(&mut owner, end), Err(Failure::Changed));
    assert_eq!(owner.primary(), Some(Failure::Changed));
    files::fixture_observe_closes(None);
    assert_eq!(tree.remove(&owner, end), Err(Failure::Changed));
    every_original_closed_once(&originals, &files::fixture_take_closes());
    assert_eq!(owner.primary(), Some(Failure::Changed));
    assert!(path.is_dir());
    assert!(
        !known.exists(),
        "cleanup retires the original declared leaf only"
    );
    let retained = fs::metadata(&extra).unwrap();
    assert_eq!(
        (retained.dev(), retained.ino()),
        (foreign.dev(), foreign.ino())
    );
    assert_eq!(fs::read(extra).unwrap(), b"unrecorded context member\n");
    archive.finish().unwrap();
    parent.finish().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn exact_source_membership_refuses_unrecorded_file_created_before_initial_directory_seals() {
    let fixture = Fixture::new();
    let mut owner = owner();
    let bytes = source_bytes(b"known source member\n");
    let (parent, mut archive) = archive(&fixture, &bytes, &mut owner);
    files::fixture_observe_closes(None);
    crate::release::build::source::fixture_source_extra_before_seal_once();
    assert_eq!(
        failed_extract(&mut archive, &fixture.0, &mut owner),
        Failure::Changed
    );
    assert_eq!(owner.primary(), Some(Failure::Changed));
    let context = fixture.0.join("context");
    assert!(context.is_dir());
    assert!(!context.join("partial.bin").exists());
    let extra = context.join("unrecorded-before-seal.txt");
    assert!(extra.is_file(), "cleanup must retain the unrecorded member");
    let context = fs::metadata(&context).unwrap();
    assert!(
        files::fixture_take_closes().iter().any(|observation| {
            !observation.regular
                && observation.succeeded
                && observation.identity == Some((context.dev(), context.ino()))
        }),
        "the failed extraction must explicitly close its original context owners"
    );
    archive.finish().unwrap();
    parent.finish().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn synthetic_post_admission_io_refusal_retires_journaled_directory_and_empty_file() {
    use crate::release::build::source::{FixtureCreated, fixture_refuse_created_once};

    for kind in [FixtureCreated::Directory, FixtureCreated::File] {
        let fixture = Fixture::new();
        let mut owner = owner();
        let existing_directories: BTreeSet<_> = fixture
            .0
            .ancestors()
            .map(|path| {
                let metadata = fs::metadata(path).unwrap();
                (metadata.dev(), metadata.ino())
            })
            .collect();
        let mut builder = tar::Builder::new(Vec::new());
        let mut directory = tar::Header::new_gnu();
        directory.set_entry_type(tar::EntryType::Directory);
        directory.set_mode(0o755);
        directory.set_size(0);
        directory.set_cksum();
        builder
            .append_data(&mut directory, "nested", Cursor::new([]))
            .unwrap();
        let bytes = b"member must not be copied before refusal";
        let mut file = tar::Header::new_gnu();
        file.set_mode(0o644);
        file.set_size(bytes.len() as u64);
        file.set_cksum();
        builder
            .append_data(&mut file, "nested/leaf", Cursor::new(bytes))
            .unwrap();
        let (parent, mut archive) = archive(&fixture, &builder.into_inner().unwrap(), &mut owner);

        // This synthetic refusal follows successful native admission and journal
        // insertion. It does not claim to reproduce an operating-system fstat fault.
        files::fixture_observe_closes(None);
        fixture_refuse_created_once(kind);
        assert_eq!(
            failed_extract(&mut archive, &fixture.0, &mut owner),
            Failure::Io
        );
        assert_eq!(owner.primary(), Some(Failure::Io));
        assert!(!fixture.0.join("context/nested/leaf").exists());
        assert!(!fixture.0.join("context/nested").exists());
        assert!(!fixture.0.join("context").exists());
        archive.finish().unwrap();
        parent.finish().unwrap();

        let observations = files::fixture_take_closes();
        assert!(observations.iter().all(|observation| observation.succeeded));
        let created_directories: BTreeSet<_> = observations
            .iter()
            .filter(|observation| !observation.regular)
            .filter_map(|observation| observation.identity)
            .filter(|identity| !existing_directories.contains(identity))
            .collect();
        assert_eq!(
            created_directories.len(),
            2,
            "both the created context and nested directory must explicitly close"
        );
        assert_eq!(
            observations
                .iter()
                .any(|observation| observation.regular && observation.bytes == Some(0)),
            kind == FixtureCreated::File,
            "file refusal must explicitly close its admitted writer before any member bytes copy"
        );
    }
}
