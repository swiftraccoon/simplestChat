//! Controlled Docker histories and one owned shell child standing in for
//! `docker events`; no daemon, guest or PostgreSQL is exercised.

use super::super::super::{
    host::events::Auditor,
    process::Running,
    protocol::{self, Request},
    sources,
};
use super::super::actors::Collector;
use super::*;
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Command as Child};

const HEALTH: &str = "/bin/sh -c fixed-health";
const QUERY: &str = "psql PRIVATE-fixed-query";

fn id(byte: char) -> String {
    std::iter::repeat_n(byte, 64).collect()
}
fn pg() -> String {
    id('a')
}
fn on(actor: &str, action: &str, exec: char, exit: &str) -> Value {
    json!({"Type":"container","Action":action,"Actor":{"ID":actor,
        "Attributes":{"execID":id(exec),"exitCode":exit,"name":"PRIVATE-name"}}})
}
fn event(action: &str, exec: char) -> Value {
    on(&pg(), action, exec, "0")
}
fn trace(command: &str, exec: char) -> Vec<Value> {
    vec![
        event(&format!("exec_create: {command}"), exec),
        event(&format!("exec_start: {command}"), exec),
        event("exec_die", exec),
    ]
}
fn probe(exec: char) -> Vec<Value> {
    trace(HEALTH, exec)
}
fn plain(action: &str) -> Value {
    json!({"Type":"container","Action":action,"Actor":{"ID":pg(),
        "Attributes":{"name":"PRIVATE-name","image":"PRIVATE-image"}}})
}
fn query() -> BTreeMap<String, usize> {
    BTreeMap::from([(QUERY.into(), 1)])
}
fn fixture_actors() -> BTreeSet<String> {
    BTreeSet::from([id('c')])
}
/// The strict check's verdict on `window` and, for a refusal, its record.
fn refusal(
    prefix: &[Value],
    window: &[Value],
    suffix: &[Value],
    commands: &BTreeMap<String, usize>,
) -> Value {
    let rows = window
        .iter()
        .filter(|row| postgres(row, &pg()))
        .cloned()
        .collect::<Vec<_>>();
    let mut audit = Auditor::new(window);
    let result = policy::pg_exec_history_observed(&rows, commands, HEALTH, &mut audit);
    assert_eq!(result, Err(Error::Refused));
    assert_eq!(result, policy::pg_exec_history(&rows, commands, HEALTH));
    let fixture = fixture_actors();
    let pg = pg();
    let edges = Edges {
        prefix,
        window,
        suffix,
        pg: &pg,
        health: HEALTH,
        fixture: &fixture,
    };
    let record = diagnose(&edges, Some(commands), audit.first_pg());
    private(&record);
    record
}
/// The record carries no identifier, name, image or command text.
fn private(record: &Value) {
    let encoded = serde_json::to_string(record).unwrap();
    assert!(encoded.len() <= 2048, "{}", encoded.len());
    for secret in [
        "PRIVATE".to_owned(),
        "fixed-health".to_owned(),
        "psql".to_owned(),
        id('a'),
        id('b'),
        id('c'),
    ] {
        assert!(
            !encoded.contains(&secret),
            "the record leaked a private value"
        );
    }
}
fn described(
    action: &str,
    command: Option<&str>,
    actor: &str,
    window: &str,
    fault: &str,
    exit: Option<u8>,
) -> Value {
    json!({"type":"container","action":action,"command":command,"actor":actor,
        "window":window,"fault":fault,"exitCode":exit})
}
fn missing(action: &str, command: &str, window: &str) -> Value {
    described(action, Some(command), "postgres", window, "missing", None)
}

#[test]
fn quiet_waits_only_for_an_unfinished_health_probe_of_the_database() {
    let open = probe('b');
    assert!(quiet(&[], &pg(), HEALTH));
    assert!(!quiet(&open[..1], &pg(), HEALTH));
    assert!(!quiet(&open[..2], &pg(), HEALTH));
    assert!(quiet(&open, &pg(), HEALTH));
    let mut rows = open[..2].to_vec();
    rows.extend(probe('c'));
    assert!(!quiet(&rows, &pg(), HEALTH));
    rows.push(open[2].clone());
    assert!(quiet(&rows, &pg(), HEALTH));
    // Only the inspected probe counts: the launcher's own executions are judged
    // whole by the check itself and are never waited for.
    assert!(quiet(&trace(QUERY, 'b')[..2], &pg(), HEALTH));
    let elsewhere = on(&id('c'), &format!("exec_create: {HEALTH}"), 'b', "0");
    assert!(quiet(&[elsewhere], &pg(), HEALTH));
    let mut network = open[0].clone();
    network["Type"] = "network".into();
    assert!(quiet(&[network], &pg(), HEALTH));
    let mut anonymous = open[0].clone();
    anonymous["Actor"]["Attributes"]["execID"] = Value::Null;
    assert!(quiet(&[anonymous], &pg(), HEALTH));
}

#[test]
fn opening_deadline_waits_ten_seconds_but_never_into_the_native_reservation() {
    let now = Instant::now();
    let ample = now + Duration::from_secs(1680);
    assert_eq!(opening_deadline(now, ample), now + Duration::from_secs(10));
    // 1,170 reserved seconds and one for the cut leave three to wait.
    let tight = now + Duration::from_secs(1174);
    assert_eq!(opening_deadline(now, tight), now + Duration::from_secs(3));
    let exact = now + Duration::from_secs(1171);
    assert_eq!(opening_deadline(now, exact), now);
    for short in [Duration::from_secs(1170), Duration::ZERO] {
        assert_eq!(opening_deadline(now, now + short), now);
    }
    // A body that already ended leaves no wait either.
    assert_eq!(
        opening_deadline(now + Duration::from_secs(5), now),
        now + Duration::from_secs(5)
    );
}

#[test]
fn a_record_is_kept_only_for_the_first_failure_without_an_earlier_record() {
    let record = json!({"scope":"fixture record"});
    let earlier = json!({"scope":"earlier record"});
    for (result, before, now, kept, emitted) in [
        (Err(Error::Refused), None, None, None, true),
        (Ok(()), None, None, None, false),
        (Err(Error::Refused), Some(Error::Io), None, None, false),
        (Err(Error::Refused), None, Some(Error::Refused), None, false),
        (
            Err(Error::Refused),
            None,
            None,
            Some(earlier.clone()),
            false,
        ),
    ] {
        let mut slot = kept.clone();
        let mut asked = false;
        retain(&mut slot, &result, before, now, || {
            asked = true;
            Some(record.clone())
        });
        assert_eq!(asked, emitted);
        assert_eq!(slot, if emitted { Some(record.clone()) } else { kept });
    }
    let mut slot = None;
    retain(&mut slot, &Err(Error::Refused), None, None, || None);
    assert!(slot.is_none());
}

#[test]
fn probes_cut_at_either_edge_are_named_with_the_side_they_fell_on() {
    let opening = probe('b');
    let closing = probe('c');
    let window = [
        vec![opening[2].clone()],
        trace(QUERY, 'd'),
        closing[..2].to_vec(),
    ]
    .concat();
    let record = refusal(&opening[..2], &window, &closing[2..], &query());
    assert_eq!(
        record,
        json!({"schemaVersion":1,"scope":"MaintainLauncherPgHistoryDiagnostic",
            "reason":"pg-missing-create","postgresEvents":6,
            "events":[missing("exec_create","health","before"),
                missing("exec_die","health","after")],
            "withheldEvents":0})
    );
    // A death the collector has not received is unobserved, not absent.
    let record = refusal(
        &[],
        &[trace(QUERY, 'd'), closing[..2].to_vec()].concat(),
        &[],
        &query(),
    );
    assert_eq!(record["reason"], "pg-incomplete-sequence");
    assert_eq!(
        record["events"],
        json!([missing("exec_die", "health", "unobserved")])
    );
    // The first absent phase is described: here the start.
    let record = refusal(
        &[],
        &[trace(QUERY, 'd'), closing[..1].to_vec()].concat(),
        &closing[1..],
        &query(),
    );
    assert_eq!(
        record["events"],
        json!([missing("exec_start", "health", "after")])
    );
    // A create that only another container shows keeps that actor's class;
    // an event of another type is not a container's.
    for (actor, class) in [(id('c'), "fixture"), (id('e'), "unknown")] {
        let earlier = on(&actor, &format!("exec_create: {HEALTH}"), 'b', "0");
        let mut decoy = on(&id('9'), &format!("exec_create: {HEALTH}"), 'b', "0");
        decoy["Type"] = "network".into();
        let record = refusal(&[decoy, earlier], &opening[1..], &[], &BTreeMap::new());
        assert_eq!(
            record["events"],
            json!([described(
                "exec_create",
                Some("health"),
                class,
                "before",
                "missing",
                None
            )])
        );
    }
    // A death alone takes its command from the creation before the window.
    let record = refusal(&opening[..2], &opening[2..], &[], &BTreeMap::new());
    assert_eq!(
        record["events"],
        json!([missing("exec_create", "health", "before")])
    );
    let record = refusal(&[], &opening[2..], &[], &BTreeMap::new());
    assert_eq!(
        record["events"],
        json!([described(
            "exec_create",
            None,
            "postgres",
            "unobserved",
            "missing",
            None
        )])
    );
}

#[test]
fn failed_unknown_and_lifecycle_events_are_described_without_their_identities() {
    let mut killed = probe('b');
    killed[2]["Actor"]["Attributes"]["exitCode"] = "137".into();
    let record = refusal(&[], &killed, &[], &BTreeMap::new());
    assert_eq!(record["reason"], "pg-nonzero-exit");
    assert_eq!(
        record["events"],
        json!([described(
            "exec_die",
            Some("health"),
            "postgres",
            "inside",
            "nonzero-exit",
            Some(137)
        )])
    );
    for (code, exit) in [
        ("1", Some(1)),
        ("255", Some(255)),
        ("00", None),
        ("256", None),
    ] {
        let mut failed = probe('b');
        failed[2]["Actor"]["Attributes"]["exitCode"] = code.into();
        let record = refusal(&[], &failed, &[], &BTreeMap::new());
        assert_eq!(record["events"][0]["exitCode"], json!(exit), "{code}");
    }
    let mut numeric = probe('b');
    numeric[2]["Actor"]["Attributes"]["exitCode"] = 0.into();
    let record = refusal(&[], &numeric, &[], &BTreeMap::new());
    assert_eq!(record["events"][0]["fault"], "nonzero-exit");
    assert!(record["events"][0]["exitCode"].is_null());
    // An unlisted execution is described once, however much of it follows.
    let created = refusal(
        &[],
        &trace("PRIVATE unlisted", 'b')[..1],
        &[],
        &BTreeMap::new(),
    );
    assert_eq!(created["events"].as_array().unwrap().len(), 1);
    let record = refusal(&[], &trace("PRIVATE unlisted", 'b'), &[], &BTreeMap::new());
    assert_eq!(record["reason"], "pg-unknown-command");
    assert_eq!(record["events"], created["events"]);
    assert_eq!(
        record["events"],
        json!([described(
            "exec_create",
            Some("unlisted"),
            "postgres",
            "inside",
            "unlisted-command",
            None
        )])
    );
    let mut shapeless = plain("die");
    shapeless["Action"] = json!(7);
    let mut short = probe('b');
    short[0]["Actor"]["Attributes"]["execID"] = "PRIVATE".into();
    let window = [
        plain("kill"),
        plain("die"),
        plain("health_status: unhealthy"),
        plain("PRIVATE-neighbor"),
        shapeless,
        short[0].clone(),
    ];
    let record = refusal(&[], &window, &[], &BTreeMap::new());
    assert_eq!(record["reason"], "pg-exec-id");
    let actions = record["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| {
            assert_eq!(event["fault"], "unadmitted");
            assert_eq!(event["window"], "inside");
            assert!(event["command"].is_null());
            event["action"].as_str().unwrap().to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        actions,
        [
            "kill",
            "die",
            "health_status",
            "other",
            "other",
            "exec_create"
        ]
    );
    // A healthy status is the probe's own expected report.
    let mut healthy = probe('b');
    healthy.push(plain("health_status: healthy"));
    healthy.push(plain("pause"));
    let record = refusal(&[], &healthy, &[], &BTreeMap::new());
    assert_eq!(record["postgresEvents"], 5);
    assert_eq!(record["events"].as_array().unwrap().len(), 1);
    assert_eq!(record["events"][0]["action"], "pause");
}

#[test]
fn order_duplicates_and_command_counts_are_named_once_per_execution() {
    let probe = probe('b');
    let none = BTreeMap::new();
    let check = |window: Vec<Value>, commands: &BTreeMap<String, usize>, expected: [&str; 3]| {
        let record = refusal(&[], &window, &[], commands);
        let events = record["events"].as_array().unwrap();
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(
            [&record["reason"], &events[0]["action"], &events[0]["fault"]],
            expected.map(Value::from).each_ref()
        );
    };
    check(
        vec![probe[0].clone(), probe[0].clone()],
        &none,
        ["pg-duplicate-create", "exec_create", "duplicate"],
    );
    check(
        vec![probe[1].clone(), probe[0].clone()],
        &none,
        ["pg-missing-create", "exec_create", "out-of-order"],
    );
    check(
        vec![probe[0].clone(), probe[1].clone(), probe[1].clone()],
        &none,
        ["pg-start-order", "exec_start", "out-of-order"],
    );
    check(
        vec![probe[0].clone(), probe[2].clone()],
        &none,
        ["pg-die-order", "exec_die", "out-of-order"],
    );
    check(
        [probe.clone(), vec![probe[2].clone()]].concat(),
        &none,
        ["pg-die-order", "exec_die", "out-of-order"],
    );
    let mut renamed = probe.clone();
    renamed[1]["Action"] = "exec_start: PRIVATE other".into();
    check(
        renamed,
        &none,
        ["pg-die-action", "exec_start", "unadmitted"],
    );
    let mut detached = probe.clone();
    detached.insert(2, event("exec_detach", 'b'));
    check(
        detached,
        &none,
        ["pg-die-action", "exec_detach", "unadmitted"],
    );
    let mut twice = trace(QUERY, 'd');
    twice.extend(trace(QUERY, 'e'));
    check(
        twice,
        &query(),
        ["pg-command-count", "exec_create", "surplus"],
    );
    // A shortfall is placed where the collector saw the creation, if anywhere.
    for (prefix, suffix, window) in [
        (trace(QUERY, 'd'), Vec::new(), "before"),
        (Vec::new(), trace(QUERY, 'd'), "after"),
        (Vec::new(), Vec::new(), "unobserved"),
    ] {
        let record = refusal(&prefix, &probe, &suffix, &query());
        assert_eq!(record["reason"], "pg-command-count");
        assert_eq!(
            record["events"],
            json!([described(
                "exec_create",
                Some("launcher"),
                "postgres",
                window,
                "missing",
                None
            )])
        );
    }
    // A creation only another container shows keeps that actor's class.
    let elsewhere = on(&id('c'), &format!("exec_create: {QUERY}"), 'd', "0");
    let record = refusal(&[elsewhere], &probe, &[], &query());
    assert_eq!(
        record["events"],
        json!([described(
            "exec_create",
            Some("launcher"),
            "fixture",
            "before",
            "missing",
            None
        )])
    );
    // A cut launcher execution is one missing event, not also a shortfall.
    let cut = trace(QUERY, 'd');
    let record = refusal(&cut[..1], &cut[1..], &[], &query());
    assert_eq!(
        record["events"],
        json!([missing("exec_create", "launcher", "before")])
    );
}

#[test]
fn records_show_eight_events_and_count_the_rest_up_to_the_row_bound() {
    let mut window = Vec::new();
    for _ in 0..12 {
        window.push(plain("top"));
    }
    let record = refusal(&[], &window, &[], &BTreeMap::new());
    assert_eq!(record["events"].as_array().unwrap().len(), 8);
    assert_eq!(record["withheldEvents"], 4);
    assert_eq!(record["postgresEvents"], 12);
    let window = vec![plain("top"); 2100];
    let record = refusal(&[], &window, &[], &BTreeMap::new());
    assert_eq!(record["withheldEvents"], 2048);
    assert_eq!(record["postgresEvents"], 2048);
    // Rows of other actors are not the PostgreSQL check's events.
    let mut other = plain("top");
    other["Actor"]["ID"] = id('c').into();
    let record = refusal(&[], &[plain("top"), other], &[], &BTreeMap::new());
    assert_eq!(record["postgresEvents"], 1);
    assert_eq!(record["events"].as_array().unwrap().len(), 1);
}

#[test]
fn a_derivation_refusal_keeps_its_reason_and_describes_no_event() {
    let fixture = fixture_actors();
    let pg = pg();
    let mut window = probe('b');
    window.push(on(&id('c'), "top", 'b', "0"));
    let edges = Edges {
        prefix: &[],
        window: &window,
        suffix: &[],
        pg: &pg,
        health: HEALTH,
        fixture: &fixture,
    };
    let record = diagnose(&edges, None, Some(PgReason::SourceMissing));
    assert_eq!(
        record,
        json!({"schemaVersion":1,"scope":"MaintainLauncherPgHistoryDiagnostic",
            "reason":"pg-source-missing","postgresEvents":3,"events":[],"withheldEvents":0})
    );
    let record = diagnose(&edges, None, None);
    assert!(record["reason"].is_null());
}

#[test]
fn closed_vocabularies_name_each_variant_once() {
    assert_eq!(
        [Command::Health, Command::Launcher, Command::Unlisted].map(Command::name),
        ["health", "launcher", "unlisted"]
    );
    assert_eq!(
        [Actor::Postgres, Actor::Fixture, Actor::Unknown].map(Actor::name),
        ["postgres", "fixture", "unknown"]
    );
    assert_eq!(
        [
            Window::Before,
            Window::Inside,
            Window::After,
            Window::Unobserved
        ]
        .map(Window::name),
        ["before", "inside", "after", "unobserved"]
    );
    assert_eq!(
        [
            Fault::Missing,
            Fault::Unadmitted,
            Fault::UnlistedCommand,
            Fault::Duplicate,
            Fault::OutOfOrder,
            Fault::NonzeroExit,
            Fault::Surplus,
        ]
        .map(Fault::name),
        [
            "missing",
            "unadmitted",
            "unlisted-command",
            "duplicate",
            "out-of-order",
            "nonzero-exit",
            "surplus"
        ]
    );
    let names = ACTIONS.iter().collect::<BTreeSet<_>>();
    assert_eq!(names.len(), ACTIONS.len());
    for name in ACTIONS {
        assert_eq!(action_name(&format!("{name}: PRIVATE detail")), name);
    }
    for neighbor in ["exec", "Exec_create", "exec_create ", "", "health_status:"] {
        assert_eq!(action_name(neighbor), "other", "{neighbor}");
    }
}

static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "maintain-pg-window-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn launcher_request() -> Request {
    let sources = sources::guest_names()
        .into_iter()
        .map(|name| (name, json!("a".repeat(64))))
        .collect::<serde_json::Map<_, _>>();
    let pin = json!({"sha256":"c".repeat(64),"bytes":123});
    let value = json!({"schemaVersion":1,"scope":"MaintainLauncherPrerequisiteRequest",
        "runId":"d".repeat(32),"revision":"e".repeat(40),"scenario":"dirty-source",
        "setupSeconds":510,"bodySeconds":1680,"endSeconds":1800,"binary":pin,
        "manifest":pin,"archive":pin,"sourceBundle":pin,"sourceTree":"f".repeat(40),
        "sourcePins":sources,"postgresImage":format!("postgres@sha256:{}","a".repeat(64)),
        "caddyImage":protocol::CADDY_IMAGE,"engineVersion":"29.0.0"});
    Request::decode(&serde_json::to_vec(&value).unwrap()).unwrap()
}
async fn read_until(fixture: &Fixture, control: &mut Control, rows: usize) -> usize {
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        let received = {
            let mut monitors = fixture.monitors.lock().unwrap();
            monitors.check(control).unwrap();
            monitors.collector.as_ref().unwrap().rows.len()
        };
        if received >= rows {
            return received;
        }
        assert!(Instant::now() < end, "the stand-in stream stalled");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
fn native_start(fixture: &Fixture) -> usize {
    fixture
        .monitors
        .lock()
        .unwrap()
        .collector
        .as_ref()
        .unwrap()
        .native_start
}
fn verdict(rows: &[Value]) -> Result<()> {
    let rows = rows
        .iter()
        .filter(|row| postgres(row, &pg()))
        .cloned()
        .collect::<Vec<_>>();
    policy::pg_exec_history(&rows, &BTreeMap::new(), HEALTH)
}
/// A launcher context over an inert wire; the tests use only its control.
fn context() -> Context {
    use super::super::super::{observation::Observations, process::Commands};
    use std::os::{fd::OwnedFd, unix::net::UnixStream};
    let (input, peer) = UnixStream::pair().unwrap();
    drop(peer);
    let output = fs::OpenOptions::new()
        .write(true)
        .open("/dev/null")
        .unwrap();
    Context {
        control: Control::new(Instant::now()).unwrap(),
        commands: Commands::new(),
        request: launcher_request(),
        source_root: PathBuf::from("/nonexistent-maintain-source"),
        sources: BTreeMap::new(),
        originals: Vec::new(),
        observed: Observations::new(),
        wire: protocol::Wire::fixture(OwnedFd::from(input).into(), output),
        socket: None,
        host_retired: BTreeSet::new(),
        host_diagnostic: None,
        pg_diagnostic: None,
    }
}
/// A fixture whose collector reads the owned shell child running `script`
/// with `rows` in its environment.
fn streaming(scratch: &Scratch, script: &str, rows: &[(&str, &Value)]) -> Fixture {
    let mut child = Child::new("/bin/sh");
    child
        .args(["-c", script])
        .env_clear()
        .env("PATH", "/usr/bin:/bin");
    for (name, row) in rows {
        child.env(name, row.to_string());
    }
    let mut fixture = Fixture::new(&launcher_request());
    fixture.pg = Some(pg());
    fixture.pg_health = HEALTH.into();
    fixture.monitors.lock().unwrap().collector =
        Some(Collector::fixture(Running::fixture(child, &scratch.0)));
    fixture
}
async fn retire(fixture: &Fixture, mut ctx: Context) {
    let mut collector = fixture.monitors.lock().unwrap().collector.take().unwrap();
    assert_eq!(collector.finish(&mut ctx.control).await, Ok(()));
    ctx.control.begin_settling();
    assert_eq!(ctx.control.finish(), Ok(()));
}

/// Reproduces the hosted refusal and its correction on a live owned stream:
/// a probe in flight at an immediate cut is refused at either edge, while the
/// launcher's waiting cuts take whole probes. The stand-in prints each probe's
/// creation and start, then its death one second later, and a third probe's
/// creation a second after that, which a close waiting to its deadline keeps.
#[tokio::test(flavor = "current_thread")]
async fn owned_event_stream_cuts_between_whole_probes_and_an_expired_wait_still_refuses() {
    let scratch = Scratch::new();
    let opening = probe('b');
    let closing = probe('c');
    let script = concat!(
        "printf '%s\\n' \"$A1\" \"$A2\"; sleep 1; printf '%s\\n' \"$A3\"; sleep 1; ",
        "printf '%s\\n' \"$B1\" \"$B2\"; sleep 1; printf '%s\\n' \"$B3\"; sleep 1; ",
        "printf '%s\\n' \"$C1\"; exec sleep 30"
    );
    let names = ["A1", "A2", "A3", "B1", "B2", "B3", "C1"];
    let rows = [opening.clone(), closing.clone(), probe('d')[..1].to_vec()].concat();
    let named = names.into_iter().zip(&rows).collect::<Vec<_>>();
    let mut fixture = streaming(&scratch, script, &named);
    let mut ctx = context();

    assert_eq!(read_until(&fixture, &mut ctx.control, 2).await, 2);
    // An expired wait cuts at once, inside the open probe, as before the wait.
    fixture
        .open_window(&mut ctx.control, Instant::now())
        .await
        .unwrap();
    assert_eq!(native_start(&fixture), 2);
    // The launcher's cut follows the probe's death, not its deadline, and
    // leaves daemon effects uncertain until the window is observed.
    assert!(!fixture.unknown_effect);
    fixture.open_native_window(&mut ctx).await.unwrap();
    assert_eq!(native_start(&fixture), 3);
    assert!(fixture.unknown_effect);

    assert_eq!(read_until(&fixture, &mut ctx.control, 5).await, 5);
    fixture
        .close_window(&mut ctx.control, Instant::now())
        .await
        .unwrap();
    assert_eq!(fixture.native_rows, closing[..2]);
    assert_eq!(verdict(&fixture.native_rows), Err(Error::Refused));
    let record = fixture.pg_refusal(Some(&BTreeMap::new()), None).unwrap();
    assert_eq!(
        record["events"],
        json!([missing("exec_die", "health", "unobserved")])
    );
    fixture.close_native_window(&mut ctx.control).await.unwrap();
    assert_eq!(fixture.native_rows, closing);
    assert_eq!(verdict(&fixture.native_rows), Ok(()));

    // The immediate cuts of both edges on the same stream: the hosted symptom.
    assert_eq!(read_until(&fixture, &mut ctx.control, 7).await, 7);
    let received = fixture
        .monitors
        .lock()
        .unwrap()
        .collector
        .as_ref()
        .unwrap()
        .rows
        .clone();
    assert_eq!(received, rows);
    fixture
        .monitors
        .lock()
        .unwrap()
        .collector
        .as_mut()
        .unwrap()
        .native_start = 2;
    fixture.native_rows = rows[2..5].to_vec();
    assert_eq!(verdict(&fixture.native_rows), Err(Error::Refused));
    let record = fixture.pg_refusal(Some(&BTreeMap::new()), None).unwrap();
    assert_eq!(
        record["events"],
        json!([
            missing("exec_create", "health", "before"),
            missing("exec_die", "health", "after")
        ])
    );
    private(&record);
    retire(&fixture, ctx).await;
}

/// The record's edges are exactly the collector's rows before the opening cut
/// and after the retained window, and every container the fixture created or
/// joined is a fixture actor.
#[tokio::test(flavor = "current_thread")]
async fn a_record_reads_the_rows_around_the_retained_window() {
    let scratch = Scratch::new();
    let mut fixture = streaming(&scratch, "exec sleep 30", &[]);
    let ctx = context();
    fixture.actors.insert(id('c'));
    fixture.app = Some(id('d'));
    fixture.proxy = Some(id('e'));
    let creates = [id('c'), id('d'), id('e')]
        .iter()
        .zip(['f', '1', '2'])
        .map(|(actor, exec)| on(actor, &format!("exec_create: {HEALTH}"), exec, "0"))
        .collect::<Vec<_>>();
    let mut window = Vec::new();
    for exec in ['f', '1', '2'] {
        window.extend(probe(exec)[1..].to_vec());
    }
    window.extend(trace(QUERY, '3'));
    let later = trace(QUERY, '4');
    {
        let mut monitors = fixture.monitors.lock().unwrap();
        let collector = monitors.collector.as_mut().unwrap();
        collector.rows = [creates.clone(), window.clone(), later.clone()].concat();
        collector.native_start = creates.len();
    }
    fixture.native_rows = window;
    let commands = BTreeMap::from([(QUERY.to_owned(), 2)]);
    let record = fixture.pg_refusal(Some(&commands), None).unwrap();
    let fixture_create = described(
        "exec_create",
        Some("health"),
        "fixture",
        "before",
        "missing",
        None,
    );
    assert_eq!(
        record["events"],
        json!([
            fixture_create,
            fixture_create,
            fixture_create,
            described(
                "exec_create",
                Some("launcher"),
                "postgres",
                "after",
                "missing",
                None
            )
        ])
    );
    assert_eq!(record["postgresEvents"], 9);
    private(&record);
    // Without the rows after the window the second execution is unobserved.
    {
        let mut monitors = fixture.monitors.lock().unwrap();
        let collector = monitors.collector.as_mut().unwrap();
        collector
            .rows
            .truncate(creates.len() + fixture.native_rows.len());
    }
    let record = fixture.pg_refusal(Some(&commands), None).unwrap();
    assert_eq!(record["events"][3]["window"], "unobserved");
    // A window the collector no longer holds yields no record.
    fixture.native_rows.extend(later);
    assert!(fixture.pg_refusal(Some(&commands), None).is_none());
    retire(&fixture, ctx).await;
}
