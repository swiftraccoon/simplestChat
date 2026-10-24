//! Actual source-built CLI routes with empty environment and empty private cwd.
//! No Git, Docker, image or host workload is available to these child processes.

#![cfg(unix)]

use simplestchat_ops::owned_process::{self, Group};
use std::{
    ffi::OsString,
    fs,
    os::unix::{fs::DirBuilderExt, process::CommandExt},
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};
use tokio::io::{AsyncRead, AsyncReadExt};

const MAX_OUTPUT: usize = 8192;
const BUILD_USAGE: &str = concat!(
    "Usage: simplestchat-ops release build --output PATH [--timeout-seconds N] ",
    "[--image-id sha256:HEX]\n",
    "       simplestchat-ops release build --licenses\n",
    "Build or export an unsigned Linux/amd64 release from this clean checkout.\n",
    "N is 60..7200 (default 3600). The active window starts after source setup.\n",
    "Whole operation: build N+300 seconds; export N+60 seconds, including cleanup.\n",
    "Export requires an exact tested local image and never builds or pulls.\n",
    "Output must be fresh, outside this checkout or inside an ignored directory.\n",
);
const IMAGE: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    root: PathBuf,
    cwd: PathBuf,
    output: PathBuf,
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
        Self { root, cwd, output }
    }
    fn arguments(&self, selectors: &[&str]) -> Vec<OsString> {
        let mut arguments = vec![
            "release".into(),
            "build".into(),
            "--output".into(),
            self.output.as_os_str().into(),
        ];
        arguments.extend(selectors.iter().map(OsString::from));
        arguments
    }
    fn unchanged(&self) {
        assert_eq!(fs::read_dir(&self.cwd).unwrap().count(), 0);
        assert!(!self.output.exists());
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
    let mut stdout = group
        .child()
        .stdout
        .take()
        .map(tokio::process::ChildStdout::from_std)
        .transpose()
        .unwrap();
    let mut stderr =
        tokio::process::ChildStderr::from_std(group.child().stderr.take().unwrap()).unwrap();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let body = tokio::time::timeout_at(body_end.into(), async {
        tokio::join!(
            async {
                if let Some(stdout) = &mut stdout {
                    read(stdout, &mut out).await
                } else {
                    Ok(())
                }
            },
            read(&mut stderr, &mut err),
            owned_process::exited(pid)
        )
    })
    .await;
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
    let stderr_closed = stderr
        .into_owned_fd()
        .and_then(|descriptor| nix::unistd::close(descriptor).map_err(std::io::Error::from))
        .is_ok();
    assert!(
        matches!(body, Ok((Ok(()), Ok(()), Ok(())))),
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
    assert_eq!(result.stderr, BUILD_USAGE.as_bytes());
    fixture.private_values_absent(result);
    fixture.unchanged();
}

#[tokio::test(flavor = "current_thread")]
async fn global_and_build_help_succeed_without_environment_source_or_cwd_contents() {
    let fixture = Fixture::new();
    for selector in ["--help", "-h"] {
        let result = cli(&fixture, &[selector.into()]).await;
        assert_eq!(result.status.code(), Some(0));
        assert!(result.stderr.is_empty());
        let text = std::str::from_utf8(&result.stdout).unwrap();
        assert!(text.starts_with("Usage: simplestchat-ops release build --output PATH "));
        assert!(text.contains("\n       simplestchat-ops release manifest --path PATH\n"));
        fixture.private_values_absent(&result);
        fixture.unchanged();
        let result = cli(
            &fixture,
            &["release".into(), "build".into(), selector.into()],
        )
        .await;
        assert_eq!(result.status.code(), Some(0));
        assert!(result.stderr.is_empty());
        assert_eq!(result.stdout, BUILD_USAGE.as_bytes());
        fixture.unchanged();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn duplicate_missing_unknown_and_private_selectors_are_usage_failures_without_effects() {
    let fixture = Fixture::new();
    for selectors in [
        vec!["--output", "PRIVATE-second-output"],
        vec!["--output=PRIVATE-second-output"],
        vec!["--timeout-seconds", "60", "--timeout-seconds=60"],
        vec!["--image-id", IMAGE, "--image-id", IMAGE],
        vec!["--unknown", "PRIVATE-selector-value"],
        vec!["--unknown=PRIVATE-selector-value"],
        vec!["--out", "PRIVATE-abbreviation"],
        vec!["PRIVATE-trailing-argument"],
        vec!["--timeout-seconds"],
        vec!["--image-id"],
        vec!["--help"],
    ] {
        let result = cli(&fixture, &fixture.arguments(&selectors)).await;
        usage(&fixture, &result);
    }
    for arguments in [
        vec!["release", "build"],
        vec!["release", "build", "--output"],
        vec!["release", "build", "--output="],
        vec!["release", "build", "--help", "PRIVATE-extra"],
    ] {
        let arguments: Vec<_> = arguments.iter().map(OsString::from).collect();
        usage(&fixture, &cli(&fixture, &arguments).await);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn timeout_domain_is_ascii_decimal_and_refuses_bounds_coercions_and_overflow() {
    let fixture = Fixture::new();
    for value in [
        "",
        "0",
        "59",
        "7201",
        "+60",
        "-60",
        "60.0",
        "NaN",
        "true",
        " 60",
        "60 ",
        "٦٠",
        "18446744073709551616",
    ] {
        let result = cli(&fixture, &fixture.arguments(&["--timeout-seconds", value])).await;
        usage(&fixture, &result);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn image_selector_requires_exact_lowercase_sha256_and_never_echoes_bad_values() {
    let fixture = Fixture::new();
    for value in [
        "PRIVATE-mutable:latest".into(),
        "".into(),
        "sha256:abc".into(),
        format!("sha256:{}", "b".repeat(63)),
        format!("sha256:{}", "b".repeat(65)),
        format!("sha256:{}", "B".repeat(64)),
        format!("SHA256:{}", "b".repeat(64)),
    ] {
        let result = cli(&fixture, &fixture.arguments(&["--image-id", &value])).await;
        usage(&fixture, &result);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn valid_split_and_equals_forms_enter_operation_and_fail_privately_without_any_tools() {
    let fixture = Fixture::new();
    let mut cases = vec![
        fixture.arguments(&[]),
        fixture.arguments(&["--image-id", IMAGE]),
    ];
    for seconds in ["60", "139", "140", "300", "7200", "060"] {
        cases.push(fixture.arguments(&["--timeout-seconds", seconds]));
        cases.push(vec![
            "release".into(),
            "build".into(),
            format!("--output={}", fixture.output.display()).into(),
            format!("--timeout-seconds={seconds}").into(),
            format!("--image-id={IMAGE}").into(),
        ]);
    }
    for arguments in cases {
        let result = cli(&fixture, &arguments).await;
        assert_eq!(result.status.code(), Some(1));
        assert!(result.stdout.is_empty());
        assert_eq!(
            result.stderr,
            b"Release build failed: release_build_input_refused\n"
        );
        fixture.private_values_absent(&result);
        fixture.unchanged();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn builder_notices_are_exact_and_need_no_environment_or_checkout() {
    use sha2::{Digest, Sha256};

    let fixture = Fixture::new();
    let result = cli(
        &fixture,
        &["release".into(), "build".into(), "--licenses".into()],
    )
    .await;
    assert_eq!(result.status.code(), Some(0));
    assert!(result.stderr.is_empty());
    let text = std::str::from_utf8(&result.stdout).unwrap();
    assert!(text.starts_with("Release-builder dependency notices (limited scope)\n"));
    assert!(text.contains("the full dependency audit is separate"));
    let mut sections = text.split("\n--- ");
    sections.next().unwrap();
    for (label, bytes, expected) in [
        (
            "Tokio 1.53.3 (MIT)",
            1070,
            "253cd04c6714889df2d32f3f64d669179a1c95c76ac43c40882c52eb06bc3552",
        ),
        (
            "Mio 1.2.4 (MIT)",
            1082,
            "07919255c7e04793d8ea760d6c2ce32d19f9ff02bdbdde3ce90b1e1880929a9b",
        ),
        (
            "signal-hook-registry 1.4.8 (MIT option)",
            1068,
            "503558bfefe66ca15e4e3f7955b3cb0ec87fd52f29bf24b336af7bd00e946d5c",
        ),
    ] {
        let section = sections.next().unwrap();
        let (actual_label, notice) = section.split_once(" ---\n").unwrap();
        assert_eq!(actual_label, label);
        assert_eq!(notice.len(), bytes);
        let digest: String = Sha256::digest(notice.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(digest, expected);
    }
    assert!(sections.next().is_none());
    fixture.private_values_absent(&result);
    fixture.unchanged();
    for selectors in [
        vec!["--licenses", "--licenses"],
        vec!["--licenses", "--help"],
        vec!["--licenses", "--output", "PRIVATE-path"],
    ] {
        let mut arguments = vec![OsString::from("release"), OsString::from("build")];
        arguments.extend(selectors.iter().map(OsString::from));
        usage(&fixture, &cli(&fixture, &arguments).await);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn builder_notices_stdout_failure_has_a_fixed_status_without_side_effects() {
    let fixture = Fixture::new();
    // The original read-only handle gives the child an unwritable stdout on
    // both Linux and macOS, without a shell, unbounded pipe or raw FD adoption.
    let output = fs::File::open("/dev/null").unwrap();
    let result = cli_with_stdout(
        &fixture,
        &["release".into(), "build".into(), "--licenses".into()],
        Stdio::from(output),
    )
    .await;
    assert_eq!(result.status.code(), Some(1));
    assert!(result.stdout.is_empty());
    assert_eq!(
        result.stderr,
        b"Release build notices could not be written.\n"
    );
    fixture.private_values_absent(&result);
    fixture.unchanged();
}
