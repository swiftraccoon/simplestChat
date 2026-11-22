//! The fixture's pure decisions against the answers the Python release
//! fixture gave for the same inputs, recorded before it was removed. Each
//! case is rebuilt here and must be the input the reference answered, bound
//! by the SHA-256 of its canonical JSON; every recorded case is replayed.

use super::super::{
    host::Paths,
    images::{self, Kind},
    plan::{self, Action},
    policy,
    render::{self, Identity, Sources},
};
use super::{Run, TOKEN};
use serde_json::{Map, Value, json};
use std::{collections::BTreeMap, path::Path};

fn recording() -> &'static Value {
    static RECORDING: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    RECORDING.get_or_init(|| serde_json::from_str(include_str!("recorded_reference.json")).unwrap())
}

/// One recorded test's cases, replayed in order.
struct Recorded {
    test: &'static str,
    cases: &'static [Value],
    next: usize,
}
impl Recorded {
    fn new(test: &'static str) -> Self {
        let cases = recording()["tests"][test]["cases"].as_array().unwrap();
        Self {
            test,
            cases,
            next: 0,
        }
    }
    /// The answer to the next case, once `given` is shown to be its input.
    fn answer(&mut self, given: &Value) -> &'static Value {
        let case = &self.cases[self.next];
        let digest = super::super::files::sha256(&serde_json::to_vec(given).unwrap());
        assert_eq!(
            case["input"].as_str(),
            Some(digest.as_str()),
            "{}: case {}",
            self.test,
            self.next
        );
        self.next += 1;
        &case["answer"]
    }
    /// The number of replayed cases, once every recorded case was asked for.
    fn finish(self) -> usize {
        assert!(
            self.next == self.cases.len(),
            "{}: cases left unasked",
            self.test
        );
        self.next
    }
}

/// The secrets every recorded rendering was given.
pub(super) fn secrets() -> [String; 6] {
    std::array::from_fn(|index| (index + 1).to_string().repeat(64))
}

const RECORDED_IMAGE: &str =
    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const RECORDED_POSTGRES: &str = "docker.io/library/postgres:18.6-bookworm@sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
const RECORDED_CADDY: &str = "docker.io/library/caddy:2.11.4-alpine@sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

/// The reference's own rendered templates of its default case.
pub(super) fn rendered_default() -> BTreeMap<String, String> {
    let given = &recording()["tests"]["render"]["cases"][0]["given"]["rendered"];
    serde_json::from_value(given.clone()).unwrap()
}

/// A recorded rendering as these paths, this baseline image and the
/// deployment's own images would have rendered it: each recorded value is a
/// distinct literal the templates substitute verbatim.
pub(super) fn localized(text: &str, paths: &Paths, image: &str) -> String {
    text.replace(
        "/etc/simplestchat-public",
        &paths.config.display().to_string(),
    )
    .replace(
        "/srv/simplestchat-public",
        &paths.root.display().to_string(),
    )
    .replace(RECORDED_IMAGE, image)
    .replace(RECORDED_POSTGRES, plan::POSTGRES)
    .replace(RECORDED_CADDY, plan::CADDY)
}

#[test]
fn the_fixture_configuration_is_the_one_the_reference_rendered() {
    let mut recorded = Recorded::new("render");
    for case in recording()["tests"]["render"]["cases"].as_array().unwrap() {
        let given = &case["given"];
        let answer = recorded.answer(given);
        let text = |key: &str| given[key].as_str().unwrap().to_owned();
        let identity = Identity {
            image: text("image"),
            revision: text("revision"),
            token: text("token"),
            postgres: text("postgresImage"),
            caddy: text("caddyImage"),
        };
        let mut rendered: BTreeMap<String, String> =
            serde_json::from_value(given["rendered"].clone()).unwrap();
        let sources = Sources {
            base: rendered.remove("docker-compose.yml").unwrap(),
            caddyfile: rendered.remove("Caddyfile").unwrap(),
            rendered,
        };
        let config = Path::new("/etc/simplestchat-public");
        let root = Path::new("/srv/simplestchat-public");
        let outputs = render::outputs(&identity, config, root, &sources).unwrap();
        let expected: BTreeMap<String, String> =
            serde_json::from_value(answer["outputs"].clone()).unwrap();
        assert!(outputs.keys().collect::<Vec<_>>() == expected.keys().collect::<Vec<_>>());
        for (name, text) in &outputs {
            if let Some(structure) = answer["yaml"].get(name) {
                let parsed: Value = serde_saphyr::from_str(text).unwrap();
                assert!(&parsed == structure, "{name}");
            } else {
                assert!(text == &expected[name], "{name}");
            }
        }
    }
    assert_eq!(recorded.finish(), 2);
}

fn identity_cases() -> Vec<Value> {
    let base = json!({
        "image": format!("sha256:{}", "a".repeat(64)), "revision": "b".repeat(40), "token": TOKEN,
        "postgresImage": RECORDED_POSTGRES, "caddyImage": RECORDED_CADDY,
    });
    let allowed = format!(
        "ghcr.io/swiftraccoon/simplestchat-caddy:2.11.7@sha256:{}",
        "e".repeat(64)
    );
    let variants: Vec<(&str, String)> = vec![
        ("image", "simplestchat:latest".into()),
        ("image", format!("sha256:{}", "A".repeat(64))),
        ("image", format!("sha256:{}", "a".repeat(63))),
        ("image", format!("sha256:{}\n", "a".repeat(64))),
        ("revision", "main".into()),
        ("revision", "B".repeat(40)),
        ("revision", "b".repeat(39)),
        ("token", "unknown".into()),
        ("token", "c".repeat(31)),
        ("token", "c".repeat(33)),
        ("postgresImage", "postgres:latest".into()),
        (
            "postgresImage",
            format!("docker.io/library/postgres:18.6@sha256:{}", "d".repeat(63)),
        ),
        (
            "postgresImage",
            format!(
                "docker.io/library/postgres:18.6_x@sha256:{}",
                "d".repeat(64)
            ),
        ),
        ("postgresImage", RECORDED_POSTGRES.into()),
        ("caddyImage", "caddy:latest".into()),
        ("caddyImage", allowed.clone()),
        (
            "caddyImage",
            allowed.replace("swiftraccoon", "another-owner"),
        ),
        (
            "caddyImage",
            allowed.replace("simplestchat-caddy", "another-image"),
        ),
        (
            "caddyImage",
            allowed.replace("ghcr.io/", "ghcr.io.example/"),
        ),
        ("caddyImage", allowed.replace("ghcr.io/", "ghcrXio/")),
        ("caddyImage", allowed.split('@').next().unwrap().into()),
        ("caddyImage", format!("{allowed}\n")),
        ("caddyImage", RECORDED_CADDY.into()),
        (
            "caddyImage",
            format!("docker.ioXlibrary/caddy:2.11.4@sha256:{}", "e".repeat(64)),
        ),
        (
            "postgresImage",
            format!("docker.io/library/postgres:@sha256:{}", "d".repeat(64)),
        ),
    ];
    let mut cases = vec![base.clone()];
    for (key, value) in variants {
        let mut case = base.clone();
        case[key] = json!(value);
        cases.push(case);
    }
    cases
}

#[test]
fn only_immutable_identities_and_reviewed_proxy_publishers_are_rendered() {
    let mut recorded = Recorded::new("identity");
    for case in identity_cases() {
        let answer = recorded.answer(&case);
        let text = |key: &str| case[key].as_str().unwrap().to_owned();
        let identity = Identity {
            image: text("image"),
            revision: text("revision"),
            token: text("token"),
            postgres: text("postgresImage"),
            caddy: text("caddyImage"),
        };
        let verdict = if identity.check().is_ok() {
            "accepted"
        } else {
            "refused"
        };
        assert!(answer == verdict);
    }
    assert_eq!(recorded.finish(), 26);
}

#[test]
fn only_the_helpers_own_fixed_stage_line_names_a_browser_failure() {
    let prefix = "Release asset rollover browser check failed: ";
    let mut stages: Vec<&str> = policy::ROLLOVER_STAGES.to_vec();
    stages.sort_unstable();
    let mut cases: Vec<String> = stages
        .iter()
        .map(|stage| format!("{prefix}{stage}\n"))
        .collect();
    cases.extend(["load-old", "cleanup"].map(|stage| format!("{prefix}{stage}")));
    cases.extend([
        "PRIVATE_TOKEN_AND_BROWSER_ERROR".into(),
        format!("{prefix}private-token\n"),
        format!("{prefix}load-old\nPRIVATE_TOKEN\n"),
        format!("PRIVATE_TOKEN\n{prefix}load-old\n"),
        format!("{prefix}load-old\n\n"),
        format!("{prefix}Load-old\n"),
        format!("{prefix}load_old\n"),
        format!("{prefix}\n"),
        format!("{prefix}load-old\r\n"),
        format!(" {prefix}load-old\n"),
        format!("{prefix} load-old\n"),
        String::new(),
        "x".repeat(policy::MAX_BROWSER_ERROR),
        "x".repeat(policy::MAX_BROWSER_ERROR + 1),
        format!(
            "{prefix}{}\n",
            "a".repeat(policy::MAX_BROWSER_ERROR - prefix.len() - 1)
        ),
    ]);
    let mut recorded = Recorded::new("browserStage");
    for text in cases {
        let answer = recorded.answer(&json!({"stderr": text}));
        assert!(answer == policy::browser_stage(text.as_bytes()), "{text:?}");
    }
    let answer = recorded.answer(&json!({"stages": stages}));
    assert!(answer == "inventory");
    assert_eq!(recorded.finish(), 54);
}

#[test]
fn a_release_outcome_is_published_only_by_its_fixed_fields() {
    let revision = "f".repeat(40);
    let summary = json!({
        "action": "deploy", "revision": revision, "phase": "startup", "passed": false,
        "failure": "Candidate readiness failed", "rollbackAttempted": true, "rollbackPassed": true,
    });
    let private = "PRIVATE_DATABASE_PASSWORD_AND_BACKUP_CONTENT";
    let mut full = summary.clone();
    full["backupSha256"] = json!("a".repeat(64));
    full["databaseDump"] = json!(private);
    full["environment"] = json!({"K": private});
    let cases = [
        summary,
        full,
        json!({"action": "stage", "revision": revision, "passed": true}),
        json!({}),
        json!({"phase": null, "passed": 1, "failure": ["x"]}),
        json!({"rollbackPassed": false, "extra": 1}),
        json!("missing"),
    ];
    let mut recorded = Recorded::new("outcome");
    for case in cases {
        let answer = recorded.answer(&json!({"outcome": case}));
        let actual = match &case {
            Value::Object(outcome) => policy::sanitized(outcome),
            _ => json!({"attempt": "<attempt>", "outcome": "missing"}),
        };
        assert!(answer == &json!({"accepted": actual}));
        assert!(!actual.to_string().contains(private));
    }
    assert_eq!(recorded.finish(), 7);
}

/// The reference's own key order of its success receipt and browser report.
const RECEIPT: [&str; 10] = [
    "passed",
    "requests",
    "scope",
    "securityHeaders",
    "contentSecurityPolicy",
    "jsonNoStore",
    "anonymousCookiesAbsent",
    "missingAuthorizationRejected",
    "privateEndpointsOmitted",
    "responseLimits",
];
const ROLLOVER: [&str; 9] = [
    "passed",
    "editions",
    "oldTabAfterUpgrade",
    "newTabAfterRollback",
    "backendMissesVerified",
    "documentsUnchanged",
    "captureCalls",
    "htmlUncached",
    "selectedVersionVerified",
];

fn variations(exact: &Value) -> Vec<Value> {
    let mut cases = vec![exact.clone()];
    for key in ROLLOVER {
        let value = &exact[key];
        let mut missing = exact.clone();
        missing.as_object_mut().unwrap().remove(key);
        cases.push(missing);
        let mut changed = |replacement: Value| {
            let mut case = exact.clone();
            case[key] = replacement;
            cases.push(case);
        };
        match value {
            Value::Bool(flag) => {
                changed(json!(1));
                changed(json!(!flag));
            }
            Value::Number(number) => {
                let number = number.as_i64().unwrap();
                changed(json!(number as f64));
                changed(json!(number + 1));
                changed(json!(false));
            }
            _ => {}
        }
    }
    cases
}

#[test]
fn the_response_receipt_is_accepted_only_exactly() {
    let success = policy::receipt();
    let mut cases = vec![success.clone()];
    let mut extra = success.clone();
    extra["private"] = json!("PRIVATE");
    cases.extend([extra, json!({"passed": true})]);
    for key in RECEIPT {
        let value = &success[key];
        let mut missing = success.clone();
        missing.as_object_mut().unwrap().remove(key);
        cases.push(missing);
        let replacements = match value {
            Value::Bool(flag) => vec![json!(1), json!(!flag), json!("true")],
            Value::Number(number) => {
                let number = number.as_i64().unwrap();
                vec![json!(number as f64), json!(number + 1), json!(true)]
            }
            Value::String(text) => vec![json!(format!("{text}x")), Value::Null],
            _ => unreachable!(),
        };
        for replacement in replacements {
            let mut case = success.clone();
            case[key] = replacement;
            cases.push(case);
        }
    }
    cases.extend([json!([]), json!("passed"), Value::Null]);
    let mut recorded = Recorded::new("receipt");
    for case in cases {
        let answer = recorded.answer(&json!({"receipt": case}));
        let actual = if case.is_object() {
            json!(case == policy::receipt())
        } else {
            json!("notAnObject")
        };
        assert!(answer == &actual, "{case}");
    }
    assert_eq!(recorded.finish(), 45);
}

#[test]
fn the_browser_report_is_accepted_only_exactly() {
    let mut cases = variations(&policy::rollover());
    let mut extra = policy::rollover();
    extra["private"] = json!("must not upload");
    cases.extend([extra, json!([]), json!("x")]);
    let mut recorded = Recorded::new("browserReport");
    for case in cases {
        let answer = recorded.answer(&json!({"report": case}));
        match answer.get("accepted") {
            Some(accepted) => assert!(case == policy::rollover() && accepted == &case),
            None => assert!(case != policy::rollover(), "{case}"),
        }
    }
    assert_eq!(recorded.finish(), 34);
}

fn owned(changes: &[(&str, Value)]) -> Value {
    let mut value = json!({
        "id": "a".repeat(64), "image": format!("sha256:{}", "d".repeat(64)), "restarts": 0,
        "state": {"Running": true, "ExitCode": 0, "OOMKilled": false, "Error": "",
            "StartedAt": "2026-09-13T00:00:00Z", "Pid": 42},
        "labels": {plan::LABEL: TOKEN, "com.docker.compose.project": plan::PROJECT,
            "com.docker.compose.service": "simplestchat"},
    });
    for (key, item) in changes {
        if let Some(label) = key.strip_prefix("label:") {
            let labels = value["labels"].as_object_mut().unwrap();
            if item.is_null() {
                labels.remove(label);
            } else {
                labels.insert(label.into(), item.clone());
            }
        } else {
            value[*key] = item.clone();
        }
    }
    value
}

#[test]
fn only_an_exact_identity_with_every_ownership_label_is_owned() {
    let container = "a".repeat(64);
    let mut cases: Vec<(String, Value)> = vec![(container.clone(), owned(&[]))];
    for service in ["postgres", "caddy", "migrate", "turn", "simplestchat "] {
        cases.push((
            container.clone(),
            owned(&[("label:com.docker.compose.service", json!(service))]),
        ));
    }
    cases.extend([
        (container.clone(), owned(&[("id", json!("b".repeat(64)))])),
        (
            container.clone(),
            owned(&[(&format!("label:{}", plan::LABEL), json!("d".repeat(32)))]),
        ),
        (
            container.clone(),
            owned(&[(&format!("label:{}", plan::LABEL), Value::Null)]),
        ),
        (
            container.clone(),
            owned(&[("label:com.docker.compose.project", json!("other"))]),
        ),
        (
            container.clone(),
            owned(&[("label:com.docker.compose.project", Value::Null)]),
        ),
        (container.clone(), owned(&[("labels", json!({}))])),
        (container.clone(), owned(&[("labels", Value::Null)])),
        ("a".repeat(63), owned(&[])),
        ("A".repeat(64), owned(&[("id", json!("A".repeat(64)))])),
        (container.clone(), owned(&[("label:extra", json!("x"))])),
    ]);
    let mut recorded = Recorded::new("ownership");
    for (identity, inspection) in cases {
        let answer = recorded.answer(&json!({"identity": identity, "inspection": inspection}));
        let actual = policy::owned(&identity, &inspection.to_string(), TOKEN);
        match (
            answer.as_str(),
            answer.get("refused").and_then(Value::as_str),
            actual,
        ) {
            (Some("accepted"), _, Ok(_)) => {}
            (_, Some("ValueError"), Err(_)) => {}
            (_, Some(message), Err(fault)) => assert!(fault.to_string() == message),
            _ => panic!("the ownership verdict differs from the recorded one"),
        }
    }
    assert_eq!(recorded.finish(), 16);
}

#[test]
fn only_a_clean_stop_or_the_deliberate_exit_is_a_healthy_exit() {
    let state = json!({"Running": false, "OOMKilled": false, "Error": "", "ExitCode": 0});
    let changes = [
        json!({}),
        json!({"ExitCode": 42}),
        json!({"ExitCode": 1}),
        json!({"Running": true}),
        json!({"OOMKilled": true}),
        json!({"Error": "boom"}),
        json!({"ExitCode": 137}),
        json!({"ExitCode": 42, "OOMKilled": true}),
    ];
    let mut recorded = Recorded::new("healthyExit");
    for deliberate in [false, true] {
        for change in &changes {
            let mut case = state.clone();
            for (key, value) in change.as_object().unwrap() {
                case[key] = value.clone();
            }
            let answer = recorded.answer(&json!({"state": case, "deliberate": deliberate}));
            let actual = policy::healthy_exit(&json!({"state": case}), deliberate);
            match answer.get("refused") {
                Some(message) => {
                    assert!(actual.unwrap_err().to_string() == message.as_str().unwrap())
                }
                None => assert!(actual.is_ok(), "{case}"),
            }
        }
    }
    assert_eq!(recorded.finish(), 16);
}

#[test]
fn a_retained_dependency_is_identified_by_its_id_image_start_and_restarts() {
    let mut recorded = Recorded::new("stable");
    for container in [
        owned(&[]),
        owned(&[("restarts", json!(3))]),
        owned(&[("image", json!(format!("sha256:{}", "e".repeat(64))))]),
    ] {
        let answer = recorded.answer(&json!({"container": container}));
        assert!(answer == &json!(policy::stable(&container).unwrap()));
    }
    assert_eq!(recorded.finish(), 3);
}

#[test]
fn a_failed_command_is_named_by_the_operation_the_reference_named() {
    let revision = "f".repeat(40);
    let path = Path::new("/x");
    // The response check is named for what it is, not for the binary's file name.
    let cases: Vec<(Vec<&str>, &str)> = vec![
        (
            vec!["inspect", "x"],
            plan::inspect(&"a".repeat(64)).operation,
        ),
        (
            vec!["image", "inspect", "x"],
            plan::image_identity("x").operation,
        ),
        (vec!["network", "rm", "x"], "docker network rm"),
        (
            vec!["buildx", "build", "x"],
            plan::build("x", path).operation,
        ),
        (
            vec!["compose", "--project-name", "p", "up"],
            "docker compose",
        ),
        (
            vec!["exec", "--interactive", "x"],
            plan::sql("x", None, None).operation,
        ),
        (vec!["logs", "--tail", "20", "x"], "docker logs"),
    ];
    let mut recorded = Recorded::new("operation");
    for (arguments, name) in cases {
        let argv: Vec<&str> = plan::DOCKER.iter().copied().chain(arguments).collect();
        let answer = recorded.answer(&json!({"argv": argv}));
        assert!(answer["operation"] == json!(name), "{argv:?}");
        assert!(answer["exitStatus"] == json!(3));
    }
    let others: Vec<(Vec<String>, String)> = vec![
        (
            plan::release(Action::Deploy, &revision)
                .argv
                .iter()
                .take(5)
                .map(|w| w.to_string_lossy().into())
                .collect(),
            plan::release(Action::Deploy, &revision).operation.into(),
        ),
        (
            vec![
                plan::OPS.into(),
                "release".into(),
                "assets".into(),
                "initialize".into(),
            ],
            plan::assets(&revision).operation.into(),
        ),
        (
            vec![plan::CURL.into(), "--fail".into()],
            plan::ready(None).operation.into(),
        ),
        (
            vec![
                plan::NSENTER.into(),
                "--net=/proc/self/fd/3".into(),
                "--".into(),
                plan::CURL.into(),
            ],
            plan::ready(Some(3)).operation.into(),
        ),
        (
            vec![plan::OPENSSL.into(), "req".into()],
            plan::certificates(TOKEN, path, path)[0].operation.into(),
        ),
        (
            vec![plan::UPDATE_TRUST.into()],
            plan::update_trust().operation.into(),
        ),
        (
            vec!["/fixture/node".into(), "release-assets.cjs".into()],
            plan::rollover(Path::new("/fixture/node"), path, [path; 3])
                .operation
                .into(),
        ),
        (
            vec!["/fixture/http-policy".into(), "release-http-policy".into()],
            "http-policy".into(),
        ),
        (
            vec![
                plan::GIT.into(),
                "-C".into(),
                "/x".into(),
                "rev-parse".into(),
                "HEAD".into(),
            ],
            plan::revision(path).operation.into(),
        ),
    ];
    for (argv, name) in others {
        let answer = recorded.answer(&json!({"argv": argv}));
        assert!(answer["operation"] == json!(name), "{argv:?}");
    }
    assert!(
        plan::response_policy(path, TOKEN, ["a", "b"], path, "d").operation
            == "release-http-policy"
    );
    assert_eq!(recorded.finish(), 16);
}

#[test]
fn the_derived_releases_are_the_reference_builds() {
    let base_tag = format!("simplestchat-release-test/base:{TOKEN}");
    let mut recorded = Recorded::new("derivative");
    for kind in Kind::ALL {
        let answer =
            recorded.answer(&json!({"kind": kind.name(), "token": TOKEN, "baseTag": base_tag}));
        let revision = images::revision(TOKEN, kind);
        assert!(answer["revision"] == json!(revision));
        assert!(answer["tag"] == json!(images::tag(&revision)));
        let files: BTreeMap<String, String> =
            serde_json::from_value(answer["files"].clone()).unwrap();
        assert!(
            images::context(&base_tag, TOKEN, kind) == files,
            "{}",
            kind.name()
        );
        let directory = Path::new("<directory>");
        let words = |request: super::super::host::Request| -> Vec<String> {
            request.argv[3..]
                .iter()
                .map(|word| word.to_string_lossy().into_owned())
                .collect()
        };
        let mut calls = vec![
            words(plan::image_identity(&base_tag)),
            words(plan::build(&images::tag(&revision), directory)),
            words(plan::image_identity(&base_tag)),
            words(plan::image_identity(&images::tag(&revision))),
        ];
        for (call, options) in calls
            .iter_mut()
            .zip(["{}", "{\"timeout\": 90}", "{}", "{}"])
        {
            call.push(options.into());
        }
        assert!(answer["calls"] == json!(calls));
        assert!(plan::build("x", directory).timeout.as_secs() == 90);
    }
    assert_eq!(recorded.finish(), 3);
}

#[test]
fn every_recorded_migration_keeps_the_digest_the_reference_recorded() {
    let checkout = super::checkout();
    let migrations = images::migrations(&checkout.join("migrations")).unwrap();
    let mut recorded = Recorded::new("migrations");
    let names = &recording()["tests"]["migrations"]["cases"][0]["answer"];
    let mut listed: Vec<String> = std::fs::read_dir(checkout.join("migrations"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".sql"))
        .collect();
    listed.sort();
    let recorded_names = names.as_object().unwrap().len();
    let answer = recorded.answer(&json!({"names": listed[..recorded_names]}));
    for (version, digest) in answer.as_object().unwrap() {
        assert!(
            migrations.get(version).map(String::as_str) == digest.as_str(),
            "{version}"
        );
    }
    assert_eq!(recorded.finish(), 1);
}

fn setup_cases() -> Vec<Value> {
    let good = json!({
        "ps": "", "network": "",
        "help": "Options:\n      --platform string   Export a specific platform\n",
        "api": "1.55", "buildx": "Name: default\nDriver: docker\n",
        "metadata": {"id": format!("sha256:{}", "d".repeat(64)), "os": "linux",
            "architecture": "amd64", "user": "10001:10001", "cmd": ["/app/simplestChat"],
            "entrypoint": null},
    });
    let mut cases = vec![good.clone()];
    for (key, value) in [
        ("ps", "abc"),
        ("network", "def"),
        ("help", "Options:\n  --platforms x\n"),
        ("help", "--platform x\n"),
        ("help", "Options:\n\t--platform\n"),
        ("api", "1.47"),
        ("api", "1.48"),
        ("api", "2.0"),
        ("api", "1.1000"),
        ("api", "1.48.1"),
        ("api", "v1.50"),
        ("api", "10.0"),
        ("buildx", "Driver: docker-container\n"),
        ("buildx", "Driver: docker\nDriver: docker\n"),
        ("buildx", "Name: x\n"),
        ("buildx", "Driver:   docker   \n"),
        ("buildx", " Driver: docker\n"),
    ] {
        let mut case = good.clone();
        case[key] = json!(value);
        cases.push(case);
    }
    for (key, value) in [
        ("os", json!("windows")),
        ("architecture", json!("arm64")),
        ("user", json!("0")),
        ("cmd", json!(["/bin/sh"])),
        ("cmd", json!("/app/simplestChat")),
        ("entrypoint", json!([])),
        ("entrypoint", json!(["/bin/sh"])),
        ("id", json!(format!("sha256:{}", "D".repeat(64)))),
        ("id", json!("abc")),
    ] {
        let mut case = good.clone();
        case["metadata"][key] = value;
        cases.push(case);
    }
    // Each operand of the API check alone: three parts, a signed number.
    for api in ["1.50.", "1.+50"] {
        let mut case = good.clone();
        case["api"] = json!(api);
        cases.push(case);
    }
    // The source revision, once the engine and image were accepted.
    for revision in [
        "f".repeat(40),
        "f".repeat(39),
        "g".repeat(40),
        "F".repeat(40),
    ] {
        let mut case = good.clone();
        case["revision"] = json!(revision);
        cases.push(case);
    }
    cases
}

#[test]
fn the_engine_and_production_image_checks_refuse_what_the_reference_refused() {
    use policy::engine;
    let mut recorded = Recorded::new("setup");
    for case in setup_cases() {
        let answer = recorded.answer(&case);
        if let Some(revision) = case.get("revision").and_then(Value::as_str) {
            // The revision check is the setup step's own: a model run answers
            // every other command as a correct host would.
            let mut run = Run::new();
            run.world().replies = vec![("git", 0, super::reply(0, revision))];
            let passed = run.complete();
            let report = &run.fixture.report;
            match answer.get("refused") {
                Some(message) => assert!(!passed && json!(report.failure) == *message),
                None => assert!(passed && report.source_revision.as_deref() == Some(revision)),
            }
            continue;
        }
        // The reference's offline doubles answered without the trimming of
        // real command output, so each check is given exactly these texts.
        let text = |key: &str| case[key].as_str().unwrap().to_owned();
        let actual = engine::empty_containers(&text("ps"))
            .and_then(|()| engine::empty_networks(&text("network")))
            .and_then(|()| engine::exports_platform(&text("help")))
            .and_then(|()| engine::api(&text("api")))
            .and_then(|()| engine::builder(&text("buildx")))
            .and_then(|()| engine::production(&case["metadata"].to_string()));
        match (answer.get("accepted"), actual) {
            (Some(image), Ok(actual)) => assert!(image == &json!(actual)),
            (None, Err(fault)) => assert!(json!(fault.to_string()) == answer["refused"], "{case}"),
            (expected, actual) => panic!("{expected:?} {actual:?} {case}"),
        }
    }
    assert_eq!(recorded.finish(), 33);
}

fn validator_cases() -> Vec<Value> {
    let name = format!("scpub-release-validate-{TOKEN}");
    let base = json!({
        "id": "a".repeat(64), "name": format!("/{name}"), "image": format!("sha256:{}", "d".repeat(64)),
        "user": "10001:10001", "entrypoint": ["/usr/bin/timeout"], "network": "none", "readOnly": true,
        "state": {"ExitCode": 128, "Status": "created", "OOMKilled": false, "Error": "exec failed"},
    });
    let case = |name: &str, inspection: &Value, code: i32, logs: i32, stderr: &str| json!({"name": name, "inspection": inspection, "code": code, "logs": logs, "stderr": stderr});
    let mut cases = vec![
        case(&name, &base, 0, 0, ""),
        case(&name, &base, 0, 0, "warning: x\n"),
        case(&name, &base, 0, 0, &"y".repeat(5000)),
        case(&name, &base, 0, 1, "x"),
        case(&name, &base, 1, 0, ""),
    ];
    for invalid in [
        String::new(),
        format!("scpub-release-validate-{}", "C".repeat(32)),
        "other".into(),
        format!("{name}0"),
        "x".repeat(129),
    ] {
        cases.push(case(&invalid, &base, 0, 0, ""));
    }
    cases.push(case(&format!("{name}\n"), &base, 0, 0, ""));
    cases.push(case(&format!("  {name}  \n"), &base, 0, 0, ""));
    for (key, value) in [
        ("id", json!("b".repeat(63))),
        ("id", json!(5)),
        ("name", json!("/other")),
        ("image", json!(format!("sha256:{}", "e".repeat(64)))),
        ("user", json!("0:0")),
        ("entrypoint", json!(["/bin/sh"])),
        ("network", json!("bridge")),
        ("readOnly", json!(false)),
        ("readOnly", json!(1)),
    ] {
        let mut inspection = base.clone();
        inspection[key] = value;
        cases.push(case(&name, &inspection, 0, 0, ""));
    }
    for change in [
        json!({"ExitCode": -1}),
        json!({"ExitCode": 256}),
        json!({"ExitCode": "1"}),
        json!({"ExitCode": true}),
        json!({"Status": "unknown"}),
        json!({"Status": "exited"}),
        json!({"Status": "dead"}),
        json!({"OOMKilled": 0}),
        json!({"OOMKilled": true}),
        json!({"Error": null}),
        json!({"Error": "e".repeat(2048)}),
        json!({"Error": "e".repeat(2049)}),
        json!({"Error": "é".repeat(2049)}),
    ] {
        let mut inspection = base.clone();
        for (key, value) in change.as_object().unwrap() {
            inspection["state"][key] = value.clone();
        }
        cases.push(case(&name, &inspection, 0, 0, ""));
    }
    let mut missing = base.clone();
    missing.as_object_mut().unwrap().remove("state");
    cases.push(case(&name, &missing, 0, 0, ""));
    cases.push(case(&name, &json!("not json"), 0, 0, ""));
    // The fixed prefix alone, a status that is no string, a float exit code.
    cases.push(case(&"c".repeat(32), &base, 0, 0, ""));
    for change in [json!({"Status": null}), json!({"ExitCode": 1.0})] {
        let mut inspection = base.clone();
        for (key, value) in change.as_object().unwrap() {
            inspection["state"][key] = value.clone();
        }
        cases.push(case(&name, &inspection, 0, 0, ""));
    }
    cases
}

#[test]
fn a_failed_releases_validator_is_described_only_after_it_is_verified() {
    let image = format!("sha256:{}", "d".repeat(64));
    let mut recorded = Recorded::new("validator");
    for case in validator_cases() {
        let answer = recorded.answer(&case);
        let mut run = Run::new();
        let attempt = run.scratch.0.join("attempt");
        std::fs::create_dir(&attempt).unwrap();
        std::fs::write(
            attempt.join("validation-name.txt"),
            case["name"].as_str().unwrap(),
        )
        .unwrap();
        let inspection = match &case["inspection"] {
            Value::String(text) => text.clone(),
            value => value.to_string(),
        };
        let code = case["code"].as_i64().unwrap();
        let logs = case["logs"].as_i64().unwrap();
        run.world().replies = vec![
            ("docker inspect", 0, json!([code, inspection, ""])),
            ("docker logs", 0, json!([logs, "", case["stderr"]])),
        ];
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(run.fixture.validator(&attempt, &image));
        assert!(run.fixture.report.validator_startup.clone().unwrap() == answer["report"]);
        assert!(json!(run.world().log.len()) == answer["calls"]);
    }
    assert_eq!(recorded.finish(), 39);
}

fn release_cases() -> Vec<Value> {
    let revision = "f".repeat(40);
    let mut cases = Vec::new();
    for failure in [false, true] {
        for code in [0, 1, 2] {
            for passed in [json!(true), json!(false), json!(0), json!(1), Value::Null] {
                cases.push(json!({"action": "deploy", "failure": failure, "code": code,
                    "outcome": {"action": "deploy", "revision": revision, "passed": passed}}));
            }
        }
    }
    let stage = |outcome: Value, attempts: Option<u64>| {
        let mut case = json!({"action": "stage", "failure": false, "code": 0, "outcome": outcome});
        if let Some(attempts) = attempts {
            case["attempts"] = json!(attempts);
        }
        case
    };
    cases.extend([
        stage(
            json!({"action": "deploy", "revision": revision, "passed": true}),
            None,
        ),
        stage(
            json!({"action": "stage", "revision": "e".repeat(40), "passed": true}),
            None,
        ),
        stage(
            json!({"action": "stage", "revision": revision, "passed": true, "phase": "done"}),
            None,
        ),
        stage(
            json!({"action": "stage", "revision": revision, "passed": true}),
            Some(0),
        ),
        stage(
            json!({"action": "stage", "revision": revision, "passed": true}),
            Some(2),
        ),
    ]);
    cases
}

#[test]
fn a_release_passes_only_when_its_status_and_its_own_outcome_agree() {
    let revision = "f".repeat(40);
    let mut recorded = Recorded::new("release");
    for case in release_cases() {
        let answer = recorded.answer(&case);
        let mut run = Run::new();
        let results = run.fixture.paths.root.join("results");
        std::fs::create_dir_all(&results).unwrap();
        let derived = images::Derived {
            revision: revision.clone(),
            tag: images::tag(&revision),
            image: format!("sha256:{}", "e".repeat(64)),
        };
        run.fixture.insert_derived(Kind::Candidate, derived);
        let attempts = case.get("attempts").and_then(Value::as_u64).unwrap_or(1);
        run.world().prepared = Some((
            case["code"].as_i64().unwrap() as i32,
            attempts,
            case["outcome"].clone(),
        ));
        let action = if case["action"] == "stage" {
            Action::Stage
        } else {
            Action::Deploy
        };
        let failure = case["failure"].as_bool().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let verdict = runtime.block_on(run.fixture.release(action, Kind::Candidate, failure));
        match (answer["verdict"].get("accepted"), verdict) {
            (Some(outcome), Ok((_, actual))) => assert!(outcome == &Value::Object(actual)),
            (None, Err(fault)) => assert!(
                json!(fault.to_string()) == answer["verdict"]["refused"],
                "{case}"
            ),
            (expected, actual) => panic!("{expected:?} {actual:?} {case}"),
        }
        assert!(
            answer["lastRelease"] == json!(run.fixture.report.last_release),
            "{case}"
        );
        let argv: Vec<String> = plan::release(action, &revision)
            .argv
            .iter()
            .map(|w| w.to_string_lossy().into())
            .collect();
        let mut expected = argv.clone();
        expected.push("{\"success\": false, \"timeout\": 240}".into());
        assert!(answer["argv"] == json!(expected));
        let _ = Map::<String, Value>::new();
    }
    assert_eq!(recorded.finish(), 35);
}
