use super::*;
use serde_json::Value;

/// A recorded case's bytes: text behind any padding, or one character
/// repeated.
fn input(entry: &Value) -> Vec<u8> {
    let number = |name: &str| usize::try_from(entry[name].as_u64().unwrap_or_default()).unwrap();
    if let Some(repeated) = entry["repeat"].as_str() {
        return repeated.repeat(number("count")).into_bytes();
    }
    let mut bytes = vec![b' '; number("pad")];
    bytes.extend_from_slice(entry["text"].as_str().unwrap().as_bytes());
    bytes
}

/// Two hundred and two recorded runs of the Python validator: every
/// document gets the recorded line, byte for byte, and the recorded status.
/// Three ways of asking that the validator answered are usage errors here:
/// the command line is exactly one of the two the launcher uses.
#[test]
fn every_recorded_judgement_is_replayed() {
    let recorded: Value = serde_json::from_str(include_str!("recorded_profile.json")).unwrap();
    let cases = recorded["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 202);
    let (mut held, mut refused, mut usage, mut misuse) = (0, 0, 0, 0);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let words = case["arguments"].as_array().unwrap().iter();
        let words: Vec<OsString> = words.map(|word| word.as_str().unwrap().into()).collect();
        let status = case["status"].as_i64().unwrap();
        let Some(kind) = parse(&words) else {
            match name.ends_with("is a misuse") {
                true => misuse += 1,
                false => {
                    assert_eq!(status, 2, "{name}");
                    usage += 1;
                }
            }
            continue;
        };
        assert!(!name.ends_with("is a misuse"), "{name}");
        let (said, holds) = line(verdict(&kind, &input(&case["input"])));
        assert_eq!(said, case["stdout"].as_str().unwrap(), "{name}");
        assert_eq!(i64::from(!holds), status, "{name}");
        match holds {
            true => held += 1,
            false => refused += 1,
        }
    }
    assert_eq!((held, refused, usage, misuse), (33, 161, 5, 3));
}

/// Five hundred and seventy-five judgements of the Python validator called
/// in process, as the release engine and the reboot helper called it: every
/// object document of the command-line recording, for the application alone
/// and with the migration service, at the fixed socket directory and at
/// another. Each is refused for the recorded reason: a fixed code, a field
/// that is not there or a value of the wrong shape.
#[test]
fn every_recorded_in_process_judgement_is_replayed() {
    let documents: Value = serde_json::from_str(include_str!("recorded_profile.json")).unwrap();
    let documents = documents["cases"].as_array().unwrap();
    let recorded = include_str!("recorded_profile_inprocess.json");
    let recorded: Value = serde_json::from_str(recorded).unwrap();
    let judged = recorded["judged"].as_array().unwrap();
    assert_eq!(judged.len(), 575);
    let (mut held, mut coded, mut absent, mut shaped) = (0, 0, 0, 0);
    for item in judged {
        let name = item["case"].as_str().unwrap();
        let mut named = documents.iter().filter(|case| case["name"] == name);
        let case = named.next().unwrap_or_else(|| panic!("{name}"));
        assert!(named.next().is_none(), "{name}");
        let document = json::decode_limited(&input(&case["input"]), MAX_CONFIGURATION).unwrap();
        let answer = match item["kind"].as_str().unwrap() {
            "image" => image(&document),
            _ => {
                let sockets = item["socket"].as_str().unwrap();
                let selected = item["image"].as_str().unwrap();
                let migration = item["migration"].as_bool().unwrap();
                compose(&document, sockets, selected, migration)
            }
        };
        let expected = match item["raised"].as_str() {
            None => Ok(()),
            Some("RuntimeProfileError") => {
                let code = item["code"].as_str().unwrap();
                // The answer's own code, when it is the recorded one.
                Err(match answer {
                    Err(Refusal::Code(said)) if said == code => Refusal::Code(said),
                    _ => Refusal::Code("another code"),
                })
            }
            Some("KeyError") => Err(Refusal::Key),
            Some("ValueError") => Err(Refusal::Value),
            Some(other) => panic!("{name}: {other}"),
        };
        assert_eq!(answer, expected, "{name}: {item}");
        match answer {
            Ok(()) => held += 1,
            Err(Refusal::Code(_)) => coded += 1,
            Err(Refusal::Key) => absent += 1,
            Err(Refusal::Value) => shaped += 1,
        }
    }
    assert_eq!((held, coded, absent, shaped), (66, 451, 22, 36));
}

/// The fields a service may carry are the reviewed ones, each named once.
#[test]
fn the_reviewed_service_fields_are_distinct() {
    let mut names = SERVICE_FIELDS.to_vec();
    names.dedup();
    assert_eq!(names.len(), 27);
    assert!(names.is_sorted());
    for hook in [
        "post_start",
        "pre_stop",
        "privileged",
        "devices",
        "cap_add",
        "pid",
    ] {
        assert!(!SERVICE_FIELDS.contains(&hook), "{hook}");
    }
}

/// Only the two command lines of the launcher are command lines.
#[test]
fn the_command_line_is_one_of_two() {
    let words = |line: &[&str]| -> Vec<OsString> { line.iter().map(OsString::from).collect() };
    let selected = format!("sha256:{}", "a".repeat(64));
    assert_eq!(parse(&words(&["image"])), Some(Kind::Image));
    let named = Some(Kind::Compose(selected.clone()));
    assert_eq!(parse(&words(&["compose", "--image-id", &selected])), named);
    assert_eq!(
        parse(&words(&["compose", &format!("--image-id={selected}")])),
        named
    );
    // Whatever is named is judged with the document, not here.
    assert_eq!(
        parse(&words(&["compose", "--image-id="])),
        Some(Kind::Compose(String::new()))
    );
    for refused in [
        &[][..],
        &["compose"],
        &["compose", "--image-id"],
        &["compose", &selected],
        &["compose", "--image-id", &selected, "extra"],
        &["compose", "--image", &selected],
        &["--image-id", &selected, "compose"],
        &["image", "--image-id", &selected],
        &["image", "image"],
        &["container"],
        &["--help"],
    ] {
        assert_eq!(parse(&words(refused)), None, "{refused:?}");
    }
    let unspeakable = std::os::unix::ffi::OsStringExt::from_vec(vec![0xff]);
    assert_eq!(
        parse(&["compose".into(), "--image-id".into(), unspeakable]),
        None
    );
}
