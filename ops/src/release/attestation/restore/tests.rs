//! Whole retained filesystem/lease/effect regressions; no database or engine.
use super::super::{control::Cause, deploy::records, files};
use super::*;
use crate::test_support::Fixture;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, FileExt, OpenOptionsExt, PermissionsExt},
    path::Path,
    time::{Duration, Instant},
};
const REV: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const IMAGE: &str = concat!(
    "sha256:",
    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
);
const CID: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
fn directory(path: &Path) {
    fs::DirBuilder::new()
        .mode(0o700)
        .recursive(true)
        .create(path)
        .unwrap();
}
fn write(path: &Path, bytes: &[u8], mode: u32) {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    let written = file
        .write_all(bytes)
        .and_then(|()| file.flush())
        .and_then(|()| file.sync_all())
        .and_then(|()| file.set_permissions(fs::Permissions::from_mode(mode)));
    let closed = nix::unistd::close(file);
    written.unwrap();
    assert!(closed.is_ok());
}
struct Data {
    fixture: Fixture,
    access: Access,
}
impl Data {
    fn new(failed: bool, subset: bool, hardlink: bool) -> Self {
        let fixture = Fixture::new();
        let base = &fixture.0;
        for name in [
            "public/results/release.fixture000",
            "public/releases",
            "config",
            "work",
            "state",
            "library",
            "tools",
        ] {
            directory(&base.join(name));
        }
        directory(&base.join("public/releases").join(REV));
        fs::set_permissions(base.join("state"), fs::Permissions::from_mode(0o750)).unwrap();
        let dump = b"PGDMP opaque inert whole-backup bytes";
        let checksum = files::hex(&Sha256::digest(dump));
        let migrations = json!({"1":"d".repeat(96),"2":"e".repeat(96)});
        let recorded = if subset {
            json!({"1":"d".repeat(96)})
        } else {
            migrations.clone()
        };
        let outcome = json!({"passed":!failed,"revision":REV,"backupSha256":checksum,
            "backupMigrations":recorded});
        write(
            &base.join("public/results/release.fixture000/outcome.json"),
            &serde_json::to_vec(&outcome).unwrap(),
            0o600,
        );
        write(
            &base.join("public/results/release.fixture000/database-before.dump"),
            dump,
            0o600,
        );
        if hardlink {
            fs::hard_link(
                base.join("public/results/release.fixture000/database-before.dump"),
                base.join("backup-alias"),
            )
            .unwrap();
        }
        let manifest = json!({"schemaVersion":1,"revision":REV,"platform":"linux/amd64",
            "archiveSha256":"f".repeat(64),
            "imageTag":format!("simplestchat-release/production:{REV}"),
            "migrations":migrations,"createdAt":"2026-10-24T00:00:00Z"});
        write(
            &base.join("public/releases").join(REV).join("release.json"),
            &serde_json::to_vec(&manifest).unwrap(),
            0o600,
        );
        write(
            &base.join("library/restore-verify.sql"),
            b"SELECT fixed_synthetic_counts;",
            0o644,
        );
        write(
            &base.join("tools/docker"),
            b"inert fixed tool, never executed",
            0o700,
        );
        let access = Access {
            root: base.join("public"),
            config: base.join("config"),
            work: base.join("work"),
            state: base.join("state"),
            library: base.join("library"),
            docker: base.join("tools/docker"),
            socket: None,
            uid: rustix::process::geteuid().as_raw(),
            nonce: Some([0x11; 16]),
            timestamp: Some("2026-10-24T00:00:00Z".into()),
            virtual_poll: true,
            fault: None,
        };
        Self { fixture, access }
    }
}
#[derive(Clone, Copy)]
enum Mode {
    Normal,
    UnknownCreate,
    KnownCloseAndRemoveFailure,
    LateCreate,
    WrongJoin,
}
struct Inert {
    mode: Mode,
    roles: Vec<Role>,
    name: String,
    held: Vec<bool>,
    remaining: Option<Duration>,
    subset: bool,
    input_readonly: bool,
    removed: bool,
}
impl Inert {
    fn new(mode: Mode, subset: bool) -> Self {
        Self {
            mode,
            roles: Vec::new(),
            name: String::new(),
            held: Vec::new(),
            remaining: None,
            subset,
            input_readonly: true,
            removed: false,
        }
    }
}
impl Executor for Inert {
    async fn run(&mut self, owner: &mut Owner, request: Invocation<'_>) -> CommandOutput {
        self.roles.push(request.role);
        let lock = fs::OpenOptions::new().read(true).write(true).open(
            request
                .program
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("work/workload.lock"),
        );
        if let Ok(file) = lock {
            let acquired =
                rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive);
            let closed = nix::unistd::close(file);
            self.held.push(acquired.is_err() && closed.is_ok());
        } else {
            self.held.push(false);
        }
        if let Some(file) = request.input {
            let flags = rustix::fs::fcntl_getfl(file);
            self.input_readonly &= flags.is_ok_and(|flags| {
                flags.intersection(rustix::fs::OFlags::ACCMODE) == rustix::fs::OFlags::RDONLY
            });
            let mut source = file;
            let mut bytes = Vec::new();
            let _ = source.read_to_end(&mut bytes);
        }
        let mut status = 0;
        let mut error = None;
        let mut settled = true;
        let stdout = match request.role {
            Role::Inspect => IMAGE.as_bytes().to_vec(),
            Role::Create => {
                if let Some(at) = request.arguments.iter().position(|s| s == "--name") {
                    self.name = request.arguments[at + 1].to_string_lossy().into_owned();
                }
                match self.mode {
                    Mode::UnknownCreate | Mode::WrongJoin => status = 7,
                    Mode::KnownCloseAndRemoveFailure => {
                        error = Some(Cause::Close);
                        settled = false;
                    }
                    Mode::LateCreate => {
                        owner.observe_at(owner.body_end() - Duration::from_secs(160))
                    }
                    Mode::Normal => {}
                }
                CID.as_bytes().to_vec()
            }
            Role::Ledger => {
                if self.subset {
                    format!("1|t|{}", "d".repeat(96)).into_bytes()
                } else {
                    format!("1|t|{}\n2|t|{}", "d".repeat(96), "e".repeat(96)).into_bytes()
                }
            }
            Role::Verify => b"{\"users\":1,\"rooms\":1}".to_vec(),
            Role::CleanupSelect => {
                self.remaining = Some(owner.body_end().saturating_duration_since(owner.now()));
                CID.as_bytes().to_vec()
            }
            Role::CleanupInspect => serde_json::to_vec(&json!({"id":CID,
                "name":format!("/{}",self.name),"image":IMAGE,
                "owner":if matches!(self.mode,Mode::WrongJoin){"foreign"}else{&self.name}}))
            .unwrap(),
            Role::CleanupRemove => {
                if matches!(self.mode, Mode::KnownCloseAndRemoveFailure) {
                    status = 7;
                } else {
                    self.removed = true;
                }
                Vec::new()
            }
            _ => Vec::new(),
        };
        let output = request.stdout.write_at(&stdout, 0);
        let errors = request.stderr.write_at(&[], 0);
        if output.as_ref().is_err() || errors.is_err() {
            error = Some(Cause::Io);
            settled = false;
        }
        CommandOutput {
            stdout_bytes: stdout.len() as u64,
            stderr_bytes: 0,
            stdout,
            status: Some(status),
            settled,
            stdout_complete: true,
            error,
        }
    }
}
struct Case {
    passed: bool,
    cause: Option<Cause>,
    failure: Option<String>,
    closed: (usize, usize),
    dirs: (usize, usize),
    held_until_output: bool,
    unlocked_after: bool,
    report: Option<Value>,
    fresh: bool,
    snapshot_exists: bool,
    roles: Vec<Role>,
    removed: bool,
    remaining: Option<Duration>,
    inputs_readonly: bool,
    fault_hits: usize,
    snapshot_unlinks: usize,
}
fn run(data: Data, mode: Mode, subset: bool) -> Case {
    let start = Instant::now();
    let mut owner = Owner::new(start, Duration::from_secs(1200), Duration::from_secs(120)).unwrap();
    let options = Options {
        attempt: "release.fixture000".into(),
        whole: Duration::from_secs(1200),
        reserve: Duration::from_secs(120),
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let mut inert = Inert::new(mode, subset);
    records::start_probe();
    files::start_probe(None);
    let outcome = runtime.block_on(execution::execute(
        &options,
        &data.access,
        &mut owner,
        &mut inert,
    ));
    drop(runtime);
    let passed = outcome.passed;
    let failure = outcome.failure.clone();
    let fault_hits = outcome.fault_hits;
    let snapshot_unlinks = outcome.snapshot_unlinks;
    let probe = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(data.access.work.join("workload.lock"));
    let mut held = false;
    let mut after = false;
    if let Ok(file) = &probe {
        held =
            rustix::fs::flock(file, rustix::fs::FlockOperation::NonBlockingLockExclusive).is_err();
    }
    outcome.finish(&mut owner);
    if let Ok(file) = probe {
        after =
            rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive).is_ok();
        let _ = rustix::fs::flock(&file, rustix::fs::FlockOperation::Unlock);
        after &= nix::unistd::close(file).is_ok();
    }
    let closed = records::take_probe();
    let dirs = files::take_probe();
    let cause = owner.finish().err();
    let attempts = data.access.state.join("restores");
    let attempt = fs::read_dir(&attempts)
        .ok()
        .and_then(|mut rows| rows.next())
        .and_then(|row| row.ok())
        .map(|row| row.path());
    let report = attempt
        .as_ref()
        .and_then(|p| fs::read(p.join("restore.json")).ok())
        .and_then(|data| serde_json::from_slice(&data).ok());
    let snapshot_exists = attempt
        .as_ref()
        .is_some_and(|p| p.join("snapshot.dump").exists());
    let fresh = data
        .access
        .state
        .join("restore-verified.timestamp")
        .exists();
    drop(data.fixture);
    Case {
        passed,
        cause,
        failure,
        closed,
        dirs,
        held_until_output: held && inert.held.iter().all(|s| *s),
        unlocked_after: after,
        report,
        fresh,
        snapshot_exists,
        roles: inert.roles,
        removed: inert.removed,
        remaining: inert.remaining,
        inputs_readonly: inert.input_readonly,
        fault_hits,
        snapshot_unlinks,
    }
}
fn peers(case: &Case) {
    assert!(case.closed.0 > 0 && case.dirs.0 > 0);
    assert_eq!(case.closed.1, 0);
    assert_eq!(case.dirs.1, 0);
    assert!(case.held_until_output && case.unlocked_after && case.inputs_readonly);
}
#[test]
fn complete_operation_accepts_failed_release_subset_and_existing_readonly_input_alias() {
    let case = run(Data::new(true, true, true), Mode::Normal, true);
    peers(&case);
    assert!(case.passed && case.fresh && case.removed && !case.snapshot_exists);
    assert_eq!(case.cause, None);
    assert_eq!(case.report.as_ref().unwrap()["verified"], true);
    assert_eq!(case.report.as_ref().unwrap()["cleanupPassed"], true);
}
#[test]
fn joined_unknown_create_recovery_removes_only_the_generated_matching_identity() {
    let case = run(Data::new(false, false, false), Mode::UnknownCreate, false);
    peers(&case);
    assert!(!case.passed && !case.fresh && case.removed && !case.snapshot_exists);
    assert_eq!(case.cause, Some(Cause::Command));
    assert_eq!(case.report.as_ref().unwrap()["containerId"], "");
    assert_eq!(case.report.as_ref().unwrap()["cleanupPassed"], true);
}
#[test]
fn complete_known_create_identity_survives_close_and_paired_cleanup_nonzero() {
    let case = run(
        Data::new(false, false, false),
        Mode::KnownCloseAndRemoveFailure,
        false,
    );
    peers(&case);
    assert_eq!(case.cause, Some(Cause::Close));
    assert_eq!(case.failure.as_deref(), Some("native_close_refused"));
    assert_eq!(case.report.as_ref().unwrap()["containerId"], CID);
    assert!(!case.removed && !case.fresh);
    assert!(case.roles.contains(&Role::CleanupRemove));
}
#[test]
fn post_create_original_candidate_deadline_keeps_cleanup160_inside_body() {
    let case = run(Data::new(false, false, false), Mode::LateCreate, false);
    peers(&case);
    assert_eq!(case.cause, Some(Cause::Deadline));
    assert_eq!(case.remaining, Some(Duration::from_secs(160)));
    assert!(case.removed && !case.fresh && !case.passed);
    assert_eq!(case.report.as_ref().unwrap()["containerId"], CID);
}
#[test]
fn a_wrong_join_refuses_exact_id_removal_without_broad_name_cleanup() {
    let case = run(Data::new(false, false, false), Mode::WrongJoin, false);
    peers(&case);
    assert_eq!(case.cause, Some(Cause::Command));
    assert!(!case.removed && !case.roles.contains(&Role::CleanupRemove));
    assert!(!case.passed && !case.fresh);
}
#[test]
fn snapshot_unlink_refusal_closes_once_and_never_publishes_freshness() {
    let mut data = Data::new(false, false, false);
    data.access.fault = Some(Fault::SnapshotUnlink);
    let case = run(data, Mode::Normal, false);
    peers(&case);
    assert_eq!(case.cause, Some(Cause::Io));
    assert!(case.snapshot_exists && case.removed && !case.fresh && !case.passed);
    assert_eq!(case.fault_hits, 1);
    assert_eq!(case.snapshot_unlinks, 1);
}
