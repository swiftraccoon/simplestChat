//! Fake owned transfers keep file sinks open until the engine cleanup boundary.

use super::*;
use crate::bench::{
    clock::RealClock,
    process::{Child, Command, Exit, Host, Stream},
};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("podman-artifacts-{}", uuid::Uuid::new_v4()));
        crate::bench::artifacts::directory(&root).unwrap();
        let executable = root.join("podman");
        fs::write(&executable, b"fixture; never executed").unwrap();
        fs::set_permissions(executable, fs::Permissions::from_mode(0o700)).unwrap();
        Self(root)
    }

    fn engine(&self, host: Arc<FakeHost>) -> Engine {
        Engine::new(
            host,
            Arc::new(RealClock::default()),
            self.0.clone(),
            BTreeMap::from([("PATH".into(), self.0.clone().into_os_string())]),
        )
        .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[derive(Debug)]
struct Call {
    arguments: Vec<String>,
    maximum: u64,
    timeout: Duration,
    cleanup: bool,
}

struct FakeHost {
    calls: Mutex<Vec<Call>>,
    archive: Vec<u8>,
    exit: Exit,
    fail_close: bool,
}

impl FakeHost {
    fn new(archive: Vec<u8>) -> Self {
        Self {
            calls: Mutex::default(),
            archive,
            exit: Exit {
                code: Some(0),
                signal: None,
            },
            fail_close: false,
        }
    }
}

struct Transfer {
    output: Option<File>,
    errors: Option<File>,
    archive: Vec<u8>,
    exit: Exit,
    fail_close: bool,
}

#[async_trait::async_trait]
impl Host for FakeHost {
    fn spawn(&self, command: Command) -> Result<Box<dyn Child>> {
        self.calls.lock().unwrap().push(Call {
            arguments: command.arguments,
            maximum: command.stdout_limit,
            timeout: command.timeout,
            cleanup: command.cleanup,
        });
        assert_eq!(command.stderr_limit, 2 * MIB);
        assert!(command.input.is_empty());
        let (Sink::File(output), Sink::File(errors)) = (command.stdout, command.stderr) else {
            anyhow::bail!("Artifact transfer must stream to owned files");
        };
        Ok(Box::new(Transfer {
            output: Some(output),
            errors: Some(errors),
            archive: self.archive.clone(),
            exit: self.exit.clone(),
            fail_close: self.fail_close,
        }))
    }

    async fn cancelled(&self) -> u8 {
        std::future::pending().await
    }
}

#[async_trait::async_trait]
impl Child for Transfer {
    fn pid(&self) -> u32 {
        1
    }
    fn take_stdout(&mut self) -> Option<Box<dyn Stream>> {
        None
    }
    async fn poll(&mut self) -> Result<Option<Exit>> {
        Ok(Some(self.exit.clone()))
    }
    async fn stop(&mut self, grace: Duration) -> Result<Exit> {
        assert_eq!(grace, Duration::ZERO);
        let mut output = self.output.take().unwrap();
        let mut errors = self.errors.take().unwrap();
        output.write_all(&self.archive)?;
        errors.write_all(b"private transfer diagnostic")?;
        nix::unistd::close(output)?;
        nix::unistd::close(errors)?;
        ensure!(!self.fail_close, "Owned transfer close failed");
        Ok(self.exit.clone())
    }
    fn output(&self) -> &[u8] {
        &[]
    }
    fn errors(&self) -> &[u8] {
        &[]
    }
}

fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, bytes) in entries {
        let mut header = tar::Header::new_ustar();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o7777);
        header.set_cksum();
        builder.append_data(&mut header, name, *bytes).unwrap();
    }
    builder.into_inner().unwrap()
}

#[tokio::test]
async fn generator_collection_awaits_closed_sinks_and_keeps_literal_immutable_cp_arguments() {
    let fixture = Fixture::new();
    let host = Arc::new(FakeHost::new(archive(&[
        ("load_test_summary.json", b"{}"),
        ("load_test_results.json", b"[]"),
    ])));
    let id = "a".repeat(64);
    let result = collect_generator(&fixture.engine(host.clone()), &id, &fixture.0)
        .await
        .unwrap();
    assert_eq!(result["complete"], true);
    assert_eq!(
        fs::read(fixture.0.join("load_test_summary.json")).unwrap(),
        b"{}"
    );
    let calls = host.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].arguments, ["cp", &format!("{id}:/results/."), "-"]);
    assert_eq!(calls[0].maximum, 144 * MIB);
    assert_eq!(calls[0].timeout, Duration::from_secs(60));
    assert!(calls[0].cleanup);
    assert!(!fixture.0.join("generator-results.tar").exists());
    assert!(fixture.0.join("generator-results.stderr").exists());
}

#[tokio::test]
async fn capture_uses_its_own_limit_manifest_and_fixed_basename() {
    let fixture = Fixture::new();
    let host = Arc::new(FakeHost::new(archive(&[(
        "handshakes.pcap",
        b"capture fixture",
    )])));
    let id = "b".repeat(64);
    collect_capture(&fixture.engine(host.clone()), &id, &fixture.0)
        .await
        .unwrap();
    let calls = host.calls.lock().unwrap();
    assert_eq!(
        calls[0].arguments,
        ["cp", &format!("{id}:/handshakes.pcap"), "-"]
    );
    assert_eq!(calls[0].maximum, 65 * MIB);
    assert!(calls[0].cleanup);
    assert_eq!(
        fs::read(fixture.0.join("handshakes.pcap")).unwrap(),
        b"capture fixture"
    );
    assert!(fixture.0.join("capture-collection.json").exists());
    assert!(!fixture.0.join("handshakes.tar").exists());
}

#[tokio::test]
async fn failed_or_signaled_or_unclosed_transfers_keep_archives_without_publishing_results() {
    for (code, signal, close) in [
        (Some(9), None, false),
        (None, Some("SIGTERM".into()), false),
        (Some(0), None, true),
    ] {
        let fixture = Fixture::new();
        let mut host = FakeHost::new(archive(&[
            ("load_test_summary.json", b"{}"),
            ("load_test_results.json", b"[]"),
        ]));
        host.exit = Exit { code, signal };
        host.fail_close = close;
        assert!(
            collect_generator(&fixture.engine(Arc::new(host)), &"c".repeat(64), &fixture.0)
                .await
                .is_err()
        );
        assert!(fixture.0.join("generator-results.tar").exists());
        assert!(!fixture.0.join("load_test_summary.json").exists());
        assert!(!fixture.0.join("collection.json").exists());
    }
}

#[tokio::test]
async fn unverified_identity_cannot_start_copy_or_create_an_archive() {
    let fixture = Fixture::new();
    let host = Arc::new(FakeHost::new(Vec::new()));
    let engine = fixture.engine(host.clone());
    for id in ["name", "--latest", "", &"a".repeat(63), &"z".repeat(64)] {
        assert!(collect_generator(&engine, id, &fixture.0).await.is_err());
    }
    assert!(host.calls.lock().unwrap().is_empty());
    assert!(!fixture.0.join("generator-results.tar").exists());
}
