//! Actual offline preparation CLI with empty environment and owned inert files.
#![cfg(unix)]
use serde_json::{Value, json};
use sha2::{Digest, Sha256, Sha384};
use simplestchat_ops::owned_process::{self, Group};
use std::{
    ffi::OsString,
    fs,
    io::Cursor,
    os::unix::{fs::DirBuilderExt, process::CommandExt},
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};
use tokio::io::{AsyncRead, AsyncReadExt};
const MAX_OUTPUT: usize = 8192;
const REV: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const USAGE: &str = concat!(
    "Usage: simplestchat-ops release attestation prepare --artifact-dir ABS ",
    "--security-dir ABS --repository OWNER/REPO --revision SHA --run-id N ",
    "--run-attempt N --timeout-seconds N --finalization-seconds M\n",
    "Prepare unsigned server metadata; 0 < M < N. No signing, Git, Docker or network.\n"
);
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture {
    root: PathBuf,
    cwd: PathBuf,
    output: PathBuf,
    security: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "PRIVATE-release-build-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let cwd = root.join("empty-cwd");
        fs::DirBuilder::new().mode(0o700).create(&cwd).unwrap();
        let output = root.join("PRIVATE-evidence-must-not-exist");
        let security = root.join("PRIVATE-security-must-not-exist");
        Self {
            root,
            cwd,
            output,
            security,
        }
    }
    fn arguments(&self) -> Vec<OsString> {
        vec![
            "release".into(),
            "attestation".into(),
            "prepare".into(),
            "--artifact-dir".into(),
            self.output.as_os_str().into(),
            "--security-dir".into(),
            self.security.as_os_str().into(),
            "--repository".into(),
            "owner/repo".into(),
            "--revision".into(),
            REV.into(),
            "--run-id".into(),
            "789".into(),
            "--run-attempt".into(),
            "1".into(),
            "--timeout-seconds".into(),
            "60".into(),
            "--finalization-seconds".into(),
            "10".into(),
        ]
    }
    fn unchanged(&self) {
        assert_eq!(fs::read_dir(&self.cwd).unwrap().count(), 0);
        assert!(!self.output.exists());
        assert!(!self.security.exists());
        let names: Vec<_> = fs::read_dir(&self.root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![OsString::from("empty-cwd")]);
    }
    fn private_values_absent(&self, result: &Output) {
        for bytes in [&result.stdout, &result.stderr] {
            let text = std::str::from_utf8(bytes).unwrap();
            assert!(!text.contains(self.root.to_str().unwrap()));
            assert!(!text.contains("PRIVATE"));
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

async fn read(reader: &mut (impl AsyncRead + Unpin), bytes: &mut Vec<u8>) -> std::io::Result<()> {
    let mut chunk = [0; 4096];
    loop {
        let count = reader.read(&mut chunk).await?;
        if count == 0 {
            return Ok(());
        }
        if bytes.len() + count > MAX_OUTPUT {
            return Err(std::io::Error::other("CLI fixture output bound"));
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
}

async fn cli(fixture: &Fixture, arguments: &[OsString]) -> Output {
    cli_with_stdout(fixture, arguments, Stdio::piped()).await
}

async fn cli_with_stdout(fixture: &Fixture, arguments: &[OsString], stdout: Stdio) -> Output {
    let end = Instant::now() + Duration::from_secs(10);
    let body_end = end - Duration::from_secs(5);
    let child = Command::new(env!("CARGO_BIN_EXE_simplestchat-ops"))
        .args(arguments)
        .env_clear()
        .current_dir(&fixture.cwd)
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let mut group = Group::new(child);
    let pid = group.pid();
    let stdout = group
        .child()
        .stdout
        .take()
        .map(tokio::process::ChildStdout::from_std)
        .transpose();
    let stdout_converted = stdout.is_ok();
    let mut stdout = stdout.ok().flatten();
    let stderr = group
        .child()
        .stderr
        .take()
        .map(tokio::process::ChildStderr::from_std)
        .transpose();
    let converted = stdout_converted && stderr.as_ref().is_ok_and(Option::is_some);
    let mut stderr = stderr.ok().flatten();
    let mut out = Vec::new();
    let mut err = Vec::new();
    // A refused consuming conversion is negative. Keep every other known
    // reader and group owner until all independent finalizers have run.
    let body = if converted {
        Some(
            tokio::time::timeout_at(body_end.into(), async {
                tokio::join!(
                    async {
                        if let Some(stdout) = &mut stdout {
                            read(stdout, &mut out).await
                        } else {
                            Ok(())
                        }
                    },
                    async {
                        if let Some(stderr) = &mut stderr {
                            read(stderr, &mut err).await
                        } else {
                            Err(std::io::Error::other("missing stderr reader"))
                        }
                    },
                    owned_process::exited(pid)
                )
            })
            .await,
        )
    } else {
        None
    };
    let retired = owned_process::finish_before(pid, end).await;
    let terminal = tokio::time::timeout_at(end.into(), owned_process::exited(pid)).await;
    // Consume and join the reserved child after group retirement. A timed-out
    // consuming reap would detach its blocking waiter instead of settling it.
    let status = group.reap().await;
    let stdout_closed = stdout.is_none_or(|stdout| {
        stdout
            .into_owned_fd()
            .and_then(|descriptor| nix::unistd::close(descriptor).map_err(std::io::Error::from))
            .is_ok()
    });
    let stderr_closed = stderr.is_some_and(|stderr| {
        stderr
            .into_owned_fd()
            .and_then(|descriptor| nix::unistd::close(descriptor).map_err(std::io::Error::from))
            .is_ok()
    });
    assert!(converted, "both consuming reader conversions must succeed");
    assert!(
        matches!(body, Some(Ok((Ok(()), Ok(()), Ok(()))))),
        "bounded CLI body failed"
    );
    assert!(retired, "the reserved child group must retire before reap");
    assert!(matches!(terminal, Ok(Ok(()))));
    assert!(
        stdout_closed && stderr_closed,
        "both original readers must close"
    );
    assert!(
        Instant::now() < end,
        "the original CLI fixture end must remain authoritative"
    );
    Output {
        status: status.unwrap(),
        stdout: out,
        stderr: err,
    }
}

fn usage(fixture: &Fixture, result: &Output) {
    assert_eq!(result.status.code(), Some(2));
    assert!(result.stdout.is_empty());
    assert_eq!(result.stderr, USAGE.as_bytes());
    fixture.private_values_absent(result);
    fixture.unchanged();
}
#[tokio::test(flavor = "current_thread")]
async fn help_is_informational_and_failed_stdout_cannot_pass() {
    let fixture = Fixture::new();
    for flag in ["--help", "-h"] {
        let result = cli(
            &fixture,
            &[
                "release".into(),
                "attestation".into(),
                "prepare".into(),
                flag.into(),
            ],
        )
        .await;
        assert_eq!(result.status.code(), Some(0));
        assert_eq!(result.stdout, USAGE.as_bytes());
        assert!(result.stderr.is_empty());
        fixture.unchanged();
    }
    let output = fs::File::open("/dev/null").unwrap();
    let result = cli_with_stdout(
        &fixture,
        &[
            "release".into(),
            "attestation".into(),
            "prepare".into(),
            "--help".into(),
        ],
        Stdio::from(output),
    )
    .await;
    assert_eq!(result.status.code(), Some(1));
    fixture.unchanged();
}
#[tokio::test(flavor = "current_thread")]
async fn canonical_paths_exact_selector_domains_and_required_budgets_precede_effects() {
    let fixture = Fixture::new();
    for (flag, value) in [
        ("--run-id", "0"),
        ("--run-id", "9007199254740992"),
        ("--run-attempt", "0"),
        ("--run-attempt", "2147483648"),
        ("--run-id", "+1"),
        ("--run-id", "１２"),
        ("--timeout-seconds", "0"),
        ("--timeout-seconds", "18446744073709551615"),
        ("--finalization-seconds", "0"),
        ("--finalization-seconds", "60"),
        ("--finalization-seconds", "61"),
        ("--artifact-dir", "relative/PRIVATE-path"),
        ("--security-dir", "/tmp//PRIVATE-alias"),
        ("--repository", "PRIVATE/invalid/extra"),
    ] {
        let mut args = fixture.arguments();
        let at = args.iter().position(|v| v == flag).unwrap();
        args[at + 1] = value.into();
        usage(&fixture, &cli(&fixture, &args).await);
    }
    for extras in [
        vec!["--run-id", "789"],
        vec!["--help"],
        vec!["--artifact", "PRIVATE"],
        vec!["--unknown=PRIVATE"],
    ] {
        let mut args = fixture.arguments();
        args.extend(extras.iter().map(OsString::from));
        usage(&fixture, &cli(&fixture, &args).await);
    }
    let mut missing = fixture.arguments();
    missing.truncate(missing.len() - 2);
    usage(&fixture, &cli(&fixture, &missing).await);
}
#[tokio::test(flavor = "current_thread")]
async fn split_and_equals_forms_reach_the_fixed_operational_failure_without_tools() {
    let fixture = Fixture::new();
    let split = fixture.arguments();
    let mut equals = split[..3].to_vec();
    for pair in split[3..].as_chunks::<2>().0 {
        let mut value = pair[0].clone();
        value.push("=");
        value.push(&pair[1]);
        equals.push(value);
    }
    for args in [split, equals] {
        let result = cli(&fixture, &args).await;
        assert_eq!(result.status.code(), Some(1));
        assert_eq!(
            result.stdout,
            b"{\"passed\":false,\"failureClass\":\"release_attestation_failed\"}\n"
        );
        assert!(result.stderr.is_empty());
        fixture.private_values_absent(&result);
        fixture.unchanged();
    }
}
fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn data(fixture: &Fixture) -> Vec<Vec<u8>> {
    fs::create_dir(&fixture.output).unwrap();
    fs::create_dir(&fixture.security).unwrap();
    fs::create_dir(fixture.security.join("spdx")).unwrap();
    let config = serde_json::to_vec(&json!({"architecture":"amd64","os":"linux","config":{
        "User":"10001:10001","Cmd":["/app/simplestChat"],
        "Labels":{"org.opencontainers.image.revision":REV}}}))
    .unwrap();
    let image_id = format!("sha256:{}", sha(&config));
    let mut tar = tar::Builder::new(Vec::new());
    for (name, bytes) in [
        ("config.json", config),
        ("layer.tar", b"inert layer".to_vec()),
        (
            "manifest.json",
            serde_json::to_vec(&json!([{"Config":"config.json",
        "RepoTags":[format!("simplestchat-release/production:{REV}")],"Layers":["layer.tar"]}]))
            .unwrap(),
        ),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        header.set_size(bytes.len().try_into().unwrap());
        header.set_cksum();
        tar.append_data(&mut header, name, Cursor::new(bytes))
            .unwrap();
    }
    let image = tar.into_inner().unwrap();
    let archive_sha = sha(&image);
    fs::write(fixture.output.join("image.tar"), image).unwrap();
    let migration: String = Sha384::digest(b"SELECT 1;")
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    fs::write(
        fixture.output.join("release.json"),
        serde_json::to_vec(&json!({"schemaVersion":1,
        "revision":REV,"platform":"linux/amd64","archiveSha256":archive_sha,
        "imageTag":format!("simplestchat-release/production:{REV}"),"migrations":{"1":migration},
        "createdAt":"2026-09-13T00:00:00Z"}))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        fixture.output.join("outcome.json"),
        serde_json::to_vec(&json!({"passed":true,
        "revision":REV,"exportedImageId":image_id}))
        .unwrap(),
    )
    .unwrap();
    fs::write(fixture.output.join("source.json"), b"opaque source").unwrap();
    fs::write(fixture.security.join("spdx/sbom.spdx.json"), b"opaque SBOM").unwrap();
    fs::write(
        fixture.security.join("vex.openvex.json"),
        b"{\"statements\":[]}",
    )
    .unwrap();
    fs::write(
        fixture.security.join("runtime-proof.json"),
        serde_json::to_vec(&json!({
        "schemaVersion":1,"required":false,"passed":true,"imageId":image_id,
        "archiveSha256":archive_sha,"revision":REV,"platform":"linux/amd64"}))
        .unwrap(),
    )
    .unwrap();
    let mut security = json!({"passed":true,"secretDetectorSelfTest":true,"imageId":image_id,
        "archiveSha256":archive_sha,"revision":REV,"platform":"linux/amd64","vexRequired":false,
        "selectedImage":{"archiveConfigSha256":image_id.strip_prefix("sha256:").unwrap()},
        "checks":{"vulnerabilities":{"passed":true},"licenses":{"passed":true},
            "secrets":{"passed":true}}});
    for (field, name) in [
        ("sbomSha256", "spdx/sbom.spdx.json"),
        ("nativeSha256", "native.json"),
        ("elfSha256", "elf.json"),
        ("secretPathMapSha256", "secret-paths.json"),
        ("databaseEvidenceSha256", "database-status.json"),
        ("runtimeProofSha256", "runtime-proof.json"),
        ("vexSha256", "vex.openvex.json"),
        (
            "runtimeLicenseEvidenceSha256",
            "runtime-license-evidence.json",
        ),
    ] {
        let path = fixture.security.join(name);
        if !path.exists() {
            fs::write(&path, []).unwrap();
        }
        security[field] = sha(&fs::read(path).unwrap()).into();
    }
    fs::write(
        fixture.security.join("outcome.json"),
        serde_json::to_vec(&security).unwrap(),
    )
    .unwrap();
    ["image.tar", "release.json", "outcome.json", "source.json"]
        .iter()
        .map(|name| fs::read(fixture.output.join(name)).unwrap())
        .collect()
}
#[tokio::test(flavor = "current_thread")]
async fn actual_offline_cli_preserves_original_four_and_creates_only_five_unsigned_outputs() {
    let fixture = Fixture::new();
    let before = data(&fixture);
    let result = cli(&fixture, &fixture.arguments()).await;
    let after: Vec<_> = ["image.tar", "release.json", "outcome.json", "source.json"]
        .iter()
        .map(|name| fs::read(fixture.output.join(name)).unwrap())
        .collect();
    assert_eq!(result.status.code(), Some(0));
    assert!(result.stdout.is_empty() && result.stderr.is_empty());
    assert_eq!(before, after);
    assert_eq!(fs::read_dir(&fixture.output).unwrap().count(), 9);
    let predicate: Value =
        serde_json::from_slice(&fs::read(fixture.output.join("release-predicate.json")).unwrap())
            .unwrap();
    assert_eq!(predicate["fileDigests"].as_object().unwrap().len(), 8);
    assert!(!fixture.output.join("release-attestation.jsonl").exists());
}
