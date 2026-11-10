//! Pure boundary tests; no path is opened and no child or service is started.

use super::*;

fn base() -> Vec<String> {
    [
        "--inventory",
        "/private/nonexistent-deploy-input/inventory.json",
        "--repository",
        REPOSITORY,
        "--origin",
        "https://chat.example.test",
    ]
    .map(str::to_owned)
    .to_vec()
}

fn supplied(flag: &str, value: &str) -> Vec<String> {
    let mut arguments = base();
    if let Some(at) = arguments.iter().position(|argument| argument == flag) {
        arguments[at + 1] = value.into();
    } else {
        arguments.extend([flag.into(), value.into()]);
    }
    arguments
}

fn admitted(arguments: &[String]) -> Options {
    parse(arguments).unwrap().unwrap()
}

fn refused(arguments: &[String]) {
    let error = parse(arguments).unwrap_err();
    let expected = failure(2);
    assert_eq!(error.code, 2);
    assert_eq!(error.message, expected.message);
}

#[test]
fn required_inputs_have_explicit_defaults_without_filesystem_admission() {
    let options = admitted(&base());
    assert_eq!(
        options.inventory,
        Path::new("/private/nonexistent-deploy-input/inventory.json")
    );
    assert_eq!(options.repository, REPOSITORY);
    assert_eq!(options.origin, "https://chat.example.test");
    assert_eq!(options.origin_host, "chat.example.test");
    assert_eq!(options.wait_seconds, 3600);
    assert_eq!(options.quiet_seconds, 600);
    assert!(!options.install_helpers);
    assert!(options.limit.is_none());
    assert!(options.ansible_playbook.is_none());
    assert!(options.gh.is_none());
    assert!(options.application_artifact_dir.is_none());
    assert!(options.ops_artifact_zip.is_none());
}

#[test]
fn every_explicit_tool_and_candidate_role_stays_distinct() {
    let mut arguments = base();
    arguments.extend(
        [
            "--limit",
            "public_vps",
            "--wait-seconds",
            "7200",
            "--quiet-seconds",
            "0",
            "--install-helpers",
            "--ansible-playbook",
            "/private/controller tools/ansible-playbook",
            "--gh",
            "/private/controller tools/gh",
            "--application-artifact-dir",
            "/private/application-candidate",
            "--ops-artifact-zip",
            "/private/ops-candidate/artifact.zip",
        ]
        .map(str::to_owned),
    );
    assert_eq!(arguments.len(), 21);
    let options = admitted(&arguments);
    assert_eq!(options.limit.as_deref(), Some("public_vps"));
    assert_eq!(options.wait_seconds, 7200);
    assert_eq!(options.quiet_seconds, 0);
    assert!(options.install_helpers);
    assert_eq!(
        options.ansible_playbook.as_deref(),
        Some(Path::new("/private/controller tools/ansible-playbook"))
    );
    assert_eq!(
        options.gh.as_deref(),
        Some(Path::new("/private/controller tools/gh"))
    );
    assert_eq!(
        options.application_artifact_dir.as_deref(),
        Some(Path::new("/private/application-candidate"))
    );
    assert_eq!(
        options.ops_artifact_zip.as_deref(),
        Some(Path::new("/private/ops-candidate/artifact.zip"))
    );
}

#[test]
fn wait_and_quiet_bounds_preserve_zero_without_coercing_other_spellings() {
    for (flag, maximum) in [("--wait-seconds", 7200), ("--quiet-seconds", 600)] {
        for number in [0, 1, maximum] {
            let options = admitted(&supplied(flag, &number.to_string()));
            let selected = if flag == "--wait-seconds" {
                options.wait_seconds
            } else {
                options.quiet_seconds
            };
            assert_eq!(selected, number);
        }
        refused(&supplied(flag, &(maximum + 1).to_string()));
        for value in [
            "",
            "00",
            "01",
            "+1",
            "-0",
            "-1",
            " 1",
            "1 ",
            "1.0",
            "1e0",
            "1_0",
            "１２",
            "null",
            "true",
            "18446744073709551616",
        ] {
            refused(&supplied(flag, value));
        }
    }
}

#[test]
fn duplicate_flags_cannot_replace_a_previously_selected_value() {
    for flag in VALUE_FLAGS {
        let original = match flag {
            "--inventory" => "/private/inventory.json",
            "--repository" => REPOSITORY,
            "--origin" => "https://chat.example.test",
            "--limit" => "public_vps",
            "--wait-seconds" | "--quiet-seconds" => "1",
            _ => "/private/selected-input",
        };
        let mut arguments = supplied(flag, original);
        arguments.extend([flag.into(), original.into()]);
        refused(&arguments);
    }
    let mut arguments = base();
    arguments.extend(["--install-helpers".into(), "--install-helpers".into()]);
    refused(&arguments);
}

#[test]
fn missing_ambiguous_and_unrelated_operation_arguments_are_refused() {
    refused(&[]);
    for flag in ["--inventory", "--repository", "--origin"] {
        let mut missing = base();
        let at = missing.iter().position(|value| value == flag).unwrap();
        missing.drain(at..at + 2);
        refused(&missing);
    }
    for extra in [
        "--force",
        "--maintenance",
        "--public-chat-smoke",
        "--inventory=/private/inventory.json",
        "--wait-seconds=1",
        "--quiet",
        "--repo",
        "--",
        "positional-value",
        "--help",
    ] {
        let mut arguments = base();
        arguments.push(extra.into());
        refused(&arguments);
    }
    let mut missing = base();
    missing.push("--ops-artifact-zip".into());
    refused(&missing);
    refused(&supplied("--repository", "other/repository"));
    refused(&supplied("--repository", "SWIFTRACCOON/simplestChat"));
}

#[test]
fn paths_reject_lexical_aliases_but_preserve_legitimate_names() {
    for flag in [
        "--inventory",
        "--ansible-playbook",
        "--gh",
        "--application-artifact-dir",
        "--ops-artifact-zip",
    ] {
        for invalid in [
            "",
            "/",
            "relative",
            "~/private/input",
            "/private//input",
            "/private/./input",
            "/private/../input",
            "/private/input/",
            "/private/unsafe\ninput",
            "/private/unsafe\u{0085}input",
        ] {
            refused(&supplied(flag, invalid));
        }
        admitted(&supplied(flag, "/private/owned inputs/λ.json"));
    }
}

#[test]
fn equivalent_https_origins_normalize_to_one_host_and_default_port() {
    for value in [
        "https://chat.example.test",
        "https://chat.example.test/",
        "https://chat.example.test:443",
        "https://chat.example.test:443/",
    ] {
        let options = admitted(&supplied("--origin", value));
        assert_eq!(options.origin, "https://chat.example.test");
        assert_eq!(options.origin_host, "chat.example.test");
    }
    let options = admitted(&supplied("--origin", "https://192.0.2.1:443/"));
    assert_eq!(options.origin, "https://192.0.2.1");
    assert_eq!(options.origin_host, "192.0.2.1");
}

#[test]
fn origin_cannot_smuggle_credentials_paths_ports_or_normalized_hosts() {
    for value in [
        "http://chat.example.test",
        "HTTPS://chat.example.test",
        "https://CHAT.example.test",
        "https://chat.example.test.",
        "https://user@chat.example.test",
        "https://user:password@chat.example.test",
        "https://chat.example.test:80",
        "https://chat.example.test:0443",
        "https://chat.example.test:4430",
        "https://chat.example.test:",
        "https://chat.example.test/health",
        "https://chat.example.test/.",
        "https://chat.example.test/%2e",
        "https://chat.example.test//",
        "https://chat.example.test?",
        "https://chat.example.test?token=PRIVATE",
        "https://chat.example.test#",
        "https://chat.example.test\\evil",
        "https://chat..example.test",
        "https://-chat.example.test",
        "https://chat-.example.test",
        "https://chat_example.test",
        "https://chât.example.test",
        "https://127.1",
        "https://0177.0.0.1",
        "https://0x7f.0.0.1",
        "https://2130706433",
        "https://[2001:db8::1]",
    ] {
        refused(&supplied("--origin", value));
    }
}

#[test]
fn exact_host_aliases_exclude_ansible_patterns_and_lists() {
    for value in ["public_vps", "Host-1.example", "192.0.2.1"] {
        assert_eq!(
            admitted(&supplied("--limit", value)).limit.as_deref(),
            Some(value)
        );
    }
    for value in [
        "",
        "*",
        "all:!other",
        "one,two",
        "group:&other",
        "host[0:1]",
        "~host.*",
        "-host",
        "host name",
        "host\nname",
        "host/name",
        "host@name",
        "höst",
    ] {
        refused(&supplied("--limit", value));
    }
}

#[test]
fn accepted_input_sizes_are_bounded_before_url_or_path_construction() {
    let largest_host = format!(
        "{}.{}.{}.{}",
        "a".repeat(63),
        "b".repeat(63),
        "c".repeat(63),
        "d".repeat(61)
    );
    assert_eq!(largest_host.len(), 253);
    admitted(&supplied("--origin", &format!("https://{largest_host}")));
    refused(&supplied("--origin", &format!("https://{largest_host}d")));
    refused(&supplied(
        "--origin",
        &format!("https://{}.test", "a".repeat(64)),
    ));
    admitted(&supplied("--limit", &"a".repeat(253)));
    refused(&supplied("--limit", &"a".repeat(254)));
    refused(&supplied("--inventory", &format!("/{}", "a".repeat(4096))));
    let arguments = vec![String::from("--install-helpers"); 22];
    refused(&arguments);
}

#[test]
fn help_is_exact_and_private_inputs_never_enter_debug_or_failures() {
    assert!(parse(&["--help".into()]).unwrap().is_none());
    refused(&["-h".into()]);
    let options = admitted(&supplied("--inventory", "/private/SECRET/inventory.json"));
    assert_eq!(format!("{options:?}"), "SignedDeploymentOptions");
    let invalid = supplied("--origin", "https://SECRET:password@chat.example.test");
    let error = parse(&invalid).unwrap_err();
    assert_eq!(error.code, 2);
    assert!(!format!("{error:?}").contains("SECRET"));
    assert!(!error.message.contains("password"));
}
