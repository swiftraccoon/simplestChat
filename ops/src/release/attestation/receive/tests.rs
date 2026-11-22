//! Actual owned child/pipe/filesystem tests; all lower tools are inert and local.
use super::super::verification_fixture as cached;
use super::*;
use sha2::{Digest, Sha256};
use std::{collections::VecDeque, fs, os::unix::fs::PermissionsExt};

struct Memory {
    lines: VecDeque<Vec<u8>>,
    outputs: Vec<Vec<u8>>,
    finished: bool,
    fail_final: bool,
}
impl Channel for Memory {
    async fn line(&mut self, owner: &mut Owner) -> Result<Vec<u8>> {
        owner.check(Window::Body)?;
        let line = self.lines.pop_front().ok_or(Cause::Input)?;
        if line.is_empty() || line.len() > 16384 || !line.ends_with(b"\n") {
            return control::failed(owner, Cause::Input);
        }
        Ok(line)
    }
    async fn write(&mut self, bytes: &[u8], final_output: bool, owner: &mut Owner) -> Result<()> {
        if final_output && self.fail_final {
            return control::failed(owner, Cause::Io);
        }
        self.outputs.push(bytes.to_vec());
        if final_output {
            owner.final_work()
        } else {
            owner.check(Window::Body)
        }
    }
    fn finish(&mut self, _owner: &mut Owner) {
        self.finished = true;
    }
}
fn write(path: &std::path::Path, bytes: &[u8], mode: u32) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}
/// Serve the candidate's current artifact from the inert curl and return the
/// envelope frame that selects it.
fn served(cached: &cached::Fixture) -> Vec<u8> {
    let mut envelope = serde_json::to_value(&cached.selection).unwrap();
    let sha = files::hex(&Sha256::digest(
        cached::read(&cached.candidate.join("release-attestation.jsonl")).unwrap(),
    ));
    envelope["attestation"] = serde_json::json!({"predicate":cached.claim,
        "bundleSha256":sha,"verificationSha256":{
        "image.tar":"d".repeat(64),"sbom.spdx.json":"d".repeat(64),
        "runtime-proof.json":"d".repeat(64),"vex.openvex.json":"d".repeat(64)}});
    let mut frame = serde_json::to_vec(&envelope).unwrap();
    frame.push(b'\n');
    let zip = cached::read(&cached.candidate.join("artifact.zip")).unwrap();
    let mut response =
        format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", zip.len()).into_bytes();
    response.extend(zip);
    write(&cached.home.join("raw-response"), &response, 0o600);
    frame
}
struct Fixture {
    cached: cached::Fixture,
    access: Access,
    frame: Vec<u8>,
}
impl Fixture {
    fn new(large: bool) -> Self {
        let mut cached = cached::Fixture::new();
        if large {
            cached
                .rewrite("sbom.spdx.json", &vec![b's'; 1024 * 1024 + 65536])
                .unwrap();
            cached.rebuild().unwrap();
        }
        let base = &cached.root;
        let public = base.join("public");
        for path in [
            &public,
            &public.join("releases"),
            &public.join("results"),
            &base.join("config"),
            &base.join("work"),
        ] {
            fs::create_dir(path).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let ca = base.join("ca.pem");
        write(&ca, b"inert fixed CA; never a trust proof\n", 0o600);
        let frame = served(&cached);
        write(
            &cached.bin.join("curl"),
            concat!(
                "#!/bin/sh\nif [ \"${1-}\" = --disable ] && [ \"${2-}\" = --version ]; then\n",
                "printf '%s\\n' 'curl 8.4.0 inert OpenSSL/3.0' 'Protocols: https'; exit 0; fi\n",
                "/bin/cat > \"$HOME/private-config\" || exit 8\n",
                "printf '%s\\n' \"$@\" > \"$HOME/private-argv\" || exit 9\n",
                "exec /bin/cat \"$HOME/raw-response\"\n",
            )
            .as_bytes(),
            0o700,
        );
        let access = Access {
            root: public,
            config: base.join("config"),
            work: base.join("work"),
            ca,
            uid: rustix::process::geteuid().as_raw(),
            environment: Environment::from([
                (OsString::from("PATH"), cached.bin.clone().into_os_string()),
                (OsString::from("HOME"), cached.home.clone().into_os_string()),
                (OsString::from("GH_TOKEN"), OsString::from("NEVER-FORWARD")),
                (
                    OsString::from("HTTPS_PROXY"),
                    OsString::from("NEVER-FORWARD"),
                ),
            ]),
            free_bytes: Some(u64::MAX),
            fault: None,
        };
        Self {
            cached,
            access,
            frame,
        }
    }
    fn channel(&self) -> Memory {
        Memory {
            lines: VecDeque::from([
                self.frame.clone(),
                b"{\"url\":\"https://fixture.blob.core.windows.net/a/../b?sig=SECRET%2F+q\"}\n"
                    .to_vec(),
            ]),
            outputs: Vec::new(),
            finished: false,
            fail_final: false,
        }
    }
    fn run(&self) -> (bool, Option<Cause>, Memory, (usize, usize)) {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let result = runtime.block_on(async {
            let started = Instant::now();
            let mut owner =
                Owner::new(started, Duration::from_secs(60), Duration::from_secs(10)).unwrap();
            let mut channel = self.channel();
            files::start_probe(None);
            let passed = execute(&self.access, &mut channel, &mut owner, started).await;
            let observations = files::take_probe();
            (passed, owner.error(), channel, observations, owner)
        });
        drop(runtime);
        let (passed, primary, channel, observations, owner) = result;
        let settled = owner.finish();
        (
            passed && settled.is_ok(),
            primary.or(settled.err()),
            channel,
            observations,
        )
    }
    fn report(channel: &Memory) -> serde_json::Value {
        serde_json::from_slice(channel.outputs.last().unwrap()).unwrap()
    }
    fn attempt(&self) -> Option<PathBuf> {
        let revision = self
            .access
            .root
            .join("releases")
            .join(&self.cached.selection.revision);
        fs::read_dir(revision)
            .ok()?
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .as_encoded_bytes()
                    .starts_with(b"download.")
            })
    }
    /// The selected revision's directory below the releases.
    fn destination(&self) -> PathBuf {
        self.access
            .root
            .join("releases")
            .join(&self.cached.selection.revision)
    }
    /// Replace one candidate member, rebuild the artifact around it and serve
    /// the rebuilt artifact with an envelope that selects it.
    fn rebuilt(mut self, name: &str, bytes: &[u8]) -> Self {
        self.cached
            .rewrite(name, bytes)
            .expect("the candidate admits every bound member");
        self.cached.rebuild().expect("a rebuilt candidate artifact");
        self.frame = served(&self.cached);
        self
    }
}
/// The sorted names in one directory, which must exist.
fn names(path: &std::path::Path) -> Vec<String> {
    let mut names = fs::read_dir(path)
        .expect("a fixture directory")
        .map(|entry| {
            let name = entry.expect("a readable entry").file_name();
            name.into_string().expect("fixture names are UTF-8")
        })
        .collect::<Vec<_>>();
    names.sort();
    names
}
/// The inode of one fixture file.
fn inode(path: &std::path::Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    fs::metadata(path).expect("a fixture file").ino()
}
/// Requires a refused run: no success, the exact class in `phase` of a
/// failed, unfinalized report that was or was not settled into an attempt,
/// the channel finished and every original closed by its checked finalizer.
fn assert_refused(
    run: &(bool, Option<Cause>, Memory, (usize, usize)),
    class: &str,
    phase: &str,
    settled: bool,
) -> serde_json::Value {
    let report = Fixture::report(&run.2);
    assert!(!run.0, "{report}");
    assert!(run.2.finished, "{report}");
    assert!(run.3.0 > 0, "{report}");
    assert_eq!(
        run.3.1, 0,
        "an original was closed without its check: {report}"
    );
    assert_eq!(report["failureClass"], class, "{report}");
    assert_eq!(report["phase"], phase, "{report}");
    assert_eq!(report["status"], "failed", "{report}");
    assert_eq!(report["passed"], false, "{report}");
    assert_eq!(report["finalized"], false, "{report}");
    assert_eq!(report["settled"], settled, "{report}");
    report
}
#[test]
fn complete_receive_publishes_both_pins_and_repeats_with_checked_original_closes() {
    use std::os::unix::fs::MetadataExt;
    let fixture = Fixture::new(false);
    let first = fixture.run();
    let first_report = Fixture::report(&first.2);
    let first_attempt = fixture.attempt().unwrap();
    let published = fixture
        .access
        .root
        .join("releases")
        .join(&fixture.cached.selection.revision);
    let inode = fs::metadata(published.join("image.tar")).unwrap().ino();
    let pair_same = fs::metadata(first_attempt.join("image.tar")).unwrap().ino() == inode;
    let journal: serde_json::Value =
        serde_json::from_slice(&cached::read(&first_attempt.join("fetch-outcome.json")).unwrap())
            .unwrap();
    let second = fixture.run();
    let second_report = Fixture::report(&second.2);
    let same = fs::metadata(published.join("image.tar")).unwrap().ino() == inode;
    let argv = cached::read(&fixture.cached.home.join("private-argv")).unwrap();
    let input = cached::read(&fixture.cached.home.join("private-config")).unwrap();
    drop(fixture);
    assert!(
        first.0 && second.0,
        "first={:?} second={:?}",
        first.1,
        second.1
    );
    assert_eq!(first.1, None);
    assert_eq!(second.1, None);
    assert!(first.2.finished && second.2.finished && pair_same && same);
    assert_eq!(first.3.1, 0);
    assert_eq!(second.3.1, 0);
    assert!(first.3.0 > 0 && second.3.0 > 0);
    assert_eq!(first_report["status"], "complete");
    assert_eq!(second_report["passed"], true);
    assert_eq!(journal["settled"], true);
    assert!(!String::from_utf8_lossy(&argv).contains("SECRET"));
    assert!(String::from_utf8_lossy(&input).contains("/a/../b?sig=SECRET%2F+q"));
}
#[test]
fn lower_read_write_extract_and_signal_failures_retain_exact_reference_prefixes() {
    let mut facts = Vec::new();
    for (fault, prefix, progress, class) in [
        (Fault::HttpRead, 1048576, 1048576, "filesystem_error"),
        (Fault::Write, 3, 0, "filesystem_error"),
        (Fault::ExtractWrite, 0, 0, "filesystem_error"),
        (Fault::Interrupt, 1048576, 1048576, "interrupted"),
        (Fault::Alarm, 1048576, 1048576, "deadline_exceeded"),
    ] {
        let mut fixture = Fixture::new(true);
        fixture.access.fault = Some(fault);
        let total = fixture.cached.selection.artifact_zip_bytes;
        let result = fixture.run();
        let report = Fixture::report(&result.2);
        let attempt = fixture.attempt().unwrap();
        let retained = fs::metadata(attempt.join(if fault == Fault::ExtractWrite {
            "image.tar"
        } else {
            "artifact.zip"
        }))
        .map(|m| m.len())
        .unwrap_or(0);
        let expected = if fault == Fault::ExtractWrite {
            3
        } else {
            prefix
        };
        let progress = if fault == Fault::ExtractWrite {
            total
        } else {
            progress
        };
        let journal = attempt.join("fetch-outcome.json").exists();
        drop(fixture);
        facts.push((
            fault, result, report, retained, expected, progress, class, journal,
        ));
    }
    for (fault, result, report, retained, expected, progress, class, journal) in facts {
        assert!(!result.0, "{fault:?}");
        assert!(result.2.finished);
        assert_eq!(result.3.1, 0, "{fault:?}");
        assert_eq!(retained, expected, "{fault:?}");
        assert_eq!(report["downloadedBytes"], progress, "{fault:?}");
        assert_eq!(report["failureClass"], class, "{fault:?}");
        if fault != Fault::Alarm {
            assert!(journal, "{fault:?}");
        }
    }
}
#[test]
fn publication_refusal_keeps_first_link_and_verified_same_inode_eexist_is_accepted() {
    let mut facts = Vec::new();
    for fault in [
        Fault::FirstLink,
        Fault::SecondLink,
        Fault::RaceEqual,
        Fault::RaceDifferent,
        Fault::RaceHardlink,
        Fault::PublishSync,
    ] {
        let mut fixture = Fixture::new(false);
        fixture.access.fault = Some(fault);
        let result = fixture.run();
        let report = Fixture::report(&result.2);
        let dest = fixture
            .access
            .root
            .join("releases")
            .join(&fixture.cached.selection.revision);
        let links = (
            dest.join("release.json").exists(),
            dest.join("image.tar").exists(),
        );
        drop(fixture);
        facts.push((fault, result, report, links));
    }
    for (fault, result, report, links) in facts {
        assert_eq!(result.3.1, 0, "{fault:?}");
        assert!(result.2.finished);
        assert_eq!(
            result.0,
            matches!(fault, Fault::RaceEqual | Fault::RaceHardlink),
            "{fault:?} {report}"
        );
        if fault == Fault::SecondLink {
            assert_eq!(links, (true, false));
        }
        if fault == Fault::FirstLink {
            assert_eq!(links, (false, false));
        }
    }
}
#[test]
fn options_and_raw_json_keep_budgets_integer_domains_and_ambiguity_distinct() {
    let parse = |args: &[&str]| {
        options::Options::parse(&args.iter().map(OsString::from).collect::<Vec<_>>())
    };
    assert!(parse(&["--timeout-seconds=300", "--finalization-seconds=30"]).is_ok());
    for args in [
        vec![],
        vec!["--timeout-seconds", "301", "--finalization-seconds", "1"],
        vec!["--timeout-seconds", "+300", "--finalization-seconds", "1"],
        vec!["--timeout-seconds", "30", "--finalization-seconds", "30"],
        vec![
            "--timeout-seconds",
            "30",
            "--timeout-seconds",
            "30",
            "--finalization-seconds",
            "1",
        ],
    ] {
        assert!(parse(&args).is_err());
    }
    let fixture = Fixture::new(false);
    let mut ambiguous = fixture.frame.clone();
    ambiguous.splice(1..1, b"\"schemaVersion\":1,".iter().copied());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let facts = runtime.block_on(async {
        let mut facts = Vec::new();
        for line in [ambiguous, b"{broken\n".to_vec(), b"[]\n".to_vec()] {
            let started = Instant::now();
            let mut owner =
                Owner::new(started, Duration::from_secs(30), Duration::from_secs(10)).unwrap();
            let mut channel = fixture.channel();
            channel.lines = VecDeque::from([line]);
            let passed = execute(&fixture.access, &mut channel, &mut owner, started).await;
            facts.push((passed, Fixture::report(&channel), channel.finished));
        }
        facts
    });
    drop(runtime);
    drop(fixture);
    for (fact, class) in
        facts
            .iter()
            .zip(["ambiguous_json", "validation_failed", "invalid_request"])
    {
        assert!(!fact.0 && fact.2);
        assert_eq!(fact.1["failureClass"], class);
    }
}

fn signal_worker(mode: &str) {
    use std::os::fd::AsFd;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let base = PathBuf::from(std::env::var_os("RECEIVE_SIGNAL_ROOT").unwrap());
    let input_flags = rustix::fs::fcntl_getfl(io::stdin().as_fd()).unwrap();
    let output_flags = rustix::fs::fcntl_getfl(io::stdout().as_fd()).unwrap();
    let (passed, cause, owner) = runtime.block_on(async {
        let started = Instant::now();
        let mut owner =
            Owner::new(started, Duration::from_secs(30), Duration::from_secs(10)).unwrap();
        let mut channel = Standard::new(&mut owner).unwrap();
        let access = Access {
            root: base.join("public"),
            config: base.join("config"),
            work: base.join("work"),
            ca: base.join("ca.pem"),
            uid: rustix::process::geteuid().as_raw(),
            environment: Environment::from([
                (OsString::from("PATH"), base.join("bin").into_os_string()),
                (OsString::from("HOME"), base.join("home").into_os_string()),
            ]),
            free_bytes: Some(u64::MAX),
            fault: None,
        };
        files::start_probe(None);
        let passed = execute(&access, &mut channel, &mut owner, started).await;
        let facts = files::take_probe();
        (passed, (owner.error(), facts), owner)
    });
    drop(runtime);
    let signal_finished = owner.finish();
    let restored = (
        rustix::fs::fcntl_getfl(io::stdin().as_fd()),
        rustix::fs::fcntl_getfl(io::stdout().as_fd()),
    );
    assert!(!passed);
    assert_eq!(
        cause.0,
        Some(if mode == "INT" {
            Cause::Interrupt
        } else {
            Cause::Terminate
        })
    );
    assert_eq!(cause.1.1, 0);
    assert_eq!(
        signal_finished,
        Err(if mode == "INT" {
            Cause::Interrupt
        } else {
            Cause::Terminate
        })
    );
    assert_eq!(restored.0.unwrap(), input_flags);
    assert_eq!(restored.1.unwrap(), output_flags);
}
fn late_worker(mode: &str) {
    let previous = nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o027));
    let fixture = Fixture::new(false);
    let started = Instant::now();
    COMPLETION_OFFSET.set((mode == "LATE").then_some(Duration::ZERO));
    RESTORE_SIGNAL.set(match mode {
        "LATE-INT" => Some(signal_hook::consts::SIGINT),
        "LATE-TERM" => Some(signal_hook::consts::SIGTERM),
        _ => None,
    });
    FRAME_CLOSED.set(false);
    COMPLETION_AFTER_FRAME.set(false);
    COMPLETION_CAUSE.set(None);
    let code = lifecycle(started, Duration::from_secs(60), || {
        let mut owner =
            Owner::new(started, Duration::from_secs(60), Duration::from_secs(10)).unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let (passed, finished, observations) = runtime.block_on(async {
            let mut channel = fixture.channel();
            files::start_probe(None);
            let passed = execute(&fixture.access, &mut channel, &mut owner, started).await;
            (passed, channel.finished, files::take_probe())
        });
        drop(runtime);
        FRAME_CLOSED.set(passed && finished && observations.1 == 0);
        (passed, Some(owner))
    });
    let current = nix::sys::stat::umask(previous);
    let closed = FRAME_CLOSED.get();
    let after = COMPLETION_AFTER_FRAME.get();
    let cause = COMPLETION_CAUSE.get();
    COMPLETION_OFFSET.set(None);
    RESTORE_SIGNAL.set(None);
    drop(fixture);
    assert!(closed);
    assert_eq!(current.bits(), 0o027);
    assert_eq!(code, ExitCode::FAILURE);
    match mode {
        "LATE-INT" => assert_eq!(cause, Some(Cause::Interrupt)),
        "LATE-TERM" => assert_eq!(cause, Some(Cause::Terminate)),
        _ => {
            assert!(after);
            assert_eq!(cause, None);
        }
    }
}

#[test]
fn actual_standard_input_signals_restore_flags_and_late_teardown_invalidates_success() {
    use crate::owned_process::{self, Group};
    use std::{
        os::unix::process::CommandExt,
        process::{Command, Stdio},
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    if let Ok(mode) = std::env::var("RECEIVE_SIGNAL_WORKER") {
        if mode.starts_with("LATE") {
            late_worker(&mode);
        } else {
            signal_worker(&mode);
        }
        return;
    }
    let fixture = Fixture::new(false);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let facts = runtime.block_on(async {
        let mut facts = Vec::new();
        for mode in ["INT", "TERM", "LATE", "LATE-INT", "LATE-TERM"] {
            let end = Instant::now() + Duration::from_secs(20);
            let child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    concat!(
                        "release::attestation::receive::tests::",
                        "actual_standard_input_signals_restore_flags_",
                        "and_late_teardown_invalidates_success"
                    ),
                    "--nocapture",
                ])
                .env_clear()
                .env("RECEIVE_SIGNAL_WORKER", mode)
                .env("RECEIVE_SIGNAL_ROOT", &fixture.cached.root)
                .process_group(0)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let mut group = Group::new(child);
            let pid = group.pid();
            let input = tokio::process::ChildStdin::from_std(group.child().stdin.take().unwrap());
            let output =
                tokio::process::ChildStdout::from_std(group.child().stdout.take().unwrap());
            let error = tokio::process::ChildStderr::from_std(group.child().stderr.take().unwrap());
            let converted = input.is_ok() && output.is_ok() && error.is_ok();
            let mut input = input.ok();
            let mut output = output.ok();
            let mut error = error.ok();
            let mut out = Vec::new();
            let mut err = Vec::new();
            let mut out_buffer = vec![0; 4096];
            let mut err_buffer = vec![0; 4096];
            let mut ready = false;
            let mut signaled = mode.starts_with("LATE");
            let mut out_eof = false;
            let mut err_eof = false;
            let write = if let Some(input) = &mut input {
                input.write_all(&fixture.frame).await
            } else {
                Err(std::io::Error::other("missing input"))
            };
            while converted && write.is_ok() && Instant::now() < end && !(out_eof && err_eof) {
                tokio::select! {
                    result=async {
                        output.as_mut().unwrap().read(&mut out_buffer).await
                    },if !out_eof => {
                        match result {
                            Ok(0)=>out_eof=true,
                            Ok(n)=>{out.extend_from_slice(&out_buffer[..n]);
                                ready|=out.windows(16).any(|s|s==b"\"status\":\"ready\"");},
                            Err(_)=>break,
                        }
                    },
                    result=async {
                        error.as_mut().unwrap().read(&mut err_buffer).await
                    },if !err_eof => {
                        match result {
                            Ok(0)=>err_eof=true,
                            Ok(n)=>err.extend_from_slice(&err_buffer[..n]),
                            Err(_)=>break,
                        }
                    },
                    _=tokio::time::sleep(Duration::from_millis(5))=>{},
                }
                if ready && !signaled {
                    signaled = rustix::process::kill_process(
                        pid,
                        if mode == "INT" {
                            rustix::process::Signal::INT
                        } else {
                            rustix::process::Signal::TERM
                        },
                    )
                    .is_ok();
                }
                if out.len() > 65536 || err.len() > 65536 {
                    break;
                }
            }
            // Natural pipe EOF can precede the kernel's terminal leader state.
            // Join that NONCONSUMING observation before retirement, keeping the
            // same original end and reserved leader until the sole reap.
            let terminal = tokio::time::timeout_at(end.into(), owned_process::exited(pid))
                .await
                .is_ok_and(|result| result.is_ok());
            let retired = owned_process::finish_before(pid, end).await;
            let status = group.reap().await;
            let mut closes = Vec::new();
            if let Some(pipe) = input.take() {
                closes.push(
                    pipe.into_owned_fd()
                        .and_then(|fd| nix::unistd::close(fd).map_err(std::io::Error::from))
                        .is_ok(),
                );
            }
            if let Some(pipe) = output.take() {
                closes.push(
                    pipe.into_owned_fd()
                        .and_then(|fd| nix::unistd::close(fd).map_err(std::io::Error::from))
                        .is_ok(),
                );
            }
            if let Some(pipe) = error.take() {
                closes.push(
                    pipe.into_owned_fd()
                        .and_then(|fd| nix::unistd::close(fd).map_err(std::io::Error::from))
                        .is_ok(),
                );
            }
            let matched =
                String::from_utf8_lossy(&out).contains("test result: ok. 1 passed; 0 failed;");
            facts.push((
                mode,
                converted,
                write.is_ok(),
                signaled,
                terminal,
                retired,
                status,
                out_eof,
                err_eof,
                closes,
                out,
                err,
                Instant::now() < end,
                matched,
            ));
        }
        facts
    });
    drop(runtime);
    drop(fixture);
    for (
        mode,
        converted,
        written,
        signaled,
        terminal,
        retired,
        status,
        out_eof,
        err_eof,
        closes,
        out,
        err,
        ontime,
        matched,
    ) in facts
    {
        assert!(
            converted
                && written
                && signaled
                && terminal
                && retired
                && out_eof
                && err_eof
                && ontime
                && matched,
            concat!(
                "{} converted={} written={} signaled={} terminal={} retired={} ",
                "outEOF={} errEOF={} ontime={} matched={} stdout={} stderr={}"
            ),
            mode,
            converted,
            written,
            signaled,
            terminal,
            retired,
            out_eof,
            err_eof,
            ontime,
            matched,
            String::from_utf8_lossy(&out),
            String::from_utf8_lossy(&err)
        );
        assert!(closes.len() == 3 && closes.iter().all(|closed| *closed));
        assert!(
            status.unwrap().success(),
            "{mode} {}",
            String::from_utf8_lossy(&err)
        );
    }
}

#[test]
fn persistent_published_alias_replacement_and_nonempty_capath_cannot_authorize_success() {
    let mut facts = Vec::new();
    for fault in [Fault::ReplacePublishedAlias, Fault::CapathMember] {
        let mut fixture = Fixture::new(false);
        fixture.access.fault = Some(fault);
        let result = fixture.run();
        let report = Fixture::report(&result.2);
        let attempt = fixture.attempt().unwrap();
        let archive = attempt.join("artifact.zip").exists();
        let invoked = fixture.cached.home.join("private-config").exists();
        let public = fixture
            .access
            .root
            .join("releases")
            .join(&fixture.cached.selection.revision);
        let retained = public.join("retained-original.json").exists();
        drop(fixture);
        facts.push((fault, result, report, archive, invoked, retained));
    }
    for (fault, result, report, archive, invoked, retained) in facts {
        assert!(!result.0, "{fault:?} {report}");
        assert_eq!(result.3.1, 0);
        assert!(result.2.finished);
        if fault == Fault::CapathMember {
            assert!(!archive && !invoked);
        } else {
            assert!(archive && invoked && retained);
            assert_eq!(result.1, Some(Cause::Changed));
        }
    }
}
#[test]
fn seal_rejects_actual_mode_change_before_adopting_new_metadata_and_closes_every_original() {
    let fixture = Fixture::new(false);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let facts = runtime.block_on(async {
        let started = Instant::now();
        let mut owner =
            Owner::new(started, Duration::from_secs(30), Duration::from_secs(10)).unwrap();
        files::start_probe(None);
        let dir = Dir::open(&fixture.access.root, &mut owner).unwrap();
        let mut file = Retained::create(dir.clone(), "seal-boundary", &mut owner).unwrap();
        files::seal_mode_fault("seal-boundary".into());
        let result = file.write(b"original bytes", &mut owner);
        let metadata = file.file().metadata();
        file.finish(&mut owner);
        dir.finish(&mut owner);
        (result, metadata, owner.error(), files::take_probe())
    });
    drop(runtime);
    drop(fixture);
    use std::os::unix::fs::MetadataExt;
    assert_eq!(facts.0, Err(Cause::Changed));
    assert_eq!(facts.2, Some(Cause::Changed));
    assert_eq!(facts.1.unwrap().mode() & 0o7777, 0o644);
    assert!(facts.3.0 > 0);
    assert_eq!(facts.3.1, 0);
}

#[test]
fn selected_publication_socket_is_refused_by_exact_regular_type_before_open() {
    let fixture = crate::test_support::Fixture::new();
    let path = fixture.0.join("s");
    let started = Instant::now();
    let mut owner = Owner::new(started, Duration::from_secs(30), Duration::from_secs(10)).unwrap();
    let directory = Dir::open(&fixture.0, &mut owner);
    let listener = if directory.is_ok() {
        std::os::unix::net::UnixListener::bind(&path)
    } else {
        Err(std::io::Error::other("socket directory admission refused"))
    };
    let protected = if listener.is_ok() {
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
    } else {
        Err(std::io::Error::other("socket admission refused"))
    };
    files::start_probe(None);
    let checked = match &directory {
        Ok(dir) if protected.is_ok() => dir.protected_file(
            OsStr::new("s"),
            rustix::process::geteuid().as_raw(),
            &mut owner,
        ),
        _ => Err(Cause::Io),
    };
    let closed = listener.map(nix::unistd::close);
    let directory_admitted = directory.is_ok();
    if let Ok(dir) = directory {
        dir.finish(&mut owner);
    }
    let primary = owner.error();
    let final_owner = owner.finish();
    let captures = files::take_probe();
    drop(fixture);
    assert!(directory_admitted && protected.is_ok());
    assert!(matches!(closed, Ok(Ok(()))));
    assert_eq!(checked, Err(Cause::Policy));
    assert_eq!(primary, Some(Cause::Policy));
    assert_eq!(final_owner, Err(Cause::Policy));
    assert_eq!(captures.1, 0);
}

/// Another holder of the workload lock refuses the receive in preflight as
/// `filesystem_error`, before a revision directory or attempt exists; once
/// that holder lets go, the same host receives.
#[test]
fn a_held_workload_lock_refuses_in_preflight_before_any_attempt() {
    let fixture = Fixture::new(false);
    let path = fixture.access.work.join("workload.lock");
    write(&path, b"", 0o600);
    let holder = fs::File::open(&path).expect("the lock file this test wrote");
    let held = rustix::fs::flock(
        &holder,
        rustix::fs::FlockOperation::NonBlockingLockExclusive,
    );
    let refused = fixture.run();
    let releases = names(&fixture.access.root.join("releases"));
    let released = nix::unistd::close(holder);
    let admitted = fixture.run();
    drop(fixture);
    assert!(held.is_ok() && released.is_ok());
    let report = assert_refused(&refused, "filesystem_error", "preflight", false);
    assert_eq!(refused.1, Some(Cause::Io));
    assert_eq!(report["evidence"], serde_json::Value::Null);
    assert_eq!(report["downloadedBytes"], 0);
    assert!(releases.is_empty(), "{releases:?}");
    assert!(admitted.0, "{}", Fixture::report(&admitted.2));
}

/// An unfinished operation journal, `current.json` in the work directory or
/// `release-state.json` in the root, refuses the receive in preflight as
/// `unfinished_operation` and is left as it was; a finalized journal in the
/// same place admits the receive.
#[test]
fn an_unfinished_journal_in_the_work_directory_or_root_refuses_in_preflight() {
    let unfinished = b"{\"schemaVersion\":1,\"finalized\":false}\n";
    let mut facts = Vec::new();
    for (in_work, name) in [(true, "current.json"), (false, "release-state.json")] {
        let fixture = Fixture::new(false);
        let path = if in_work {
            fixture.access.work.join(name)
        } else {
            fixture.access.root.join(name)
        };
        write(&path, unfinished, 0o600);
        let refused = fixture.run();
        let releases = names(&fixture.access.root.join("releases"));
        let kept = fs::read(&path).expect("the journal this test wrote");
        write(&path, b"{\"schemaVersion\":1,\"finalized\":true}\n", 0o600);
        let admitted = fixture.run();
        drop(fixture);
        facts.push((name, refused, releases, kept, admitted));
    }
    for (name, refused, releases, kept, admitted) in facts {
        let report = assert_refused(&refused, "unfinished_operation", "preflight", false);
        assert_eq!(refused.1, Some(Cause::Policy), "{name}");
        assert_eq!(report["evidence"], serde_json::Value::Null, "{name}");
        assert!(releases.is_empty(), "{name}: {releases:?}");
        assert_eq!(kept, unfinished, "{name}");
        assert!(admitted.0, "{name}: {}", Fixture::report(&admitted.2));
    }
}

/// A publication target that is not the verified member is refused in publish
/// as `retained_artifact_differs` and never replaced: a foreign image or a
/// same-length foreign manifest left where an earlier receive published, a
/// foreign image before any publication (both names are checked before
/// either own link, so the manifest stays unpublished), and a foreign
/// manifest that appears just before the own link.
#[test]
fn a_foreign_or_racing_publication_target_is_refused_and_never_replaced() {
    let mut facts = Vec::new();
    for case in [
        "foreign-image",
        "foreign-manifest",
        "first-foreign-image",
        "racing",
    ] {
        let mut fixture = Fixture::new(false);
        let destination = fixture.destination();
        // The racing fault places its foreign manifest, so the image is the
        // name that must stay unpublished.
        let (name, other) = if matches!(case, "foreign-manifest" | "racing") {
            ("release.json", "image.tar")
        } else {
            ("image.tar", "release.json")
        };
        let mut foreign = b"owned receive conflict fixture\n".to_vec();
        if case == "first-foreign-image" {
            fs::create_dir(&destination).expect("an absent revision directory");
            fs::set_permissions(&destination, fs::Permissions::from_mode(0o700))
                .expect("a private revision directory");
            write(&destination.join(name), &foreign, 0o600);
        } else if case == "racing" {
            fixture.access.fault = Some(Fault::RaceDifferent);
            foreign = b"racing-different".to_vec();
        } else {
            let first = fixture.run();
            assert!(first.0, "{}", Fixture::report(&first.2));
            let published = destination.join(name);
            if case == "foreign-manifest" {
                foreign = fs::read(&published).expect("the published manifest");
                foreign[0] ^= 1;
            }
            fs::remove_file(&published).expect("the published name");
            write(&published, &foreign, 0o600);
        }
        let before = (destination.join(other).exists()).then(|| inode(&destination.join(other)));
        let held = (case != "racing").then(|| inode(&destination.join(name)));
        let refused = fixture.run();
        let report = Fixture::report(&refused.2);
        let evidence = PathBuf::from(report["evidence"].as_str().expect("a settled attempt"));
        let journal: serde_json::Value = serde_json::from_slice(
            &cached::read(&evidence.join("fetch-outcome.json")).expect("the attempt's journal"),
        )
        .expect("a JSON journal");
        let after = (
            fs::read(destination.join(name)).expect("the foreign target"),
            (case != "racing").then(|| inode(&destination.join(name))),
            (destination.join(other).exists()).then(|| inode(&destination.join(other))),
        );
        drop(fixture);
        facts.push((case, refused, journal, foreign, held, before, after));
    }
    for (case, refused, journal, foreign, held, before, after) in facts {
        let report = assert_refused(&refused, "retained_artifact_differs", "publish", true);
        assert_eq!(
            journal["failureClass"], "retained_artifact_differs",
            "{case}"
        );
        assert_eq!(journal["phase"], "publish", "{case}");
        assert_eq!(journal["settled"], true, "{case}");
        assert_eq!(after.0, foreign, "{case}: {report}");
        assert_eq!(after.1, held, "{case}");
        // The pair's other name is as the earlier receive left it, and absent
        // when nothing was published before.
        assert_eq!(after.2, before, "{case}");
        assert_eq!(before.is_some(), case.starts_with("foreign-"), "{case}");
    }
}

/// An envelope whose archive checksum is not the served archive's refuses in
/// download as `zip_checksum_mismatch`: the attempt keeps the received bytes
/// beside its envelope and journal, nothing is extracted and nothing is
/// published.
#[test]
fn a_wrong_archive_checksum_refuses_in_download_and_publishes_nothing() {
    let mut fixture = Fixture::new(false);
    let served = fixture.cached.selection.zip_sha256.clone();
    let wrong = if served == "0".repeat(64) { "1" } else { "0" }.repeat(64);
    let mut envelope: serde_json::Value =
        serde_json::from_slice(&fixture.frame).expect("the fixture's own envelope");
    envelope["zipSha256"] = wrong.clone().into();
    fixture.frame = serde_json::to_vec(&envelope).expect("an encodable envelope");
    fixture.frame.push(b'\n');
    let refused = fixture.run();
    let attempt = fixture.attempt().expect("the refused attempt");
    let kept = names(&attempt);
    let received = files::hex(&Sha256::digest(
        cached::read(&attempt.join("artifact.zip")).expect("the received archive"),
    ));
    let destination = names(&fixture.destination());
    drop(fixture);
    let report = assert_refused(&refused, "zip_checksum_mismatch", "download", true);
    assert_eq!(report["zipSha256"], wrong);
    assert_eq!(
        kept,
        ["artifact.zip", "envelope.json", "fetch-outcome.json"]
    );
    assert_eq!(received, served);
    assert_eq!(destination.len(), 1, "{destination:?}");
    assert!(destination[0].starts_with("download."), "{destination:?}");
}

/// A manifest whose revision is not the selected one refuses in verify as
/// `release_revision_mismatch`, although the build outcome and source record
/// name the selected revision; nothing is published.
#[test]
fn a_manifest_for_another_revision_refuses_in_verify_and_publishes_nothing() {
    let fixture = Fixture::new(false);
    let selected = fixture.cached.selection.revision.clone();
    let other = "b".repeat(40);
    let mut manifest: serde_json::Value = serde_json::from_slice(
        &cached::read(&fixture.cached.candidate.join("release.json"))
            .expect("the candidate manifest"),
    )
    .expect("a JSON manifest");
    manifest["revision"] = other.clone().into();
    manifest["imageTag"] = format!("simplestchat-release/production:{other}").into();
    let manifest = serde_json::to_vec(&manifest).expect("an encodable manifest");
    let fixture = fixture.rebuilt("release.json", &manifest);
    let refused = fixture.run();
    let attempt = fixture.attempt().expect("the refused attempt");
    let extracted = cached::read(&attempt.join("release.json")).expect("the extracted manifest");
    let destination = names(&fixture.destination());
    drop(fixture);
    assert_ne!(other, selected);
    let report = assert_refused(&refused, "release_revision_mismatch", "verify", true);
    assert_eq!(report["revision"], selected);
    assert_eq!(extracted, manifest);
    assert_eq!(destination.len(), 1, "{destination:?}");
    assert!(destination[0].starts_with("download."), "{destination:?}");
}
