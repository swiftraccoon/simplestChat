//! Actual inert child fixtures for the shared Native combined-log mode.

use super::*;
use crate::ci::{bounded_log::Log, command::Program};
use std::{
    fs::{self, File, OpenOptions},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    path: PathBuf,
    environment: Environment,
}
impl Fixture {
    fn new(body: &str) -> Self {
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "native-log-fixture-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let program = path.join("docker");
        fs::write(
            &program,
            format!(
                "#!/usr/bin/python3\nimport os,signal,sys,time\nfrom pathlib import Path\nroot=Path(os.environ['FIXTURE_ROOT'])\n{body}\n"
            ),
        )
        .unwrap();
        fs::set_permissions(program, fs::Permissions::from_mode(0o700)).unwrap();
        let environment = Environment::from([
            ("PATH".into(), path.clone().into()),
            ("FIXTURE_ROOT".into(), path.clone().into()),
        ]);
        Self { path, environment }
    }
    fn log(&self, maximum: u64) -> Log {
        let output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(self.path.join("output.log"))
            .unwrap();
        Log::start(output, maximum).unwrap()
    }
    fn request(&self, maximum: u64) -> Command {
        let mut command = Command::capture(Program::Docker, &["literal-argument"]);
        command.capture = None;
        command.combined_log = Some(Arc::new(self.log(maximum)));
        command.timeout = Duration::from_secs(3);
        command.grace = Duration::from_millis(20);
        command
    }
    fn native(&self, signals: Signals) -> Native<'_> {
        Native::new(&self.path, &self.environment, signals)
    }
    fn bytes(&self) -> Vec<u8> {
        fs::read(self.path.join("output.log")).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).unwrap();
    }
}
fn signals() -> Signals {
    let (_, receiver) = tokio::sync::mpsc::unbounded_channel();
    Signals::injected(receiver)
}
async fn ready(path: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
fn close(file: File) {
    nix::unistd::close(file).unwrap();
}

impl Fixture {
    fn separate(&self, stdout_maximum: u64, stderr_maximum: u64) -> (Command, Vec<i32>) {
        use std::os::fd::AsRawFd;
        let mut descriptors = Vec::new();
        let mut log = |name, maximum| {
            let file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(self.path.join(name))
                .unwrap();
            descriptors.push(file.as_raw_fd());
            let log = Log::start(file, maximum).unwrap();
            descriptors.extend(log.pipe_fds());
            log
        };
        let logs = SeparateLogs {
            stdout: log("stdout.bin", stdout_maximum),
            stderr: log("stderr.log", stderr_maximum),
        };
        let mut command = Command::capture(Program::Docker, &["literal-argument"]);
        command.capture = None;
        command.separate_logs = Some(Arc::new(logs));
        command.timeout = Duration::from_secs(3);
        command.grace = Duration::from_millis(20);
        (command, descriptors)
    }
    fn input(&self, bytes: &[u8], offset: u64) -> (Arc<File>, i32) {
        use std::os::fd::AsRawFd;
        fs::write(self.path.join("input.bin"), bytes).unwrap();
        let mut file = File::open(self.path.join("input.bin")).unwrap();
        file.seek(std::io::SeekFrom::Start(offset)).unwrap();
        let descriptor = file.as_raw_fd();
        (Arc::new(file), descriptor)
    }
}
fn closed(descriptors: &[i32]) {
    for descriptor in descriptors {
        assert!(
            !Path::new(&format!("/dev/fd/{descriptor}")).exists(),
            "descriptor {descriptor} was not closed"
        );
    }
}

#[tokio::test]
async fn selected_zstd_keeps_binary_stdout_separate_and_consumes_current_input_cursor() {
    let fixture = Fixture::new(concat!(
        "assert sys.argv[1:]==['literal-argument']\n",
        "os.write(1,sys.stdin.buffer.read())\n",
        "os.write(2,b'PRIVATE_STDERR\\x00\\xff')"
    ));
    let proof = Arc::new(
        crate::ci::release_checks::files::Executable::open(&fixture.path.join("docker")).unwrap(),
    );
    let payload: Vec<u8> = (0..=255).collect();
    let source = [b"skip".as_slice(), &payload].concat();
    let (input, input_fd) = fixture.input(&source, 4);
    let (mut command, mut descriptors) = fixture.separate(512, 128);
    command.program = Program::Zstd;
    command.zstd = Some(proof.clone());
    command.stdin = Some(input);
    descriptors.push(input_fd);
    // The proof must select its retained target even with no ambient zstd.
    let environment = Environment::from([("FIXTURE_ROOT".into(), fixture.path.clone().into())]);
    let mut native = Native::new(&fixture.path, &environment, signals());
    assert!(native.run(command).await.unwrap().is_empty());
    assert!(native.cleanup_passed());
    assert_eq!(fs::read(fixture.path.join("stdout.bin")).unwrap(), payload);
    assert_eq!(
        fs::read(fixture.path.join("stderr.log")).unwrap(),
        b"PRIVATE_STDERR\x00\xff"
    );
    closed(&descriptors);
    Arc::try_unwrap(proof).unwrap().close().unwrap();
}

#[tokio::test]
async fn separate_mode_retires_descendants_before_finishing_both_pipe_eofs() {
    use std::os::unix::process::CommandExt;
    let mut unrelated = std::process::Command::new("/bin/sleep");
    unrelated.arg("30").process_group(0);
    let unrelated = Group::new(unrelated.spawn().unwrap());
    let fixture = Fixture::new(concat!(
        "child=os.fork()\n",
        "if child==0:\n",
        " signal.signal(signal.SIGTERM,signal.SIG_IGN)\n",
        " (root/'worker.ready').write_text('ready')\n",
        " while True:time.sleep(.01)\n",
        "while not (root/'worker.ready').exists():time.sleep(.005)\n",
        "os.write(1,b'binary\\x00\\xff')\n",
        "os.write(2,b'private diagnostic')"
    ));
    let (command, descriptors) = fixture.separate(128, 128);
    let mut native = fixture.native(signals());
    assert!(native.run(command).await.unwrap().is_empty());
    assert!(native.cleanup_passed());
    assert!(!crate::owned_process::exited_now(unrelated.pid()).unwrap());
    assert_eq!(
        fs::read(fixture.path.join("stdout.bin")).unwrap(),
        b"binary\x00\xff"
    );
    assert_eq!(
        fs::read(fixture.path.join("stderr.log")).unwrap(),
        b"private diagnostic"
    );
    closed(&descriptors);
    drop(unrelated);
}

#[tokio::test]
async fn either_separate_stream_overflow_refuses_success_and_closes_both_logs() {
    for stream in [1, 2] {
        let fixture = Fixture::new(&format!(
            concat!(
                "try:\n",
                " while True:os.write({stream},b'PRIVATE_OUTPUT'*1024)\n",
                "except BrokenPipeError:\n",
                " while True:time.sleep(.01)"
            ),
            stream = stream
        ));
        let (command, descriptors) = fixture.separate(8, 8);
        let mut native = fixture.native(signals());
        let error = native.run(command).await.unwrap_err();
        assert_eq!(error.code, 1);
        assert!(!error.message.contains("PRIVATE"));
        assert!(native.cleanup_passed());
        assert_eq!(
            fs::metadata(fixture.path.join(if stream == 1 {
                "stdout.bin"
            } else {
                "stderr.log"
            }))
            .unwrap()
            .len(),
            8
        );
        closed(&descriptors);
    }
}

#[tokio::test]
async fn separate_known_statuses_survive_unproven_retirement_and_close_input() {
    for code in [124, 130, 143] {
        let fixture = Fixture::new(IDLE);
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut native = fixture.native(Signals::injected(receiver));
        let (mut command, mut descriptors) = fixture.separate(128, 128);
        let (input, input_fd) = fixture.input(b"owned input", 0);
        command.stdin = Some(input);
        descriptors.push(input_fd);
        if code == 124 {
            command.timeout = Duration::from_secs(1);
        }
        let cancel = async {
            ready(&fixture.path.join("ready")).await;
            if code != 124 {
                sender.send(code).unwrap();
            }
        };
        let (result, ()) = tokio::join!(native.execute_with(command, &Unproven), cancel);
        assert_eq!(result.unwrap_err().code, code);
        assert!(!native.cleanup_passed());
        closed(&descriptors);
    }
    for code in [0, 42, 130, 143] {
        let fixture = Fixture::new(&format!("raise SystemExit({code})"));
        let (command, descriptors) = fixture.separate(128, 128);
        let mut native = fixture.native(signals());
        let error = native
            .execute_with(command, &UnprovenCleanup)
            .await
            .unwrap_err();
        assert_eq!(error.code, if code == 0 { 1 } else { code });
        assert!(!native.cleanup_passed());
        closed(&descriptors);
    }
}

struct ChangeFinalLog(PathBuf);
impl Finalizer for ChangeFinalLog {
    async fn finish(&self, group: &mut Group) -> (bool, Result<std::process::ExitStatus>) {
        let result = Finalize.finish(group).await;
        // The parent's writer remains owned until Log.finish. Wait for the
        // bounded reader's actual write, then change its final length before
        // EOF. This deterministically fails final metadata without leaking a
        // child or relying on a scheduling delay.
        tokio::time::timeout(Duration::from_secs(3), async {
            while fs::metadata(&self.0).unwrap().len() != 6 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let file = OpenOptions::new().write(true).open(&self.0).unwrap();
        file.set_len(7).unwrap();
        close(file);
        result
    }
}

#[tokio::test]
async fn final_sink_failure_attempts_peer_closes_and_preserves_known_primary() {
    for code in [0, 42, 124, 130, 143] {
        let fixture = Fixture::new(&format!(
            concat!(
                "os.write(1,b'logged')\n",
                "signal.signal(signal.SIGTERM,signal.SIG_IGN)\n",
                "signal.signal(signal.SIGINT,signal.SIG_IGN)\n",
                "(root/'ready').write_text('ready')\n",
                "if {code} in (0,42):raise SystemExit({code})\n",
                "while True:time.sleep(.01)"
            ),
            code = code
        ));
        let (mut command, mut descriptors) = fixture.separate(128, 128);
        let (input, input_fd) = fixture.input(b"owned input", 0);
        command.stdin = Some(input);
        descriptors.push(input_fd);
        if code == 124 {
            command.timeout = Duration::from_secs(1);
        }
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut native = fixture.native(Signals::injected(receiver));
        let cancel = async {
            if matches!(code, 130 | 143) {
                ready(&fixture.path.join("ready")).await;
                sender.send(code).unwrap();
            }
        };
        let finalizer = ChangeFinalLog(fixture.path.join("stdout.bin"));
        let (result, ()) = tokio::join!(native.execute_with(command, &finalizer), cancel);
        assert_eq!(result.unwrap_err().code, if code == 0 { 1 } else { code });
        assert!(native.cleanup_passed());
        closed(&descriptors);
    }
}

#[tokio::test]
async fn paired_setup_and_spawn_refusals_settle_every_unique_owner_without_dispatch() {
    for failure in [
        "capture",
        "combined",
        "stdout",
        "role",
        "missing",
        "permission",
        "cancel",
    ] {
        use std::os::fd::AsRawFd;
        let fixture = Fixture::new("(root/'dispatched').write_text('unexpected')");
        let (mut command, mut descriptors) = fixture.separate(128, 128);
        let (input, input_fd) = fixture.input(b"owned input", 0);
        command.stdin = Some(input);
        descriptors.push(input_fd);
        let expected = match failure {
            "capture" => {
                command.capture = Some(8);
                1
            }
            "combined" => {
                let log = fixture.log(128);
                descriptors.extend(log.pipe_fds());
                command.combined_log = Some(Arc::new(log));
                1
            }
            "stdout" => {
                let file = OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .mode(0o600)
                    .open(fixture.path.join("legacy-sink"))
                    .unwrap();
                descriptors.push(file.as_raw_fd());
                command.stdout = Some(Arc::new(file));
                1
            }
            "role" => {
                command.program = Program::Zstd;
                1
            }
            "missing" => {
                fs::remove_file(fixture.path.join("docker")).unwrap();
                127
            }
            "permission" => {
                fs::set_permissions(
                    fixture.path.join("docker"),
                    fs::Permissions::from_mode(0o600),
                )
                .unwrap();
                126
            }
            _ => 143,
        };
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        if failure == "cancel" {
            sender.send(143).unwrap();
        }
        let mut native = fixture.native(Signals::injected(receiver));
        assert_eq!(native.run(command).await.unwrap_err().code, expected);
        assert!(!fixture.path.join("dispatched").exists());
        assert!(native.cleanup_passed());
        closed(&descriptors);
    }
}

#[tokio::test]
async fn invalid_or_borrowed_input_cannot_skip_unique_peer_log_settlement() {
    use std::os::fd::AsRawFd;
    for kind in ["writable", "directory", "pipe", "borrowed"] {
        let fixture = Fixture::new("(root/'dispatched').write_text('unexpected')");
        let (mut command, mut descriptors) = fixture.separate(128, 128);
        let (file, writer) = match kind {
            "writable" => (
                OpenOptions::new()
                    .create_new(true)
                    .read(true)
                    .write(true)
                    .open(fixture.path.join("input.bin"))
                    .unwrap(),
                None,
            ),
            "directory" => (File::open(&fixture.path).unwrap(), None),
            "pipe" => {
                let (read, write) = nix::unistd::pipe().unwrap();
                (File::from(read), Some(File::from(write)))
            }
            _ => {
                fs::write(fixture.path.join("input.bin"), b"input").unwrap();
                (File::open(fixture.path.join("input.bin")).unwrap(), None)
            }
        };
        let input_fd = file.as_raw_fd();
        let mut retained = None;
        let input = Arc::new(file);
        if kind == "borrowed" {
            retained = Some(input.clone());
        } else {
            descriptors.push(input_fd);
        }
        command.stdin = Some(input);
        let mut native = fixture.native(signals());
        assert_eq!(native.run(command).await.unwrap_err().code, 1);
        assert!(!fixture.path.join("dispatched").exists());
        closed(&descriptors);
        if let Some(input) = retained {
            assert!(Path::new(&format!("/dev/fd/{input_fd}")).exists());
            close(Arc::try_unwrap(input).unwrap());
        }
        if let Some(writer) = writer {
            close(writer);
        }
    }
}

#[tokio::test]
async fn unforwarded_and_borrowed_log_refusals_close_unique_input_and_sink_peers() {
    for borrowed in [false, true] {
        let fixture = Fixture::new("(root/'dispatched').write_text('unexpected')");
        let (mut command, mut descriptors) = fixture.separate(128, 128);
        let (input, input_fd) = fixture.input(b"input", 0);
        command.stdin = Some(input);
        descriptors.push(input_fd);
        let retained = if borrowed {
            Some(command.separate_logs.as_ref().unwrap().clone())
        } else {
            None
        };
        let result = settle_unforwarded(command).await;
        assert_eq!(result.is_err(), borrowed);
        assert!(!fixture.path.join("dispatched").exists());
        if let Some(logs) = retained {
            // Only the unique input was acquired. Borrowed log ownership stays
            // outside Native until this caller supplies the final owner.
            closed(&[input_fd]);
            finish_separate(Some(Arc::try_unwrap(logs).unwrap()))
                .await
                .unwrap();
        }
        closed(&descriptors);
    }
}

#[tokio::test]
async fn zstd_proof_is_disjoint_and_parent_stdio_clone_closes_before_body_settlement() {
    use std::os::unix::fs::MetadataExt;
    let fixture = Fixture::new(concat!(
        "(root/'ready').write_text('ready')\n",
        "while not (root/'release').exists():time.sleep(.005)\n",
        "os.write(1,sys.stdin.buffer.read())"
    ));
    let proof = Arc::new(
        crate::ci::release_checks::files::Executable::open(&fixture.path.join("docker")).unwrap(),
    );
    let (mut command, mut descriptors) = fixture.separate(128, 128);
    let (input, input_fd) = fixture.input(b"bound input", 0);
    // Darwin's fdescfs stat reports its own device number; compare aliases
    // within the same descriptor namespace instead of to the file's st_dev.
    let stamp = fs::metadata(format!("/dev/fd/{input_fd}")).unwrap();
    command.stdin = Some(input);
    command.program = Program::Zstd;
    command.zstd = Some(proof.clone());
    descriptors.push(input_fd);
    let mut native = fixture.native(signals());
    let release = async {
        ready(&fixture.path.join("ready")).await;
        let aliases = fs::read_dir("/dev/fd")
            .unwrap()
            .filter_map(|entry| fs::metadata(entry.ok()?.path()).ok())
            .filter(|value| value.dev() == stamp.dev() && value.ino() == stamp.ino())
            .count();
        assert_eq!(aliases, 1, "transferred stdio clone remained in the parent");
        fs::write(fixture.path.join("release"), b"release").unwrap();
    };
    let (result, ()) = tokio::join!(native.run(command), release);
    result.unwrap();
    closed(&descriptors);
    Arc::try_unwrap(proof).unwrap().close().unwrap();

    for role in [Program::Zstd, Program::Node] {
        let fixture = Fixture::new("(root/'dispatched').write_text('unexpected')");
        let proof = Arc::new(
            crate::ci::release_checks::files::Executable::open(&fixture.path.join("docker"))
                .unwrap(),
        );
        let (mut command, descriptors) = fixture.separate(128, 128);
        command.program = role;
        if role == Program::Zstd {
            command.node = Some(proof.clone());
        } else {
            command.zstd = Some(proof.clone());
        }
        let mut native = fixture.native(signals());
        assert_eq!(native.run(command).await.unwrap_err().code, 1);
        assert!(!fixture.path.join("dispatched").exists());
        closed(&descriptors);
        Arc::try_unwrap(proof).unwrap().close().unwrap();
    }
}

fn manual_tools(fixture: &Fixture) -> crate::ci::load_generator_image::tools::Tools {
    for name in ["git", "zstd"] {
        fs::copy(fixture.path.join("docker"), fixture.path.join(name)).unwrap();
    }
    crate::ci::load_generator_image::tools::Tools::select(&fixture.path, &fixture.environment)
        .unwrap()
}

#[tokio::test]
async fn manual_git_and_docker_use_retained_selection_after_path_poisoning() {
    let fixture = Fixture::new(concat!(
        "assert sys.argv[1:]==['literal-argument']\n",
        "os.write(1,Path(sys.argv[0]).name.encode())\n",
        "os.write(2,b'PRIVATE_SELECTED_DIAGNOSTIC')"
    ));
    let tools = manual_tools(&fixture);
    for program in [Program::Git, Program::Docker] {
        let invocation = tools.invocation(program).unwrap();
        assert_eq!(
            invocation.validate(program).unwrap(),
            fixture.path.join(program.name())
        );
    }
    let zstd = tools.zstd();
    assert_eq!(zstd.validate().unwrap(), fixture.path.join("zstd"));
    drop(zstd);
    let poison = fixture.path.join("poison");
    fs::create_dir(&poison).unwrap();
    for name in ["git", "docker"] {
        let file = poison.join(name);
        fs::write(
            &file,
            "#!/usr/bin/python3\nraise SystemExit('unexpected PATH fallback')\n",
        )
        .unwrap();
        fs::set_permissions(file, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let mut environment = fixture.environment.clone();
    environment.insert("PATH".into(), poison.into());
    let mut native = Native::new(&fixture.path, &environment, signals());
    for program in [Program::Git, Program::Docker] {
        let directory = fixture.path.join(program.name());
        let logs = fixture.path.join(format!("logs-{}", program.name()));
        fs::create_dir(&logs).unwrap();
        let local = Fixture {
            path: logs,
            environment: Environment::new(),
        };
        let (mut command, descriptors) = local.separate(128, 128);
        command.program = program;
        command.load_image = tools.invocation(program);
        native.run(command).await.unwrap();
        assert_eq!(
            fs::read(local.path.join("stdout.bin")).unwrap(),
            directory.file_name().unwrap().as_encoded_bytes()
        );
        assert_eq!(
            fs::read(local.path.join("stderr.log")).unwrap(),
            b"PRIVATE_SELECTED_DIAGNOSTIC"
        );
        closed(&descriptors);
    }
    assert!(native.cleanup_passed());
    tools.finish().unwrap();
}

#[tokio::test]
async fn manual_tool_capsules_refuse_cross_roles_and_multiple_proofs_before_dispatch() {
    for case in ["git-as-docker", "docker-as-git", "extra-proof", "node"] {
        let fixture = Fixture::new("(root/'dispatched').write_text('unexpected')");
        let tools = manual_tools(&fixture);
        let (mut command, mut descriptors) = fixture.separate(128, 128);
        let (input, input_fd) = fixture.input(b"owned input", 0);
        command.stdin = Some(input);
        descriptors.push(input_fd);
        match case {
            "git-as-docker" => {
                command.program = Program::Docker;
                command.load_image = tools.invocation(Program::Git);
            }
            "docker-as-git" => {
                command.program = Program::Git;
                command.load_image = tools.invocation(Program::Docker);
            }
            "extra-proof" => {
                command.program = Program::Git;
                command.load_image = tools.invocation(Program::Git);
                command.zstd = Some(tools.zstd());
            }
            _ => {
                command.program = Program::Node;
                command.load_image = tools.invocation(Program::Git);
            }
        }
        let mut native = fixture.native(signals());
        assert_eq!(native.run(command).await.unwrap_err().code, 1);
        assert!(!fixture.path.join("dispatched").exists());
        assert!(native.cleanup_passed());
        closed(&descriptors);
        tools.finish().unwrap();
    }
}

#[tokio::test]
async fn changed_manual_tool_binding_refuses_without_ambient_fallback() {
    for program in [Program::Git, Program::Docker] {
        let fixture = Fixture::new("(root/'dispatched').write_text('unexpected')");
        let tools = manual_tools(&fixture);
        let selected = fixture.path.join(program.name());
        fs::rename(&selected, selected.with_extension("retained")).unwrap();
        fs::write(&selected, b"#!/usr/bin/python3\nraise SystemExit(0)\n").unwrap();
        fs::set_permissions(selected, fs::Permissions::from_mode(0o700)).unwrap();
        let (mut command, descriptors) = fixture.separate(128, 128);
        command.program = program;
        command.load_image = tools.invocation(program);
        let mut native = fixture.native(signals());
        assert_eq!(native.run(command).await.unwrap_err().code, 1);
        assert!(!fixture.path.join("dispatched").exists());
        closed(&descriptors);
        assert!(tools.finish().is_err());
    }
}

#[tokio::test]
async fn combined_mode_keeps_ordered_streams_private_and_closes_before_success() {
    let fixture = Fixture::new(
        "assert sys.argv[1:]==['literal-argument']\nos.write(1,b'output\\n')\nos.write(2,b'private-error\\n')",
    );
    let mut native = fixture.native(signals());
    assert!(native.run(fixture.request(64)).await.unwrap().is_empty());
    assert!(native.cleanup_passed());
    assert_eq!(fixture.bytes(), b"output\nprivate-error\n");
}

#[tokio::test]
async fn normal_leader_exit_retires_pipe_holding_descendant_before_eof() {
    use std::os::unix::process::CommandExt;
    let mut unrelated_command = std::process::Command::new("/bin/sleep");
    unrelated_command.arg("30").process_group(0);
    let unrelated = Group::new(unrelated_command.spawn().unwrap());
    let fixture = Fixture::new(
        r#"child=os.fork()
if child==0:
    signal.signal(signal.SIGTERM,signal.SIG_IGN)
    (root/'worker').write_text(str(os.getpid()))
    while True:time.sleep(.01)
while not (root/'worker').exists():time.sleep(.005)
os.write(1,b'leader completed\n')
"#,
    );
    let mut native = fixture.native(signals());
    native.run(fixture.request(128)).await.unwrap();
    assert!(native.cleanup_passed());
    assert_eq!(fixture.bytes(), b"leader completed\n");
    assert!(!crate::owned_process::exited_now(unrelated.pid()).unwrap());
    // Darwin can retain dead orphan zombies; live worker states are refused.
    let worker = fs::read_to_string(fixture.path.join("worker")).unwrap();
    let observed = std::process::Command::new("/bin/ps")
        .args(["-o", "stat=", "-p", worker.trim()])
        .output()
        .unwrap();
    let state = String::from_utf8(observed.stdout).unwrap();
    assert!(state.trim().is_empty() || state.trim_start().starts_with('Z'));
    drop(unrelated);
}

#[tokio::test]
async fn overflow_caps_real_file_and_retires_the_child_without_echoing_output() {
    let fixture = Fixture::new("os.write(1,b'PRIVATE-SENTINEL'*4096)\nwhile True:time.sleep(.01)");
    let mut native = fixture.native(signals());
    let error = native.run(fixture.request(32)).await.unwrap_err();
    assert_eq!(error.code, 1);
    assert!(!error.message.contains("PRIVATE-SENTINEL"));
    assert_eq!(fixture.bytes().len(), 32);
    assert!(native.cleanup_passed());
}

const IDLE: &str = r#"signal.signal(signal.SIGTERM,signal.SIG_IGN)
signal.signal(signal.SIGINT,signal.SIG_IGN)
os.write(1,b'private-output\n')
(root/'ready').write_text(str(os.getpid()))
while True:time.sleep(.01)
"#;

#[tokio::test]
async fn combined_timeout_and_interrupt_keep_status_after_closed_log_and_group_cleanup() {
    for code in [124, 130, 143] {
        let fixture = Fixture::new(IDLE);
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut native = fixture.native(Signals::injected(receiver));
        let mut command = fixture.request(128);
        if code == 124 {
            command.timeout = Duration::from_millis(300);
        }
        let cancel = async {
            ready(&fixture.path.join("ready")).await;
            if code != 124 {
                sender.send(code).unwrap();
            }
        };
        let (result, ()) = tokio::join!(native.run(command), cancel);
        assert_eq!(result.unwrap_err().code, code);
        assert_eq!(fixture.bytes(), b"private-output\n");
        assert!(native.cleanup_passed());
    }
}

struct Unproven;
impl Finalizer for Unproven {
    async fn finish(&self, group: &mut Group) -> (bool, Result<std::process::ExitStatus>) {
        // Actually reclaim the inert fixture before injecting an unproven
        // terminal/reap result; the caller must not erase its known primary.
        let _ = Finalize.finish(group).await;
        (
            false,
            Err(Failure::new(1, "Injected terminal/reap failure.")),
        )
    }
}
#[tokio::test]
async fn known_timeout_and_interrupt_survive_unproven_terminal_or_reap() {
    for combined in [false, true] {
        for code in [124, 130, 143] {
            let fixture = Fixture::new(IDLE);
            let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
            let mut native = fixture.native(Signals::injected(receiver));
            let mut command = if combined {
                fixture.request(128)
            } else {
                Command::capture(Program::Docker, &[])
            };
            command.grace = Duration::from_millis(20);
            if code == 124 {
                command.timeout = Duration::from_millis(300);
            }
            let cancel = async {
                ready(&fixture.path.join("ready")).await;
                if code != 124 {
                    sender.send(code).unwrap();
                }
            };
            let (result, ()) = tokio::join!(native.execute_with(command, &Unproven), cancel);
            assert_eq!(result.unwrap_err().code, code);
            assert!(!native.cleanup_passed());
        }
    }
}

struct UnprovenCleanup;
impl Finalizer for UnprovenCleanup {
    async fn finish(&self, group: &mut Group) -> (bool, Result<std::process::ExitStatus>) {
        let (_, reaped) = Finalize.finish(group).await;
        (false, reaped)
    }
}
#[tokio::test]
async fn reaped_nonzero_status_survives_unproven_cleanup_while_zero_is_refused() {
    for combined in [false, true] {
        for code in [0, 42, 130, 143] {
            let fixture = Fixture::new(&format!("raise SystemExit({code})"));
            let mut native = fixture.native(signals());
            let command = if combined {
                fixture.request(128)
            } else {
                Command::capture(Program::Docker, &[])
            };
            let error = native
                .execute_with(command, &UnprovenCleanup)
                .await
                .unwrap_err();
            assert_eq!(error.code, if code == 0 { 1 } else { code });
            assert!(!native.cleanup_passed());
        }
    }
}

#[tokio::test]
async fn setup_spawn_and_prior_cancel_failures_finish_every_admitted_log_descriptor() {
    use std::os::fd::AsRawFd;
    for failure in ["mode", "role", "missing", "permission", "cancel"] {
        let fixture = Fixture::new("(root/'dispatched').write_text('unexpected')");
        let output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(fixture.path.join("output.log"))
            .unwrap();
        let output_fd = output.as_raw_fd();
        let log = Log::start(output, 128).unwrap();
        let pipe_fds = log.pipe_fds();
        let mut command = Command::capture(Program::Docker, &[]);
        command.capture = None;
        command.combined_log = Some(Arc::new(log));
        let expected = match failure {
            "mode" => {
                command.capture = Some(8);
                1
            }
            "role" => {
                command.program = Program::Node;
                1
            }
            "missing" => {
                fs::remove_file(fixture.path.join("docker")).unwrap();
                127
            }
            "permission" => {
                fs::set_permissions(
                    fixture.path.join("docker"),
                    fs::Permissions::from_mode(0o600),
                )
                .unwrap();
                126
            }
            _ => 143,
        };
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        if failure == "cancel" {
            sender.send(143).unwrap();
        }
        let mut native = fixture.native(Signals::injected(receiver));
        assert_eq!(native.run(command).await.unwrap_err().code, expected);
        assert!(!fixture.path.join("dispatched").exists());
        assert!(native.cleanup_passed());
        for fd in [output_fd, pipe_fds[0], pipe_fds[1]] {
            assert!(
                !Path::new(&format!("/dev/fd/{fd}")).exists(),
                "{failure}: {fd}"
            );
        }
        assert!(fixture.bytes().is_empty());
    }
}

struct Finalized(Arc<AtomicBool>);
impl Finalizer for Finalized {
    async fn finish(&self, group: &mut Group) -> (bool, Result<std::process::ExitStatus>) {
        let result = Finalize.finish(group).await;
        self.0.store(true, Ordering::SeqCst);
        result
    }
}
#[tokio::test]
async fn interruption_during_final_log_completion_cannot_return_zero() {
    let fixture = Fixture::new("os.write(1,b'closed child\\n')");
    let log = fixture.log(128);
    let writer = log.writer().unwrap();
    let mut command = Command::capture(Program::Docker, &[]);
    command.capture = None;
    command.combined_log = Some(Arc::new(log));
    let finalized = Arc::new(AtomicBool::new(false));
    let finalizer = Finalized(finalized.clone());
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut native = fixture.native(Signals::injected(receiver));
    let cancel = async {
        while !finalized.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
        sender.send(143).unwrap();
        close(writer);
    };
    let (result, ()) = tokio::join!(native.execute_with(command, &finalizer), cancel);
    assert_eq!(result.unwrap_err().code, 143);
    assert!(native.cleanup_passed());
    assert_eq!(fixture.bytes(), b"closed child\n");
}

#[tokio::test]
async fn combined_mode_refuses_ambiguous_modes_and_borrowed_owners_before_spawn() {
    for borrowed in [false, true] {
        let fixture = Fixture::new("(root/'dispatched').write_text('unexpected')");
        let log = Arc::new(fixture.log(128));
        let mut command = Command::capture(Program::Docker, &[]);
        command.combined_log = Some(log.clone());
        if borrowed {
            command.capture = None;
        }
        let mut native = fixture.native(signals());
        assert!(native.run(command).await.is_err());
        assert!(!fixture.path.join("dispatched").exists());
        assert!(native.cleanup_passed());
        Arc::try_unwrap(log).unwrap().finish().await.unwrap();
    }
}

#[tokio::test]
async fn replay_known_interrupt_and_timeout_survive_unproven_finalization() {
    use std::process::Stdio;
    for code in [124, 130, 143] {
        let fixture = Fixture::new(IDLE);
        let mut command = std::process::Command::new(fixture.path.join("docker"));
        command
            .env_clear()
            .envs(&fixture.environment)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut signals = Signals::injected(receiver);
        let deadline =
            std::time::Instant::now() + Duration::from_secs(if code == 124 { 21 } else { 30 });
        let cancel = async {
            ready(&fixture.path.join("ready")).await;
            if code != 124 {
                sender.send(code).unwrap();
            }
        };
        let (result, ()) = tokio::join!(
            crate::ci::release_controller::replay::run_with_finalizer(
                command,
                b"owned bytes",
                &mut signals,
                deadline,
                &Unproven
            ),
            cancel
        );
        assert_eq!(result.unwrap_err().code, code);
    }
}

#[tokio::test]
async fn replay_unfit_deadline_cannot_dispatch_and_pending_interrupt_has_precedence() {
    use std::process::Stdio;
    for code in [124, 130, 143] {
        let fixture = Fixture::new("(root/'dispatched').write_text('unexpected')");
        let mut command = std::process::Command::new(fixture.path.join("docker"));
        command
            .env_clear()
            .envs(&fixture.environment)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        if code != 124 {
            sender.send(code).unwrap();
        }
        let mut signals = Signals::injected(receiver);
        let result = crate::ci::release_controller::replay::run(
            command,
            b"owned bytes",
            &mut signals,
            std::time::Instant::now(),
        )
        .await;
        assert_eq!(result.unwrap_err().code, code);
        assert!(!fixture.path.join("dispatched").exists());
    }
}

#[tokio::test]
async fn factored_replay_keeps_literal_role_bytes_and_selected_executable_proof() {
    for changed in [false, true] {
        let body = concat!(
            "assert sys.argv[1:]==['ci','release-controller','__replay']\n",
            "(root/'replayed').write_bytes(sys.stdin.buffer.read())\n"
        );
        let fixture = Fixture::new(&format!(
            "{body}{}",
            if changed {
                "with Path(sys.argv[0]).open('a') as output:output.write('# changed\\n')"
            } else {
                ""
            }
        ));
        let outcome = replay_selected(
            &fixture.path,
            &fixture.environment,
            &mut signals(),
            b"192.0.2.44\n",
            false,
            std::time::Instant::now() + Duration::from_secs(30),
            &fixture.path.join("docker"),
        )
        .await;
        assert!(outcome.admitted && outcome.settled);
        assert_eq!(outcome.result.is_ok(), !changed);
        assert_eq!(
            fs::read(fixture.path.join("replayed")).unwrap(),
            b"192.0.2.44\n"
        );
    }
}

#[tokio::test]
async fn factored_replay_refuses_missing_proof_or_pending_outcome_without_dispatch() {
    let fixture = Fixture::new("(root/'dispatched').write_text('unexpected')");
    let missing = replay_selected(
        &fixture.path,
        &fixture.environment,
        &mut signals(),
        b"fixed\n",
        false,
        std::time::Instant::now() + Duration::from_secs(30),
        &fixture.path.join("missing"),
    )
    .await;
    assert!(!missing.admitted && !missing.settled && missing.result.is_err());
    for code in [124, 130, 143] {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        if code != 124 {
            sender.send(code).unwrap();
        }
        let outcome = replay_selected(
            &fixture.path,
            &fixture.environment,
            &mut Signals::injected(receiver),
            b"fixed\n",
            false,
            std::time::Instant::now(),
            &fixture.path.join("docker"),
        )
        .await;
        assert!(outcome.admitted && !outcome.settled);
        assert_eq!(outcome.result.unwrap_err().code, code);
        assert!(!fixture.path.join("dispatched").exists());
    }
}

#[tokio::test]
async fn mdns_evidence_distinguishes_actual_exit124_from_observed_timeout() {
    use crate::ci::mdns::tools::{FixtureTool, Role};
    for timed_out in [false, true] {
        let fixture = Fixture::new(if timed_out { IDLE } else { "sys.exit(124)" });
        let proof = FixtureTool::new(Role::Probe, &fixture.path.join("docker")).unwrap();
        let (mut command, descriptors) = fixture.separate(128, 128);
        command.program = Program::MdnsProbe;
        command.mdns = Some(proof.invocation.clone());
        if timed_out {
            command.timeout = Duration::from_millis(250);
        }
        let mut native = fixture.native(signals());
        assert_eq!(native.run(command).await.unwrap_err().code, 124);
        let observed = proof.invocation.observed().unwrap();
        assert!(observed.settled && observed.group_closed && observed.io_closed);
        assert_eq!(observed.timed_out, timed_out);
        if !timed_out {
            assert_eq!(observed.exit, Some(124));
        }
        assert_eq!(observed.interrupted, None);
        closed(&descriptors);
        proof.finish().unwrap();
    }
}

#[tokio::test]
async fn mdns_admission_spawn_and_final_stream_failures_never_acquire_clean_evidence() {
    use crate::ci::mdns::tools::{FixtureTool, Role};
    for mode in ["missing", "role", "spawn", "stream"] {
        let fixture = Fixture::new("os.write(1,b'FINAL!');sys.exit(42)");
        if mode == "spawn" {
            // A missing literal interpreter fails spawn without depending on
            // platform-specific ENOEXEC shell fallback for an unknown format.
            fs::write(
                fixture.path.join("docker"),
                b"#!/no/such/mdns-fixture-interpreter\n",
            )
            .unwrap();
        }
        let proof = FixtureTool::new(Role::Probe, &fixture.path.join("docker")).unwrap();
        let (mut command, descriptors) = fixture.separate(128, 128);
        command.program = if mode == "role" {
            Program::MdnsCompiler
        } else {
            Program::MdnsProbe
        };
        if mode != "missing" {
            command.mdns = Some(proof.invocation.clone());
        }
        let mut native = fixture.native(signals());
        let result = if mode == "stream" {
            native
                .execute_with(command, &ChangeFinalLog(fixture.path.join("stdout.bin")))
                .await
        } else {
            native.run(command).await
        };
        assert_eq!(
            result.unwrap_err().code,
            match mode {
                "stream" => 42,
                "spawn" => 127,
                _ => 1,
            }
        );
        let evidence = proof.invocation.observed().unwrap();
        if mode == "stream" {
            assert!(evidence.settled && evidence.group_closed && !evidence.io_closed);
            assert_eq!(evidence.exit, Some(42));
        } else {
            assert!(!evidence.settled && !evidence.group_closed);
        }
        assert!(!evidence.timed_out);
        closed(&descriptors);
        proof.finish().unwrap();
    }
}

#[tokio::test]
async fn mdns_proofs_refuse_extra_capsules_reuse_and_changed_selected_binding() {
    use crate::ci::mdns::tools::{FixtureTool, Role};
    for mode in ["extra", "reuse", "replacement"] {
        let fixture = Fixture::new("(root/'dispatched').write_text('unexpected')");
        let proof = FixtureTool::new(Role::Probe, &fixture.path.join("docker")).unwrap();
        let (mut command, descriptors) = fixture.separate(128, 128);
        command.program = Program::MdnsProbe;
        command.mdns = Some(proof.invocation.clone());
        let extra = if mode == "extra" {
            Some(Arc::new(
                crate::ci::release_checks::files::Executable::open(&fixture.path.join("docker"))
                    .unwrap(),
            ))
        } else {
            None
        };
        command.node = extra.clone();
        if mode == "reuse" {
            proof.invocation.begin().unwrap();
        }
        if mode == "replacement" {
            fs::rename(fixture.path.join("docker"), fixture.path.join("retained")).unwrap();
            fs::write(fixture.path.join("docker"), b"#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(
                fixture.path.join("docker"),
                fs::Permissions::from_mode(0o700),
            )
            .unwrap();
        }
        let mut native = fixture.native(signals());
        assert_eq!(native.run(command).await.unwrap_err().code, 1);
        assert!(!fixture.path.join("dispatched").exists());
        if mode == "reuse" {
            proof.invocation.outcome(Some(0), false, None, true);
            proof.invocation.closed(true);
            assert!(proof.invocation.observed().is_err());
        } else {
            let observed = proof.invocation.observed().unwrap();
            assert!(!observed.settled);
            assert!(observed.io_closed);
        }
        if let Some(extra) = extra {
            Arc::try_unwrap(extra).unwrap().close().unwrap();
        }
        closed(&descriptors);
        proof.finish().unwrap();
    }
}

#[tokio::test]
async fn mdns_actual_interruption_stays_distinct_after_group_and_pipe_settlement() {
    use crate::ci::mdns::tools::{FixtureTool, Role};
    for code in [130, 143] {
        let fixture = Fixture::new(IDLE);
        let proof = FixtureTool::new(Role::Probe, &fixture.path.join("docker")).unwrap();
        let (mut command, descriptors) = fixture.separate(128, 128);
        command.program = Program::MdnsProbe;
        command.mdns = Some(proof.invocation.clone());
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut native = fixture.native(Signals::injected(receiver));
        let cancel = async {
            ready(&fixture.path.join("ready")).await;
            sender.send(code).unwrap();
        };
        let (result, ()) = tokio::join!(native.run(command), cancel);
        assert_eq!(result.unwrap_err().code, code);
        let evidence = proof.invocation.observed().unwrap();
        assert_eq!(evidence.interrupted, Some(code));
        assert!(!evidence.timed_out);
        assert!(evidence.settled && evidence.group_closed && evidence.io_closed);
        closed(&descriptors);
        proof.finish().unwrap();
    }
}
