//! Replays of the recorded installer and direct tests of what it has no record of.
//!
//! `recorded_tools.json` holds what `build/security_tools.py` answered for lock
//! documents, for archives built with its interpreter's own writers, and for
//! installations against scripted transport answers, with the files each step
//! left behind. Where the interpreter failed with one of its own exception
//! names, `named` gives the code this installer has for that failure.

use super::{
    Answer, Fault, Limits, Site, Transport, archive, cli, digest, files, install,
    lock::{Asset, Format, Lock},
    path, platform, transport,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};

const RECORDED: &str = include_str!("recorded_tools.json");
const SCANNER: &str = "https://github.com/example/scanner/releases/download/v1.2.3/scanner.tar.gz";

/// Addresses the recorded installer admitted and this one refuses: its
/// transport is handed the address as it is written.
const PLAIN_ADDRESSES_ONLY: [&str; 14] = [
    "a scheme in capitals",
    "a host in capitals",
    "an address with an empty port",
    "an address with a space",
    "an address with a tab",
    "an address with braces",
    "an address with brackets",
    "an address with a letter outside ASCII",
    "an address with a comma",
    "an address with an opening bracket",
    "an address with an opening brace",
    "an address with a colon in its path",
    "an address with an exclamation mark",
    "a wheel address with a space",
];

/// Archives the recorded installer read up to their damage and accepted.
const WHOLE_ARCHIVES_ONLY: [&str; 2] = [
    "a tar cut inside its second header",
    "a tar whose second header is damaged",
];

fn recorded() -> Value {
    serde_json::from_str(RECORDED).expect("the recording is JSON")
}

fn named(recorded: &str) -> &str {
    match recorded {
        "JSONDecodeError" => "invalid_tool_json",
        "FileNotFoundError" => "tool_entry_missing",
        "OSError" => "tool_entry_unavailable",
        "IsADirectoryError" => "nonregular_tool_file",
        "HTTPError" => "tool_download_status",
        "URLError" => "tool_download_transport",
        "BadGzipFile" | "EOFError" | "LZMAError" | "NotImplementedError" => {
            "tool_archive_compression"
        }
        "BadZipFile" | "ReadError" => "tool_archive_format",
        code => code,
    }
}

/// The code of a refusal, or nothing for an answer.
fn code<T>(judged: &super::Result<T>) -> Option<&'static str> {
    judged.as_ref().err().map(|fault| fault.0)
}

fn text(value: &Value) -> &str {
    value.as_str().expect("a recorded string")
}

fn base64(text: &str) -> Vec<u8> {
    let (mut bytes, mut bits, mut held) = (Vec::new(), 0_u32, 0_u32);
    for letter in text.bytes().filter(|letter| *letter != b'=') {
        let value = match letter {
            b'A'..=b'Z' => letter - b'A',
            b'a'..=b'z' => letter - b'a' + 26,
            b'0'..=b'9' => letter - b'0' + 52,
            b'+' => 62,
            _ => 63,
        };
        bits = (bits << 6) | u32::from(value);
        held += 6;
        if held >= 8 {
            held -= 8;
            bytes.push(u8::try_from((bits >> held) & 0xff).expect("one byte"));
        }
    }
    bytes
}

/// A directory of one test's own, removed with it.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "simplestchat-security-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("scratch directory");
        Self(path.canonicalize().expect("scratch path"))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn every_recorded_lock_is_judged_alike() {
    let scratch = Scratch::new("locks");
    let path = scratch.0.join("lock.json");
    let (mut held, mut refused, mut stricter) = (0, 0, 0);
    let recording = recorded();
    for case in recording["locks"].as_array().expect("locks") {
        let name = text(&case["name"]);
        let mut document = text(&case["text"]).to_owned();
        if let Some(count) = case.get("files").and_then(Value::as_u64) {
            let mut lock: Value = serde_json::from_str(&document).expect("base lock");
            let files: serde_json::Map<String, Value> = (0..count)
                .map(|index| (format!("m{index}"), json!(format!("d{index}"))))
                .collect();
            lock["tools"]["scanner"]["platforms"]["linux-x86_64"]["files"] = files.into();
            document = lock.to_string();
        }
        let pad = case.get("pad").and_then(Value::as_u64).unwrap_or(0);
        document.push_str(&" ".repeat(usize::try_from(pad).expect("pad")));
        std::fs::write(&path, &document).expect("lock");
        let judged = Lock::load(&path);
        let result = &case["result"];
        if PLAIN_ADDRESSES_ONLY.contains(&name) {
            assert!(result.get("sha256").is_some(), "{name}");
            assert_eq!(code(&judged), Some("invalid_tool_url"), "{name}");
            stricter += 1;
        } else if let Some(expected) = result.get("code") {
            assert_eq!(code(&judged), Some(named(text(expected))), "{name}");
            refused += 1;
        } else {
            let lock = judged.unwrap_or_else(|fault| panic!("{name}: {fault}"));
            // A document this test wrote out itself has no recorded digest.
            if let Some(recorded) = result.get("sha256") {
                assert_eq!(lock.digest, text(recorded), "{name}");
            }
            let tools: Vec<Value> = lock.tools.keys().map(|tool| json!(tool)).collect();
            assert_eq!(json!(tools), result["tools"], "{name}");
            held += 1;
        }
    }
    assert_eq!((held, refused, stricter), (21, 100, 14));
}

fn asset(case: &Value) -> Asset {
    Asset {
        url: SCANNER.to_owned(),
        sha256: text(&case["sha256"]).to_owned(),
        format: match text(&case["format"]) {
            "tar.gz" => Format::TarGz,
            "tar.xz" => Format::TarXz,
            "zip" => Format::Zip,
            _ => Format::Binary,
        },
        files: case["files"]
            .as_object()
            .expect("files")
            .iter()
            .map(|(member, destination)| (member.clone(), text(destination).to_owned()))
            .collect(),
    }
}

fn limits(given: &Value) -> Limits {
    let bound =
        |name: &str, reviewed: u64| given.get(name).and_then(Value::as_u64).unwrap_or(reviewed);
    let reviewed = Limits::REVIEWED;
    Limits {
        archive: bound("archive", reviewed.archive),
        executable: bound("executable", reviewed.executable),
        members: usize::try_from(bound("members", reviewed.members as u64)).expect("members"),
        unpacked: bound("unpacked", reviewed.unpacked),
    }
}

#[test]
fn every_recorded_archive_is_judged_alike() {
    let (mut held, mut refused, mut stricter) = (0, 0, 0);
    let recording = recorded();
    for case in recording["archives"].as_array().expect("archives") {
        let name = text(&case["name"]);
        let bytes = base64(text(&case["archive"]));
        let judged = archive::selected(&asset(case), &bytes, &limits(&case["limits"]));
        let result = &case["result"];
        if WHOLE_ARCHIVES_ONLY.contains(&name) {
            assert!(result.get("files").is_some(), "{name}");
            assert_eq!(code(&judged), Some("tool_archive_format"), "{name}");
            stricter += 1;
        } else if let Some(expected) = result.get("code") {
            assert_eq!(code(&judged), Some(named(text(expected))), "{name}");
            refused += 1;
        } else {
            let found: Vec<Value> = judged
                .unwrap_or_else(|fault| panic!("{name}: {fault}"))
                .iter()
                .map(|(destination, content)| {
                    json!({
                        "name": destination,
                        "sha256": digest(content),
                        "size": content.len(),
                    })
                })
                .collect();
            assert_eq!(json!(found), result["files"], "{name}");
            held += 1;
        }
    }
    assert_eq!((held, refused, stricter), (42, 68, 2));
}

#[test]
fn every_recorded_host_names_its_platform() {
    let recording = recorded();
    let hosts = recording["hosts"].as_array().expect("hosts");
    for host in hosts {
        let judged = platform(text(&host["system"]), text(&host["machine"]));
        let expected = &host["result"];
        match expected.get("platform") {
            Some(name) => assert_eq!(judged, Ok(text(name)), "{host}"),
            None => assert_eq!(code(&judged), expected["code"].as_str(), "{host}"),
        }
    }
    assert_eq!(hosts.len(), 10);
}

/// The recorded answers of one step, in order, and what was asked of them.
struct Scripted {
    bodies: BTreeMap<String, Vec<u8>>,
    answers: BTreeMap<String, VecDeque<Value>>,
    clock: Duration,
    requests: Vec<String>,
    pauses: Vec<f64>,
    allowed: Vec<(u64, Duration)>,
    /// Where a directory an answer says `appears` is created meanwhile.
    base: PathBuf,
}

impl Scripted {
    fn new(bodies: &Value, answers: Option<&Value>) -> Self {
        let table = |value: &Value| value.as_object().expect("a table").clone();
        Self {
            bodies: table(bodies)
                .iter()
                .map(|(address, body)| (address.clone(), base64(text(body))))
                .collect(),
            answers: answers
                .map(table)
                .unwrap_or_default()
                .iter()
                .map(|(address, list)| {
                    let list = list.as_array().expect("answers").iter().cloned().collect();
                    (address.clone(), list)
                })
                .collect(),
            clock: Duration::from_secs(1000),
            requests: Vec::new(),
            pauses: Vec::new(),
            allowed: Vec::new(),
            base: PathBuf::new(),
        }
    }
}

impl Transport for Scripted {
    fn now(&mut self) -> Duration {
        self.clock
    }

    fn pause(&mut self, time: Duration) {
        self.pauses.push(time.as_secs_f64());
        self.clock += time;
    }

    fn fetch(&mut self, address: &str, limit: u64, within: Duration) -> Answer {
        self.requests.push(address.to_owned());
        self.allowed.push((limit, within));
        let answer = self
            .answers
            .get_mut(address)
            .and_then(VecDeque::pop_front)
            .unwrap_or_else(|| json!({"kind": "body"}));
        let took = answer.get("seconds").and_then(Value::as_f64).unwrap_or(0.0);
        self.clock += Duration::from_secs_f64(took);
        if let Some(name) = answer.get("appears") {
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(self.base.join(text(name)))
                .expect("the directory that appears");
        }
        match text(&answer["kind"]) {
            "status" => Answer::Status(
                u16::try_from(answer["status"].as_u64().expect("status")).expect("status"),
            ),
            "error" => Answer::Failed,
            "late" => Answer::Late,
            "oversized" => Answer::Oversized,
            _ => Answer::Body {
                bytes: answer
                    .get("bytes")
                    .map_or_else(|| self.bodies[address].clone(), |bytes| base64(text(bytes))),
                address: answer.get("address").map_or(address, text).to_owned(),
            },
        }
    }
}

fn octal(mode: u32) -> String {
    format!("0o{:o}", mode & 0o7777)
}

/// Everything below `base` as the recorder listed it; a link is not followed.
fn listing(base: &Path) -> BTreeMap<String, Value> {
    fn walk(base: &Path, directory: &Path, found: &mut BTreeMap<String, Value>) {
        for entry in std::fs::read_dir(directory).expect("directory") {
            let path = entry.expect("entry").path();
            let metadata = path.symlink_metadata().expect("metadata");
            let name = path.strip_prefix(base).expect("below the base");
            let name = name.to_str().expect("a UTF-8 name").to_owned();
            if metadata.is_symlink() {
                let target = std::fs::read_link(&path).expect("link");
                found.insert(name, json!({"link": target.to_str().expect("target")}));
            } else if metadata.is_dir() {
                found.insert(name, json!({"directory": octal(metadata.mode())}));
                walk(base, &path, found);
            } else {
                let data = std::fs::read(&path).expect("file");
                let mut entry = json!({
                    "mode": octal(metadata.mode()),
                    "sha256": digest(&data),
                    "size": data.len(),
                });
                let lock = path.file_name().is_some_and(|name| name == "lock.json");
                if (lock || data.len() < 4096)
                    && let Ok(text) = String::from_utf8(data)
                {
                    entry["text"] = json!(text);
                }
                found.insert(name, entry);
            }
        }
    }
    let mut found = BTreeMap::new();
    walk(base, base, &mut found);
    found
}

fn remove(path: &Path) {
    match path.symlink_metadata() {
        Ok(metadata) if metadata.is_dir() => std::fs::remove_dir_all(path).expect("removed"),
        Ok(_) => std::fs::remove_file(path).expect("removed"),
        Err(_) => {}
    }
}

fn mode(text: &str) -> std::fs::Permissions {
    let bits = u32::from_str_radix(text.trim_start_matches("0o"), 8).expect("a mode");
    std::fs::Permissions::from_mode(bits)
}

/// Bring the files to the state the recorder's own alteration left.
fn alter(base: &Path, step: &Value) {
    for path in step["removed"].as_array().expect("removed").iter().rev() {
        remove(&base.join(text(path)));
    }
    for (name, entry) in step["changed"].as_object().expect("changed") {
        let path = base.join(name);
        if let Some(target) = entry.get("link") {
            remove(&path);
            std::os::unix::fs::symlink(text(target), &path).expect("link");
        } else if let Some(permissions) = entry.get("directory") {
            if !path
                .symlink_metadata()
                .is_ok_and(|metadata| metadata.is_dir())
            {
                remove(&path);
                std::fs::create_dir(&path).expect("directory");
            }
            std::fs::set_permissions(&path, mode(text(permissions))).expect("mode");
        } else {
            if let Some(content) = entry.get("text") {
                remove(&path);
                std::fs::write(&path, text(content)).expect("file");
            }
            std::fs::set_permissions(&path, mode(text(&entry["mode"]))).expect("mode");
        }
    }
}

/// What changed below `base` since `previous`, in the recorder's two lists.
fn changes(base: &Path, previous: &mut BTreeMap<String, Value>) -> (Value, Value) {
    let now = listing(base);
    let changed: serde_json::Map<String, Value> = now
        .iter()
        .filter(|(name, entry)| previous.get(*name) != Some(*entry))
        .map(|(name, entry)| (name.clone(), entry.clone()))
        .collect();
    let removed: Vec<&String> = previous
        .keys()
        .filter(|name| !now.contains_key(*name))
        .collect();
    let answer = (changed.into(), json!(removed));
    *previous = now;
    answer
}

/// The recorded changes with a parent directory's mode, which the creating
/// process's mask decides, left out of the comparison.
fn alike(recorded: &Value, mut observed: Value) -> bool {
    for (name, entry) in recorded.as_object().expect("changed") {
        if entry["directory"] == "parent" && observed[name].get("directory").is_some() {
            observed[name] = entry.clone();
        }
    }
    *recorded == observed
}

struct Replayed {
    scenarios: usize,
    installed: usize,
    found: usize,
    refused: usize,
    altered: usize,
    requests: usize,
    pauses: usize,
}

fn replayed() -> Replayed {
    let recording = recorded();
    let mut tally = Replayed {
        scenarios: 0,
        installed: 0,
        found: 0,
        refused: 0,
        altered: 0,
        requests: 0,
        pauses: 0,
    };
    for scenario in recording["installs"].as_array().expect("installs") {
        let name = text(&scenario["name"]);
        let scratch = Scratch::new("installs");
        let base = &scratch.0;
        std::fs::create_dir(base.join("build")).expect("build");
        let lock = base.join("build/lock.json");
        std::fs::write(&lock, text(&recording["lock"])).expect("lock");
        let cache = base.join(".cache/security-tools");
        let mut previous = listing(base);
        for step in scenario["steps"].as_array().expect("steps") {
            let what = format!("{name}: {step}");
            if step["do"] == "alter" {
                alter(base, step);
                let (changed, removed) = changes(base, &mut previous);
                assert!(
                    changed == step["changed"] && removed == step["removed"],
                    "{what}"
                );
                tally.altered += 1;
                continue;
            }
            let site = Site {
                lock: &lock,
                cache: &cache,
                platform: step.get("platform").map_or("linux-x86_64", text),
                limits: limits(step.get("limits").unwrap_or(&Value::Null)),
            };
            let directory = step["directory"]
                .as_str()
                .map(|directory| base.join(directory));
            let mut transport = Scripted::new(&recording["bodies"], step.get("answers"));
            transport.base.clone_from(base);
            let judged = if step["do"] == "install" {
                let tools: Vec<String> = step["tools"]
                    .as_array()
                    .expect("tools")
                    .iter()
                    .map(|tool| text(tool).to_owned())
                    .collect();
                install(&site, &tools, directory.as_deref(), &mut transport)
            } else {
                path(&site, text(&step["name"]), directory.as_deref())
            };
            match (judged, step["result"].get("path")) {
                (Ok(answer), Some(expected)) => {
                    assert_eq!(answer, base.join(text(expected)), "{what}");
                    if step["do"] == "install" {
                        tally.installed += 1;
                    } else {
                        tally.found += 1;
                    }
                }
                (Err(fault), None) => {
                    assert_eq!(fault.0, named(text(&step["result"]["code"])), "{what}");
                    tally.refused += 1;
                }
                (judged, _) => panic!("{what}: {judged:?}"),
            }
            assert_eq!(json!(transport.requests), step["requests"], "{what}");
            assert_eq!(json!(transport.pauses), step["sleeps"], "{what}");
            assert!(
                transport
                    .allowed
                    .iter()
                    .all(|(limit, _)| *limit == site.limits.archive),
                "{what}"
            );
            let (changed, removed) = changes(base, &mut previous);
            assert!(
                alike(&step["changed"], changed.clone()) && removed == step["removed"],
                "{what}\n{changed:#}"
            );
            tally.requests += transport.requests.len();
            tally.pauses += transport.pauses.len();
        }
        tally.scenarios += 1;
    }
    tally
}

#[test]
fn every_recorded_installation_is_replayed() {
    let tally = replayed();
    assert_eq!(
        (
            tally.scenarios,
            tally.installed,
            tally.found,
            tally.refused,
            tally.altered,
            tally.requests,
            tally.pauses
        ),
        (81, 62, 11, 74, 41, 100, 11)
    );
}

fn scanner(recording: &Value) -> Asset {
    Asset {
        url: SCANNER.to_owned(),
        sha256: digest(&base64(text(&recording["bodies"][SCANNER]))),
        format: Format::TarGz,
        files: BTreeMap::new(),
    }
}

#[test]
fn every_attempt_is_given_what_is_left_of_three_minutes() {
    let recording = recorded();
    let answers = json!({SCANNER: [
        {"kind": "status", "status": 503, "seconds": 1.5},
        {"kind": "status", "status": 502},
    ]});
    let mut transport = Scripted::new(&recording["bodies"], Some(&answers));
    let limits = Limits {
        archive: 4096,
        ..Limits::REVIEWED
    };
    let body = transport::download(&scanner(&recording), &limits, &mut transport);
    assert_eq!(body.ok(), Some(base64(text(&recording["bodies"][SCANNER]))));
    let seconds = |seconds: f64| Duration::from_secs_f64(seconds);
    assert_eq!(
        transport.allowed,
        [
            (4096, seconds(180.0)),
            (4096, seconds(176.5)),
            (4096, seconds(172.5))
        ]
    );
    assert_eq!(transport.pauses, [2.0, 4.0]);
}

#[test]
fn a_late_or_oversized_transfer_is_final_under_its_own_code() {
    let recording = recorded();
    for (kind, expected) in [
        ("late", "tool_download_timeout"),
        ("oversized", "tool_download_size"),
    ] {
        let answers = json!({SCANNER: [{"kind": kind}]});
        let mut transport = Scripted::new(&recording["bodies"], Some(&answers));
        let judged = transport::download(&scanner(&recording), &Limits::REVIEWED, &mut transport);
        assert_eq!(code(&judged), Some(expected));
        assert_eq!(transport.requests.len(), 1);
    }
}

#[test]
fn the_transfer_is_curl_held_to_https_and_the_callers_bounds() {
    let arguments = transport::arguments(SCANNER, 4096, Duration::from_millis(176_500));
    assert_eq!(
        arguments,
        [
            "--disable",
            "--silent",
            "--globoff",
            "--location",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--tlsv1.2",
            "--max-redirs",
            "5",
            "--connect-timeout",
            "30",
            "--speed-limit",
            "1",
            "--speed-time",
            "30",
            "--output",
            "-",
            "--write-out",
            "%{stderr}%{http_code} %{url_effective}",
            "--max-time",
            "176",
            "--max-filesize",
            "4096",
            "--url",
            SCANNER,
        ]
    );
    // A transfer is never told it has no time at all.
    let brief = transport::arguments(SCANNER, 1, Duration::from_millis(300));
    assert_eq!(brief[21..25], ["--max-time", "1", "--max-filesize", "1"]);
}

#[test]
fn a_finished_transfer_is_read_from_its_status_and_its_one_line() {
    let judged = |status: Option<i32>, stopped: bool, report: &[u8]| {
        transport::judge(status, stopped, b"body".to_vec(), 4, report)
    };
    let body = |address: &str| Answer::Body {
        bytes: b"body".to_vec(),
        address: address.to_owned(),
    };
    let served = "https://objects.example/scanner?signature=a b";
    assert_eq!(
        judged(Some(0), false, format!("200 {served}").as_bytes()),
        body(served)
    );
    assert_eq!(judged(Some(0), false, b"299 https://a"), body("https://a"));
    assert_eq!(
        judged(Some(0), false, b"300 https://a"),
        Answer::Status(300)
    );
    assert_eq!(
        judged(Some(0), false, b"199 https://a"),
        Answer::Status(199)
    );
    assert_eq!(
        judged(Some(0), false, b"404 https://a"),
        Answer::Status(404)
    );
    assert_eq!(
        judged(Some(0), false, b"503 https://a"),
        Answer::Status(503)
    );
    assert_eq!(
        judged(Some(0), false, b"599 https://a"),
        Answer::Status(599)
    );
    for report in [
        &b"000 https://a"[..],
        b"099 https://a",
        b"600 https://a",
        b"200",
        b"",
        b"ok https://a",
        b"200 \xff",
    ] {
        assert_eq!(judged(Some(0), false, report), Answer::Failed, "{report:?}");
    }
    // A response line does not make up for how the transfer ended.
    for status in [Some(1), Some(7), Some(35), Some(56), None] {
        assert_eq!(
            judged(status, false, b"200 https://a"),
            Answer::Failed,
            "{status:?}"
        );
    }
    assert_eq!(judged(Some(28), false, b"000 https://a"), Answer::Late);
    assert_eq!(judged(Some(0), true, b"200 https://a"), Answer::Late);
    assert_eq!(judged(None, true, b""), Answer::Late);
    assert_eq!(judged(Some(63), false, b"200 https://a"), Answer::Oversized);
    let over =
        |status, stopped| transport::judge(status, stopped, b"bytes".to_vec(), 4, b"200 https://a");
    assert_eq!(over(Some(0), false), Answer::Oversized);
    assert_eq!(over(None, true), Answer::Oversized);
}

fn words(line: &str) -> Vec<String> {
    line.split_whitespace().map(str::to_owned).collect()
}

#[test]
fn a_command_line_is_one_of_the_two_forms() {
    let request =
        |name: Option<&str>, tools: &[&str], directory: Option<&str>, platform: Option<&str>| {
            Some(cli::Request {
                name: name.map(str::to_owned),
                tools: tools.iter().map(|tool| (*tool).to_owned()).collect(),
                directory: directory.map(PathBuf::from),
                platform: platform.map(str::to_owned),
            })
        };
    let parsed = |line: &str| cli::parse(&words(line));
    assert_eq!(parsed("install"), request(None, &[], None, None));
    assert_eq!(
        parsed("install --tools gh"),
        request(None, &["gh"], None, None)
    );
    assert_eq!(
        parsed("install --tools gh syft --directory out --platform linux-x86_64"),
        request(None, &["gh", "syft"], Some("out"), Some("linux-x86_64"))
    );
    assert_eq!(
        parsed("install --platform darwin-arm64 --directory /opt/tools --tools gh"),
        request(None, &["gh"], Some("/opt/tools"), Some("darwin-arm64"))
    );
    assert_eq!(parsed("path gh"), request(Some("gh"), &[], None, None));
    assert_eq!(
        parsed("path gh --directory out --platform linux-x86_64"),
        request(Some("gh"), &[], Some("out"), Some("linux-x86_64"))
    );
    assert_eq!(
        parsed("path --directory out gh"),
        request(Some("gh"), &[], Some("out"), None)
    );
    for refused in [
        "",
        "remove",
        "install gh",
        "install --tools",
        "install --tools --directory out",
        "install --tools gh --tools syft",
        "install --directory",
        "install --directory out other",
        "install --directory out --directory other",
        "install --platform",
        "install --platform one --platform two",
        "install --unknown value",
        "path",
        "path --directory out",
        "path gh syft",
        "path --tools gh",
        "path gh --tools syft",
        "path gh --directory",
        "path gh --platform",
    ] {
        assert_eq!(parsed(refused), None, "{refused}");
    }
}

#[test]
fn a_command_reads_the_checkouts_lock_and_installs_below_its_cache() {
    let recording = recorded();
    let scratch = Scratch::new("command");
    let base = &scratch.0;
    std::fs::create_dir(base.join("build")).expect("build");
    let lock = text(&recording["lock"]);
    std::fs::write(base.join("build/security-tools.lock.json"), lock).expect("lock");
    let mut transport = Scripted::new(&recording["bodies"], None);
    let mut run = |line: &str| {
        let request = cli::parse(&words(line)).expect("a command");
        cli::execute(base, &request, &mut transport)
    };
    let cache = base
        .join(".cache/security-tools")
        .join(digest(lock.as_bytes()));
    assert_eq!(
        run("install --tools scanner --platform linux-x86_64"),
        Ok(cache.join("linux-x86_64"))
    );
    assert_eq!(
        run("path scanner --platform linux-x86_64"),
        Ok(cache.join("linux-x86_64/bin/scanner"))
    );
    assert_eq!(
        code(&run("path scanner --platform darwin-arm64")),
        Some("tool_entry_missing")
    );
    let own = base.join("own");
    let own = own.to_str().expect("a UTF-8 path");
    assert_eq!(
        run(&format!(
            "install --tools single --directory {own} --platform linux-x86_64"
        )),
        Ok(PathBuf::from(own))
    );
    assert_eq!(
        run(&format!(
            "path single --directory {own} --platform linux-x86_64"
        )),
        Ok(Path::new(own).join("bin/single"))
    );
    // A command that names no platform is for the host it runs on. The
    // recorded lock has the scanner for two of the four.
    let name = rustix::system::uname();
    let host = platform(
        name.sysname().to_str().expect("system"),
        name.machine().to_str().expect("machine"),
    );
    match host {
        Ok(host @ ("linux-x86_64" | "darwin-arm64")) => {
            assert_eq!(run("install --tools scanner"), Ok(cache.join(host)));
        }
        _ => assert_eq!(
            code(&run("install --tools scanner")),
            Some("unsupported_tool_platform")
        ),
    }
    // The scanner for linux-x86_64 is already there; only a darwin-arm64
    // host fetches it again, for its own platform.
    assert_eq!(
        transport.requests.len(),
        2 + usize::from(host == Ok("darwin-arm64"))
    );
}

#[test]
fn the_committed_lock_is_admitted_whole() {
    let lock = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../build/security-tools.lock.json");
    let lock = Lock::load(&lock).expect("the committed lock");
    let tools: Vec<&str> = lock.tools.keys().map(String::as_str).collect();
    assert_eq!(
        tools,
        [
            "actionlint",
            "cargo-audit",
            "cargo-auditable",
            "cargo-deny",
            "cargo-mutants",
            "gh",
            "gitleaks",
            "grype",
            "semgrep-core",
            "squawk",
            "syft",
            "zizmor",
        ]
    );
    assert!(
        super::FAST_TOOLS
            .iter()
            .all(|tool| lock.tools.contains_key(*tool))
    );
}

#[test]
fn the_reviewed_bounds_are_the_recorded_installers() {
    let reviewed = Limits::REVIEWED;
    assert_eq!(reviewed.archive, 128 * 1024 * 1024);
    assert_eq!(reviewed.executable, 256 * 1024 * 1024);
    assert_eq!(reviewed.members, 10_000);
    assert_eq!(reviewed.unpacked, 512 * 1024 * 1024);
}

/// A program that answers as curl would for the last word of its address,
/// after writing down its arguments and its environment.
const PROGRAM: &str = r#"#!/bin/sh
printf '%s\n' "$@" > "$0.arguments"
/usr/bin/env > "$0.environment"
if [ /dev/stdin -ef /dev/null ]; then printf 'nothing' > "$0.input"; fi
case "$*" in
*/body) printf 'abc'; printf '200 https://objects.example/served' >&2 ;;
*/missing) printf 'no such release'; printf '404 https://github.com/missing' >&2 ;;
*/large) printf 'abcdefghijklmnopqrstuvwxyz'; printf '200 https://github.com/large' >&2
  exec /bin/sleep 30 ;;
*/slow) exit 28 ;;
*/stuck) exec /bin/sleep 30 ;;
*/refused) printf '000 https://github.com/refused' >&2; exit 7 ;;
esac
"#;

#[test]
fn a_transfer_is_one_bounded_run_of_the_program_without_the_callers_environment() {
    use std::os::unix::fs::OpenOptionsExt as _;
    let scratch = Scratch::new("program");
    let program = scratch.0.join("curl");
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o700)
        .open(&program)
        .and_then(|mut file| std::io::Write::write_all(&mut file, PROGRAM.as_bytes()))
        .expect("program");
    let grace = Duration::from_millis(300);
    let mut curl = transport::Curl::running(program.clone(), grace);
    let long = Duration::from_secs(20);
    let started = std::time::Instant::now();

    let answer = curl.fetch("https://github.com/body", 3, long);
    assert_eq!(
        answer,
        Answer::Body {
            bytes: b"abc".to_vec(),
            address: "https://objects.example/served".to_owned(),
        }
    );
    let noted = |suffix: &str| {
        let mut path = program.clone().into_os_string();
        path.push(suffix);
        std::fs::read_to_string(path).expect("the program's note")
    };
    let mut expected = transport::arguments("https://github.com/body", 3, long).join("\n");
    expected.push('\n');
    assert_eq!(noted(".arguments"), expected);
    assert!(std::env::vars().any(|(name, _)| name.starts_with("CARGO_")));
    assert_eq!(noted(".input"), "nothing");
    let environment = noted(".environment");
    assert!(
        !environment.contains("CARGO_") && !environment.contains("HOME="),
        "{environment}"
    );

    assert_eq!(
        curl.fetch("https://github.com/missing", 64, long),
        Answer::Status(404)
    );
    // One byte over the bound ends the transfer: the rest is never read and
    // the program is not waited for.
    assert_eq!(
        curl.fetch("https://github.com/large", 25, long),
        Answer::Oversized
    );
    assert_eq!(
        curl.fetch("https://github.com/slow", 64, long),
        Answer::Late
    );
    assert_eq!(
        curl.fetch("https://github.com/refused", 64, long),
        Answer::Failed
    );
    // A program that outlives its own limit and the grace is stopped.
    assert_eq!(
        curl.fetch("https://github.com/stuck", 64, Duration::from_millis(200)),
        Answer::Late
    );
    assert!(started.elapsed() < Duration::from_secs(15));
    assert!(curl.now() >= started.elapsed().saturating_sub(Duration::from_secs(1)));

    let reviewed = transport::Curl::default();
    assert_eq!(reviewed.program, Path::new("/usr/bin/curl"));
    assert_eq!(reviewed.grace, Duration::from_secs(10));
    let mut absent = transport::Curl::running(scratch.0.join("absent"), grace);
    assert_eq!(
        absent.fetch("https://github.com/body", 3, long),
        Answer::Failed
    );
}

#[test]
fn a_private_file_is_only_ever_created() {
    let scratch = Scratch::new("private");
    let path = scratch.0.join("file");
    assert_eq!(files::write_private(&path, b"first", 0o640), Ok(()));
    let metadata = path.metadata().expect("written");
    assert_eq!(metadata.mode() & 0o7777, 0o640 & !umask(&scratch.0));
    assert_eq!(
        files::write_private(&path, b"second", 0o600),
        Err(Fault("tool_entry_unavailable"))
    );
    assert_eq!(std::fs::read(&path).expect("kept"), b"first");
    // A link is never written through, even to a path that does not exist.
    let link = scratch.0.join("link");
    std::os::unix::fs::symlink("elsewhere", &link).expect("link");
    assert_eq!(
        files::write_private(&link, b"through", 0o600),
        Err(Fault("tool_entry_unavailable"))
    );
    assert!(!scratch.0.join("elsewhere").exists());
    assert_eq!(
        files::write_private(&scratch.0.join("absent/file"), b"", 0o600),
        Err(Fault("tool_entry_missing"))
    );
}

/// The mask this process creates files under, read from a file it creates.
fn umask(directory: &Path) -> u32 {
    use std::os::unix::fs::OpenOptionsExt as _;
    let probe = directory.join("mask-probe");
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o777)
        .open(&probe)
        .expect("probe");
    let mask = !probe.metadata().expect("probe").mode() & 0o777;
    std::fs::remove_file(probe).expect("probe removed");
    mask
}

#[test]
fn what_another_account_owns_is_not_private() {
    // The system's own files belong to its administrator. As that account
    // itself there is nothing here to tell apart.
    if rustix::process::getuid().is_root() {
        return;
    }
    assert_eq!(
        files::private_directory(Path::new("/")),
        Err(Fault("unprotected_tool_directory"))
    );
    assert_eq!(
        files::bounded(Path::new("/etc/hosts"), 1024 * 1024, true),
        Err(Fault("unprotected_tool_file"))
    );
    assert!(files::bounded(Path::new("/etc/hosts"), 1024 * 1024, false).is_ok());
}
