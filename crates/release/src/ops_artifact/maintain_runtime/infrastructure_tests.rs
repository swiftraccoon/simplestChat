//! The infrastructure job's steps against scripted commands: exact command
//! lines, order, bounds and the evidence each failure leaves.

use super::{
    Error, Result, control, infrastructure::*, options::Purpose, vm::watchdog_seconds_for,
};
use serde_json::{Value, json};
use simplestchat_ops::release::infrastructure::Action;
use std::{
    collections::VecDeque,
    ffi::OsString,
    path::Path,
    time::{Duration, Instant},
};

const RUN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const GOOD: &str =
    "vmfixture : ok=9 changed=2 unreachable=0 failed=0 skipped=1 rescued=0 ignored=0\n";

/// One command as the fake saw it.
#[derive(Debug)]
struct Asked {
    role: String,
    tool: String,
    args: Vec<String>,
    cap: Duration,
    accepted: Vec<i32>,
    credential: bool,
}

/// Scripted answers, consumed in order; the clock moves only by pauses and
/// by `tick` per command.
struct Fake {
    answers: VecDeque<Result<(i32, Vec<u8>)>>,
    asked: Vec<Asked>,
    paused: Vec<Duration>,
    elapsed: Duration,
    tick: Duration,
    checks: usize,
    unhealthy: Option<usize>,
}

impl Fake {
    fn new(answers: Vec<Result<(i32, Vec<u8>)>>) -> Self {
        Self {
            answers: answers.into(),
            asked: Vec::new(),
            paused: Vec::new(),
            elapsed: Duration::ZERO,
            tick: Duration::ZERO,
            checks: 0,
            unhealthy: None,
        }
    }
    fn roles(&self) -> Vec<&str> {
        self.asked.iter().map(|asked| asked.role.as_str()).collect()
    }
    fn next(&mut self, asked: Asked) -> Result<(i32, Vec<u8>)> {
        self.elapsed += self.tick;
        self.asked.push(asked);
        self.answers
            .pop_front()
            .expect("a command without a scripted answer")
    }
}

fn text(args: &[OsString]) -> Vec<String> {
    args.iter()
        .map(|arg| arg.to_str().unwrap().to_owned())
        .collect()
}

impl Steps for Fake {
    async fn run(
        &mut self,
        role: &str,
        tool: &str,
        args: Vec<OsString>,
        cap: Duration,
        accepted: &[i32],
    ) -> Result<(i32, Vec<u8>)> {
        let answer = self.next(Asked {
            role: role.into(),
            tool: tool.into(),
            args: text(&args),
            cap,
            accepted: accepted.to_vec(),
            credential: false,
        })?;
        if accepted.contains(&answer.0) {
            Ok(answer)
        } else {
            Err(Error::Child(answer.0))
        }
    }
    async fn credential(
        &mut self,
        role: &str,
        tool: &str,
        args: Vec<OsString>,
        cap: Duration,
    ) -> Result<Vec<u8>> {
        let (status, output) = self.next(Asked {
            role: role.into(),
            tool: tool.into(),
            args: text(&args),
            cap,
            accepted: vec![0],
            credential: true,
        })?;
        if status == 0 {
            Ok(output)
        } else {
            Err(Error::Child(status))
        }
    }
    fn ssh(&mut self, command: &[String]) -> Result<Vec<OsString>> {
        let mut argv: Vec<OsString> = vec!["fixture@127.0.0.1".into()];
        argv.extend(command.iter().map(OsString::from));
        Ok(argv)
    }
    fn check(&mut self) -> Result<()> {
        self.checks += 1;
        if self.unhealthy == Some(self.checks) {
            return Err(Error::Refused);
        }
        Ok(())
    }
    async fn pause(&mut self, pause: Duration) -> Result<()> {
        self.paused.push(pause);
        self.elapsed += pause;
        Ok(())
    }
    fn elapsed(&self) -> Duration {
        self.elapsed
    }
    fn issued(&self) -> usize {
        self.asked.len()
    }
}

fn ok(text: &str) -> Result<(i32, Vec<u8>)> {
    Ok((0, text.as_bytes().to_vec()))
}

fn receipt(action: &str, fields: Value) -> String {
    let mut value = json!({"schemaVersion":1,"runId":RUN,"action":action,"passed":true});
    for (name, field) in fields.as_object().unwrap() {
        value[name] = field.clone();
    }
    value.to_string()
}

fn counts() -> Value {
    json!({"users":1,"rooms":1,"sessions":0,"credentials":0,"activeIncidents":1,
        "resolvedIncidents":1})
}

/// The answers of one complete exercise: two site runs, the delivery, the
/// staging, each public run with its snapshot, the database, two backup
/// runs and the restore.
fn complete() -> Vec<Result<(i32, Vec<u8>)>> {
    let unchanged = GOOD.replace("changed=2", "changed=0");
    vec![
        ok(GOOD),
        ok(&unchanged),
        ok(GOOD),
        ok(GOOD),
        ok(GOOD),
        ok(&receipt(
            "snapshot",
            json!({"configurationFiles":13,"idempotent":false}),
        )),
        ok(&unchanged),
        ok(&receipt(
            "snapshot",
            json!({"configurationFiles":13,"idempotent":true}),
        )),
        ok(&receipt(
            "database",
            json!({"migrations":38,"ready":true,"counts":counts()}),
        )),
        ok(GOOD),
        ok(&unchanged),
        ok(&receipt(
            "backup-restore",
            json!({"archiveValidated":true,"restoreVerified":true,"cleanupPassed":true,
                "counts":counts()}),
        )),
    ]
}

async fn run_exercise(fake: &mut Fake) -> (Result<()>, Value, Notes) {
    let mut report = json!({"phases":{}});
    let mut notes = Notes::default();
    let delivery = vec![OsString::from("-i"), OsString::from("/w/inventory.json")];
    let result = exercise(
        fake,
        &mut report,
        &mut notes,
        Path::new("/checkout"),
        Path::new("/w/inventory.json"),
        delivery,
        RUN,
    )
    .await;
    (result, report, notes)
}

/// The job applies site twice, delivers Ops, stages, runs each public
/// application with its snapshot, the database, backup twice and the
/// restore, each with its own command line, bound and admitted statuses.
#[tokio::test]
async fn a_complete_exercise_runs_every_step_once_in_order() {
    let mut fake = Fake::new(complete());
    let (result, report, notes) = run_exercise(&mut fake).await;
    assert_eq!(result, Ok(()));
    assert_eq!(
        fake.roles(),
        [
            "site-first",
            "site-second",
            "ops-install",
            "stage-first",
            "public-first",
            "snapshot",
            "public-second",
            "snapshot",
            "database",
            "backup-first",
            "backup-second",
            "backup-restore",
        ]
    );
    let site = &fake.asked[0];
    assert_eq!(
        site.args,
        [
            "-i",
            "/w/inventory.json",
            "--limit",
            "vmfixture",
            "--forks",
            "1",
            "/checkout/ops/ansible/site.yml",
            "--tags",
            "host,docker,benchmark",
        ]
    );
    assert_eq!(
        (site.tool.as_str(), site.cap),
        ("ansible-playbook", Duration::from_secs(600))
    );
    assert_eq!(site.accepted, [0, 1, 2, 4, 8, 99, 250, 255]);
    for (index, path) in [
        (3, "/checkout/security/vm/stage.yml"),
        (4, "/checkout/ops/ansible/public.yml"),
        (9, "/checkout/ops/ansible/backup.yml"),
    ] {
        assert_eq!(fake.asked[index].args.last().unwrap(), path);
        assert_eq!(fake.asked[index].args.len(), 7);
    }
    let delivery = &fake.asked[2];
    assert!(delivery.credential && delivery.tool == "ansible-playbook");
    assert_eq!(delivery.args, ["-i", "/w/inventory.json"]);
    let snapshot = &fake.asked[5];
    assert_eq!(
        snapshot.args,
        [
            "fixture@127.0.0.1",
            "/usr/bin/sudo",
            "-n",
            "/usr/local/libexec/simplestchat-public/simplestchat-ops",
            "release",
            "infrastructure-guest",
            "snapshot",
            "--run-id",
            RUN,
        ]
    );
    assert_eq!(
        (snapshot.tool.as_str(), snapshot.cap),
        ("ssh", Duration::from_secs(400))
    );
    assert_eq!(snapshot.accepted, [0, 1]);
    assert!(fake.asked.iter().filter(|asked| asked.credential).count() == 1);
    let phases = report["phases"].as_object().unwrap();
    assert_eq!(phases.keys().map(String::as_str).collect::<Vec<_>>(), {
        let mut keys = vec![
            "siteFirst",
            "siteSecond",
            "opsInstall",
            "stage",
            "publicFirst",
            "snapshotFirst",
            "publicSecond",
            "snapshotSecond",
            "database",
            "backupFirst",
            "backupSecond",
            "restore",
        ];
        keys.sort_unstable();
        keys
    });
    assert_eq!(report["phases"]["snapshotSecond"]["idempotent"], true);
    assert_eq!(report["phases"]["siteSecond"]["changed"], 0);
    assert_eq!(notes.recaps.len(), 8);
    assert!(notes.failures.is_empty());
    // One guest check before every playbook run, delivery and action, and one at the end.
    assert_eq!(fake.checks, 13);
}

/// A failure at any step stops the job there with the step's code; nothing
/// after it runs.
#[tokio::test]
async fn a_failure_at_any_step_stops_the_exercise_with_that_steps_code() {
    let steps = complete().len();
    for failing in 0..steps {
        let mut answers = complete();
        answers.truncate(failing);
        answers.push(Ok((2, GOOD.replace("failed=0", "failed=1").into_bytes())));
        let mut fake = Fake::new(answers);
        let (result, report, notes) = run_exercise(&mut fake).await;
        assert!(result.is_err(), "{failing}");
        assert_eq!(fake.asked.len(), failing + 1, "{failing}");
        let role = fake.asked[failing].role.clone();
        match role.as_str() {
            // The delivery and the guest actions admit no such status: the
            // run fails with it at its own checkpoint.
            "ops-install" | "snapshot" | "database" | "backup-restore" => {
                assert_eq!(result, Err(Error::Child(2)), "{failing}");
                assert_eq!(report["failure"], json!(role), "{failing}");
            }
            _ => {
                assert_eq!(
                    report["failure"],
                    format!("vm_command_failed_{role}"),
                    "{failing}"
                );
                // The failed run's recap is kept before it is refused.
                assert_eq!(notes.recaps[&failing]["failed"], 1);
            }
        }
    }
}

/// The second application must change nothing; an incomplete or
/// ambiguous recap is refused, the ambiguous one without counts.
#[tokio::test]
async fn recaps_are_kept_then_judged_for_completeness_and_idempotence() {
    for (output, second, code, kept) in [
        (GOOD.to_owned(), true, "vm_ansible_not_idempotent", true),
        (
            GOOD.replace("ok=9", "ok=0"),
            false,
            "vm_ansible_incomplete",
            true,
        ),
        (
            GOOD.replace("rescued=0", "rescued=1"),
            false,
            "vm_ansible_incomplete",
            true,
        ),
        (
            format!("{GOOD}{GOOD}"),
            false,
            "vm_ansible_recap_missing_or_ambiguous",
            false,
        ),
        (
            GOOD.replace("vmfixture", "other"),
            false,
            "vm_ansible_recap_missing_or_ambiguous",
            false,
        ),
        // One changed task is already a second application that was not
        // idempotent.
        (
            GOOD.replace("changed=2", "changed=1"),
            true,
            "vm_ansible_not_idempotent",
            true,
        ),
        // A count is ASCII digits only, though the retired parser's `\d`
        // admitted any decimal digit.
        (
            GOOD.replace("ok=9", "ok=\u{663}"),
            false,
            "vm_ansible_recap_missing_or_ambiguous",
            false,
        ),
    ] {
        let mut fake = Fake::new(vec![Ok((0, output.into_bytes()))]);
        let mut report = json!({"phases":{}});
        let mut notes = Notes::default();
        let result = apply(
            &mut fake,
            &mut report,
            &mut notes,
            Vec::new(),
            "site",
            second,
        )
        .await;
        assert_eq!(result, Err(Error::Refused));
        assert_eq!(report["failure"], code);
        assert_eq!(notes.recaps.contains_key(&0), kept, "{code}");
        assert_eq!(report["phases"], json!({}));
    }
    let mut fake = Fake::new(vec![ok(GOOD)]);
    let mut report = json!({"phases":{}});
    let mut notes = Notes::default();
    apply(
        &mut fake,
        &mut report,
        &mut notes,
        Vec::new(),
        "backup",
        false,
    )
    .await
    .unwrap();
    assert_eq!(report["checkpoint"], "backup-first");
    assert_eq!(report["phases"]["backupFirst"]["changed"], 2);
    // One task done is a complete run.
    let mut fake = Fake::new(vec![ok(&GOOD.replace("ok=9", "ok=1"))]);
    let mut report = json!({"phases":{}});
    let mut notes = Notes::default();
    let first = apply(
        &mut fake,
        &mut report,
        &mut notes,
        Vec::new(),
        "site",
        false,
    )
    .await;
    assert_eq!(first, Ok(()));
    assert_eq!(report["phases"]["siteFirst"]["ok"], 1);
    // A second application that changed nothing is admitted.
    let mut fake = Fake::new(vec![ok(&GOOD.replace("changed=2", "changed=0"))]);
    let mut report = json!({"phases":{}});
    let mut notes = Notes::default();
    let second = apply(&mut fake, &mut report, &mut notes, Vec::new(), "site", true).await;
    assert_eq!(second, Ok(()));
    assert_eq!(report["phases"]["siteSecond"]["changed"], 0);
}

/// A guest failure keeps only its four fixed labels; a malformed receipt is
/// refused by its code; a passed one is its aggregate projection.
#[tokio::test]
async fn guest_receipts_are_admitted_with_their_fixed_labels_only() {
    let failure = json!({"schemaVersion":1,"runId":RUN,"action":"database","passed":false,
        "phase":"database","context":"unspecified","code":"unclassified",
        "exceptionClass":"release_error"});
    let mut fake = Fake::new(vec![Ok((1, failure.to_string().into_bytes()))]);
    let mut report = json!({});
    let mut notes = Notes::default();
    let result = act(&mut fake, &mut report, &mut notes, RUN, Action::Database).await;
    assert_eq!(result, Err(Error::Refused));
    assert_eq!(report["failure"], "vm_guest_assertion_failed");
    assert_eq!(
        notes.failures[&0],
        json!({"phase":"database","context":"unspecified","code":"unclassified",
            "exceptionClass":"release_error"})
    );
    let foreign = receipt(
        "database",
        json!({"migrations":38,"ready":true,"counts":counts()}),
    )
    .replace(RUN, &"b".repeat(32));
    let mut fake = Fake::new(vec![ok(&foreign)]);
    let result = act(&mut fake, &mut report, &mut notes, RUN, Action::Database).await;
    assert_eq!(result, Err(Error::Refused));
    assert_eq!(report["failure"], "invalid_vm_guest_receipt");
    let passed = receipt(
        "snapshot",
        json!({"configurationFiles":13,"idempotent":true}),
    );
    let mut fake = Fake::new(vec![ok(&passed)]);
    let projection = act(&mut fake, &mut report, &mut notes, RUN, Action::Snapshot).await;
    assert_eq!(
        projection,
        Ok(json!({"action":"snapshot","passed":true,"configurationFiles":13,"idempotent":true}))
    );
    let mut fake = Fake::new(vec![Ok((2, passed.clone().into_bytes()))]);
    let result = act(&mut fake, &mut report, &mut notes, RUN, Action::Snapshot).await;
    assert_eq!(result, Err(Error::Child(2)));
    // A receipt of another action or another run is refused whether it
    // passed or failed, and no failure labels are kept for it.
    let snapshot_failure = failure
        .to_string()
        .replace("\"action\":\"database\"", "\"action\":\"snapshot\"");
    let foreign_failure = failure.to_string().replace(RUN, &"b".repeat(32));
    for (status, printed, asked) in [
        (0, passed.clone(), Action::BackupRestore),
        (1, snapshot_failure.clone(), Action::BackupRestore),
        (1, foreign_failure, Action::Database),
    ] {
        let mut fake = Fake::new(vec![Ok((status, printed.into_bytes()))]);
        let mut report = json!({});
        let mut notes = Notes::default();
        let result = act(&mut fake, &mut report, &mut notes, RUN, asked).await;
        assert_eq!(result, Err(Error::Refused), "{status} {}", asked.name());
        assert_eq!(report["failure"], "invalid_vm_guest_receipt");
        assert!(notes.failures.is_empty());
    }
    // The same failure of the asked action is admitted with its labels.
    let mut fake = Fake::new(vec![Ok((1, snapshot_failure.into_bytes()))]);
    let mut notes = Notes::default();
    let result = act(&mut fake, &mut json!({}), &mut notes, RUN, Action::Snapshot).await;
    assert_eq!(result, Err(Error::Refused));
    assert_eq!(notes.failures.len(), 1);
}

/// Each second application, of `site.yml`, `public.yml` and `backup.yml`,
/// is refused when it changed even one task, at its own checkpoint and with
/// its recap kept; nothing after it runs.
#[tokio::test]
async fn every_second_application_must_change_nothing() {
    for (index, role) in [
        (1, "site-second"),
        (6, "public-second"),
        (10, "backup-second"),
    ] {
        for changed in ["changed=1", "changed=2"] {
            let mut answers = complete();
            answers.truncate(index);
            answers.push(ok(&GOOD.replace("changed=2", changed)));
            let mut fake = Fake::new(answers);
            let (result, report, notes) = run_exercise(&mut fake).await;
            assert_eq!(result, Err(Error::Refused), "{role} {changed}");
            assert_eq!(report["checkpoint"], role);
            assert_eq!(report["failure"], "vm_ansible_not_idempotent", "{role}");
            assert_eq!(fake.asked.len(), index + 1, "{role}");
            assert!(notes.recaps.contains_key(&index), "{role}");
        }
    }
}

/// The delivery exits 0 only as a complete run: one that did nothing or
/// left a task unreachable, failed, rescued or ignored is refused at
/// `ops-install` with its recap kept, and staging never starts.
#[tokio::test]
async fn an_incomplete_delivery_is_refused_at_its_own_step() {
    for (from, to) in [
        ("ok=9", "ok=0"),
        ("unreachable=0", "unreachable=1"),
        ("failed=0", "failed=1"),
        ("rescued=0", "rescued=1"),
        ("ignored=0", "ignored=1"),
    ] {
        let mut answers = complete();
        answers[2] = ok(&GOOD.replace(from, to));
        let mut fake = Fake::new(answers);
        let (result, report, notes) = run_exercise(&mut fake).await;
        assert_eq!(result, Err(Error::Refused), "{to}");
        assert_eq!(report["checkpoint"], "ops-install", "{to}");
        assert_eq!(report["failure"], "vm_ansible_incomplete", "{to}");
        assert_eq!(fake.roles().last(), Some(&"ops-install"), "{to}");
        assert!(notes.recaps.contains_key(&2), "{to}");
        assert!(report["phases"].get("opsInstall").is_none(), "{to}");
    }
    let mut answers = complete();
    answers[2] = ok("no recap\n");
    let mut fake = Fake::new(answers);
    let (result, report, notes) = run_exercise(&mut fake).await;
    assert_eq!(result, Err(Error::Refused));
    assert_eq!(report["failure"], "vm_ansible_recap_missing_or_ambiguous");
    assert!(!notes.recaps.contains_key(&2));
}

fn run_record(status: &str, conclusion: Value) -> String {
    let repository = json!({"id":1_155_881_649,"full_name":"swiftraccoon/simplestChat",
        "fork":false,"owner":{"id":47_705_353,"login":"swiftraccoon"}});
    json!({"id":77,"run_attempt":2,"head_sha":"b".repeat(40),"head_branch":"main","name":"CI",
        "path":".github/workflows/ci.yml","event":"push","status":status,
        "conclusion":conclusion,"repository":repository,"head_repository":repository})
    .to_string()
}

fn artifacts(id: u64, name: &str) -> String {
    let url =
        format!("https://api.github.com/repos/swiftraccoon/simplestChat/actions/artifacts/{id}");
    json!({"total_count":1,"artifacts":[{"id":id,"name":name,"url":url,
        "archive_download_url":format!("{url}/zip"),"expired":false,"size_in_bytes":1024,
        "digest":format!("sha256:{}", "e".repeat(64)),
        "workflow_run":{"id":77,"head_sha":"b".repeat(40),"repository_id":1_155_881_649_u64,
            "head_repository_id":1_155_881_649_u64}}]})
    .to_string()
}

/// The revision's one CI push run is waited for every thirty seconds, then
/// its two artifacts are selected and the run must still have succeeded.
#[tokio::test]
async fn selection_waits_for_the_revisions_ci_then_selects_both_artifacts() {
    let revision = "b".repeat(40);
    let listing = json!({"total_count":1,"workflow_runs":[
        serde_json::from_str::<Value>(&run_record("in_progress", Value::Null)).unwrap()]});
    let mut fake = Fake::new(vec![
        ok(&listing.to_string()),
        ok(&run_record("in_progress", Value::Null)),
        ok(&run_record("completed", json!("success"))),
        ok(&artifacts(
            5,
            &format!("simplestchat-production-{revision}"),
        )),
        ok(&artifacts(
            6,
            &format!("simplestchat-ops-{revision}-x86_64-unknown-linux-musl"),
        )),
        ok(&run_record("completed", json!("success"))),
    ]);
    let mut report = json!({});
    let signed = select(&mut fake, &mut report, &revision).await.unwrap();
    assert_eq!(
        signed,
        Signed {
            run: 77,
            attempt: 2,
            application: 5,
            ops: 6
        }
    );
    assert_eq!(
        fake.roles(),
        [
            "select-run",
            "ci-status",
            "ci-status",
            "select-application",
            "select-ops",
            "ci-final"
        ]
    );
    assert!(
        fake.asked
            .iter()
            .all(|asked| asked.credential && asked.tool == "gh")
    );
    assert!(
        fake.asked
            .iter()
            .all(|asked| asked.cap == Duration::from_secs(20))
    );
    assert_eq!(fake.paused, [Duration::from_secs(30)]);
    assert_eq!(
        fake.asked[0].args.last().unwrap(),
        &format!(
            "repos/swiftraccoon/simplestChat/actions/workflows/ci.yml/runs?event=push\
             &branch=main&head_sha={revision}&per_page=100"
        )
    );
    assert_eq!(
        &fake.asked[0].args[..5],
        ["api", "--hostname", "github.com", "--method", "GET"]
    );
    // A run that never finishes is refused once the wait is spent.
    let mut answers = vec![ok(&listing.to_string())];
    answers.extend((0..32).map(|_| ok(&run_record("in_progress", Value::Null))));
    let mut fake = Fake::new(answers);
    let refused = select(&mut fake, &mut report, &revision).await;
    assert_eq!(refused, Err(Error::Refused));
    assert_eq!(report["failure"], "ci_wait_timeout");
    assert_eq!(fake.paused.len(), 30);
    // A failed run is the frontend's own refusal.
    let mut fake = Fake::new(vec![
        ok(&listing.to_string()),
        ok(&run_record("completed", json!("failure"))),
    ]);
    assert_eq!(
        select(&mut fake, &mut report, &revision).await,
        Err(Error::Refused)
    );
    assert_eq!(report["failure"], "ci_failed");
    // A run that is no longer a success when its artifacts were chosen is
    // refused.
    let mut fake = Fake::new(vec![
        ok(&listing.to_string()),
        ok(&run_record("completed", json!("success"))),
        ok(&artifacts(
            5,
            &format!("simplestchat-production-{revision}"),
        )),
        ok(&artifacts(
            6,
            &format!("simplestchat-ops-{revision}-x86_64-unknown-linux-musl"),
        )),
        ok(&run_record("in_progress", Value::Null)),
    ]);
    assert_eq!(
        select(&mut fake, &mut report, &revision).await,
        Err(Error::Refused)
    );
    assert_eq!(report["failure"], "ci_status_changed");
}

/// The request is the full scope or the explicit boot scope, with one
/// absolute normal output.
#[test]
fn the_request_is_one_scope_and_one_output() {
    // Every refused form fails before the workflow's revision is read.
    let words = |values: &[&str]| {
        values
            .iter()
            .map(|word| (*word).to_owned())
            .collect::<Vec<_>>()
    };
    assert!(matches!(Request::parse(&words(&["--help"])), Ok(None)));
    for refused in [
        vec![],
        vec!["--output"],
        vec!["--output", "relative"],
        vec!["--output", "/a/../b"],
        vec!["--boot-only"],
        vec!["--output", "/a", "--output", "/b"],
        vec!["--boot-only", "--boot-only", "--output", "/a"],
        vec!["--artifact-dir", "/a", "--output", "/b"],
    ] {
        assert!(Request::parse(&words(&refused)).is_err(), "{refused:?}");
    }
    // With the workflow's revision: the full scope, the boot scope in either
    // order, and only a normal absolute output and a 40-hex revision.
    let revision = "b".repeat(40);
    let parsed = |values: &[&str], revision: Option<&str>| {
        Request::parse_for(&words(values), revision.map(str::to_owned))
    };
    for (values, purpose) in [
        (&["--output", "/runner/out"][..], Purpose::Infrastructure),
        (
            &["--boot-only", "--output", "/runner/out"][..],
            Purpose::Boot,
        ),
        (
            &["--output", "/runner/out", "--boot-only"][..],
            Purpose::Boot,
        ),
    ] {
        let request = parsed(values, Some(&revision)).unwrap().unwrap();
        assert_eq!(request.purpose, purpose, "{values:?}");
        assert_eq!(request.output, Path::new("/runner/out"));
        assert_eq!(request.revision, revision);
    }
    for output in ["relative", "/a/../b", "/a/./b", "/a//b"] {
        assert!(
            parsed(&["--output", output], Some(&revision)).is_err(),
            "{output}"
        );
    }
    for refused in [
        None,
        Some("B".repeat(40)),
        Some("b".repeat(39)),
        Some("b".repeat(41)),
    ] {
        let refused = refused.as_deref();
        assert!(
            parsed(&["--output", "/runner/out"], refused).is_err(),
            "{refused:?}"
        );
    }
}

/// The inventory names only the loopback guest, binds the run, the
/// revision and the authenticated release, and refuses another identity.
#[test]
fn the_inventory_binds_the_run_revision_and_authenticated_release() {
    let value = inventory_value(
        Path::new("/w"),
        2222,
        RUN,
        Path::new("/release/artifact"),
        &"b".repeat(40),
    )
    .unwrap();
    let hosts = value["benchmark_hosts"]["hosts"].as_object().unwrap();
    assert_eq!(hosts.keys().collect::<Vec<_>>(), ["vmfixture"]);
    let host = &hosts["vmfixture"];
    assert_eq!(host["ansible_host"], "127.0.0.1");
    assert_eq!(host["vm_fixture_revision"], "b".repeat(40));
    assert_eq!(host["scpub_release_revision"], "b".repeat(40));
    assert_eq!(host["vm_fixture_artifact_dir"], "/release/artifact");
    assert_eq!(host["scpub_turn_enabled"], false);
    for (port, run, revision) in [
        (1023, RUN.to_owned(), "b".repeat(40)),
        (2222, "A".repeat(32), "b".repeat(40)),
        (2222, RUN.to_owned(), "b".repeat(39)),
    ] {
        let refused = inventory_value(Path::new("/w"), port, &run, Path::new("/r"), &revision);
        assert_eq!(refused, Err(Error::Input));
    }
}

/// The job's budgets: twenty minutes of selection and authentication, at
/// least thirty of guest work, a QEMU watchdog no longer than the job, and
/// boot diagnosis within its own thirty-two.
#[test]
fn the_job_keeps_its_selection_guest_and_boot_budgets() {
    let start = Instant::now();
    let full = control::Control::for_purpose(start, Purpose::Infrastructure).unwrap();
    assert_eq!(full.end() - start, Duration::from_secs(3240));
    assert_eq!(full.body_end() - start, Duration::from_secs(1200));
    assert_eq!(full.whole_body_end() - start, Duration::from_secs(3120));
    assert_eq!(full.debt(), Duration::from_secs(1800));
    let boot = control::Control::for_purpose(start, Purpose::Boot).unwrap();
    assert_eq!(boot.end() - start, Duration::from_secs(1920));
    assert_eq!(boot.body_end() - start, Duration::from_secs(1800));
    let now = Instant::now();
    let far = now + Duration::from_secs(9000);
    assert_eq!(
        watchdog_seconds_for(Purpose::Infrastructure, far, now),
        Ok(3240)
    );
    assert_eq!(watchdog_seconds_for(Purpose::Boot, far, now), Ok(1920));
    // The workflow's step limits hold each whole budget.
    let workflow = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../.github/workflows/infrastructure-security.yml"
    ));
    let workflow: Value = serde_saphyr::from_str(workflow).unwrap();
    let steps = workflow["jobs"]["infrastructure"]["steps"]
        .as_array()
        .unwrap();
    let step = |name: &str| {
        steps
            .iter()
            .find(|step| {
                step["name"]
                    .as_str()
                    .is_some_and(|value| value.starts_with(name))
            })
            .unwrap()
    };
    let full_step = step("Acquire the signed main release");
    let boot_step = step("Diagnose only the owned Debian guest boot");
    let minutes = |value: &Value| Duration::from_secs(value.as_u64().unwrap() * 60);
    assert_eq!(
        minutes(&full_step["timeout-minutes"]),
        Duration::from_secs(55 * 60)
    );
    assert_eq!(
        minutes(&boot_step["timeout-minutes"]),
        Duration::from_secs(35 * 60)
    );
    assert!(minutes(&full_step["timeout-minutes"]) > full.end() - start);
    assert!(minutes(&boot_step["timeout-minutes"]) > boot.end() - start);
    assert_eq!(workflow["jobs"]["infrastructure"]["timeout-minutes"], 60);
}

/// Only a checkout at the workflow's own revision with no tracked or
/// untracked change is used; each refusal is a fixed code.
#[tokio::test]
async fn the_checkout_must_be_the_workflows_clean_revision() {
    let revision = "b".repeat(40);
    let head = format!("{revision}\n");
    let cases = [
        (vec![ok(&head), ok("")], None),
        (vec![ok(&head), ok("\n")], None),
        (
            vec![ok(&"c".repeat(40)), ok("")],
            Some("vm_requires_workflow_revision"),
        ),
        (vec![ok(""), ok("")], Some("vm_requires_workflow_revision")),
        (
            vec![ok(&head), ok("?? new.txt\n")],
            Some("vm_requires_clean_checkout"),
        ),
        (
            vec![ok(&head), ok(" M src/main.rs\n")],
            Some("vm_requires_clean_checkout"),
        ),
    ];
    for (answers, refused) in cases {
        let mut fake = Fake::new(answers);
        let mut report = json!({});
        let result = source(&mut fake, &mut report, &revision).await;
        match refused {
            None => assert!(result.is_ok() && report == json!({})),
            Some(code) => {
                assert!(matches!(result, Err(Error::Refused)));
                assert_eq!(report["failure"], code);
            }
        }
        let wrong = refused == Some("vm_requires_workflow_revision");
        assert_eq!(fake.issued(), if wrong { 1 } else { 2 });
        let asked = &fake.asked[0];
        assert_eq!(
            (asked.role.as_str(), asked.tool.as_str()),
            ("source-revision", "git")
        );
        assert_eq!(asked.args, ["rev-parse", "HEAD"]);
        assert_eq!(
            (asked.cap, asked.accepted.as_slice()),
            (Duration::from_secs(120), &[0][..])
        );
        if let Some(asked) = fake.asked.get(1) {
            assert_eq!(asked.role, "source-clean");
            assert_eq!(
                asked.args,
                ["status", "--porcelain", "--untracked-files=normal"]
            );
            assert!(!asked.credential);
        }
    }
    let mut fake = Fake::new(vec![Err(Error::Child(128))]);
    let result = source(&mut fake, &mut json!({}), &revision).await;
    assert!(matches!(result, Err(Error::Child(128))));
}

/// A summary passes only with no recorded failure and status 0; its cleanup
/// passes only when cleanup finished and no step of it failed; only a passing
/// full run validates the deployment; a failure keeps its step's name.
#[test]
fn the_summary_passes_only_without_failure_status_or_cleanup_fault() {
    let ending = Ending {
        cleaned: true,
        cleanup_failed: false,
        first: None,
        status: 0,
        boot: false,
    };
    let concluded = |ending: Ending, failure: Value| {
        let mut report = json!({"failure":failure,"failureKind":null});
        conclude(&mut report, ending);
        report
    };
    let report = concluded(ending, Value::Null);
    assert_eq!(report["passed"], true);
    assert_eq!(report["cleanupPassed"], true);
    assert_eq!(report["fullDeploymentValidated"], true);
    assert_eq!(report["failureKind"], Value::Null);
    let boot = concluded(
        Ending {
            boot: true,
            ..ending
        },
        Value::Null,
    );
    assert_eq!(
        (&boot["passed"], &boot["fullDeploymentValidated"]),
        (&json!(true), &json!(false))
    );
    for cleanup in [
        Ending {
            cleaned: false,
            ..ending
        },
        Ending {
            cleanup_failed: true,
            ..ending
        },
    ] {
        let report = concluded(cleanup, Value::Null);
        assert_eq!(report["cleanupPassed"], false);
    }
    let status = concluded(
        Ending {
            status: 1,
            ..ending
        },
        Value::Null,
    );
    assert_eq!(status["passed"], false);
    assert_eq!(status["fullDeploymentValidated"], false);
    let failed = Ending {
        first: Some(Error::Deadline),
        ..ending
    };
    let report = concluded(failed, Value::Null);
    assert_eq!(report["passed"], false);
    assert_eq!(report["fullDeploymentValidated"], false);
    assert_eq!(report["failureKind"], "deadline");
    assert_eq!(report["failure"], "finalization");
    let named = concluded(failed, json!("vm_guest_assertion_failed"));
    assert_eq!(named["failure"], "vm_guest_assertion_failed");
}
