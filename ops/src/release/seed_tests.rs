//! Every recorded run of the earlier seeding helper, replayed against the
//! same private files and the same scripted backend.

use super::*;
use crate::host::state::tests::Scratch;
use serde_json::{Map, Value, json};
use simplestchat_lobby_seed::testing::{Backend, RECORDED, actions, concrete, stricter};
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::time::Instant;

fn recorded() -> Value {
    serde_json::from_str(RECORDED).unwrap()
}

fn index(value: &Value) -> usize {
    usize::try_from(value.as_u64().unwrap()).unwrap()
}

fn limits() -> Limits {
    Limits {
        request: Duration::from_millis(600),
        farewell: Duration::from_secs(5),
        until: Instant::now() + Duration::from_secs(30),
    }
}

/// The bytes a recorded entry stands for.
fn content(entry: &Map<String, Value>) -> Vec<u8> {
    if let Some(repeat) = entry.get("repeat") {
        return repeat[0]
            .as_str()
            .unwrap()
            .as_bytes()
            .repeat(index(&repeat[1]));
    }
    if let Some(hexadecimal) = entry.get("hex") {
        let digits = hexadecimal.as_str().unwrap().as_bytes();
        let byte = |pair: &[u8]| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16);
        return digits.chunks(2).map(|pair| byte(pair).unwrap()).collect();
    }
    concrete(entry["text"].as_str().unwrap()).into_bytes()
}

/// Make a recorded configuration directory's entries. A name that ends in
/// `.again` is a second name of the file it repeats.
fn build(directory: &Path, tree: &Map<String, Value>) {
    for (name, entry) in tree {
        // A pending record left by an earlier run has some random name.
        let actual = name.replace("<name>", &"0".repeat(24));
        let (path, entry) = (directory.join(actual), entry.as_object().unwrap());
        if let Some(target) = entry.get("link") {
            std::os::unix::fs::symlink(target.as_str().unwrap(), &path).unwrap();
        } else if let Some(mode) = entry.get("directory") {
            std::fs::create_dir(&path).unwrap();
            let mode = std::fs::Permissions::from_mode(u32::try_from(index(mode)).unwrap());
            std::fs::set_permissions(&path, mode).unwrap();
        } else if let Some(first) = name.strip_suffix(".again") {
            std::fs::hard_link(directory.join(first), &path).unwrap();
        } else {
            std::fs::write(&path, content(entry)).unwrap();
            let mode = u32::try_from(index(&entry["mode"])).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        }
    }
}

/// A directory's entries as a recording lists them: what each is, its mode,
/// how many names it has and its bytes. A pending record's random name is
/// made stable.
fn listed(directory: &Path) -> BTreeMap<String, (String, u32, u64, Vec<u8>)> {
    let mut entries = BTreeMap::new();
    for entry in std::fs::read_dir(directory).unwrap() {
        let entry = entry.unwrap();
        let mut name = entry.file_name().into_string().unwrap();
        if let Some(random) = name.strip_prefix("owner.pending.") {
            assert!(crate::host::hexadecimal(random, 24), "{name}");
            name = "owner.pending.<name>".to_owned();
        }
        let information = std::fs::symlink_metadata(entry.path()).unwrap();
        let listing = if information.is_symlink() {
            let target = std::fs::read_link(entry.path()).unwrap();
            (
                "link".to_owned(),
                0,
                0,
                target.into_os_string().into_encoded_bytes(),
            )
        } else if information.is_dir() {
            (
                "directory".to_owned(),
                information.mode() & 0o7777,
                0,
                Vec::new(),
            )
        } else {
            let (mode, names) = (information.mode() & 0o7777, information.nlink());
            (
                "file".to_owned(),
                mode,
                names,
                std::fs::read(entry.path()).unwrap(),
            )
        };
        entries.insert(name, listing);
    }
    entries
}

/// A recorded tree in the shape `listed` gives.
fn expected(tree: &Map<String, Value>) -> BTreeMap<String, (String, u32, u64, Vec<u8>)> {
    let entry = |(name, entry): (&String, &Value)| {
        let entry = entry.as_object().unwrap();
        let listing = if let Some(target) = entry.get("link") {
            (
                "link".to_owned(),
                0,
                0,
                target.as_str().unwrap().as_bytes().to_vec(),
            )
        } else if let Some(mode) = entry.get("directory") {
            (
                "directory".to_owned(),
                u32::try_from(index(mode)).unwrap(),
                0,
                Vec::new(),
            )
        } else {
            let mode = u32::try_from(index(&entry["mode"])).unwrap();
            (
                "file".to_owned(),
                mode,
                entry["links"].as_u64().unwrap(),
                content(entry),
            )
        };
        (name.clone(), listing)
    };
    tree.iter().map(entry).collect()
}

/// All 230 recorded runs of the earlier helper, each against the recorded
/// configuration directory and a backend that gives the recorded answers.
/// Every run prints the recorded report and leaves the recorded files: the
/// owner's record created once with its recorded bytes, private and under
/// one name, or untouched. Two runs with arguments are syntax errors here,
/// and fourteen end as the stricter client ends them.
#[test]
fn every_recorded_seeding_is_replayed_with_its_files_and_its_backend() {
    let recorded = recorded();
    let scenarios = recorded["scenarios"].as_array().unwrap();
    assert_eq!(scenarios.len(), 230);
    let uid = rustix::process::geteuid().as_raw();
    let (mut usage, mut tightened, mut passed, mut created) = (0, 0, 0, 0);
    for scenario in scenarios {
        let name = scenario["name"].as_str().unwrap();
        let words = scenario["arguments"].as_array().unwrap();
        if !words.is_empty() {
            // The helper reported these as a failed run.
            let words: Vec<OsString> = words
                .iter()
                .map(|word| word.as_str().unwrap().into())
                .collect();
            assert_eq!(run(&words), ExitCode::from(2), "{name}");
            usage += 1;
            continue;
        }
        let scratch = Scratch::new();
        let (config, real) = (scratch.path().join("etc"), scratch.path().join("real"));
        let before = scenario["before"]
            .as_u64()
            .map(|_| index(&scenario["before"]));
        let held = match (&scenario["missing"], before) {
            (Value::String(_), _) => {
                // The usual directory, reached only through a link.
                let usual = index(&scenarios[0]["before"]);
                std::fs::create_dir(&real).unwrap();
                build(&real, recorded["trees"][usual].as_object().unwrap());
                std::os::unix::fs::symlink(&real, &config).unwrap();
                real.clone()
            }
            (_, Some(tree)) => {
                std::fs::create_dir(&config).unwrap();
                build(&config, recorded["trees"][tree].as_object().unwrap());
                let mode = u32::try_from(index(&scenario["mode"])).unwrap();
                std::fs::set_permissions(&config, std::fs::Permissions::from_mode(mode)).unwrap();
                config.clone()
            }
            (_, None) => config.clone(),
        };
        // Another seeding holds the lock for as long as this one runs.
        let holder = (scenario["busy"] == true).then(|| {
            let lock = File::open(held.join("seed.lock")).unwrap();
            flock(&lock, FlockOperation::NonBlockingLockExclusive).unwrap();
            lock
        });
        let script = actions(&recorded, scenario);
        let backend = (scenario["backend"] == "scripted").then(|| Backend::start(script.clone()));
        let address = backend
            .as_ref()
            .map_or_else(Backend::absent, |backend| backend.address);
        // A directory of another owner: this caller's files are not root's.
        let owner = uid + u32::from(scenario["foreign"] == true);
        let privileged = scenario["euid"] == 0;
        let (line, succeeded) = execute(&config, address, owner, privileged, &limits());
        drop(holder);
        let (received, unused) = backend.map_or((Vec::new(), 0), Backend::finish);
        let mut report = recorded["reports"][index(&scenario["report"])].clone();
        let mut made = script.len();
        if let Some(outcome) = stricter(name) {
            tightened += 1;
            report["passed"] = json!(false);
            report["failure"] = json!(outcome.failure);
            report["refreshSessionRevoked"] = json!(outcome.revoked);
            report["existingSettingsPreserved"] = json!(outcome.preserved);
            made = outcome.requests;
        }
        report["ownerCredentialFile"] = json!(format!("{}/owner.json", config.display()));
        let printed: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(printed, report, "{name}");
        assert!(
            line.ends_with('\n') && line.matches('\n').count() == 1,
            "{name}"
        );
        assert_eq!(succeeded, report["passed"] == true, "{name}");
        assert_eq!(
            (received.len(), unused),
            (made, script.len() - made),
            "{name}"
        );
        // A directory behind a link is left exactly as it was.
        let after = match scenario["missing"].is_string() {
            true => Some(index(&scenarios[0]["before"])),
            false => scenario["after"]
                .as_u64()
                .map(|_| index(&scenario["after"])),
        };
        match after {
            Some(tree) => {
                let tree = recorded["trees"][tree].as_object().unwrap();
                // Contents are compared without being shown: a record holds
                // the owner's credentials.
                assert!(
                    listed(&held) == expected(tree),
                    "{name}: the directory differs"
                );
            }
            None => assert!(!held.exists(), "{name}"),
        }
        passed += usize::from(succeeded);
        created += usize::from(printed["ownerFileCreated"] == true);
    }
    assert_eq!((usage, tightened, passed, created), (2, 14, 35, 3));
}

/// A hundred and two environments and secrets give the origin and the owner the
/// earlier helper read from them, or are refused for its reason: one origin
/// line among lines ended as that helper's language ends them, a literal
/// HTTPS origin of a named host with an optional port, and a printable
/// password of 32 to 128 bytes. One differs: an escaped half of a
/// character pair is not JSON here, where the helper failed encoding it.
#[test]
fn an_origin_and_an_owner_are_read_from_configuration_as_recorded() {
    let recorded = recorded();
    let vectors = recorded["vectors"]["configuration"].as_array().unwrap();
    assert_eq!(vectors.len(), 102);
    let (mut read, mut refused, mut differing) = (0, 0, 0);
    for (position, vector) in vectors.iter().enumerate() {
        let environment = vector["environment"].as_str().unwrap();
        let secrets = concrete(vector["secrets"].as_str().unwrap());
        let result =
            parse(secrets.as_bytes()).and_then(|secrets| configuration(environment, &secrets));
        let outcome = &vector["result"];
        let expected = if let Some(origin) = outcome["origin"].as_str() {
            read += 1;
            let owner = Owner {
                email: outcome["email"].as_str().unwrap().to_owned(),
                password: concrete(outcome["password"].as_str().unwrap()),
            };
            Ok((origin.to_owned(), owner))
        } else if let Some(code) = outcome["refused"].as_str() {
            refused += 1;
            Err(code)
        } else {
            differing += 1;
            assert_eq!(outcome["raised"], "UnicodeEncodeError");
            Err("invalid_json")
        };
        // Never shown: a result holds the configured password.
        assert!(result == expected, "configuration {position} differs");
    }
    assert_eq!((read, refused, differing), (36, 65, 1));
}

/// A retained record is this owner's exactly where the earlier helper said
/// so: both fields and no other, in any order.
#[test]
fn a_retained_record_is_this_owner_only_as_recorded() {
    let recorded = recorded();
    let vectors = recorded["vectors"]["sameOwner"].as_array().unwrap();
    assert_eq!(vectors.len(), 17);
    let owner = Owner {
        email: "owner@chat.example.test".to_owned(),
        password: concrete("<password A>"),
    };
    let mut same = 0;
    for (position, vector) in vectors.iter().enumerate() {
        let retained = parse(concrete(vector["value"].as_str().unwrap()).as_bytes()).unwrap();
        let judged = same_owner(&retained, &owner);
        assert!(judged == vector["same"], "record {position} differs");
        same += usize::from(judged);
    }
    assert_eq!(same, 2);
    assert!(alike("", "") && !alike("a", "") && !alike("", "a") && !alike("ab", "ac"));
}

/// The report is one line of ten fields in a fixed order, and a file is
/// private only to the owner the caller names.
#[test]
fn the_report_is_one_fixed_line_and_a_private_file_has_one_owner() {
    let scratch = Scratch::new();
    let config = scratch.path().join("etc");
    let (line, passed) = execute(&config, Backend::absent(), 0, false, &limits());
    let expected = format!(
        concat!(
            r#"{{"schemaVersion":1,"passed":false,"changed":false,"ownerCreated":false,"#,
            r#""roomCreated":false,"ownerFileCreated":false,"existingSettingsPreserved":false,"#,
            r#""refreshSessionRevoked":null,"failure":"root_required","#,
            r#""ownerCredentialFile":"{}/owner.json"}}"#,
            "\n"
        ),
        config.display()
    );
    assert_eq!((line, passed), (expected, false));
    assert!(!config.exists());
    // The backend's private listener, and the time before the sign-out.
    assert_eq!(
        (BACKEND.to_string().as_str(), WHOLE),
        ("127.0.0.1:3000", Duration::from_secs(55))
    );
    std::fs::create_dir(&config).unwrap();
    let path = config.join("app.env");
    std::fs::write(&path, "ALLOWED_ORIGINS=https://chat.example.test\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let uid = rustix::process::geteuid().as_raw();
    let directory = File::open(&config).unwrap();
    let mine = read(directory.as_fd(), "app.env", uid);
    assert_eq!(mine.map(|text| text.map(|text| text.len())), Ok(Some(42)));
    assert_eq!(read(directory.as_fd(), "app.env", uid + 1), Err(UNSAFE));
    assert_eq!(read(directory.as_fd(), "absent", uid), Ok(None));
    // Exactly the owner may read and write it, and it is a regular file: a
    // pipe of that name is refused at once instead of being waited on.
    for mode in [0o604, 0o640, 0o700, 0o400, 0o601] {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        assert_eq!(
            read(directory.as_fd(), "app.env", uid),
            Err(UNSAFE),
            "{mode:o}"
        );
    }
    let pipe = config.join("secrets.json");
    let made = std::process::Command::new("mkfifo")
        .arg("-m")
        .arg("600")
        .arg(&pipe)
        .status();
    assert!(made.unwrap().success());
    assert_eq!(read(directory.as_fd(), "secrets.json", uid), Err(UNSAFE));
}

/// A link named like the owner's record is refused before anything is
/// written: the directory is not even touched by a pending record.
#[test]
fn a_link_named_like_the_record_is_refused_before_anything_is_written() {
    let scratch = Scratch::new();
    let config = scratch.path().join("etc");
    std::fs::create_dir(&config).unwrap();
    std::os::unix::fs::symlink("absent", config.join("owner.json")).unwrap();
    // Any later change of the directory would carry a later time.
    std::thread::sleep(Duration::from_millis(50));
    let changed = || std::fs::metadata(&config).unwrap().modified().unwrap();
    let before = changed();
    let directory = File::open(&config).unwrap();
    let owner = Owner {
        email: "owner@chat.example.test".to_owned(),
        password: concrete("<password A>"),
    };
    let uid = rustix::process::geteuid().as_raw();
    assert_eq!(retain(directory.as_fd(), uid, &owner), Err(FAILED));
    assert_eq!(changed(), before);
    assert_eq!(std::fs::read_dir(&config).unwrap().count(), 1);
    // Without the link the record is retained, which does touch it.
    std::fs::remove_file(config.join("owner.json")).unwrap();
    std::thread::sleep(Duration::from_millis(50));
    let before = changed();
    assert_eq!(retain(directory.as_fd(), uid, &owner), Ok(true));
    assert!(changed() > before);
    assert_eq!(retain(directory.as_fd(), uid, &owner), Ok(false));
}
