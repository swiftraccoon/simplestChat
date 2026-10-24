//! Actual registered INT/TERM during a retained hash; all fixtures are inert.

use super::*;
use crate::owned_process::{self, Group};
use std::{
    fs,
    os::unix::process::CommandExt,
    process::{Command, Stdio},
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt};

const CHILD: &str = "release::attestation::prepare::signal_tests::retained_hash_signal_child";
const READY: &[u8] = b"PREPARE-HASH-READY\n";
const MAXIMUM: usize = 65536;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "owned child fixture; invoke only through its parent signal test"]
async fn retained_hash_signal_child() {
    let expected = match std::env::var("SC_PREPARE_EXPECT_SIGNAL").as_deref() {
        Ok("INT") => Cause::Interrupt,
        Ok("TERM") => Cause::Terminate,
        _ => panic!("owned signal fixture requires its parent"),
    };
    let start = Instant::now();
    let mut owner = Owner::new(start, Duration::from_secs(30), Duration::from_secs(5)).unwrap();
    let artifact = std::env::var_os("SC_PREPARE_ARTIFACT").unwrap();
    let security = std::env::var_os("SC_PREPARE_SECURITY").unwrap();
    let options = Options::parse(&[
        "--artifact-dir".into(),
        artifact,
        "--security-dir".into(),
        security,
        "--repository".into(),
        "owner/repo".into(),
        "--revision".into(),
        super::tests::REV.into(),
        "--run-id".into(),
        "789".into(),
        "--run-attempt".into(),
        "1".into(),
        "--timeout-seconds".into(),
        "30".into(),
        "--finalization-seconds".into(),
        "5".into(),
    ])
    .unwrap()
    .unwrap();
    files::start_probe(None);
    let mut state = State::new();
    let hashed: Result<()> = (|| {
        let file = state.input(6, &options, &mut owner)?;
        files::announce_next_hash();
        file.hash(&mut owner).map(|_| ())
    })();
    if let Err(error) = hashed {
        owner.record(error);
    }
    state.finish(&mut owner);
    let closes = files::take_probe();
    assert_eq!(owner.error(), Some(expected));
    assert!(closes.0 > 0 && closes.1 == 0);
}

async fn read(
    reader: &mut (impl AsyncRead + Unpin),
    bytes: &mut Vec<u8>,
    ready: bool,
) -> std::io::Result<()> {
    let mut buffer = vec![0; 4096];
    loop {
        if ready && bytes.windows(READY.len()).any(|part| part == READY) {
            return Ok(());
        }
        let size = reader.read(&mut buffer).await?;
        if size == 0 {
            return if ready {
                Err(std::io::Error::other("owned child closed before readiness"))
            } else {
                Ok(())
            };
        }
        if bytes.len() + size > MAXIMUM {
            return Err(std::io::Error::other("owned child output limit"));
        }
        bytes.extend_from_slice(&buffer[..size]);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn original_interrupt_and_termination_during_retained_hash_settle_all_handles() {
    for (name, signal) in [
        ("INT", rustix::process::Signal::INT),
        ("TERM", rustix::process::Signal::TERM),
    ] {
        let entered = Instant::now();
        let end = entered + Duration::from_secs(15);
        let fixture = super::tests::Fixture::new();
        let file = fs::OpenOptions::new()
            .write(true)
            .open(fixture.security.join("native.json"))
            .unwrap();
        let sized = file.set_len(512 * 1024 * 1024);
        let closed = nix::unistd::close(file);
        sized.unwrap();
        closed.unwrap();
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                CHILD,
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env_clear()
            .env("SC_PREPARE_EXPECT_SIGNAL", name)
            .env("SC_PREPARE_ARTIFACT", &fixture.artifact)
            .env("SC_PREPARE_SECURITY", &fixture.security)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .unwrap();
        let mut group = Group::new(child);
        let pid = group.pid();
        let out = group
            .child()
            .stdout
            .take()
            .map(tokio::process::ChildStdout::from_std)
            .transpose();
        let err = group
            .child()
            .stderr
            .take()
            .map(tokio::process::ChildStderr::from_std)
            .transpose();
        let converted =
            out.as_ref().is_ok_and(Option::is_some) && err.as_ref().is_ok_and(Option::is_some);
        let mut out = out.ok().flatten();
        let mut err = err.ok().flatten();
        let mut output = Vec::new();
        let mut errors = Vec::new();
        let announced = match out.as_mut() {
            Some(out) if converted => Some(
                tokio::time::timeout_at(
                    (end - Duration::from_secs(8)).into(),
                    read(out, &mut output, true),
                )
                .await,
            ),
            _ => None,
        };
        let signalled = if matches!(announced, Some(Ok(Ok(())))) && owned_process::reserved(pid) {
            rustix::process::kill_process_group(pid, signal).is_ok()
        } else {
            false
        };
        let drained = match (out.as_mut(), err.as_mut()) {
            (Some(out), Some(err)) => Some(
                tokio::time::timeout_at((end - Duration::from_secs(3)).into(), async {
                    tokio::join!(
                        read(out, &mut output, false),
                        read(err, &mut errors, false),
                        owned_process::exited(pid)
                    )
                })
                .await,
            ),
            _ => None,
        };
        let retired = owned_process::finish_before(pid, end).await;
        let status = group.reap().await;
        let out_closed = out.is_some_and(|out| {
            out.into_owned_fd()
                .and_then(|fd| nix::unistd::close(fd).map_err(std::io::Error::from))
                .is_ok()
        });
        let err_closed = err.is_some_and(|err| {
            err.into_owned_fd()
                .and_then(|fd| nix::unistd::close(fd).map_err(std::io::Error::from))
                .is_ok()
        });
        assert!(converted, "both consuming reader conversions must succeed");
        assert!(retired && signalled);
        assert!(matches!(announced, Some(Ok(Ok(())))));
        assert!(matches!(drained, Some(Ok((Ok(()), Ok(()), Ok(()))))));
        assert!(out_closed && err_closed);
        assert!(status.unwrap().success(), "owned signal child failed");
        assert!(Instant::now() < end);
    }
}
