//! Whole unchanged JavaScript, its seven tests, and native-controller parity.
use crate::{
    ci::{
        bounded_log::Log,
        command::{Command, Program, SeparateLogs},
        process::{Executor, Native},
    },
    owned_process::Signals,
    tooling::{Environment, Failure, Result},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Temporary(PathBuf);
impl Temporary {
    fn new() -> Self {
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "public-smoke-oracle-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
    fn create(&self, name: &str) -> File {
        OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(self.0.join(name))
            .unwrap()
    }
}
impl Drop for Temporary {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn failure() -> Failure {
    Failure::new(1, "Inert public-smoke reference failed.")
}
fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[test]
fn original_complete_source_and_seven_tests_are_exact_80_bytes() {
    assert_eq!(
        sha(include_bytes!("reference/public-smoke.mjs")),
        "1c48cfe99296f65ad8855b4bf5c2fcd945c943259953e0d137edd2276b6f47bf"
    );
    assert_eq!(
        sha(include_bytes!("reference/public-smoke.test.mjs")),
        "16645211b9685d4c94829d70ab6c7ffc32db1f9427d7f1b73c11ebe6805d0187"
    );
}

async fn reference(cases: &[Value], cli: bool) -> Result<Value> {
    let signals = Signals::new()?;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let node = crate::ci::release_checks::files::select(
        root,
        &crate::tooling::inherited_env(),
        std::ffi::OsStr::new("node"),
    )?;
    let node = Arc::new(node);
    let temporary = Temporary::new();
    let mut input = temporary.create("input.json");
    input
        .write_all(&serde_json::to_vec(&json!({"cases":cases,"cli":cli})).unwrap())
        .unwrap();
    nix::unistd::close(input).unwrap();
    let input = File::open(temporary.0.join("input.json")).unwrap();
    let stdout = match Log::start(temporary.create("stdout"), 4 * 1024 * 1024) {
        Ok(stdout) => stdout,
        Err(error) => {
            let _ = nix::unistd::close(input);
            let _ = Arc::try_unwrap(node).unwrap().close();
            return Err(error);
        }
    };
    let stderr = match Log::start(temporary.create("stderr"), 64 * 1024) {
        Ok(stderr) => stderr,
        Err(error) => {
            let _ = stdout.finish().await;
            let _ = nix::unistd::close(input);
            let _ = Arc::try_unwrap(node).unwrap().close();
            return Err(error);
        }
    };
    let environment: Environment = [("PATH", "/usr/bin:/bin"), ("LC_ALL", "C"), ("LANG", "C")]
        .into_iter()
        .map(|(key, value)| (key.into(), value.into()))
        .collect();
    let mut command = Command::capture(
        Program::Node,
        &["--experimental-vm-modules", "--no-warnings"],
    );
    command.arguments.push(
        root.join("xtask/src/ci/public_smoke/oracle.mjs")
            .into_os_string(),
    );
    command.capture = None;
    command.stdin = Some(Arc::new(input));
    command.separate_logs = Some(Arc::new(SeparateLogs { stdout, stderr }));
    command.node = Some(node.clone());
    command.timeout = Duration::from_secs(10);
    let mut native = Native::new(root, &environment, signals);
    let outcome = native.run(command).await;
    let clean = native.cleanup_passed();
    drop(native);
    let closed = Arc::try_unwrap(node).unwrap().close();
    outcome?;
    closed?;
    if !clean {
        return Err(failure());
    }
    let mut file = File::open(temporary.0.join("stdout")).map_err(|_| failure())?;
    let mut bytes = Vec::new();
    let read = (&mut file)
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| failure());
    let closed = nix::unistd::close(file).map_err(|_| failure());
    read?;
    closed?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(failure());
    }
    serde_json::from_slice(&bytes).map_err(|_| failure())
}

fn common_cases() -> Vec<Value> {
    let mut cases = vec![
        json!({}),
        json!({"httpStatus":503}),
        json!({"httpPath":"/ready","body":"PRIVATE_BAD_JSON"}),
        json!({"httpPath":"/ready","body":"{\"status\":\"wrong\"}"}),
        json!({"missingBody":true}),
        json!({"httpType":"text/plain"}),
        json!({"fetchFailure":true}),
        json!({"fetchDeadline":true}),
        json!({"tlsDisabled":true}),
        json!({"body":"{\"status\":\"PRIVATE\",\"status\":\"ok\",\"PRIVATE\":1e300}"}),
        json!({"duplicateJoin":true}),
        json!({"duplicateAck":true}),
        json!({"duplicateDelivery":true}),
        json!({"invalidUtf8":"unknown"}),
        json!({"invalidUtf8":"status"}),
    ];
    for header in [
        "strict-transport-security",
        "x-content-type-options",
        "x-frame-options",
        "referrer-policy",
        "cross-origin-opener-policy",
        "cross-origin-resource-policy",
        "permissions-policy",
        "content-security-policy",
    ] {
        cases.push(json!({"missingHeader":header}));
    }
    for path in [
        "/",
        "/assets/index-A1.js",
        "/assets/index-B2.css",
        "/metrics",
        "/diagnostics",
        "/diagnostics/media",
    ] {
        cases.push(json!({"httpPath":path,"httpStatus":502}));
    }
    for (path, limit) in [
        ("/health", 4096),
        ("/", 128 * 1024),
        ("/assets/index-A1.js", 2 * 1024 * 1024),
        ("/metrics", 4096),
    ] {
        cases.push(json!({"httpPath":path,"bodyBytes":limit,"padBody":true}));
        cases.push(json!({"httpPath":path,"bodyBytes":limit+1}));
    }
    for key in [
        "socketFailure",
        "socketClosed",
        "binaryFrame",
        "malformedFrame",
        "missingType",
        "largeFrame",
        "emptyParticipant",
        "duplicateParticipant",
        "joinMissing",
        "ackMismatch",
        "deliveryMismatch",
        "ackMissing",
        "deliveryMissing",
        "chatIdentityMismatch",
        "leaveMissing",
        "leaveWrong",
        "sendFailure",
        "leaveFailure",
        "closeThrow",
        "closeMissing",
        "closeUnclean",
    ] {
        cases.push(json!({key:true}));
    }
    for rejection in [
        "error",
        "socialError",
        "roomPasswordRequired",
        "roomClosed",
        "lobbyWaiting",
    ] {
        cases.push(json!({"roomRejected":rejection}));
    }
    for raw in [
        "null",
        "true",
        "1e300",
        "[]",
        "{}",
        "{\"type\":null}",
        "{\"type\":\"unrelated\",\"PRIVATE\":1e300}",
        "{\"type\":\"roomClosed\",\"type\":\"unrelated\",\"PRIVATE\":[-0,1e300]}",
        "{\"type\":\"unrelated\",\"type\":\"roomClosed\"}",
    ] {
        cases.push(json!({"rawFrame":raw}));
    }
    for patch in [
        json!({"content":"PRIVATE_OTHER_MESSAGE"}),
        json!({"clientMessageId":"PRIVATE_OTHER_RUN"}),
        json!({"messageId":""}),
    ] {
        cases.push(json!({"ackPatch":patch}));
        cases.push(json!({"deliveryPatch":patch}));
    }
    cases.push(json!({"ackOuterPatch":{"clientMessageId":"PRIVATE_OTHER_RUN"}}));
    for html in [
        "",
        concat!(
            "<script type=\"module\" src=\"https://PRIVATE.invalid/x.js\"></script>",
            "<link rel=\"stylesheet\" href=\"/assets/x.css\">"
        ),
        concat!(
            "<script type=\"module\" src=\"/assets/x.js\"></script>",
            "<script type=\"module\" src=\"/assets/x.js\"></script>",
            "<link rel=\"stylesheet\" href=\"/assets/x.css\">"
        ),
    ] {
        cases.push(json!({"html":html}));
    }
    cases.push(json!({"httpPath":"/assets/index-A1.js","body":"   "}));
    cases.push(json!({"httpPath":"/assets/index-B2.css","httpType":"text/html"}));
    cases
}

#[tokio::test]
async fn actual_seven_tests_full_cli_and_whole_native_shared_domain_match() {
    let cases = common_cases();
    let old = reference(&cases, true).await.unwrap();
    assert_eq!(old["unchangedReferenceTests"], 7);
    let cli = old["cli"].as_array().unwrap();
    assert_eq!(
        cli.iter()
            .map(|row| row["status"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [0, 1, 1, 0, 1, 1, 1]
    );
    for index in [0, 1, 2, 4] {
        assert_eq!(cli[index]["requests"], 0);
        assert_eq!(cli[index]["sockets"], 0);
    }
    assert_eq!(cli[6]["summary"]["failure"]["code"], "total_deadline");
    assert_eq!(cli[6]["summary"]["completed"], false);
    for (index, case) in cases.iter().enumerate() {
        let actual = super::tests::oracle_case(case).await;
        let expected = &old["results"][index];
        let mut expected_summary = expected["summary"].clone();
        if case["fetchDeadline"] == true {
            // The original catches its 8s AbortSignal as operation_failed/1.
            // The native transport observes a typed request watchdog outcome.
            assert_eq!(expected_summary["failure"]["code"], "operation_failed");
            expected_summary["failure"]["code"] = json!("request_deadline");
        }
        assert_eq!(actual["summary"], expected_summary, "case{index}: {case}");
        assert_eq!(actual["trace"], expected["trace"], "case{index}: {case}");
        let code = expected["summary"]["failure"]["code"].as_str();
        if case["fetchDeadline"] == true
            || matches!(code, Some("work_deadline" | "cleanup_deadline"))
        {
            // The original CLI flattened these to 1. The native owner keeps 124.
            assert_eq!(expected["status"], 1);
            assert_eq!(actual["status"], 124, "case{index}: {case}");
        } else {
            assert_eq!(actual["status"], expected["status"], "case{index}: {case}");
        }
    }
}

#[tokio::test]
async fn named_native_identity_limit_and_late_fences_refuse_original_positive_cases() {
    let cases = [
        json!({"identityEncoding":true}),
        json!({"lateResponse":true}),
        json!({"lateClose":true}),
        json!({"closeAtDeadline":true}),
        json!({"extraFrames":128}),
        json!({"rawFrame":r#"{"type":"\ud800"}"#}),
        json!({"unsupportedNode":true}),
    ];
    let old = reference(&cases, false).await.unwrap();
    for (index, case) in cases.iter().take(6).enumerate() {
        assert_eq!(old["results"][index]["status"], 0, "{case}");
        assert_eq!(old["results"][index]["summary"]["passed"], true);
        let actual = super::tests::oracle_case(case).await;
        assert_ne!(actual["status"], 0, "named stricter boundary: {case}");
        assert_eq!(actual["summary"]["passed"], false, "{case}");
    }
    // This is a Node runtime availability gate, not a Rust transport failure.
    assert_eq!(old["results"][6]["status"], 1);
    assert_eq!(
        old["results"][6]["summary"]["failure"]["code"],
        "unsupported_node"
    );
    assert_eq!(old["results"][6]["trace"]["requests"], json!([]));
    assert_eq!(old["results"][6]["trace"]["connects"], json!([]));
}
