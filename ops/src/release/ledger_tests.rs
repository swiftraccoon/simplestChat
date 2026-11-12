use super::*;
use crate::host::state::tests::Scratch;
use serde_json::Value;

fn hex(text: &str) -> Vec<u8> {
    let byte = |pair: &[u8]| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap();
    text.as_bytes().chunks(2).map(byte).collect()
}

/// Twenty-nine recorded runs of the launcher's inline comparison, on the
/// recorded source files and ledger text: each matches or is refused as
/// recorded, in the script's own words where it had any. Four end
/// differently on purpose: a version the migration tool never issues, or a
/// spelling the database never prints, is no ledger here.
#[test]
fn every_recorded_comparison_is_replayed() {
    let recorded: Value = serde_json::from_str(include_str!("recorded_ledger.json")).unwrap();
    let cases = recorded["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 29);
    let revision = recorded["revision"].as_str().unwrap();
    let stricter = recorded["stricter"].as_array().unwrap();
    let (mut matched, mut refused, mut tightened) = (0, 0, 0);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let scratch = Scratch::new();
        if let Some(files) = case["source"].as_object() {
            let directory = scratch.path().join(revision).join("migrations");
            std::fs::create_dir_all(&directory).unwrap();
            for (file, text) in files {
                std::fs::write(directory.join(file), text.as_str().unwrap()).unwrap();
            }
        }
        let ledger = match case["hex"].as_str() {
            Some(spelled) => hex(spelled),
            None => case["ledger"].as_str().unwrap().as_bytes().to_vec(),
        };
        let judged = judge(scratch.path(), revision, &ledger);
        if stricter.iter().any(|stricter| stricter == name) {
            assert_eq!(case["status"], 0, "{name}");
            assert_eq!(judged, Err(UNMET), "{name}");
            tightened += 1;
            continue;
        }
        let said = case["stderr"].as_str().unwrap();
        let expected = match (case["status"].as_i64().unwrap(), said) {
            (0, _) => Ok(()),
            (_, DIFFERS) => Err(DIFFERS),
            // The script's own refusal, or the exception it died of.
            _ => Err(UNMET),
        };
        assert_eq!(judged, expected, "{name}");
        match judged {
            Ok(()) => {
                assert_eq!(case["stdout"], MATCHES, "{name}");
                matched += 1;
            }
            Err(_) => refused += 1,
        }
    }
    assert_eq!((matched, refused, tightened), (8, 17, 4));
}

/// The command line names one full revision and one ledger, in that order.
#[test]
fn the_command_line_names_a_revision_and_a_ledger() {
    let words = |line: &[&str]| -> Vec<OsString> { line.iter().map(OsString::from).collect() };
    let revision = "a".repeat(40);
    let line = words(&["--revision", &revision, "--ledger", "/results/ledger.txt"]);
    assert_eq!(
        parse(&line),
        Some((revision.as_str(), PathBuf::from("/results/ledger.txt")))
    );
    let upper = "A".repeat(40);
    for refused in [
        &[][..],
        &["--revision", &revision],
        &["--ledger", "/l", "--revision", &revision],
        &["--revision", &revision, "--ledger"],
        &["--revision", &revision, "--ledger", "/l", "extra"],
        &["--revision", "main", "--ledger", "/l"],
        &["--revision", &upper, "--ledger", "/l"],
        &["--revision", &revision[..39], "--ledger", "/l"],
        &["--revision", "../../../../etc", "--ledger", "/l"],
        &["--rev", &revision, "--ledger", "/l"],
        &["--revision", &revision, "--file", "/l"],
    ] {
        assert_eq!(parse(&words(refused)), None, "{refused:?}");
    }
}

/// A row is `version t checksum`: the largest version the migration tool
/// can issue is one, and nothing beyond it is.
#[test]
fn a_version_is_a_positive_signed_integer() {
    assert_eq!(rows(b"9223372036854775807 t x\n").unwrap().len(), 1);
    assert_eq!(rows(b"9223372036854775808 t x\n"), Err(UNMET));
    assert_eq!(rows(b"0 t x\n"), Err(UNMET));
    assert_eq!(rows(b""), Ok(BTreeMap::new()));
}
