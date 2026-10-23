//! Execute whole retained tests and actual untransformed Bash/JQ/AWK blocks.
//! Neither original launcher is run: fixed host paths/namespace work stay held.
use super::policy::{self, SummarySelection};
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
        let name = format!(
            "container-benchmark-oracle-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        );
        let path = std::env::temp_dir().canonicalize().unwrap().join(name);
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
    fn file(&self, name: &str) -> File {
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
    Failure::new(1, "Inert container benchmark reference failed.")
}
fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[test]
fn complete_original_scripts_and_original_tests_are_exact_81_bytes() {
    for (bytes, length, expected) in [
        (
            include_bytes!("reference/build/benchmark-container.sh").as_slice(),
            16168,
            "a329d731b73630b5823e4a2d626178858b1adc305e1be2972b3d8b8b90edf5de",
        ),
        (
            include_bytes!("reference/build/cleanup-container-benchmark.sh").as_slice(),
            5725,
            "f6a5fffd69fd1cb8edbfda3ba669c41a973f1d78de462b6cd27cbfc2c760f83e",
        ),
        (
            include_bytes!("reference/build/tests/container-benchmark.test.mjs").as_slice(),
            16799,
            "590341ce46218f81f1a2d1c86ca32231cfd3f4f82b5569f861981f99be530a3b",
        ),
    ] {
        assert_eq!(bytes.len(), length);
        assert_eq!(sha(bytes), expected);
    }
}
async fn reference(cases: &[Value]) -> Result<Value> {
    let signals = Signals::new()?;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let captured = crate::tooling::inherited_env();
    let node = Arc::new(crate::ci::release_checks::files::select(
        root,
        &captured,
        std::ffi::OsStr::new("node"),
    )?);
    let temporary = Temporary::new();
    let mut input = temporary.file("input.json");
    let written = input.write_all(&serde_json::to_vec(&json!({"cases":cases})).unwrap());
    let closed = nix::unistd::close(input);
    if written.is_err() || closed.is_err() {
        let _ = Arc::try_unwrap(node).unwrap().close();
        return Err(failure());
    }
    let input = match File::open(temporary.0.join("input.json")) {
        Ok(file) => file,
        Err(_) => {
            let _ = Arc::try_unwrap(node).unwrap().close();
            return Err(failure());
        }
    };
    let stdout = match Log::start(temporary.file("stdout"), 256 * 1024) {
        Ok(log) => log,
        Err(error) => {
            let _ = nix::unistd::close(input);
            let _ = Arc::try_unwrap(node).unwrap().close();
            return Err(error);
        }
    };
    let stderr = match Log::start(temporary.file("stderr"), 64 * 1024) {
        Ok(log) => log,
        Err(error) => {
            let _ = stdout.finish().await;
            let _ = nix::unistd::close(input);
            let _ = Arc::try_unwrap(node).unwrap().close();
            return Err(error);
        }
    };
    let mut environment: Environment = [("LC_ALL", "C"), ("LANG", "C")]
        .into_iter()
        .map(|(key, value)| (key.into(), value.into()))
        .collect();
    if let Some(value) = captured.get(std::ffi::OsStr::new("PATH")) {
        environment.insert("PATH".into(), value.clone());
    }
    let mut command = Command::capture(
        Program::Node,
        &["--experimental-vm-modules", "--no-warnings"],
    );
    command.arguments.push(
        root.join("xtask/src/ci/container_benchmark/oracle.mjs")
            .into_os_string(),
    );
    command.capture = None;
    command.stdin = Some(Arc::new(input));
    command.separate_logs = Some(Arc::new(SeparateLogs { stdout, stderr }));
    command.node = Some(node.clone());
    command.timeout = Duration::from_secs(30);
    let mut native = Native::new(root, &environment, signals);
    let outcome = native.run(command).await;
    let cleanup = native.cleanup_passed();
    drop(native);
    let closed = Arc::try_unwrap(node).unwrap().close();
    outcome?;
    closed?;
    if !cleanup {
        return Err(failure());
    }
    let mut file = File::open(temporary.0.join("stdout")).map_err(|_| failure())?;
    let mut bytes = Vec::new();
    let read = (&mut file)
        .take(256 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| failure());
    let closed = nix::unistd::close(file).map_err(|_| failure());
    read?;
    closed?;
    if bytes.len() > 256 * 1024 {
        return Err(failure());
    }
    serde_json::from_slice(&bytes).map_err(|_| failure())
}
fn selected(clients: u32, rooms: u32) -> SummarySelection {
    SummarySelection {
        clients,
        rooms,
        duration: 60,
        ramp_up: 5,
        warmup: 10,
        run_id: "a".repeat(32),
        server_image: format!("sha256:{}", "b".repeat(64)),
        generator_image: format!("sha256:{}", "c".repeat(64)),
    }
}
fn summary(selection: &SummarySelection) -> Value {
    let consumers: u32 = (0..selection.rooms)
        .map(|room| {
            let members = selection.clients / selection.rooms
                + u32::from(room < selection.clients % selection.rooms);
            members * members.saturating_sub(1).min(4) * 2
        })
        .sum();
    json!({"schemaVersion":2,"totalErrors":0,"failedConnections":0,"failedConsumers":0,
        "validatedConsumers":consumers,"skippedShortLivedConsumers":0,
        "run":{"completed":true,"passed":true,"configuration":{"numClients":selection.clients,
            "numRooms":selection.rooms,"durationSecs":60,"rampUpSecs":5,"warmupSecs":10,
            "runLabel":selection.run_id},"provenance":{"serverRevision":selection.server_image,
            "generatorRevision":selection.generator_image}},
        "attemptCoverage":{"version":1,"scope":"stable-publishers","available":true,
            "attempts":selection.clients,"passedAttempts":selection.clients,"failedAttempts":0,
            "missingCoverageAttempts":0,"skippedShortTailAttempts":0,
            "requestedChurners":0,"validatedChurners":0}})
}
fn summary_case(input: String, selection: &SummarySelection) -> Value {
    json!({"kind":"summary","input":input,"selection":{"clients":selection.clients,
        "rooms":selection.rooms,"duration":selection.duration,"rampUp":selection.ramp_up,
        "warmup":selection.warmup,"run":selection.run_id,"server":selection.server_image,
        "generator":selection.generator_image}})
}
fn baseline_metrics() -> String {
    "simplestchat_rooms_active 0\nsimplestchat_participants_active 0\n\
     simplestchat_connections_active 0\nsimplestchat_participants_snapshot_complete 1\n"
        .into()
}
fn cases() -> Vec<Value> {
    let mut cases = Vec::new();
    for (clients, rooms) in [(4, 1), (10, 1), (10, 4), (30, 4), (2, 2)] {
        let selection = selected(clients, rooms);
        cases.push(summary_case(
            serde_json::to_string(&summary(&selection)).unwrap(),
            &selection,
        ));
    }
    let selection = selected(10, 1);
    let original = summary(&selection);
    for pointer in [
        "/schemaVersion",
        "/totalErrors",
        "/failedConnections",
        "/failedConsumers",
        "/validatedConsumers",
        "/skippedShortLivedConsumers",
        "/run/completed",
        "/run/passed",
        "/run/configuration/numClients",
        "/run/configuration/numRooms",
        "/run/configuration/durationSecs",
        "/run/configuration/rampUpSecs",
        "/run/configuration/warmupSecs",
        "/run/configuration/runLabel",
        "/run/provenance/serverRevision",
        "/run/provenance/generatorRevision",
        "/attemptCoverage/version",
        "/attemptCoverage/scope",
        "/attemptCoverage/available",
        "/attemptCoverage/attempts",
        "/attemptCoverage/passedAttempts",
        "/attemptCoverage/failedAttempts",
        "/attemptCoverage/missingCoverageAttempts",
        "/attemptCoverage/skippedShortTailAttempts",
        "/attemptCoverage/requestedChurners",
        "/attemptCoverage/validatedChurners",
    ] {
        let mut missing = original.clone();
        let (parent, key) = pointer.rsplit_once('/').unwrap();
        missing
            .pointer_mut(parent)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove(key);
        cases.push(summary_case(
            serde_json::to_string(&missing).unwrap(),
            &selection,
        ));
        for replacement in [json!(true), json!("PRIVATE"), json!(null)] {
            let mut candidate = original.clone();
            *candidate.pointer_mut(pointer).unwrap() = replacement;
            cases.push(summary_case(
                serde_json::to_string(&candidate).unwrap(),
                &selection,
            ));
        }
    }
    let raw = serde_json::to_string(&original).unwrap();
    for (field, literal) in [
        ("schemaVersion", "2.0"),
        ("schemaVersion", "0.2e1"),
        ("schemaVersion", "2.00000000000000000001"),
        ("schemaVersion", "1.99999999999999999999"),
        ("schemaVersion", "2e999"),
        ("totalErrors", "-0"),
        ("totalErrors", "0.0"),
        ("totalErrors", "0e999999999999999999999"),
        ("totalErrors", "1e-999"),
        ("totalErrors", "false"),
    ] {
        let expected = if field == "schemaVersion" { 2 } else { 0 };
        let candidate = raw.replace(
            &format!("\"{field}\":{expected}"),
            &format!("\"{field}\":{literal}"),
        );
        cases.push(summary_case(candidate, &selection));
    }
    for candidate in [
        raw.replacen('{', "{\"PRIVATE\":1e999,", 1),
        raw.replacen('{', "{\"schemaVersion\":0,", 1),
        format!("{},\"schemaVersion\":0}}", raw.trim_end_matches('}')),
        raw.replace("\"numClients\":10", "\"numClients\":0,\"numClients\":10"),
        format!("\u{feff}{raw}"),
    ] {
        cases.push(summary_case(candidate, &selection));
    }
    let metrics = baseline_metrics();
    cases.push(json!({"kind":"counts","input":metrics}));
    for line in metrics.lines() {
        cases.push(json!({"kind":"counts","input":metrics.replace(&format!("{line}\n"),"")}));
        cases.push(json!({"kind":"counts","input":format!("{metrics}{line}\n")}));
    }
    for value in ["1", "-1", "0.0", "0e0", "0\r", "000", "0 extra", ""] {
        let input = metrics.replace("rooms_active 0", &format!("rooms_active {value}"));
        cases.push(json!({"kind":"counts","input":input}));
    }
    for value in [
        "01",
        "1.0",
        "1e0",
        "+1",
        ".1e1",
        "1.00000000000000000001",
        "1\r",
        "\r1",
        "\u{000b}1",
        "\u{000c}1",
        "1\u{000b}",
        "1\u{000c}",
        "1\u{0085}",
        "\u{00a0}1",
        "true",
        "1e999",
        "1 extra",
        "",
    ] {
        let input = metrics.replace("snapshot_complete 1", &format!("snapshot_complete {value}"));
        cases.push(json!({"kind":"counts","input":input}));
    }
    let owned = json!({"id":"ID","image":"IMAGE","run":"RUN","role":"server"});
    cases.push(json!({"kind":"ownership","input":owned.to_string()}));
    for field in ["id", "image", "run", "role"] {
        for replacement in [json!(null), json!("foreign"), json!(false), json!(1)] {
            let mut value = owned.clone();
            value[field] = replacement;
            cases.push(json!({"kind":"ownership","input":value.to_string()}));
        }
    }
    let terminal =
        json!({"Running":false,"ExitCode":0,"OOMKilled":false,"Error":"","Restarting":false});
    cases.push(json!({"kind":"exit","input":terminal.to_string()}));
    for field in ["Running", "ExitCode", "OOMKilled", "Error", "Restarting"] {
        let mut value = terminal.clone();
        value.as_object_mut().unwrap().remove(field);
        cases.push(json!({"kind":"exit","input":value.to_string()}));
    }
    for (field, value) in [
        ("Running", json!(true)),
        ("ExitCode", json!(137)),
        ("ExitCode", json!(false)),
        ("ExitCode", json!(0.0)),
        ("OOMKilled", json!(true)),
        ("Error", json!("PRIVATE")),
        ("Restarting", json!(true)),
    ] {
        let mut state = terminal.clone();
        state[field] = value;
        cases.push(json!({"kind":"exit","input":state.to_string()}));
    }
    cases
}
fn native_policy(case: &Value) -> bool {
    let bytes = case["input"].as_str().unwrap().as_bytes();
    match case["kind"].as_str().unwrap() {
        "summary" => {
            let value = &case["selection"];
            let selection = SummarySelection {
                clients: value["clients"].as_u64().unwrap() as u32,
                rooms: value["rooms"].as_u64().unwrap() as u32,
                duration: value["duration"].as_u64().unwrap() as u32,
                ramp_up: value["rampUp"].as_u64().unwrap() as u32,
                warmup: value["warmup"].as_u64().unwrap() as u32,
                run_id: value["run"].as_str().unwrap().into(),
                server_image: value["server"].as_str().unwrap().into(),
                generator_image: value["generator"].as_str().unwrap().into(),
            };
            policy::admit_summary(bytes, &selection).is_ok()
        }
        "counts" => policy::zero_counts(bytes).is_ok(),
        "ownership" => policy::ownership(
            &serde_json::from_slice(bytes).unwrap(),
            "ID",
            "IMAGE",
            "RUN",
            "server",
        ),
        "exit" => policy::exit_passed(&serde_json::from_slice(bytes).unwrap()),
        _ => panic!("closed fixture case"),
    }
}
#[tokio::test]
async fn actual_original_47_tests_and_selected_jq_awk_predicates_match_the_native_policy() {
    let cases = cases();
    let evidence = reference(&cases).await.unwrap();
    assert_eq!(evidence["originalTests"], 47);
    for tool in ["jq", "awk", "bash"] {
        assert!(!evidence["provider"][tool].as_str().unwrap().is_empty());
    }
    assert_eq!(evidence["results"].as_array().unwrap().len(), cases.len());
    for (index, (case, result)) in cases
        .iter()
        .zip(evidence["results"].as_array().unwrap())
        .enumerate()
    {
        assert_eq!(
            result["passed"],
            native_policy(case),
            "case {index}: {}",
            case["kind"]
        );
    }
    assert!(!evidence.to_string().contains("PRIVATE"));
}
#[tokio::test]
async fn actual_bounded_public_guard_and_emergency_blocks_keep_their_failure_evidence() {
    let mut cases = Vec::new();
    let mut expected = Vec::new();
    for (value, passed) in [
        ("1", true),
        ("4", true),
        ("0", false),
        ("5", false),
        ("-1", false),
        ("01", false),
        ("1.5", false),
        ("", false),
        ("1+1", false),
        ("$(printf unsafe)", false),
    ] {
        cases.push(json!({"kind":"bounded","value":value}));
        expected.push(passed);
    }
    for (containers, status, passed) in [
        ("", 0, true),
        ("ID", 0, false),
        (" ", 0, false),
        ("", 1, false),
        ("", 124, false),
        ("ID", 1, false),
    ] {
        cases.push(json!({"kind":"public","containers":containers,"status":status}));
        expected.push(passed);
    }
    cases.extend([
        json!({"kind":"emergency","prior":null,"copyFailure":false}),
        json!({"kind":"emergency","prior":"PRIVATE ORIGINAL\n","copyFailure":false}),
        json!({"kind":"emergency","prior":"PRIVATE ORIGINAL\n","copyFailure":true}),
    ]);
    expected.extend([true, true, false]);
    let evidence = reference(&cases).await.unwrap();
    for (result, expected) in evidence["results"].as_array().unwrap().iter().zip(expected) {
        assert_eq!(result["passed"], expected);
    }
    let rows = evidence["results"].as_array().unwrap();
    assert_eq!(rows[rows.len() - 2]["retained"], true);
    assert_eq!(rows.last().unwrap()["originalUnchanged"], true);
    assert!(!evidence.to_string().contains("PRIVATE"));
}
