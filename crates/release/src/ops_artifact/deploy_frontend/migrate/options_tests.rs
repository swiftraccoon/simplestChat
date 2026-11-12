//! Pure boundary tests; no path is opened and no child or service is started.

use super::*;

const SOURCE: &str = "/private/nonexistent-migration-input/source.json";
const TARGET: &str = "/private/nonexistent-migration-input/target.json";

fn base() -> Vec<String> {
    [
        "--source-inventory",
        SOURCE,
        "--source-origin",
        "https://chat.example.test",
        "--inventory",
        TARGET,
        "--origin",
        "https://next.chat.example.test",
        "--repository",
        REPOSITORY,
    ]
    .map(str::to_owned)
    .to_vec()
}

fn with(extra: &[&str]) -> Vec<String> {
    let mut arguments = base();
    arguments.extend(extra.iter().map(|word| (*word).to_owned()));
    arguments
}

fn replaced(flag: &str, value: &str) -> Vec<String> {
    let mut arguments = base();
    let at = arguments.iter().position(|argument| argument == flag);
    arguments[at.unwrap() + 1] = value.into();
    arguments
}

fn refused(arguments: &[String]) {
    let error = parse(arguments).unwrap_err();
    assert_eq!(
        (error.code, error.message),
        (2, failure(2).message),
        "{arguments:?}"
    );
}

#[test]
fn both_sides_are_explicit_and_everything_else_has_a_default() {
    let options = parse(&base()).unwrap().unwrap();
    assert_eq!(options.source.inventory, std::path::Path::new(SOURCE));
    assert_eq!(options.target.inventory, std::path::Path::new(TARGET));
    assert_eq!(options.source.origin, "https://chat.example.test");
    assert_eq!(options.source.origin_host, "chat.example.test");
    assert_eq!(options.target.origin, "https://next.chat.example.test");
    assert_eq!(options.target.origin_host, "next.chat.example.test");
    assert_eq!(options.repository, REPOSITORY);
    assert_eq!(
        (options.wait_seconds, options.dns_wait_seconds),
        (3600, 600)
    );
    assert!(options.source.limit.is_none() && options.target.limit.is_none());
    assert!(options.revision.is_none() && !options.update_canary);
    assert!(options.ansible_playbook.is_none() && options.gh.is_none());
    assert!(options.activate_inventory.is_none());
    assert!(options.application_artifact_dir.is_none() && options.ops_artifact_zip.is_none());
    assert!(!options.keeps_hostname());
    assert_eq!(format!("{options:?}"), "SignedMigrationOptions");
    assert!(parse(&["--help".to_owned()]).unwrap().is_none());
    assert!(USAGE.starts_with("usage: simplestchat-release migrate --source-inventory ABS"));
    for flag in VALUE_FLAGS.iter().chain([&LAST_VALUE_FLAG, &UPDATE_CANARY]) {
        assert!(USAGE.contains(flag), "{flag}");
    }
}

#[test]
fn every_option_is_admitted_once_in_any_order() {
    let revision = "b".repeat(40);
    let options = parse(&with(&[
        "--update-canary",
        "--source-limit",
        "old_vps",
        "--limit",
        "new-vps.1",
        "--wait-seconds",
        "0",
        "--revision",
        &revision,
        "--dns-wait-seconds",
        "1800",
        "--ansible-playbook",
        "/private/controller tools/ansible-playbook",
        "--gh",
        "/private/controller tools/gh",
        "--activate-inventory",
        SOURCE,
        "--application-artifact-dir",
        "/private/application-candidate",
        "--ops-artifact-zip",
        "/private/ops-candidate/artifact.zip",
    ]))
    .unwrap()
    .unwrap();
    assert_eq!(options.source.limit.as_deref(), Some("old_vps"));
    assert_eq!(options.target.limit.as_deref(), Some("new-vps.1"));
    assert_eq!((options.wait_seconds, options.dns_wait_seconds), (0, 1800));
    assert_eq!(options.revision.as_deref(), Some(revision.as_str()));
    assert!(options.update_canary);
    assert_eq!(
        options.activate_inventory.as_deref(),
        Some(std::path::Path::new(SOURCE))
    );
    assert!(options.gh.is_some() && options.ansible_playbook.is_some());
    assert!(options.application_artifact_dir.is_some() && options.ops_artifact_zip.is_some());
    // A kept hostname is the same origin on both sides, however it is spelled.
    let kept = parse(&replaced("--origin", "https://chat.example.test/"))
        .unwrap()
        .unwrap();
    assert!(kept.keeps_hostname());
    assert_eq!(kept.target.origin, "https://chat.example.test");
}

/// The six command lines the Python controller refused before any work, and
/// the spellings this parser has no meaning for.
#[test]
fn malformed_ambiguous_and_unsupported_command_lines_are_refused() {
    refused(&with(&["--revision", "main"]));
    refused(&with(&["--dns-wait-seconds", "1801"]));
    refused(&with(&["--force"]));
    refused(&with(&["--artifact-dir", "/private/fixture/artifact"]));
    refused(&replaced("--source-origin", "http://chat.example.test"));
    let mut missing = base();
    missing.drain(4..6);
    refused(&missing);
    for (flag, value) in [
        ("--revision", "B".repeat(40)),
        ("--revision", "b".repeat(39)),
        ("--revision", "b".repeat(41)),
        ("--wait-seconds", "7201".to_owned()),
        ("--wait-seconds", "01".to_owned()),
        ("--dns-wait-seconds", "-1".to_owned()),
        ("--source-limit", "*".to_owned()),
        ("--limit", "all:!old".to_owned()),
        ("--activate-inventory", "relative/inventory.yml".to_owned()),
        ("--gh", "/private/../gh".to_owned()),
    ] {
        refused(&with(&[flag, &value]));
    }
    // No argument is longer than a path can be or carries a control character.
    let long = format!("/{}", "a".repeat(4096));
    for value in [
        long.as_str(),
        "/private/tools\n/gh",
        "/private/tools\u{7f}/gh",
    ] {
        refused(&with(&["--gh", value]));
    }
    refused(&replaced("--repository", "owner/repo"));
    refused(&replaced("--inventory", "target.json"));
    refused(&replaced("--origin", "https://next.chat.example.test:8443"));
    refused(&replaced("--origin", "https://Next.chat.example.test"));
    refused(&with(&["--update-canary", "--update-canary"]));
    refused(&with(&["--limit", "a", "--limit", "b"]));
    refused(&with(&["--update-canary=true"]));
    refused(&with(&["--limit"]));
    refused(&with(&["extra"]));
    refused(&[]);
}
