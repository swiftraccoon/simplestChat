//! Inert process and isolated tiny local Git fixtures.

use super::*;
use crate::{ci::process::Native, owned_process::Signals};
use std::{
    fs,
    io::Write,
    os::unix::fs::{PermissionsExt, symlink},
    path::PathBuf,
    process::{Command as ChildCommand, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    root: PathBuf,
    repo: PathBuf,
    output: PathBuf,
    tool: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "ops-source-fixture-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let repo = root.join("repo");
        fs::create_dir_all(repo.join(".git/objects/info")).unwrap();
        let tool = root.join("fake-git");
        fs::write(&tool, b"#!/bin/sh\nexit 129\n").unwrap();
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            output: root.join("export"),
            root,
            repo,
            tool,
        }
    }
    fn request(&self, revision: &str) -> Request {
        let mut value = super::tests::wire();
        value["repository"] = self.repo.to_str().unwrap().into();
        value["git"] = serde_json::json!({"path":self.tool,"bytes":fs::metadata(&self.tool).unwrap().len(),
            "sha256":sha(&fs::read(&self.tool).unwrap())});
        value["output"] = self.output.to_str().unwrap().into();
        value["selection"]["revision"] = revision.into();
        let bytes = serde_json::to_vec(&value).unwrap();
        Request::decode(&bytes, &sha(&bytes)).unwrap()
    }
    fn real(&mut self) {
        fs::remove_dir_all(&self.repo).unwrap();
        self.tool = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .filter_map(|directory| directory.join("git").canonicalize().ok())
            .find(|path| fs::metadata(path).is_ok_and(|m| m.is_file()))
            .expect("owned Git fixtures need an installed regular Git");
        fs::create_dir(self.root.join("templates")).unwrap();
        fs::create_dir(self.root.join("hooks")).unwrap();
        self.git(
            &[
                "init",
                "--initial-branch=fixture",
                "--template",
                self.root.join("templates").to_str().unwrap(),
                self.repo.to_str().unwrap(),
            ],
            None,
        );
    }
    fn git(&self, args: &[&str], input: Option<&[u8]>) -> Vec<u8> {
        let mut command = ChildCommand::new(&self.tool);
        command
            .args([
                "-c",
                "user.name=Owned Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
            ])
            .arg(format!(
                "core.hooksPath={}",
                self.root.join("hooks").display()
            ))
            .args(args)
            .env_clear()
            .env("HOME", &self.root)
            .env("LC_ALL", "C")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .current_dir(if self.repo.exists() {
                &self.repo
            } else {
                &self.root
            })
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().unwrap();
        if let Some(input) = input {
            child.stdin.take().unwrap().write_all(input).unwrap();
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "owned fixture Git failed: {:?}",
            output.stderr
        );
        output.stdout
    }
    fn commit(&self) -> String {
        self.git(&["add", "--all"], None);
        self.git(&["commit", "-m", "owned inert fixture"], None);
        catalog::oid(&self.git(&["rev-parse", "HEAD"], None)).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

struct Setup {
    repository: Repository,
    run: OwnedRun,
    view: view::View,
    environment: Environment,
    budget: Budget,
}
impl Setup {
    fn new(request: &Request) -> Self {
        let budget = Budget::default();
        let repository = Repository::open(&request.repository, &budget).unwrap();
        let run = OwnedRun::local(&request.output, &budget).unwrap();
        let view = view::View::create(&run).unwrap();
        let environment = environment(&run, &repository).unwrap();
        Self {
            repository,
            run,
            view,
            environment,
            budget,
        }
    }
    async fn run<E: Executor>(&self, request: &Request, executor: &mut E) -> Result<Vec<u8>> {
        let clock = NativeClock;
        let deadline = Deadline::new(&clock);
        let git = Arc::new(Git::open(&request.git, &self.budget, executor, &deadline).await?);
        let _native = self.budget.reserve(16)?;
        let context = Context {
            git: &git,
            repository: &self.repository,
            run: &self.run,
            view: &self.view,
            deadline: &deadline,
        };
        let result = export(request, &context, executor).await;
        let closed = Arc::try_unwrap(git)
            .map_err(|_| failure(1))
            .and_then(Git::finish);
        result.and_then(|value| closed.map(|()| value))
    }
    fn finish(self) {
        self.view.finish(&self.run).unwrap();
        self.repository.finish().unwrap();
        self.run.finish().unwrap();
        assert_eq!(self.budget.count(), 0);
    }
}
fn make_signals() -> (tokio::sync::mpsc::UnboundedSender<u8>, Signals) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    (tx, Signals::injected(rx))
}
struct Fake {
    calls: usize,
    mutation: Option<(PathBuf, Vec<u8>)>,
    failed: bool,
}
impl Executor for Fake {
    async fn run(&mut self, command: Command) -> Result<Vec<u8>> {
        assert_eq!(command.program, Program::OpsGit);
        assert!(command.source_git.is_some());
        command.source_git.as_ref().unwrap().validate()?;
        let args = command
            .arguments
            .iter()
            .map(|x| x.to_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            &args[..6],
            &[
                "--no-pager",
                "--no-replace-objects",
                "--no-lazy-fetch",
                "--no-optional-locks",
                "--git-dir",
                "git-view"
            ]
        );
        self.calls += 1;
        let result = match &args[6..] {
            ["cat-file", "-t", _] => b"commit\n".to_vec(),
            ["rev-parse", "--verify", "--end-of-options", _] => {
                format!("{}\n", "b".repeat(40)).into_bytes()
            }
            ["ls-tree", ..] => format!("100755 blob {} 7\trun\0", "a".repeat(40)).into_bytes(),
            ["cat-file", "commit", _] => {
                format!("tree {}\n\nPRIVATE AUTHOR\n", "b".repeat(40)).into_bytes()
            }
            ["cat-file", "tree", _] => b"fake raw tree".to_vec(),
            ["cat-file", "blob", _] => b"PRIVATE".to_vec(),
            ["hash-object", "--no-filters", "-t", kind, "--", path] => {
                assert!(Path::new(path).is_absolute());
                let selected = match *kind {
                    "commit" => 'c',
                    "tree" => 'b',
                    "blob" => 'a',
                    _ => panic!(),
                };
                if *kind == "blob"
                    && let Some((path, bytes)) = self.mutation.take()
                {
                    fs::write(path, bytes).unwrap();
                }
                format!("{}\n", selected.to_string().repeat(40)).into_bytes()
            }
            _ => panic!("unexpected fixed command"),
        };
        if self.failed {
            Err(failure(127))
        } else {
            Ok(result)
        }
    }
    async fn ready(&mut self) -> Result<()> {
        Ok(())
    }
}
#[tokio::test]
async fn fake_export_binds_request_git_modes_frame_and_private_projection() {
    let fixture = Fixture::new();
    let request = fixture.request(&"c".repeat(40));
    let setup = Setup::new(&request);
    let mut fake = Fake {
        calls: 0,
        mutation: None,
        failed: false,
    };
    let result = setup.run(&request, &mut fake).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&result).unwrap();
    assert_eq!(value["source"]["profile"], "ops-producer-tree-v1");
    assert_eq!(value["source"]["files"], 1);
    assert_eq!(value["source"]["entries"], 2);
    assert_eq!(value["source"]["bytes"], 7);
    assert_eq!(value["requestSha256"], request.sha256);
    assert_eq!(value["gitSha256"], request.git.sha256);
    assert_eq!(value["selection"]["revision"], "c".repeat(40));
    assert!(value.get("passed").is_none());
    assert!(!String::from_utf8(result).unwrap().contains("PRIVATE"));
    assert_eq!(
        fs::metadata(fixture.output.join("source/run"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o700
    );
    setup.finish();
}
#[tokio::test]
async fn destination_change_and_known_command_failure_never_publish_observation() {
    for failed in [false, true] {
        let fixture = Fixture::new();
        let request = fixture.request(&"c".repeat(40));
        let setup = Setup::new(&request);
        let mut fake = Fake {
            calls: 0,
            failed,
            mutation: Some((fixture.output.join("source/run"), b"CHANGED".to_vec())),
        };
        assert_eq!(
            setup.run(&request, &mut fake).await.unwrap_err().code,
            if failed { 127 } else { 1 }
        );
        assert!(!fixture.output.join("source-observation.json").exists());
        setup.finish();
    }
}
#[tokio::test]
async fn git_hash_binding_missing_proof_pending_signal_and_unsupported_flags_fail_closed() {
    let fixture = Fixture::new();
    let mut request = fixture.request(&"c".repeat(40));
    let setup = Setup::new(&request);
    request.git.sha256 = "f".repeat(64);
    let mut fake = Fake {
        calls: 0,
        mutation: None,
        failed: false,
    };
    assert!(setup.run(&request, &mut fake).await.is_err());
    assert_eq!(fake.calls, 0);
    request.git.sha256 = sha(&fs::read(&fixture.tool).unwrap());
    let (tx, signals) = make_signals();
    let mut native = Native::new(&setup.run.path, &setup.environment, signals);
    assert_eq!(
        native
            .run(Command::capture(Program::OpsGit, &["--version"]))
            .await
            .unwrap_err()
            .code,
        1
    );
    assert_eq!(
        setup.run(&request, &mut native).await.unwrap_err().code,
        129
    );
    assert!(native.cleanup_passed());
    tx.send(143).unwrap();
    assert_eq!(
        setup.run(&request, &mut native).await.unwrap_err().code,
        143
    );
    drop(native);
    setup.finish();
}
#[tokio::test]
async fn real_git_exports_committed_bytes_ignoring_dirty_worktree_config_and_archive_attributes() {
    let mut fixture = Fixture::new();
    fixture.real();
    fs::create_dir(fixture.repo.join("nested")).unwrap();
    fs::write(fixture.repo.join("nested/space name"), b"committed\0raw\n").unwrap();
    fs::write(fixture.repo.join("run"), b"#!/bin/false\n").unwrap();
    fs::set_permissions(fixture.repo.join("run"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(
        fixture.repo.join(".gitattributes"),
        b"run export-ignore\nnested/* export-subst\n",
    )
    .unwrap();
    let revision = fixture.commit();
    fs::write(
        fixture.repo.join("nested/space name"),
        b"DIRTY PRIVATE BYTES",
    )
    .unwrap();
    fs::write(fixture.repo.join("untracked"), b"not selected").unwrap();
    fs::write(
        fixture.repo.join(".git/config"),
        b"[include]\npath=/PRIVATE/MISSING/config\n",
    )
    .unwrap();
    let request = fixture.request(&revision);
    let setup = Setup::new(&request);
    assert_eq!(
        setup
            .environment
            .get(std::ffi::OsStr::new("GIT_OBJECT_DIRECTORY"))
            .unwrap(),
        fixture.repo.join(".git/objects").as_os_str()
    );
    assert!(!setup.environment.contains_key(std::ffi::OsStr::new("PATH")));
    let (_tx, signals) = make_signals();
    let mut native = Native::new(&setup.run.path, &setup.environment, signals);
    let result = setup.run(&request, &mut native).await.unwrap();
    assert!(native.cleanup_passed());
    drop(native);
    assert_eq!(
        fs::read(fixture.output.join("source/nested/space name")).unwrap(),
        b"committed\0raw\n"
    );
    assert!(fixture.output.join("source/run").exists());
    assert!(!fixture.output.join("source/untracked").exists());
    let value: serde_json::Value = serde_json::from_slice(&result).unwrap();
    assert_eq!(value["commit"], revision);
    assert!(!String::from_utf8(result).unwrap().contains("DIRTY"));
    setup.finish();
}

#[test]
fn ordinary_object_database_and_fresh_output_reject_links_alternates_and_reuse() {
    let fixture = Fixture::new();
    let budget = Budget::default();
    for name in [
        ".git/commondir",
        ".git/gitdir",
        ".git/objects/info/alternates",
        ".git/objects/info/http-alternates",
    ] {
        fs::write(fixture.repo.join(name), b"PRIVATE").unwrap();
        assert!(Repository::open(&fixture.repo, &budget).is_err());
        fs::remove_file(fixture.repo.join(name)).unwrap();
        assert_eq!(budget.count(), 0);
    }
    fs::rename(
        fixture.repo.join(".git/objects"),
        fixture.root.join("objects"),
    )
    .unwrap();
    symlink(
        fixture.root.join("objects"),
        fixture.repo.join(".git/objects"),
    )
    .unwrap();
    assert!(Repository::open(&fixture.repo, &budget).is_err());
    let run = OwnedRun::local(&fixture.output, &budget).unwrap();
    assert!(OwnedRun::local(&fixture.output, &budget).is_err());
    run.finish().unwrap();
    assert_eq!(budget.count(), 0);
}

#[tokio::test]
async fn real_git_preserves_explicit_empty_tree_and_refuses_committed_symlink() {
    let mut fixture = Fixture::new();
    fixture.real();
    let empty = catalog::oid(&fixture.git(&["mktree"], Some(b""))).unwrap();
    let blob =
        catalog::oid(&fixture.git(&["hash-object", "-w", "--stdin"], Some(b"data"))).unwrap();
    let raw = format!("040000 tree {empty}\tempty\n100644 blob {blob}\tdata\n");
    let tree = catalog::oid(&fixture.git(&["mktree"], Some(raw.as_bytes()))).unwrap();
    let revision =
        catalog::oid(&fixture.git(&["commit-tree", &tree, "-m", "owned empty tree"], None))
            .unwrap();
    let request = fixture.request(&revision);
    let setup = Setup::new(&request);
    let (_tx, signals) = make_signals();
    let mut native = Native::new(&setup.run.path, &setup.environment, signals);
    let result = setup.run(&request, &mut native).await.unwrap();
    drop(native);
    assert!(fixture.output.join("source/empty").is_dir());
    let value: serde_json::Value = serde_json::from_slice(&result).unwrap();
    assert_eq!(value["source"]["entries"], 3);
    setup.finish();

    let mut fixture = Fixture::new();
    fixture.real();
    symlink("/PRIVATE/OUTSIDE", fixture.repo.join("link")).unwrap();
    let revision = fixture.commit();
    let request = fixture.request(&revision);
    let setup = Setup::new(&request);
    let (_tx, signals) = make_signals();
    let mut native = Native::new(&setup.run.path, &setup.environment, signals);
    assert!(setup.run(&request, &mut native).await.is_err());
    drop(native);
    assert!(!fixture.output.join("source").exists());
    setup.finish();
}

#[tokio::test]
async fn real_git_nested_tree_corruption_cannot_relabel_replacement_blob_as_selected_commit() {
    use flate2::{Compression, write::ZlibEncoder};
    let mut fixture = Fixture::new();
    fixture.real();
    fs::create_dir(fixture.repo.join("nested")).unwrap();
    fs::write(fixture.repo.join("nested/data"), b"original").unwrap();
    let revision = fixture.commit();
    let subtree = catalog::oid(&fixture.git(&["rev-parse", "HEAD:nested"], None)).unwrap();
    let replacement =
        catalog::oid(&fixture.git(&["hash-object", "-w", "--stdin"], Some(b"replaced"))).unwrap();
    let mut raw = fixture.git(&["cat-file", "tree", &subtree], None);
    let start = raw.iter().position(|byte| *byte == 0).unwrap() + 1;
    for i in 0..20 {
        raw[start + i] = u8::from_str_radix(&replacement[2 * i..2 * i + 2], 16).unwrap();
    }
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    write!(encoder, "tree {}\0", raw.len()).unwrap();
    encoder.write_all(&raw).unwrap();
    let path = fixture
        .repo
        .join(".git/objects")
        .join(&subtree[..2])
        .join(&subtree[2..]);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(path, encoder.finish().unwrap()).unwrap();
    // Real ls-tree succeeds and reports the replacement blob under the old
    // subtree OID. Checking only blobs and a repeated catalogue would pass.
    let listed = fixture.git(&["ls-tree", "-r", "-t", "-l", "-z", "HEAD"], None);
    assert!(
        listed
            .windows(40)
            .any(|part| part == replacement.as_bytes())
    );
    let request = fixture.request(&revision);
    let setup = Setup::new(&request);
    let (_tx, signals) = make_signals();
    let mut native = Native::new(&setup.run.path, &setup.environment, signals);
    assert_eq!(setup.run(&request, &mut native).await.unwrap_err().code, 1);
    assert!(native.cleanup_passed());
    drop(native);
    assert!(!fixture.output.join("source").exists());
    assert!(!fixture.output.join("source-observation.json").exists());
    setup.finish();
}

#[tokio::test]
async fn unlisted_output_and_equal_size_blob_changes_refuse_success() {
    let fixture = Fixture::new();
    let request = fixture.request(&"c".repeat(40));
    let setup = Setup::new(&request);
    let mut fake = Fake {
        calls: 0,
        failed: false,
        mutation: Some((fixture.output.join("source/unlisted"), b"extra".to_vec())),
    };
    assert!(setup.run(&request, &mut fake).await.is_err());
    assert!(fixture.output.join("source/unlisted").exists());
    assert!(!fixture.output.join("source-observation.json").exists());
    setup.finish();
}

#[tokio::test]
async fn retained_git_stamp_and_closed_role_cannot_be_replaced_or_reused() {
    let fixture = Fixture::new();
    let alias = fixture.root.join("installed-git-alias");
    fs::hard_link(&fixture.tool, &alias).unwrap();
    let request = fixture.request(&"c".repeat(40));
    let setup = Setup::new(&request);
    let clock = NativeClock;
    let deadline = Deadline::new(&clock);
    let mut fake = Fake {
        calls: 0,
        mutation: None,
        failed: false,
    };
    let git = Arc::new(
        Git::open(&request.git, &setup.budget, &mut fake, &deadline)
            .await
            .unwrap(),
    );
    let (_tx, signals) = make_signals();
    let mut native = Native::new(&setup.run.path, &setup.environment, signals);
    let mut wrong = Command::capture(Program::Git, &["--version"]);
    wrong.source_git = Some(git.clone());
    assert_eq!(native.run(wrong).await.unwrap_err().code, 1);
    assert!(native.cleanup_passed());
    drop(native);
    // Installed aliases are allowed, but changing that observed link count
    // retires the exact same proof even before executable bytes change.
    fs::remove_file(alias).unwrap();
    assert!(git.validate().is_err());
    fs::write(&fixture.tool, b"#!/bin/sh\nexit 128\n").unwrap();
    assert!(git.validate().is_err());
    assert!(Arc::try_unwrap(git).unwrap().finish().is_err());
    assert_eq!(fake.calls, 0);
    setup.finish();
}

#[test]
fn fresh_view_config_and_namespace_cannot_adopt_replacements() {
    for name in ["config", "refs", "extra"] {
        let fixture = Fixture::new();
        let request = fixture.request(&"c".repeat(40));
        let setup = Setup::new(&request);
        let path = fixture.output.join("git-view").join(name);
        if name == "refs" {
            fs::set_permissions(path, fs::Permissions::from_mode(0o777)).unwrap();
        } else {
            fs::write(path, b"PRIVATE").unwrap();
        }
        assert!(setup.view.check(&setup.run).is_err());
        assert!(setup.view.finish(&setup.run).is_err());
        setup.repository.finish().unwrap();
        setup.run.finish().unwrap();
        assert_eq!(setup.budget.count(), 0);
    }
}

struct Finish {
    calls: usize,
    interrupt: Option<usize>,
    replay_failure: Option<u8>,
    settled: bool,
    unsettle_on_replay: bool,
}
impl Executor for Finish {
    async fn run(&mut self, _: Command) -> Result<Vec<u8>> {
        panic!("no finalization command");
    }
    async fn ready(&mut self) -> Result<()> {
        self.calls += 1;
        if self.interrupt == Some(self.calls) {
            Err(failure(143))
        } else {
            Ok(())
        }
    }
    async fn replay(&mut self, bytes: &[u8], stderr: bool, _: std::time::Instant) -> Result<()> {
        assert_eq!(bytes, b"{}\n");
        assert!(!stderr);
        if self.unsettle_on_replay {
            self.settled = false;
        }
        self.replay_failure
            .map_or(Ok(()), |code| Err(failure(code)))
    }
    fn cleanup_passed(&self) -> bool {
        self.settled
    }
}
#[tokio::test]
async fn final_close_replay_late_signal_deadline_and_settlement_fence_zero() {
    use crate::ci::ops_artifact::fixture::FakeClock;
    let clock = FakeClock::new();
    let deadline = Deadline::new(&clock);
    let budget = Budget::default();
    let mut finish = Finish {
        calls: 0,
        interrupt: None,
        replay_failure: None,
        settled: true,
        unsettle_on_replay: false,
    };
    assert_eq!(
        complete(
            Err(failure(100)),
            Err(failure(1)),
            &budget,
            &mut finish,
            &deadline
        )
        .await
        .unwrap_err()
        .code,
        100
    );
    assert_eq!(finish.calls, 0);
    assert!(
        complete(
            Ok(b"{}".to_vec()),
            Err(failure(1)),
            &budget,
            &mut finish,
            &deadline
        )
        .await
        .is_err()
    );
    finish.replay_failure = Some(130);
    assert_eq!(
        complete(Ok(b"{}".to_vec()), Ok(()), &budget, &mut finish, &deadline)
            .await
            .unwrap_err()
            .code,
        130
    );
    finish.replay_failure = None;
    finish.calls = 0;
    finish.interrupt = Some(2);
    assert_eq!(
        complete(Ok(b"{}".to_vec()), Ok(()), &budget, &mut finish, &deadline)
            .await
            .unwrap_err()
            .code,
        143
    );
    finish.interrupt = None;
    finish.unsettle_on_replay = true;
    assert!(
        complete(Ok(b"{}".to_vec()), Ok(()), &budget, &mut finish, &deadline)
            .await
            .is_err()
    );
    finish.unsettle_on_replay = false;
    finish.settled = true;
    clock.0.set(clock.now() + Duration::from_secs(20 * 60));
    assert_eq!(
        complete(Ok(b"{}".to_vec()), Ok(()), &budget, &mut finish, &deadline)
            .await
            .unwrap_err()
            .code,
        124
    );
    assert_eq!(budget.count(), 0);
}
