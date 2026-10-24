//! Owned inert host transactions. External command observations are supplied;
//! actual Rust request, journal, worker, collection and recovery policy runs.
//! These tests do not prove authentication, systemd, Docker or Linux FD-exec.

use super::{
    Failure, Result,
    control::Owner,
    files::{self, Access, Fs},
    host::{CURRENT, ROOT, Session, Stage},
    process::{Output, Runner},
    protocol::{self, Action, Header, Members, Mode, Pin, ResultData, Source, Workload},
};
use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell},
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
};

const SECOND: u64 = 1_000_000_000;
const RUN: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const REVISION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const INNER: &str = "cccccccccccccccc";

fn owned() -> String {
    "1".repeat(64)
}
fn unknown() -> String {
    "2".repeat(64)
}
fn server() -> String {
    format!("sha256:{}", "d".repeat(64))
}
fn generator() -> String {
    format!("sha256:{}", "e".repeat(64))
}

pub(super) struct Temporary(PathBuf);
impl Temporary {
    pub(super) fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "ops-capacity-host-{}-{}-{}",
            std::process::id(),
            super::control::now().unwrap(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
    pub(super) fn path(&self, absolute: &str) -> PathBuf {
        self.0.join(absolute.trim_start_matches('/'))
    }
    pub(super) fn directory(&self, absolute: &str, mode: u32) {
        let path = self.path(absolute);
        fs::create_dir_all(&path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }
    pub(super) fn file(&self, absolute: &str, bytes: &[u8], mode: u32) {
        let path = self.path(absolute);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }
}
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn clock() -> Rc<Cell<u64>> {
    Rc::new(Cell::new(SECOND))
}
fn access(temporary: &Temporary) -> Access {
    Access {
        root: Some(temporary.0.clone()),
        hook: None,
    }
}
fn pin(bytes: &[u8]) -> Pin {
    Pin {
        sha256: files::sha(bytes),
        bytes: bytes.len() as u64,
    }
}
fn workload() -> Workload {
    Workload::decode(
        &serde_json::to_vec(&json!({"revision":REVISION,"run":RUN,
        "workload":"meetings","label":"private-vps","firstSize":30,"steps":1,
        "meetingSize":30,"speakers":30,"runtimeSeconds":120,"quick":false,
        "audioOnly":true,"chatIntervalMs":30000,"serverCpus":1.0,"generatorCpus":4.8,
        "appCpus":null,"monthlyPrice":null,"portMbps":2000.0}))
        .unwrap(),
    )
    .unwrap()
}
fn header() -> Header {
    Header {
        schema_version: 1,
        scope: "OpsCapacityBootstrap".into(),
        mode: Mode::Invoke,
        run: RUN.into(),
        selection: Source {
            repository: "owner/repo".into(),
            revision: REVISION.into(),
            package_version: "0.1.0".into(),
            protocol_version: 1,
            target: "x86_64-unknown-linux-musl".into(),
            build_run_id: 17,
            run_attempt: 2,
        },
        artifact_id: 29,
        transfer: pin(b"transfer"),
        members: Members {
            manifest: pin(b"fixture manifest"),
            selection: pin(b"fixture selection"),
            binary: pin(b"inert candidate, never executed"),
        },
        request: pin(b"frame"),
    }
}
fn unit(load: &str, active: &str, sub: &str, pid: &str) -> String {
    format!(
        "LoadState={load}\nActiveState={active}\nSubState={sub}\n\
         MainPID={pid}\nResult=success\nExecMainCode=exited\nExecMainStatus=0\n"
    )
}

#[test]
fn loaned_writer_keeps_its_original_parent_through_trim_and_accepts_expected_growth() {
    use std::io::Write;

    let temporary = Temporary::new();
    temporary.directory("/state/kept/leaf", 0o700);
    temporary.directory("/unrelated/leaf", 0o700);
    let mut fs = Fs::new(access(&temporary)).unwrap();
    let mut owner = Owner::fixture(clock());
    let parent = fs.directory("/state/kept/leaf", true).unwrap();
    let before = fs.dir_stamp(parent).unwrap();
    let id = fs.writer("/state/kept/leaf/archive").unwrap();
    let mut original = fs.take(id).unwrap();
    assert!(matches!(fs.take(id), Err(Failure::Unproven)));
    assert_eq!(fs.check(id), Err(Failure::Unproven));
    fs.directory("/unrelated/leaf", true).unwrap();
    fs.trim_directories().unwrap();
    let retained = fs.dir_stamp(parent).unwrap();
    assert_eq!(
        (before.dev(), before.ino()),
        (retained.dev(), retained.ino())
    );
    assert_eq!(fs.verify(), Err(Failure::Unproven));
    original.write_all(b"original bytes after trim").unwrap();
    original.sync_all().unwrap();
    let completed = original.metadata().unwrap();
    fs.put(id, original).unwrap();
    fs.written(id).unwrap();
    assert!(files::same(&completed, fs.stamp(id)));
    fs.close(id).unwrap();
    let reader = fs
        .open("/state/kept/leaf/archive", 64, Some(0o600))
        .unwrap();
    assert_eq!(
        fs.read(reader, 64, &mut owner).unwrap(),
        b"original bytes after trim"
    );
    fs.verify().unwrap();
    fs.finish().unwrap();
    owner.finish().unwrap();
}

#[test]
fn loaned_descriptors_consume_the_same_capacity_as_retained_descriptors() {
    fn fill(loan: bool) -> usize {
        let temporary = Temporary::new();
        temporary.directory("/state", 0o700);
        let mut fs = Fs::new(access(&temporary)).unwrap();
        let first = fs.writer("/state/first").unwrap();
        let original = if loan {
            Some(fs.take(first).unwrap())
        } else {
            None
        };
        let mut admitted = 0;
        loop {
            match fs.writer(&format!("/state/peer-{admitted}")) {
                Ok(_) => admitted += 1,
                Err(error) => {
                    assert_eq!(error, Failure::Bounds);
                    break;
                }
            }
        }
        if let Some(original) = original {
            fs.put(first, original).unwrap();
            fs.written(first).unwrap();
        }
        fs.finish().unwrap();
        admitted
    }
    let retained = fill(false);
    let loaned = fill(true);
    assert!(retained > 1);
    assert_eq!(loaned, retained);
}

#[test]
fn outstanding_loan_refuses_finish_but_every_retained_peer_close_is_attempted() {
    for fail_close in [false, true] {
        let temporary = Temporary::new();
        temporary.directory("/state", 0o700);
        let closes = Rc::new(Cell::new(0));
        let observed = closes.clone();
        let mut fs = Fs::new(Access {
            root: Some(temporary.0.clone()),
            hook: Some(Rc::new(move |event| {
                if event == "close" {
                    observed.set(observed.get() + 1);
                    if fail_close {
                        return Err(Failure::Close);
                    }
                }
                Ok(())
            })),
        })
        .unwrap();
        let loan = fs.writer("/state/loan").unwrap();
        fs.writer("/state/retained").unwrap();
        let original = fs.take(loan).unwrap();
        assert_eq!(fs.verify(), Err(Failure::Unproven));
        let expected = fs.owned_descriptors().len();
        let before = closes.get();
        assert_eq!(fs.finish(), Err(Failure::Unproven));
        assert_eq!(closes.get() - before, expected);
        assert!(original.metadata().unwrap().is_file());
        nix::unistd::close(original).unwrap();
    }
}

#[test]
fn rejected_returns_close_incoming_files_without_adopting_or_erasing_the_original_loan() {
    for kind in ["foreign", "closed", "mode", "invalid"] {
        let temporary = Temporary::new();
        temporary.directory("/state", 0o700);
        let closes = Rc::new(Cell::new(0));
        let fail_close = Rc::new(Cell::new(false));
        let observed = closes.clone();
        let fail = fail_close.clone();
        let mut fs = Fs::new(Access {
            root: Some(temporary.0.clone()),
            hook: Some(Rc::new(move |event| {
                if event == "close" {
                    observed.set(observed.get() + 1);
                    if fail.replace(false) {
                        return Err(Failure::Close);
                    }
                }
                Ok(())
            })),
        })
        .unwrap();
        let id = fs.writer("/state/loan").unwrap();
        let peer = fs.writer("/state/peer").unwrap();
        let mut original = Some(fs.take(id).unwrap());
        let (target, incoming, expected) = match kind {
            "foreign" => (
                id,
                std::fs::File::open(temporary.path("/state/peer")).unwrap(),
                Failure::Changed,
            ),
            "closed" => {
                let incoming = fs.file(peer).try_clone().unwrap();
                fs.close(peer).unwrap();
                (peer, incoming, Failure::Unproven)
            }
            "mode" => {
                let incoming = original.take().unwrap();
                incoming
                    .set_permissions(fs::Permissions::from_mode(0o640))
                    .unwrap();
                (id, incoming, Failure::Changed)
            }
            "invalid" => (
                usize::MAX,
                original.as_ref().unwrap().try_clone().unwrap(),
                Failure::Unproven,
            ),
            _ => unreachable!(),
        };
        let before = closes.get();
        fail_close.set(true);
        assert_eq!(fs.put(target, incoming), Err(expected), "{kind}");
        assert_eq!(closes.get() - before, 1, "rejected incoming file closes");
        assert_eq!(fs.verify(), Err(Failure::Unproven));
        if let Some(original) = original {
            fs.put(id, original).unwrap();
            fs.written(id).unwrap();
            fs.verify().unwrap();
            fs.finish().unwrap();
        } else {
            assert_eq!(fs.finish(), Err(Failure::Unproven));
        }
    }
}

#[test]
fn atomic_replacement_updates_every_retained_alias_without_losing_original_bytes() {
    let temporary = Temporary::new();
    temporary.directory("/state", 0o700);
    let mut fs = Fs::new(access(&temporary)).unwrap();
    let mut owner = Owner::fixture(clock());
    let first = fs
        .create("/state/current.json", b"first", &mut owner)
        .unwrap();
    let alias = fs.open("/state/current.json", 64, Some(0o600)).unwrap();
    let second = fs
        .replace("/state/current.json", b"second", &mut owner)
        .unwrap();
    assert_eq!(fs.read(first, 64, &mut owner).unwrap(), b"first");
    assert_eq!(fs.read(alias, 64, &mut owner).unwrap(), b"first");
    assert_eq!(fs.read(second, 64, &mut owner).unwrap(), b"second");
    fs.verify().unwrap();
    fs.finish().unwrap();
    owner.finish().unwrap();
}

#[test]
fn after_effect_failures_close_every_peer_and_keep_foreign_names() {
    for event in ["write", "fsync", "close", "rename", "mkdir"] {
        let temporary = Temporary::new();
        temporary.directory("/state", 0o700);
        let failed = Rc::new(Cell::new(false));
        let closes = Rc::new(Cell::new(0));
        let armed = failed.clone();
        let count = closes.clone();
        let mut fs = Fs::new(Access {
            root: Some(temporary.0.clone()),
            hook: Some(Rc::new(move |name| {
                if name == "close" {
                    count.set(count.get() + 1);
                }
                if name == event && !armed.replace(true) {
                    Err(Failure::Io)
                } else {
                    Ok(())
                }
            })),
        })
        .unwrap();
        let mut owner = Owner::fixture(clock());
        let result = if event == "mkdir" {
            fs.mkdir("/state/owned").map(|_| ())
        } else {
            fs.replace("/state/current.json", b"evidence", &mut owner)
                .map(|_| ())
        };
        assert_eq!(result, Err(Failure::Io), "{event}");
        assert!(failed.get());
        // A same-named foreign temporary is never opened with truncation or removed.
        temporary.file("/state/foreign.json.ops-capacity-tmp", b"foreign", 0o600);
        assert!(
            fs.replace("/state/foreign.json", b"new", &mut owner)
                .is_err()
        );
        assert_eq!(
            fs::read(temporary.path("/state/foreign.json.ops-capacity-tmp")).unwrap(),
            b"foreign"
        );
        let descriptors = fs.owned_descriptors();
        let expected = descriptors.len();
        let before = closes.get();
        let _ = fs.finish();
        assert_eq!(closes.get() - before, expected, "{event}");
        for (fd, before) in descriptors {
            assert!(
                !std::fs::metadata(format!("/proc/self/fd/{fd}"))
                    .is_ok_and(|after| after.dev() == before.dev() && after.ino() == before.ino())
            );
        }
        owner.finish().unwrap();
    }
}

#[test]
fn replacement_readback_cannot_accept_equal_bytes_from_an_exchanged_inode() {
    let temporary = Temporary::new();
    temporary.directory("/state", 0o700);
    let path = temporary.path("/state/value");
    let exchanged = Rc::new(Cell::new(false));
    let marker = exchanged.clone();
    let mut fs = Fs::new(Access {
        root: Some(temporary.0.clone()),
        hook: Some(Rc::new(move |event| {
            if event == "close" && !marker.replace(true) {
                fs::rename(&path, path.with_extension("original")).unwrap();
                fs::write(&path, b"equal").unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            }
            Ok(())
        })),
    })
    .unwrap();
    let mut owner = Owner::fixture(clock());
    assert_eq!(
        fs.create("/state/value", b"equal", &mut owner),
        Err(Failure::Changed)
    );
    assert_eq!(fs::read(temporary.path("/state/value")).unwrap(), b"equal");
    let _ = fs.finish();
    owner.finish().unwrap();
}

#[test]
fn original_clock_covers_setup_release_and_finalization_and_keeps_first_control_status() {
    let time = clock();
    let mut owner = Owner::fixture(time.clone());
    let origin = time.get();
    time.set(origin + 59 * SECOND);
    owner.admit(Action::Reserve).unwrap();
    time.set(origin + 119 * SECOND);
    owner.ready().unwrap();
    time.set(origin + 1739 * SECOND);
    assert_eq!(owner.begin_release().unwrap(), origin + 1769 * SECOND);
    time.set(origin + 1769 * SECOND);
    assert_eq!(owner.check(false), Err(Failure::Deadline));
    owner.begin_cleanup();
    time.set(origin + 1799 * SECOND);
    owner.cleanup_time().unwrap();
    assert_eq!(owner.finish().err(), Some(Failure::Deadline));
    for first in [Failure::Interrupted, Failure::Terminated, Failure::Deadline] {
        let time = clock();
        let mut owner = Owner::fixture(time.clone());
        owner.latch(first);
        owner.begin_cleanup();
        owner.latch(Failure::Close);
        time.set(1000 * SECOND);
        assert_eq!(owner.finish().err(), Some(first));
    }
    let time = clock();
    let mut owner = Owner::fixture(time.clone());
    let original = time.get();
    time.set(original + 59 * SECOND);
    owner.admit(Action::Recover).unwrap();
    assert_eq!(owner.active_end(), original + 600 * SECOND);
    assert_eq!(owner.end(), original + 660 * SECOND);
    owner.finish().unwrap();
}

#[test]
fn cleanup_clock_regression_is_unproven_and_never_synthesizes_a_timeout() {
    let time = clock();
    let mut owner = Owner::fixture(time.clone());
    time.set(3 * SECOND);
    owner.check(false).unwrap();
    owner.begin_cleanup();
    time.set(2 * SECOND);
    assert_eq!(owner.cleanup_time(), Err(Failure::Unproven));
    assert_eq!(owner.finish().err(), Some(Failure::Unproven));
    let time = clock();
    let mut owner = Owner::fixture(time.clone());
    owner.latch(Failure::Interrupted);
    time.set(0);
    owner.begin_cleanup();
    assert_eq!(owner.cleanup_time(), Err(Failure::Unproven));
    assert_eq!(owner.finish().err(), Some(Failure::Interrupted));
}

struct Host {
    root: PathBuf,
    unit: String,
    containers: Vec<String>,
    quality: bool,
    leave_residuals: bool,
    include_unknown: bool,
    lose_ack: bool,
    build_failed: bool,
    actions: Vec<String>,
    destructive: Vec<Vec<String>>,
    capacity_arguments: Vec<String>,
    systemd_properties: Vec<String>,
    starts: usize,
    engine_runs: usize,
}
impl Host {
    fn path(&self, absolute: &str) -> PathBuf {
        self.root.join(absolute.trim_start_matches('/'))
    }
    fn command(&mut self, program: &Path, args: &[String]) -> Result<Output> {
        let text = if program == Path::new("/usr/bin/git") {
            if args.get(2).map(String::as_str) == Some("rev-parse") {
                format!("{REVISION}\n")
            } else if args.get(2).map(String::as_str) == Some("status") {
                String::new()
            } else {
                return Err(Failure::Unproven);
            }
        } else if program == Path::new("/usr/bin/systemctl") {
            match args.first().map(String::as_str) {
                Some("show")
                    if args
                        .last()
                        .is_some_and(|v| v == "--property=ControlGroup,Slice") =>
                {
                    "ControlGroup=\nSlice=system.slice\n".into()
                }
                Some("show") if args.get(1) == Some(&header().capacity_unit()) => self.unit.clone(),
                Some("show") => {
                    let value = unit("loaded", "inactive", "dead", "0");
                    if self.build_failed
                        && args.get(1).map(String::as_str) == Some(protocol::BUILD_UNIT)
                    {
                        value
                            .replace("Result=success", "Result=exit-code")
                            .replace("ExecMainStatus=0", "ExecMainStatus=1")
                    } else {
                        value
                    }
                }
                Some("list-units") => {
                    if self.unit.contains("ActiveState=active\n") {
                        format!(
                            "{} loaded active running Fixture\n",
                            header().capacity_unit()
                        )
                    } else {
                        String::new()
                    }
                }
                Some("start") => {
                    self.actions.push("build".into());
                    String::new()
                }
                Some("stop") => {
                    assert!(
                        self.path(&format!("{}/unit-result.json", header().evidence_path()))
                            .is_file()
                    );
                    assert!(
                        self.path(&format!("{}/collection.json", header().evidence_path()))
                            .is_file()
                    );
                    self.unit = unit("not-found", "inactive", "dead", "0");
                    self.actions.push("gc".into());
                    String::new()
                }
                _ => return Err(Failure::Unproven),
            }
        } else if program == Path::new("/usr/bin/systemd-run") {
            self.starts += 1;
            self.actions.push("start".into());
            self.systemd_properties = args
                .iter()
                .filter(|v| v.starts_with("--property="))
                .cloned()
                .collect();
            self.unit = unit("loaded", "active", "running", "77");
            if self.lose_ack {
                return Err(Failure::Command);
            }
            String::new()
        } else if program == Path::new("/usr/bin/journalctl") {
            "inert unit journal\n".into()
        } else if program == Path::new("/usr/bin/docker") {
            self.docker(args)?
        } else if program.to_str()
            == Some(&format!(
                "{ROOT}/artifacts/ops-runtime.{RUN}/simplestchat-ops"
            ))
        {
            self.engine_runs += 1;
            self.capacity_arguments = args.to_vec();
            let directory = format!("{}/workload", header().evidence_path());
            fs::create_dir(self.path(&directory)).unwrap();
            fs::set_permissions(self.path(&directory), fs::Permissions::from_mode(0o700)).unwrap();
            let report = json!({"schemaVersion":1,"measurement":{"meetingSize":30,
                "browser":{"audioOnly":true,"speakers":30,"chatIntervalMs":30000}},
                "images":{"server":{"id":server(),"reference":server(),"revision":REVISION},
                "generator":{"id":generator(),"reference":generator(),"revision":REVISION}},
                "steps":[{"workload":"meetings","size":30,"audioOnly":true,"speakers":30,
                "meetingSize":30,"chatIntervalMs":30000,"valid":true,"passed":self.quality}],
                "ceilings":{"meetings":{"size":30,"lowerBound":true}},
                "projection":{"lowerBound":true}});
            self.save(&format!("{directory}/calibration.json"), &report);
            self.save(
                &format!("{directory}/ownership.json"),
                &json!({"schemaVersion":1,"runId":INNER,
                "containers":[{"name":"capacity-gen-fixture","image":generator(),"id":owned()}]}),
            );
            if self.leave_residuals {
                self.containers = vec![owned()];
                if self.include_unknown {
                    self.containers.push(unknown());
                }
            }
            "inert capacity child\n".into()
        } else {
            return Err(Failure::Unproven);
        };
        Ok(Output {
            status: Some(0),
            stdout: text.as_bytes().to_vec(),
            stderr: Vec::new(),
            combined: text.into_bytes(),
        })
    }
    fn save(&self, path: &str, value: &Value) {
        let path = self.path(path);
        fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    fn docker(&mut self, args: &[String]) -> Result<String> {
        assert_eq!(&args[..2], ["--host", "unix:///var/run/docker.sock"]);
        match args.get(2).map(String::as_str) {
            Some("ps") => {
                if args
                    .iter()
                    .any(|v| v == "label=com.docker.compose.project=simplestchat-public")
                {
                    return Ok(String::new());
                }
                let ids = if args.iter().any(|v| v == "--all") {
                    self.containers.clone()
                } else {
                    vec![]
                };
                let ids = if args
                    .iter()
                    .any(|v| v == &format!("label=simplestchat.capacity.run={INNER}"))
                {
                    ids.into_iter()
                        .filter(|v| v == &owned())
                        .collect::<Vec<_>>()
                } else {
                    ids
                };
                Ok(ids.join("\n"))
            }
            Some("image") => Ok(
                json!([{"Id":args.last().unwrap(),"Config":{"User":"10001:10001",
                "Labels":{"org.opencontainers.image.revision":REVISION}}}])
                .to_string(),
            ),
            Some("inspect") => {
                let id = args.last().unwrap();
                assert!(self.containers.contains(id));
                Ok(json!([{"Id":id,"Name":if id==&owned(){"/capacity-gen-fixture"}else{"/foreign"},
                    "Image":generator(),"Config":{"Image":generator(),"Env":["PRIVATE_SENTINEL"],
                    "Labels":if id==&owned(){
                        json!({"simplestchat.capacity.run":INNER})
                    }else{json!({})}},
                    "State":{"Status":"exited","Running":false,"ExitCode":0,"OOMKilled":false,
                    "StartedAt":"inert","FinishedAt":"inert"}}]).to_string())
            }
            Some("stop" | "rm") => {
                let id = args.last().unwrap();
                assert_eq!(id, &owned());
                assert!(!args.iter().any(|v| v == "--force"));
                self.destructive.push(args[2..].to_vec());
                if args[2] == "rm" {
                    self.containers.retain(|v| v != id);
                }
                Ok(id.clone())
            }
            _ => Err(Failure::Unproven),
        }
    }
}

struct Harness {
    temporary: Temporary,
    header: Header,
    workload: Workload,
    host: Rc<RefCell<Host>>,
}
impl Harness {
    fn new() -> Self {
        let temporary = Temporary::new();
        let header = header();
        let workload = workload();
        for (directory, mode) in [
            (ROOT.to_owned(), 0o750),
            (format!("{ROOT}/sources"), 0o700),
            (format!("{ROOT}/artifacts"), 0o700),
            (format!("{ROOT}/results"), 0o700),
            ("/run/simplestchat-bench".into(), 0o700),
            (format!("{ROOT}/sources/{REVISION}"), 0o755),
            (format!("{ROOT}/artifacts/{REVISION}"), 0o700),
            (format!("{ROOT}/artifacts/ops-runtime.{RUN}"), 0o700),
        ] {
            temporary.directory(&directory, mode);
        }
        for (name, bytes, mode) in [
            ("ops-release.json", b"fixture manifest".as_slice(), 0o600),
            ("ops-selection.json", b"fixture selection".as_slice(), 0o600),
            (
                "simplestchat-ops",
                b"inert candidate, never executed".as_slice(),
                0o755,
            ),
        ] {
            temporary.file(
                &format!("{ROOT}/artifacts/ops-runtime.{RUN}/{name}"),
                bytes,
                mode,
            );
        }
        temporary.file("/usr/lib/os-release", b"ID=debian\nVERSION_ID=13\n", 0o644);
        temporary.file(
            "/usr/local/libexec/simplestchat-bench/build-images",
            format!("revision='{REVISION}'\nsource_root='{ROOT}/sources/{REVISION}'\n").as_bytes(),
            0o755,
        );
        for path in [
            "/etc/systemd/system/simplestchat-image-build.service".into(),
            format!("{ROOT}/sources/{REVISION}/ops/ansible/templates/image-build.service.j2"),
        ] {
            temporary.file(&path, b"inert canonical build unit\n", 0o644);
        }
        temporary.file(
            &format!("{ROOT}/artifacts/{REVISION}/images.json"),
            json!({
                "schemaVersion":1,"passed":true,"revision":REVISION,
                "serverImage":server(),"generatorImage":generator()
            })
            .to_string()
            .as_bytes(),
            0o600,
        );
        let host = Rc::new(RefCell::new(Host {
            root: temporary.0.clone(),
            unit: unit("not-found", "inactive", "dead", "0"),
            containers: vec![],
            quality: true,
            leave_residuals: false,
            include_unknown: true,
            lose_ack: false,
            build_failed: false,
            actions: vec![],
            destructive: vec![],
            capacity_arguments: vec![],
            systemd_properties: vec![],
            starts: 0,
            engine_runs: 0,
        }));
        Self {
            temporary,
            header,
            workload,
            host,
        }
    }
    fn session<T>(&self, body: impl FnOnce(&mut Session<'_>, &Stage) -> Result<T>) -> Result<T> {
        self.session_with(access(&self.temporary), None, body)
    }
    fn session_with<T>(
        &self,
        access: Access,
        pending: Option<Rc<Cell<Option<Failure>>>>,
        body: impl FnOnce(&mut Session<'_>, &Stage) -> Result<T>,
    ) -> Result<T> {
        let mut fs = Fs::new(access)?;
        let mut owner = Owner::fixture(clock());
        if let Some(pending) = pending {
            owner.fixture_control(pending);
        }
        let stage = Stage::fixture(&self.header, &mut fs, &mut owner)?;
        let host = self.host.clone();
        let mut runner =
            Runner::fixture(Box::new(move |program, args, environment, seconds, _| {
                assert_eq!(environment.get("LC_ALL").map(String::as_str), Some("C"));
                assert!(seconds > 0);
                host.borrow_mut().command(program, args)
            }));
        let result = body(
            &mut Session {
                header: &self.header,
                fs: &mut fs,
                owner: &mut owner,
                runner: &mut runner,
            },
            &stage,
        );
        if let Err(error) = &result {
            owner.latch(*error);
            owner.begin_cleanup();
        }
        let finalized = fs.finish();
        let control = owner.finish();
        result.and_then(|value| {
            finalized?;
            control?;
            Ok(value)
        })
    }
    fn rpc(&self, action: Action) -> Result<ResultData> {
        self.session(|session, stage| {
            let result = if action == Action::Retire {
                session.retire(&self.workload, stage)?
            } else {
                session.dispatch(action, Some(&self.workload), stage)?
            };
            protocol::Response::encode(&self.header, &result)?;
            Ok(result)
        })
    }
    fn worker(&self) -> Result<()> {
        self.host.borrow_mut().unit = unit(
            "loaded",
            "active",
            "running",
            &rustix::process::getpid().as_raw_pid().to_string(),
        );
        let result = self.session(|session, stage| {
            super::worker::fixture_execute(&self.header, &self.workload, stage, session)
        });
        let mut host = self.host.borrow_mut();
        host.unit = unit("loaded", "active", "exited", "0");
        if result.is_err() {
            host.unit = host
                .unit
                .replace("Result=success", "Result=exit-code")
                .replace("ExecMainStatus=0", "ExecMainStatus=1");
        }
        result
    }
    fn json(&self, path: &str) -> Value {
        serde_json::from_slice(&fs::read(self.temporary.path(path)).unwrap()).unwrap()
    }
    fn start(&self) {
        self.rpc(Action::Start).unwrap();
    }
}

#[test]
fn whole_success_uses_original_request_worker_assessment_retained_unit_and_cleanup() {
    let harness = Harness::new();
    harness.rpc(Action::Preflight).unwrap();
    harness.rpc(Action::Build).unwrap();
    harness.start();
    harness.worker().unwrap();
    let ResultData::Collect(result) = harness.rpc(Action::Collect).unwrap() else {
        panic!("collection")
    };
    assert!(result.passed && result.cleanup.clean);
    assert_eq!(result.request_json.as_bytes(), harness.workload.as_json());
    let ResultData::Collect(second) = harness.rpc(Action::Collect).unwrap() else {
        panic!("collection")
    };
    assert!(second.passed);
    assert_eq!(second.unit.retained, Some(true));
    let current = harness.json(CURRENT);
    assert_eq!(current["passed"], true);
    assert_eq!(current["finalized"], true);
    let host = harness.host.borrow();
    assert_eq!(host.starts, 1);
    assert_eq!(host.engine_runs, 1);
    assert!(
        host.systemd_properties
            .contains(&"--property=RuntimeMaxSec=120".into())
    );
    assert!(
        host.systemd_properties
            .contains(&"--property=TimeoutStopSec=600".into())
    );
}

#[test]
fn quality_failure_and_missing_outcome_never_become_success_when_cleanup_is_empty() {
    for missing in [false, true] {
        let harness = Harness::new();
        harness.host.borrow_mut().quality = false;
        harness.start();
        assert!(harness.worker().is_err());
        if missing {
            fs::remove_file(
                harness
                    .temporary
                    .path(&format!("{}/outcome.json", harness.header.evidence_path())),
            )
            .unwrap();
            harness.host.borrow().save(
                CURRENT,
                &json!({"schemaVersion":1,"operation":"capacity","run":RUN,"finalized":false}),
            );
        }
        let ResultData::Collect(result) = harness.rpc(Action::Collect).unwrap() else {
            panic!("collection")
        };
        assert!(!result.passed);
        assert!(result.cleanup.clean);
        if missing {
            assert!(result.outcome_json.contains("worker_outcome_missing"));
            assert_eq!(harness.json(CURRENT)["finalized"], false);
        } else {
            let outcome: Value = serde_json::from_str(&result.outcome_json).unwrap();
            assert_eq!(outcome["exitStatus"], 0);
            assert_eq!(outcome["failedSteps"].as_array().unwrap().len(), 1);
        }
    }
}

#[test]
fn lost_start_ack_keeps_same_workload_and_lookup_does_not_start_or_change_runtime() {
    let harness = Harness::new();
    harness.host.borrow_mut().lose_ack = true;
    assert!(harness.rpc(Action::Start).is_err());
    let ResultData::Lookup(result) = harness.rpc(Action::Lookup).unwrap() else {
        panic!("lookup")
    };
    assert_eq!(result.request_json.as_bytes(), harness.workload.as_json());
    assert_eq!(
        result.request_sha256,
        files::sha(harness.workload.as_json())
    );
    assert!(harness.rpc(Action::Start).is_err());
    assert_eq!(harness.host.borrow().starts, 1);
    harness.worker().unwrap();
    assert_eq!(harness.host.borrow().engine_runs, 1);
}

#[test]
fn recovery_rechecks_exact_owned_identity_and_retains_unknown_residual_and_old_failure() {
    for retain_unknown in [false, true] {
        let harness = Harness::new();
        harness.host.borrow_mut().leave_residuals = true;
        harness.start();
        assert!(harness.worker().is_err());
        if !retain_unknown {
            harness
                .host
                .borrow_mut()
                .containers
                .retain(|v| v != &unknown());
        }
        let outcome_path = format!("{}/outcome.json", harness.header.evidence_path());
        let original = fs::read(harness.temporary.path(&outcome_path)).unwrap();
        let ResultData::Recover(receipt) = harness.rpc(Action::Recover).unwrap() else {
            panic!("recover")
        };
        assert_eq!(receipt.removed, vec![owned()]);
        assert_eq!(receipt.finalized, !retain_unknown);
        assert_eq!(
            fs::read(harness.temporary.path(&outcome_path)).unwrap(),
            original
        );
        let host = harness.host.borrow();
        assert_eq!(
            host.destructive,
            vec![
                vec!["stop".into(), "--time".into(), "20".into(), owned()],
                vec!["rm".into(), "--volumes".into(), owned()]
            ]
        );
        assert_eq!(
            host.containers,
            if retain_unknown {
                vec![unknown()]
            } else {
                vec![]
            }
        );
        assert_eq!(harness.json(CURRENT)["finalized"], !retain_unknown);
        if !retain_unknown {
            assert_eq!(harness.json(CURRENT)["passed"], false);
        }
    }
}

#[test]
fn prestart_retirement_unlinks_only_same_pinned_three_files_and_finishes_aliases() {
    let harness = Harness::new();
    let ResultData::Retire(receipt) = harness.rpc(Action::Retire).unwrap() else {
        panic!("retire")
    };
    assert!(receipt.retired);
    assert_eq!(
        receipt.request_sha256,
        files::sha(harness.workload.as_json())
    );
    assert!(
        !harness
            .temporary
            .path(&format!("{ROOT}/artifacts/ops-runtime.{RUN}"))
            .exists()
    );
    assert!(
        !harness
            .temporary
            .path(&harness.header.evidence_path())
            .exists()
    );
}

#[test]
fn cleanup_failure_precedes_late_control_and_all_evidence_and_peer_finalizers_are_attempted() {
    let harness = Harness::new();
    harness.start();
    let armed = Rc::new(Cell::new(false));
    let pending = Rc::new(Cell::new(None));
    let attempted = Rc::new(Cell::new(0));
    let when = armed.clone();
    let signal = pending.clone();
    let count = attempted.clone();
    let hooked = Access {
        root: Some(harness.temporary.0.clone()),
        hook: Some(Rc::new(move |event| {
            if when.get() && event == "write" {
                signal.set(Some(Failure::Interrupted));
                count.set(count.get() + 1);
            }
            Ok(())
        })),
    };
    harness.host.borrow_mut().unit = unit(
        "loaded",
        "active",
        "running",
        &rustix::process::getpid().as_raw_pid().to_string(),
    );
    let result = harness.session_with(hooked, Some(pending), |session, stage| {
        let host = harness.host.clone();
        let fail = armed.clone();
        *session.runner = Runner::fixture(Box::new(move |program, args, _, _, cleanup| {
            if cleanup {
                fail.set(true);
                return Err(Failure::Ownership);
            }
            host.borrow_mut().command(program, args)
        }));
        super::worker::fixture_execute(&harness.header, &harness.workload, stage, session)
    });
    assert_eq!(result, Err(Failure::Ownership));
    assert!(attempted.get() >= 2);
    let directory = harness.header.evidence_path();
    let outcome = harness.json(&format!("{directory}/outcome.json"));
    assert_eq!(outcome["passed"], false);
    assert_eq!(outcome["cleanupFailure"], true);
    assert_eq!(outcome["failureClass"], "ops_capacity_ownership");
    assert_eq!(harness.json(CURRENT)["finalized"], false);
}

#[test]
fn terminal_retirement_requires_original_cleanup_receipt_and_empty_fixed_cgroup() {
    let harness = Harness::new();
    harness.start();
    harness.worker().unwrap();
    harness.rpc(Action::Collect).unwrap();
    let group = format!(
        "/sys/fs/cgroup/system.slice/{}/cgroup.procs",
        harness.header.capacity_unit()
    );
    harness.temporary.file(&group, b"123\n", 0o644);
    assert!(harness.rpc(Action::Retire).is_err());
    assert!(
        harness
            .temporary
            .path(&format!(
                "{ROOT}/artifacts/ops-runtime.{RUN}/simplestchat-ops"
            ))
            .is_file()
    );
    harness.temporary.file(&group, b"", 0o644);
    harness.rpc(Action::Retire).unwrap();
    assert!(
        harness
            .temporary
            .path(&format!("{}/request.json", harness.header.evidence_path()))
            .is_file()
    );
}

#[test]
fn owned_retirement_after_effect_errors_still_finish_all_proven_remaining_unlinks() {
    for event in ["unlink", "rmdir"] {
        let harness = Harness::new();
        let failed = Rc::new(Cell::new(false));
        let marker = failed.clone();
        let access = Access {
            root: Some(harness.temporary.0.clone()),
            hook: Some(Rc::new(move |name| {
                if name == event && !marker.replace(true) {
                    Err(Failure::Io)
                } else {
                    Ok(())
                }
            })),
        };
        let result = harness.session_with(access, None, |session, stage| {
            session.retire(&harness.workload, stage).map(|_| ())
        });
        assert_eq!(result, Err(Failure::Io));
        assert!(failed.get());
        assert!(
            !harness
                .temporary
                .path(&format!("{ROOT}/artifacts/ops-runtime.{RUN}"))
                .exists()
        );
    }
}

#[test]
fn malformed_positive_outcome_never_persists_a_positive_collection_before_wire_refusal() {
    let harness = Harness::new();
    harness.start();
    harness.worker().unwrap();
    let path = format!("{}/outcome.json", harness.header.evidence_path());
    let mut outcome = harness.json(&path);
    outcome["cleanup"]["residuals"] = json!([{}]);
    harness.host.borrow().save(&path, &outcome);
    assert!(harness.rpc(Action::Collect).is_err());
    assert!(
        !harness
            .temporary
            .path(&format!(
                "{}/collection.json",
                harness.header.evidence_path()
            ))
            .exists()
    );
}

#[test]
fn each_retirement_member_hash_is_proven_before_its_own_unlink() {
    let harness = Harness::new();
    let result = harness.session(|session, stage| {
        let mut mismatched = harness.header.clone();
        mismatched.members.selection.sha256 = "f".repeat(64);
        stage.retire(&mismatched, session.fs, session.owner)
    });
    assert_eq!(result, Err(Failure::Changed));
    let directory = format!("{ROOT}/artifacts/ops-runtime.{RUN}");
    assert!(
        !harness
            .temporary
            .path(&format!("{directory}/ops-release.json"))
            .exists()
    );
    assert!(
        harness
            .temporary
            .path(&format!("{directory}/ops-selection.json"))
            .is_file()
    );
    assert!(
        harness
            .temporary
            .path(&format!("{directory}/simplestchat-ops"))
            .is_file()
    );
}

#[test]
fn after_effect_retirement_exchange_retains_foreign_equal_bytes_and_stops_ambiguous_unlinks() {
    let harness = Harness::new();
    let changed = Rc::new(Cell::new(false));
    let marker = changed.clone();
    let binary = harness.temporary.path(&format!(
        "{ROOT}/artifacts/ops-runtime.{RUN}/simplestchat-ops"
    ));
    let original = harness.temporary.path("/original-binary");
    let foreign = binary.clone();
    let access = Access {
        root: Some(harness.temporary.0.clone()),
        hook: Some(Rc::new(move |event| {
            if event == "unlink" && !marker.replace(true) {
                fs::rename(&binary, &original).unwrap();
                fs::write(&binary, b"inert candidate, never executed").unwrap();
                fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
            }
            Ok(())
        })),
    };
    let result = harness.session_with(access, None, |session, stage| {
        stage.retire(&harness.header, session.fs, session.owner)
    });
    assert_eq!(result, Err(Failure::Changed));
    assert!(changed.get());
    assert_eq!(
        fs::read(foreign).unwrap(),
        b"inert candidate, never executed"
    );
    assert!(harness.temporary.path("/original-binary").is_file());
}

#[test]
fn repeated_failed_temporary_comparisons_never_adopt_a_foreign_opened_inode() {
    let temporary = Temporary::new();
    temporary.directory("/state", 0o700);
    let path = temporary.path("/state/value.ops-capacity-tmp");
    let changed = Rc::new(Cell::new(false));
    let marker = changed.clone();
    let mut fs = Fs::new(Access {
        root: Some(temporary.0.clone()),
        hook: Some(Rc::new(move |event| {
            if event == "close" && !marker.replace(true) {
                fs::rename(&path, path.with_extension("original")).unwrap();
                fs::write(&path, b"foreign").unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
                return Err(Failure::Io);
            }
            Ok(())
        })),
    })
    .unwrap();
    let mut owner = Owner::fixture(clock());
    assert!(fs.replace("/state/value", b"owned", &mut owner).is_err());
    assert!(fs.replace("/state/value", b"again", &mut owner).is_err());
    assert_eq!(
        fs::read(temporary.path("/state/value.ops-capacity-tmp")).unwrap(),
        b"foreign"
    );
    let _ = fs.finish();
    owner.finish().unwrap();
}

#[test]
fn same_inode_changed_bytes_with_restored_mtime_are_not_rebased_as_our_rename() {
    let temporary = Temporary::new();
    temporary.directory("/state", 0o700);
    let path = temporary.path("/state/value.ops-capacity-tmp");
    let changed = Rc::new(Cell::new(false));
    let marker = changed.clone();
    let mut fs = Fs::new(Access {
        root: Some(temporary.0.clone()),
        hook: Some(Rc::new(move |event| {
            if event == "before-rename" && !marker.replace(true) {
                let modified = fs::metadata(&path).unwrap().modified().unwrap();
                fs::write(&path, b"other").unwrap();
                fs::OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_modified(modified)
                    .unwrap();
            }
            Ok(())
        })),
    })
    .unwrap();
    let mut owner = Owner::fixture(clock());
    assert_eq!(
        fs.replace("/state/value", b"owned", &mut owner),
        Err(Failure::Changed)
    );
    assert!(changed.get());
    assert_eq!(fs::read(temporary.path("/state/value")).unwrap(), b"other");
    let _ = fs.finish();
    owner.finish().unwrap();
}

fn reference(scenario: &str) -> Value {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let prefix = std::env::var_os("PYTHON_CHECK_ENV")
        .map(PathBuf::from)
        .unwrap_or_else(|| repository.join("ops/ansible/.venv"));
    let program = prefix.join("bin/python3");
    let arguments = vec![
        "-B".into(),
        repository
            .join("ops/ansible/tests/vps_capacity_oracle.py")
            .to_str()
            .unwrap()
            .into(),
        "--scenario".into(),
        scenario.into(),
    ];
    let environment = std::collections::BTreeMap::from([
        ("PATH".into(), prefix.join("bin").to_str().unwrap().into()),
        ("LC_ALL".into(), "C".into()),
        ("LANG".into(), "C".into()),
    ]);
    let mut owner = Owner::new(super::control::now().unwrap(), Mode::Invoke).unwrap();
    let mut runner = Runner::new();
    let result = runner.run(
        super::process::Invocation {
            program: &program,
            arguments: &arguments,
            environment: &environment,
            seconds: 20,
            maximum: protocol::MAX_RESPONSE,
            cleanup: false,
        },
        &mut owner,
    );
    let finished = owner.finish();
    let output = result.unwrap();
    finished.unwrap();
    assert_eq!(output.status, Some(0), "original Python oracle failed");
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schemaVersion"], 1);
    assert_eq!(value["scenario"], scenario);
    value["observation"].clone()
}

#[test]
#[ignore = "requires pinned controller Python; run by operations automation"]
fn whole_host_policy_matches_original_python_oracle() {
    for scenario in [
        "success",
        "quality-failure",
        "lost-start-ack",
        "recover-clean",
        "recover-unknown",
        "missing-outcome",
        "unit-gc",
        "build-failure",
    ] {
        let original = reference(scenario);
        let harness = Harness::new();
        {
            let mut host = harness.host.borrow_mut();
            host.quality = scenario != "quality-failure";
            host.leave_residuals = matches!(scenario, "recover-clean" | "recover-unknown");
            host.include_unknown = scenario == "recover-unknown";
            host.lose_ack = matches!(
                scenario,
                "lost-start-ack" | "recover-clean" | "recover-unknown"
            );
            host.build_failed = scenario == "build-failure";
        }
        harness.rpc(Action::Preflight).unwrap();
        harness.rpc(Action::Build).unwrap();
        if scenario == "build-failure" {
            let state = harness
                .session(|session, _| session.unit(protocol::Unit::Build, false))
                .unwrap();
            assert!(!state.successful());
            assert_eq!(original["runs"][0]["passed"], false);
            assert_eq!(harness.host.borrow().starts, 0);
            assert_eq!(harness.host.borrow().engine_runs, 0);
            continue;
        }
        let start = harness.rpc(Action::Start);
        assert_eq!(start.is_err(), harness.host.borrow().lose_ack);
        let worker = harness.worker();
        assert_eq!(
            worker.is_ok(),
            !matches!(
                scenario,
                "quality-failure" | "recover-clean" | "recover-unknown"
            )
        );
        {
            let host = harness.host.borrow();
            let normalized = host.capacity_arguments[1..]
                .iter()
                .map(|value| value.replace(ROOT, "<host>"))
                .collect::<Vec<_>>();
            assert_eq!(
                serde_json::to_value(normalized).unwrap(),
                original["capacityArguments"],
                "{scenario}"
            );
            let old = original["systemdProperties"].as_array().unwrap();
            for invariant in [
                "--property=Type=exec",
                "--property=RemainAfterExit=yes",
                "--property=RuntimeMaxSec=120",
                "--property=TimeoutStopSec=600",
                "--property=KillMode=mixed",
                "--property=Restart=no",
                "--property=UMask=0077",
                "--property=LimitCORE=0",
            ] {
                assert!(
                    host.systemd_properties.iter().any(|v| v == invariant),
                    "{scenario}: {invariant}"
                );
                assert!(
                    old.contains(&json!(invariant)),
                    "reference: {scenario}: {invariant}"
                );
            }
        }
        if scenario == "lost-start-ack" {
            let ResultData::Lookup(lookup) = harness.rpc(Action::Lookup).unwrap() else {
                panic!("lookup")
            };
            assert_eq!(lookup.request_json.as_bytes(), harness.workload.as_json());
            assert_eq!(original["runs"][0]["passed"], false);
            assert!(
                original["retainedFiles"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("outcome.json"))
            );
            assert!(
                harness
                    .temporary
                    .path(&format!("{}/outcome.json", harness.header.evidence_path()))
                    .is_file()
            );
            continue;
        }
        if scenario == "missing-outcome" {
            fs::remove_file(
                harness
                    .temporary
                    .path(&format!("{}/outcome.json", harness.header.evidence_path())),
            )
            .unwrap();
            let mut host = harness.host.borrow_mut();
            host.unit = unit("loaded", "active", "exited", "0")
                .replace("Result=success", "Result=timeout")
                .replace("ExecMainStatus=0", "ExecMainStatus=137");
            host.save(
                CURRENT,
                &json!({"schemaVersion":1,"run":RUN,"finalized":false}),
            );
        }
        if matches!(scenario, "recover-clean" | "recover-unknown") {
            harness.rpc(Action::Recover).unwrap();
        }
        let ResultData::Collect(mut collection) = harness.rpc(Action::Collect).unwrap() else {
            panic!("collection")
        };
        if scenario == "unit-gc" {
            let ResultData::Collect(repeated) = harness.rpc(Action::Collect).unwrap() else {
                panic!("collection")
            };
            collection = repeated;
            assert_eq!(collection.unit.retained, Some(true));
        }
        let old = original["runs"].as_array().unwrap().last().unwrap();
        assert_eq!(json!(collection.passed), old["passed"], "{scenario}");
        assert_eq!(
            json!(collection.cleanup.clean),
            old["cleanupClean"],
            "{scenario}"
        );
        let outcome = protocol::Outcome::decode(collection.outcome_json.as_bytes()).unwrap();
        assert_eq!(json!(outcome.passed()), old["outcomePassed"], "{scenario}");
        assert_eq!(
            json!(outcome.finalized()),
            old["outcomeFinalized"],
            "{scenario}"
        );
        assert_eq!(
            harness.json(CURRENT)["finalized"],
            original["current"]["finalized"],
            "{scenario}"
        );
        assert_eq!(
            serde_json::to_value(&harness.host.borrow().destructive).unwrap(),
            original["destructive"],
            "{scenario}"
        );
        assert_eq!(
            serde_json::to_value(&harness.host.borrow().containers).unwrap(),
            original["containers"],
            "{scenario}"
        );
        assert_eq!(harness.host.borrow().starts, 1);
        assert_eq!(harness.host.borrow().engine_runs, 1);
    }
}
