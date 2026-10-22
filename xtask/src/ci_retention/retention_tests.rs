//! Actual decisions and fake-CLI transactions; no host GitHub client is reachable.

use super::*;
use chrono::{DateTime, Duration as TimeDelta, SecondsFormat, Utc};
use policy::{Entry, REFERENCE, REPOSITORY};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

static NEXT: AtomicUsize = AtomicUsize::new(0);
const REVISION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const FAKE: &str = r#"
import json, os, pathlib, subprocess, sys, time
root = pathlib.Path(os.environ['RETENTION_FIXTURE'])
calls = root / 'calls.jsonl'
index = len(calls.read_text().splitlines()) if calls.exists() else 0
args = sys.argv[1:]
with calls.open('a') as stream:
    stream.write(json.dumps({'args':args,'cwd':os.getcwd(),'env':{
        key:os.environ.get(key) for key in ('GH_DEBUG','GH_HOST','GH_PAGER','GH_PROMPT_DISABLED')
    }})+'\n')
responses = json.loads((root / 'responses.json').read_text())
assert index < len(responses), ('unexpected request', args)
response = responses[index]
assert args[:5] == ['api','--hostname','github.com','--method',response.get('method','GET')]
assert args[5:9] == ['-H','Accept: application/vnd.github+json','-H','X-GitHub-Api-Version: 2022-11-28']
assert args[9].startswith('repos/swiftraccoon/simplestChat/')
if 'suffix' in response: assert args[9] == 'repos/swiftraccoon/simplestChat/'+response['suffix']
if response.get('wait'):
    child_program = '''import pathlib,signal,sys,time
ready, stopped = map(pathlib.Path,sys.argv[1:])
def stop(*_):
    stopped.touch()
    raise SystemExit(0)
signal.signal(signal.SIGTERM,stop)
signal.signal(signal.SIGINT,stop)
ready.touch()
while True: time.sleep(10)
'''
    child = subprocess.Popen([sys.executable,'-c',child_program,str(root/'ready'),str(root/'stopped')])
    (root/'child.pid').write_text(str(child.pid))
    if response.get('exitLeader'):
        while not (root/'ready').exists(): time.sleep(.001)
        sys.exit(0)
    while True: time.sleep(10)
if 'stdoutBytes' in response: sys.stdout.write('x'*response['stdoutBytes'])
elif 'raw' in response: sys.stdout.write(response['raw'])
else: print(json.dumps(response.get('body')))
if 'stderrBytes' in response: sys.stderr.write('s'*response['stderrBytes'])
if 'stderr' in response: sys.stderr.write(response['stderr'])
sys.exit(response.get('status',0))
"#;

struct Fixture {
    root: PathBuf,
    environment: crate::tooling::Environment,
    python: PathBuf,
}

impl Fixture {
    fn new(responses: Vec<Value>) -> Self {
        let root = std::env::temp_dir().join(format!(
            "simplestchat-retention-{}-{} with spaces",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let bin = root.join("fake commands");
        fs::create_dir(&bin).unwrap();
        let python = Command::new("python3")
            .args(["-c", "import sys; print(sys.executable)"])
            .output()
            .expect("Python is the maintained reference interpreter");
        assert!(python.status.success());
        let python = String::from_utf8(python.stdout).unwrap().trim().to_owned();
        let script = root.join("fake gh.py");
        fs::write(&script, FAKE).unwrap();
        let gh = bin.join("gh");
        fs::write(
            &gh,
            format!(
                "#!/bin/sh\nexec {} {} \"$@\"\n",
                quote(&python),
                quote(script.to_str().unwrap())
            ),
        )
        .unwrap();
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(
            root.join("responses.json"),
            serde_json::to_vec(&responses).unwrap(),
        )
        .unwrap();
        let environment = [
            ("PATH", bin.as_os_str().to_owned()),
            ("RETENTION_FIXTURE", root.as_os_str().to_owned()),
            ("GH_DEBUG", "api-sensitive".into()),
            ("GH_HOST", "wrong.invalid".into()),
            ("GH_PAGER", "wrong".into()),
            ("GH_PROMPT_DISABLED", "0".into()),
            ("PYTHONDONTWRITEBYTECODE", "1".into()),
        ]
        .into_iter()
        .map(|(name, value)| (name.into(), value))
        .collect();
        Self {
            root,
            environment,
            python: python.into(),
        }
    }

    fn client(&self, apply: bool) -> client::Github {
        let (_, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut client = client::Github::new(
            self.root.clone(),
            self.environment.clone(),
            apply,
            process::Signals::injected(receiver),
        );
        client.now = now;
        client
    }

    fn calls(&self) -> Vec<Value> {
        fs::read_to_string(self.root.join("calls.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Only an inert child written by this fixture can be named here.
        if let Ok(pid) = fs::read_to_string(self.root.join("child.pid")) {
            let _ = Command::new("/bin/kill")
                .args(["-KILL", "--", pid.trim()])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}
fn now() -> DateTime<Utc> {
    "2026-10-05T00:00:00Z".parse().unwrap()
}
fn entry(id: u64) -> Entry {
    named(id, "codeql-analyzed-v2-rust-main-linux-x86_64")
}
fn named(id: u64, prefix: &str) -> Entry {
    let created = now() - TimeDelta::days(2) + TimeDelta::minutes(i64::try_from(id).unwrap());
    Entry {
        id,
        key: format!("{prefix}-{id:064x}"),
        reference: REFERENCE.into(),
        version: "f".repeat(64),
        size: 1024,
        created,
        accessed: created,
    }
}
fn metadata(entry: &Entry) -> Value {
    json!({"id":entry.id,"key":entry.key,"ref":entry.reference,"version":entry.version,
        "size_in_bytes":entry.size,"created_at":entry.created.to_rfc3339_opts(SecondsFormat::AutoSi,true),
        "last_accessed_at":entry.accessed.to_rfc3339_opts(SecondsFormat::AutoSi,true)})
}
fn page(entries: &[Entry]) -> Value {
    json!({"body":{"total_count":entries.len(),"actions_caches":entries.iter().map(metadata).collect::<Vec<_>>()}})
}
fn head() -> Value {
    json!({"body":{"object":{"sha":REVISION}},"suffix":"git/ref/heads/main"})
}
fn trusted() -> Value {
    json!({"body":{"id":123,"name":"CI","event":"push","status":"completed","conclusion":"success",
        "head_branch":"main","head_sha":REVISION,"path":".github/workflows/ci.yml",
        "repository":{"full_name":REPOSITORY},"head_repository":{"full_name":REPOSITORY}},"suffix":"actions/runs/123"})
}
fn ids(entries: &[Entry]) -> Vec<u64> {
    entries.iter().map(|entry| entry.id).collect()
}

#[test]
fn latest_unknown_other_refs_and_namespaces_are_protected() {
    let mut entries = (1..=4).map(entry).collect::<Vec<_>>();
    entries.extend((5..=7).map(|id| named(id, "buildx-v3-main-Linux-X64")));
    entries.push(named(8, "unrecognized"));
    let mut pull = entry(9);
    pull.reference = "refs/pull/7/merge".into();
    entries.push(pull);
    assert_eq!(ids(&policy::plan(&entries, now())), [1, 2, 5, 6]);
    let entries = vec![
        named(1, "codeql-analyzed-v1-rust"),
        entry(2),
        entry(3),
        named(4, "codeql-analyzed-v2-rust-main-linux-aarch64"),
        named(5, "codeql-analyzed-v2-rust-untrusted-linux-x86_64"),
        named(6, "codeql-analyzed-v2-c-cpp-main-linux-x86_64"),
    ];
    assert_eq!(ids(&policy::plan(&entries, now())), [1]);
}

#[test]
fn freshness_budget_and_deletion_cap_preserve_last_copies() {
    let mut entries = (1..=4).map(entry).collect::<Vec<_>>();
    entries[0].accessed = now() - TimeDelta::minutes(5);
    entries[1].created = now() + TimeDelta::minutes(5);
    assert_eq!(
        ids(&policy::plan_with_limits(&entries, now(), 1, 100)),
        [3, 4]
    );
    let entries = vec![entry(1), entry(2), named(3, "unknown")];
    assert_eq!(
        ids(&policy::plan_with_limits(&entries, now(), 1024, 100)),
        [1]
    );
    assert_eq!(
        policy::plan_with_limits(&(1..=7).map(entry).collect::<Vec<_>>(), now(), u64::MAX, 1).len(),
        1
    );
    let mut boundary = entry(1);
    boundary.accessed = now() - TimeDelta::hours(2);
    assert!(boundary.recent(now()));
    boundary.accessed -= TimeDelta::microseconds(1);
    assert!(!boundary.recent(now()));
}

#[test]
fn metadata_rejects_malformed_numbers_identity_and_timestamps() {
    assert_eq!(Entry::parse(&metadata(&entry(1))).unwrap(), entry(1));
    for (field, malformed) in [
        ("id", json!(true)),
        ("id", json!(1.0)),
        ("id", json!(0)),
        ("size_in_bytes", json!(-1)),
        ("size_in_bytes", json!(100 * 1024_u64.pow(3) + 1)),
        ("version", json!("F".repeat(64))),
        ("key", json!("")),
        ("key", json!("é".repeat(513))),
        ("ref", json!("é".repeat(513))),
        ("created_at", json!("2026-10-01T00:00:00+00:00")),
        ("created_at", json!("0000-10-01T00:00:00Z")),
        ("created_at", json!("2026-10-01T00:00:60Z")),
        ("last_accessed_at", json!(null)),
    ] {
        let mut value = metadata(&entry(1));
        value[field] = malformed;
        assert!(Entry::parse(&value).is_err(), "{field}: {value}");
    }
    let mut unicode = metadata(&entry(1));
    unicode["key"] = json!("é".repeat(512));
    assert!(Entry::parse(&unicode).is_ok());
}

#[tokio::test]
async fn inventory_rejects_incomplete_duplicate_oversized_or_shifted_lists() {
    let first = metadata(&entry(1));
    for responses in [
        vec![json!({"body":{"total_count":2,"actions_caches":[]}})],
        vec![json!({"body":{"total_count":2,"actions_caches":[first.clone(),first.clone()]}})],
        vec![json!({"body":{"total_count":1001,"actions_caches":[]}})],
        vec![
            json!({"body":{"total_count":2,"actions_caches":[first.clone()]}}),
            json!({"body":{"total_count":1,"actions_caches":[]}}),
        ],
        vec![json!({"body":{"total_count":true,"actions_caches":[]}})],
        vec![json!({"body":{"total_count":0.0,"actions_caches":[]}})],
        vec![json!({"body":{"total_count":101,"actions_caches":
            (1..=101).map(entry).map(|item| metadata(&item)).collect::<Vec<_>>()}})],
    ] {
        let fixture = Fixture::new(responses);
        assert!(fixture.client(false).inventory(None).await.is_err());
    }
    let fixture = Fixture::new(vec![page(&[entry(1)])]);
    assert_eq!(
        fixture.client(false).inventory(None).await.unwrap(),
        [entry(1)]
    );
    let calls = fixture.calls();
    assert!(calls[0]["args"][9].as_str().unwrap().ends_with(
        "actions/caches?ref=refs%2Fheads%2Fmain&per_page=100&page=1&sort=created_at&direction=desc"
    ));
    assert_eq!(
        calls[0]["env"],
        json!({"GH_DEBUG":null,"GH_HOST":"github.com","GH_PAGER":"cat","GH_PROMPT_DISABLED":"1"})
    );
    assert_eq!(calls[0]["cwd"], fixture.root.to_str().unwrap());
}

#[tokio::test]
async fn complete_inventory_reads_exactly_ten_bounded_pages() {
    let entries = (1..=1000).map(entry).collect::<Vec<_>>();
    let responses = entries.chunks(100).map(|entries|
        json!({"body":{"total_count":1000,"actions_caches":entries.iter().map(metadata).collect::<Vec<_>>()}})
    ).collect();
    let fixture = Fixture::new(responses);
    assert_eq!(
        fixture
            .client(false)
            .inventory(Some("key with & characters"))
            .await
            .unwrap(),
        entries
    );
    for (index, call) in fixture.calls().iter().enumerate() {
        let endpoint = call["args"][9].as_str().unwrap();
        assert!(endpoint.contains(&format!("&page={}&", index + 1)));
        assert!(endpoint.ends_with("&key=key+with+%26+characters"));
    }
    assert_eq!(fixture.calls().len(), 10);
}

#[tokio::test]
async fn trusted_run_requires_exact_successful_main_push_and_current_head() {
    for (field, value) in [
        ("event", json!("pull_request")),
        ("conclusion", json!("failure")),
        ("head_branch", json!("other")),
        ("head_sha", json!("b".repeat(40))),
        ("path", json!(".github/workflows/other.yml")),
        ("name", json!("other")),
        ("status", json!("in_progress")),
        ("id", json!(true)),
        ("id", json!(122)),
        ("repository", json!({"full_name":"fork/other"})),
        ("head_repository", json!({"full_name":"fork/other"})),
    ] {
        let mut response = trusted();
        response["body"][field] = value;
        let fixture = Fixture::new(vec![response]);
        assert!(
            fixture
                .client(true)
                .trusted_run(123, REVISION)
                .await
                .is_err()
        );
        assert_eq!(fixture.calls().len(), 1);
    }
    let fixture = Fixture::new(vec![
        trusted(),
        json!({"body":{"object":{"sha":"b".repeat(40)}}}),
    ]);
    assert!(
        fixture
            .client(true)
            .trusted_run(123, REVISION)
            .await
            .unwrap_err()
            .to_string()
            .contains("stale_run")
    );
    let fixture = Fixture::new(vec![trusted(), head()]);
    fixture
        .client(true)
        .trusted_run(123, REVISION)
        .await
        .unwrap();
}

#[tokio::test]
async fn endpoint_and_authorization_guards_make_no_cli_calls() {
    for apply in [false, true] {
        let fixture = Fixture::new(vec![]);
        let mut client = fixture.client(apply);
        for suffix in [
            "actions/runs/123",
            "actions/artifacts/123",
            "git/refs/heads/main",
            "../another/actions/caches/1",
            "actions/caches/1?all=true",
            "actions/caches/-1",
        ] {
            assert!(client.request(suffix, true).await.is_err());
        }
        assert!(client.request("actions/caches/1", true).await.is_err());
        assert!(client.remove(&entry(1), REVISION).await.is_err());
        assert!(fixture.calls().is_empty());
    }
    assert!(!client::allowed_endpoint(
        "actions/caches/1",
        true,
        true,
        100
    ));
    assert!(!client::allowed_endpoint(
        "actions/artifacts/1",
        false,
        true,
        0
    ));
}

#[tokio::test]
async fn deletion_rechecks_head_identity_freshness_and_last_copies() {
    let candidate = entry(1);
    let fixture = Fixture::new(vec![
        trusted(),
        head(),
        head(),
        page(&[candidate.clone(), entry(2), entry(3)]),
        json!({"method":"DELETE","suffix":"actions/caches/1","raw":""}),
    ]);
    let mut client = fixture.client(true);
    client.trusted_run(123, REVISION).await.unwrap();
    assert!(client.remove(&candidate, REVISION).await.unwrap());
    assert_eq!(client.deleted, [1]);
    assert_eq!(fixture.calls().len(), 5);
    for mode in ["recent", "changed", "last", "missing", "newer-main"] {
        let mut current = candidate.clone();
        if mode == "recent" {
            current.accessed = now();
        }
        if mode == "changed" {
            current.version = "b".repeat(64);
        }
        let entries = if mode == "last" {
            vec![current]
        } else if mode == "missing" {
            vec![entry(2), entry(3)]
        } else {
            vec![current, entry(2), entry(3)]
        };
        let fresh_head = if mode == "newer-main" {
            json!({"body":{"object":{"sha":"b".repeat(40)}}})
        } else {
            head()
        };
        let fixture = Fixture::new(vec![trusted(), head(), fresh_head, page(&entries)]);
        let mut client = fixture.client(true);
        client.trusted_run(123, REVISION).await.unwrap();
        let result = client.remove(&candidate, REVISION).await;
        if mode == "changed" || mode == "newer-main" {
            assert!(result.is_err());
        } else {
            assert!(!result.unwrap());
        }
        assert!(client.deleted.is_empty());
        assert!(fixture.calls().iter().all(|call| call["args"][4] == "GET"));
    }
}

#[tokio::test]
async fn default_execution_is_read_only_and_failure_keeps_exact_deleted_ids() {
    let fixture = Fixture::new(vec![page(&[entry(1), entry(2), entry(3)])]);
    let mut client = fixture.client(false);
    let report = execute(
        &mut client,
        &Options {
            apply: false,
            run_id: "0".into(),
            revision: String::new(),
        },
    )
    .await
    .unwrap();
    assert_eq!(report["dryRun"], true);
    assert_eq!(report["delete"][0]["id"], 1);
    assert_eq!(fixture.calls().len(), 1);
    assert!(client.deleted.is_empty());
    let entries = (1..=4).map(entry).collect::<Vec<_>>();
    let fixture = Fixture::new(vec![
        trusted(),
        head(),
        page(&entries),
        head(),
        page(&entries),
        json!({"method":"DELETE","suffix":"actions/caches/1","raw":""}),
        head(),
        page(&entries[1..]),
        json!({"method":"DELETE","suffix":"actions/caches/2","status":1,"stderr":"private-error"}),
    ]);
    let mut client = fixture.client(true);
    let result = execute(
        &mut client,
        &Options {
            apply: true,
            run_id: "123".into(),
            revision: REVISION.into(),
        },
    )
    .await;
    assert_eq!(result.unwrap_err().to_string(), "cache_retention_api");
    assert_eq!(client.deleted, [1]);
    let mut protected = named(1, "unknown");
    protected.size = policy::SOFT_BYTES + 1;
    let fixture = Fixture::new(vec![page(&[protected])]);
    let mut client = fixture.client(false);
    let report = execute(
        &mut client,
        &Options {
            apply: false,
            run_id: "0".into(),
            revision: String::new(),
        },
    )
    .await
    .unwrap();
    assert_eq!(report["protectedBytesExceedLimit"], true);
    assert_eq!(report["delete"], json!([]));
}

#[tokio::test]
async fn fake_client_transactions_and_reports_match_the_python_cli() {
    for apply in [false, true] {
        let entries = [entry(1), entry(2), entry(3)];
        let responses = if apply {
            vec![
                trusted(),
                head(),
                page(&entries),
                head(),
                page(&entries),
                json!({"method":"DELETE","suffix":"actions/caches/1","raw":""}),
            ]
        } else {
            vec![page(&entries)]
        };
        let native = Fixture::new(responses.clone());
        let mut client = native.client(apply);
        let report = execute(
            &mut client,
            &Options {
                apply,
                run_id: "123".into(),
                revision: REVISION.into(),
            },
        )
        .await
        .unwrap();
        let reference = Fixture::new(responses);
        let script = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("build/ci_cache_retention.py");
        let mut command = Command::new(&reference.python);
        command
            .arg(script)
            .current_dir(&reference.root)
            .env_clear()
            .envs(&reference.environment);
        if apply {
            command.args(["--apply", "--run-id", "123", "--revision", REVISION]);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let reports = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(reports[0], report);
        if apply {
            assert_eq!(reports[1], json!({"deletedIds":client.deleted}));
        } else {
            assert_eq!(reports.len(), 1);
        }
        let arguments = |fixture: &Fixture| {
            fixture
                .calls()
                .iter()
                .map(|call| call["args"].clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(arguments(&native), arguments(&reference));
    }
}

#[tokio::test]
async fn response_bounds_invalid_json_deadlines_and_missing_gh_fail_safely() {
    for response in [
        json!({"raw":"not json"}),
        json!({"stdoutBytes":4*1024*1024+1}),
        json!({"stderrBytes":65537}),
        json!({"status":17,"stderr":"credential-must-remain-private"}),
    ] {
        let fixture = Fixture::new(vec![response]);
        let error = fixture
            .client(false)
            .request("git/ref/heads/main", false)
            .await
            .unwrap_err();
        assert!(!format!("{error:#}").contains("credential"));
    }
    let fixture = Fixture::new(vec![]);
    let mut client = fixture.client(false);
    client.deadline = Instant::now() - Duration::from_secs(1);
    assert!(client.inventory(None).await.is_err());
    assert!(fixture.calls().is_empty());
    fs::remove_file(fixture.root.join("fake commands/gh")).unwrap();
    let error = fixture.client(false).inventory(None).await.unwrap_err();
    assert_eq!(error.to_string(), "cache_retention_api");
}

async fn ready(path: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("owned fake process ready");
}

#[tokio::test]
async fn timeout_stops_descendant_after_its_leader_has_already_exited() {
    let fixture = Fixture::new(vec![json!({"wait":true,"exitLeader":true})]);
    let mut client = fixture.client(false);
    client.per_request = Duration::from_secs(2);
    let error = client
        .request("git/ref/heads/main", false)
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "cache_retention_deadline");
    assert!(fixture.root.join("ready").exists());
    let pid = fs::read_to_string(fixture.root.join("child.pid")).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = Command::new("/bin/ps")
                .args(["-p", pid.trim(), "-o", "stat="])
                .output()
                .unwrap();
            if !status.status.success()
                || String::from_utf8_lossy(&status.stdout)
                    .trim()
                    .starts_with('Z')
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("owned descendant terminated even after leader exit");
}

#[tokio::test]
async fn cancellation_stops_only_owned_group_and_cannot_resume_requests() {
    let mut unrelated = Command::new("/bin/sleep").arg("30").spawn().unwrap();
    for code in [130, 143] {
        let fixture = Fixture::new(vec![json!({"wait":true})]);
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut client = client::Github::new(
            fixture.root.clone(),
            fixture.environment.clone(),
            false,
            process::Signals::injected(receiver),
        );
        let child_ready = fixture.root.join("ready");
        let request = client.request("git/ref/heads/main", false);
        let signal = async {
            ready(&child_ready).await;
            sender.send(code).unwrap();
        };
        let (result, ()) = tokio::join!(request, signal);
        assert_eq!(
            result
                .unwrap_err()
                .downcast_ref::<process::Interrupted>()
                .unwrap()
                .0,
            code
        );
        ready(&fixture.root.join("stopped")).await;
        assert!(client.inventory(None).await.is_err());
        assert_eq!(fixture.calls().len(), 1);
        assert!(unrelated.try_wait().unwrap().is_none());
    }
    unrelated.kill().unwrap();
    unrelated.wait().unwrap();
}

#[test]
fn command_line_defaults_help_and_invalid_values_are_explicit() {
    assert!(!options(&[]).unwrap().unwrap().apply);
    assert!(options(&["--help".into()]).is_none());
    for arguments in [
        vec!["--unknown"],
        vec!["--run-id"],
        vec!["--run-id=not-integer"],
        vec!["--", "extra"],
    ] {
        assert!(
            options(&arguments.into_iter().map(str::to_owned).collect::<Vec<_>>())
                .unwrap()
                .is_err()
        );
    }
    let parsed = options(&[
        "--apply".into(),
        "--run-id=123".into(),
        format!("--revision={REVISION}"),
    ])
    .unwrap()
    .unwrap();
    assert!(parsed.apply);
    assert_eq!(parsed.run_id, "123");
    assert_eq!(parsed.revision, REVISION);
}

#[test]
fn pure_family_and_plan_match_the_retained_python_reference() {
    let prefixes = [
        "buildx-v2-main-Linux-X64",
        "buildx-v3-main-Linux-X64",
        "buildx-v3-untrusted-Linux-ARM64",
        "codeql-analyzed-v1-rust",
        "codeql-analyzed-v1-c-cpp",
        "codeql-analyzed-v2-rust-main-linux-x86_64",
        "codeql-analyzed-v2-rust-main-linux-aarch64",
        "codeql-analyzed-v2-rust-untrusted-linux-x86_64",
        "codeql-analyzed-v2-c-cpp-main-linux-x86_64",
        "codeql-rust-cargo-v1-main-linux-x86_64",
        "codeql-cli-v1-main-Linux-X64",
        "codeql-openssl-v1-untrusted-Linux-ARM64",
        "openssl-v3-main-linux-x86_64",
        "native-dtls-v2-main-linux-x86_64",
        "vendored-v2-main-Linux-X64-rust-check",
        "vendored-v2-main-Linux-X64-rust-build",
        "vendored-v2-main-Linux-X64-rust-test",
        "native-prepared-v1-main-amd64",
        "native-prepared-v1-untrusted-arm64",
        "native-compiled-v1-asan-main-arm64",
        "native-compiled-v1-ubsan-main-amd64",
        "native-compiled-v1-replay-untrusted-arm64",
        "unknown",
    ];
    let mut keys = prefixes
        .iter()
        .map(|prefix| named(1, prefix).key)
        .collect::<Vec<_>>();
    for trust in ["main", "untrusted", "other"] {
        keys.push(format!("grype-db-v1-{trust}-{}-2026-10-05", "a".repeat(64)));
        keys.push(format!(
            "grype-db-v2-{trust}-{}-{}",
            "a".repeat(64),
            "b".repeat(64)
        ));
    }
    keys.push("v3-rust-main-X64-stable-Linux-x64-a1b2c3d4-b1c2d3e4".into());
    keys.push("v3-rust-untrusted-ARM64-nightly-macOS-arm64-a1b2c3d4-b1c2d3e4".into());
    let valid = keys.clone();
    for key in valid {
        keys.push(format!("{key}\n"));
        keys.push(format!("{key}-suffix"));
        keys.push(key.to_uppercase());
        keys.push(format!("prefix-{key}"));
    }
    keys.push(format!(
        "grype-db-v1-main-{}-２０２６-１０-０５",
        "a".repeat(64)
    ));
    let families = keys
        .iter()
        .map(|key| match policy::family(key) {
            Some((name, keep)) => json!([name, keep]),
            None => Value::Null,
        })
        .collect::<Vec<_>>();
    let mut scenarios = Vec::new();
    let mut plans = Vec::new();
    let mut random = 19_u64;
    for scenario in 0..200 {
        let mut entries = Vec::new();
        for id in 1..=25 {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            let mut item = named(
                id,
                prefixes[usize::try_from(random % prefixes.len() as u64).unwrap()],
            );
            item.size = (random % 12 + 1) * 1024 * 1024 * 1024;
            if random.is_multiple_of(7) {
                item.accessed = now() - TimeDelta::minutes(30);
            }
            if random.is_multiple_of(11) {
                item.created = now() + TimeDelta::minutes(1);
            }
            if random.is_multiple_of(13) {
                item.reference = "refs/pull/9/merge".into();
            }
            if random.is_multiple_of(5) {
                item.created = now() - TimeDelta::days(3);
            }
            entries.push(item);
        }
        let soft = (scenario % 10) * 1024 * 1024 * 1024;
        let maximum = scenario as usize % 101;
        plans.push(ids(&policy::plan_with_limits(
            &entries,
            now(),
            soft,
            maximum,
        )));
        scenarios.push(json!({"entries":entries.iter().map(metadata).collect::<Vec<_>>(),"soft":soft,"maximum":maximum}));
    }
    let timestamps = [
        "2026-10-01T12:30:45Z",
        "20261001T123045Z",
        "2026-10-01 12:30:45Z",
        "2026-10-01t12:30:45Z",
        "2026-W40-4T12:30:45Z",
        "2026W404T123045Z",
        "2026-W40T12:30Z",
        "2026W40T12Z",
        "2026-10-01T12Z",
        "2026-10-01T1230Z",
        "2026-10-01T12:30:45.123456Z",
        "2026-10-01T12:30:45,123456789Z",
        "2026-10-01\n12:30:45Z",
        "2026-10-01T12:30:45.123456789123Z",
        "2026-10-01T12:30:45+00:00",
        "2026-10-01T12:30:60Z",
        "2026-10-01T25:00:00Z",
        "2026-10-01T1230:45Z",
        "2026-10-01T12:30:45.zZ",
        "0000-10-01T12:00:00Z",
    ];
    let timestamp_inputs = timestamps
        .iter()
        .map(|timestamp| {
            let mut value = metadata(&entry(1));
            value["created_at"] = json!(timestamp);
            value
        })
        .collect::<Vec<_>>();
    let parsed_timestamps = timestamp_inputs
        .iter()
        .map(|value| {
            Entry::parse(value)
                .ok()
                .map(|entry| entry.created.timestamp_micros())
        })
        .collect::<Vec<_>>();
    let edge_inputs = ["2026-10-01T24:00:00Z", "2026-10-01T12.123456789Z"].map(|timestamp| {
        let mut value = metadata(&entry(1));
        value["created_at"] = json!(timestamp);
        value
    });
    let parsed_edges = edge_inputs
        .iter()
        .map(|value| {
            Entry::parse(value)
                .ok()
                .map(|entry| entry.created.timestamp_micros())
        })
        .collect::<Vec<_>>();
    assert_eq!(
        Entry::parse(&edge_inputs[0]).unwrap().created,
        "2026-10-02T00:00:00Z".parse::<DateTime<Utc>>().unwrap()
    );
    assert!(Entry::parse(&edge_inputs[1]).is_err());
    let fixture = Fixture::new(vec![]);
    let data = fixture.root.join("reference-input.json");
    fs::write(
        &data,
        serde_json::to_vec(
            &json!({"keys":keys,"scenarios":scenarios,"timestamps":timestamp_inputs,"edges":edge_inputs}),
        )
        .unwrap(),
    )
    .unwrap();
    let program = r"
import json,sys
from datetime import datetime,UTC,timedelta
sys.path.insert(0,sys.argv[1])
import ci_cache_retention as reference
data=json.load(open(sys.argv[2]))
plans=[]
for scenario in data['scenarios']:
    reference.SOFT_BYTES=scenario['soft']
    reference.MAX_DELETIONS=scenario['maximum']
    plans.append([item.identifier for item in reference.plan(
        [reference.Entry.parse(item) for item in scenario['entries']],datetime(2026,10,5,tzinfo=UTC))])
def timestamp(raw):
    try:
        return (reference.Entry.parse(raw).created-datetime(1970,1,1,tzinfo=UTC))//timedelta(microseconds=1)
    except (ValueError, TypeError, reference.ToolError): return None
print(json.dumps({'families':[reference.family(key) for key in data['keys']],'plans':plans,
    'timestamps':[timestamp(item) for item in data['timestamps']],
    'edges':[timestamp(item) for item in data['edges']],'pythonVersion':list(sys.version_info[:2])}))
";
    let output = Command::new("python3")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .args(["-c", program])
        .arg(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .join("build"),
        )
        .arg(data)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(expected["families"], json!(families));
    assert_eq!(expected["plans"], json!(plans));
    assert_eq!(expected["timestamps"], json!(parsed_timestamps));
    // GitHub's canonical UTC metadata above is decisive on every supported
    // interpreter. Noncanonical ISO extensions differ in Python before 3.14.
    if expected["pythonVersion"][0].as_u64().unwrap() > 3
        || expected["pythonVersion"][1].as_u64().unwrap() >= 14
    {
        assert_eq!(expected["edges"], json!(parsed_edges));
    }
}
