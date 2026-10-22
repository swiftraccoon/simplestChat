//! Unchanged shell invoked only with owned fake uname/sudo; no kernel command.
use super::policy::Rules;
use std::{fs, os::unix::fs::PermissionsExt, process::Command};
#[test]
fn old_linux_block_reference_retains_exact_port_and_packet_semantics() {
    let directory = std::env::temp_dir().join(format!("udp shell {}", uuid::Uuid::new_v4()));
    fs::create_dir(&directory).unwrap();
    let capture = directory.join("arguments");
    for (name, script) in [
        ("uname", "#!/bin/sh\nprintf 'Linux\\n'\n"),
        (
            "sudo",
            "#!/bin/sh\nprintf '%s\\0' \"$@\" >>\"$CAPTURE\"\nprintf '\\0' >>\"$CAPTURE\"\n",
        ),
    ] {
        fs::write(directory.join(name), script).unwrap();
        fs::set_permissions(directory.join(name), fs::Permissions::from_mode(0o700)).unwrap();
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    for action in ["block-udp", "unblock-udp"] {
        fs::write(&capture, b"").unwrap();
        let output = Command::new("/bin/sh")
            .arg(root.join("build/impair.sh"))
            .arg(action)
            .env_clear()
            .env("PATH", &directory)
            .env("CAPTURE", &capture)
            .env("IMPAIR_UDP_PORT", "41100")
            .env("IMPAIR_WORKERS", "1")
            .output()
            .unwrap();
        assert!(output.status.success());
        let bytes = fs::read(&capture).unwrap();
        let calls: Vec<Vec<String>> = bytes
            .split(|byte| *byte == 0)
            .fold(vec![Vec::new()], |mut calls, item| {
                if item.is_empty() {
                    calls.push(Vec::new());
                } else {
                    calls
                        .last_mut()
                        .unwrap()
                        .push(String::from_utf8(item.to_vec()).unwrap());
                }
                calls
            })
            .into_iter()
            .filter(|call| !call.is_empty())
            .collect();
        let deletes: Vec<Vec<String>> = ["--dport", "--sport"]
            .map(|port| {
                [
                    "iptables",
                    "-D",
                    "INPUT",
                    "-i",
                    "lo",
                    "-p",
                    "udp",
                    port,
                    "41100:41100",
                    "-j",
                    "DROP",
                ]
                .map(str::to_owned)
                .to_vec()
            })
            .to_vec();
        assert_eq!(&calls[..2], deletes);
        if action == "block-udp" {
            assert_eq!(calls.len(), 5);
            for (call, port) in calls[2..4].iter().zip(["--dport", "--sport"]) {
                assert_eq!(
                    *call,
                    [
                        "iptables",
                        "-I",
                        "INPUT",
                        "-i",
                        "lo",
                        "-p",
                        "udp",
                        port,
                        "41100:41100",
                        "-j",
                        "DROP"
                    ]
                    .map(str::to_owned)
                );
            }
            assert_eq!(calls[4], ["iptables", "-S", "INPUT"]);
        } else {
            assert_eq!(calls.len(), 2);
        }
    }
    // The new chain changes ownership, not the original loopback IPv4 UDP
    // predicate. Enumerate both media directions and untouched TCP/other ports.
    let rules = Rules::new("11223344-5566-7788-99aa-bbccddeeff00").unwrap();
    assert!(rules.rendering(4).starts_with("-i lo -p udp "));
    for interface in ["lo", "eth0"] {
        for protocol in ["udp", "tcp"] {
            for source in [41099, 41100, 41101] {
                for destination in [41099, 41100, 41101] {
                    let reference = interface == "lo"
                        && protocol == "udp"
                        && (source == 41100 || destination == 41100);
                    let chain_jump = interface == "lo" && protocol == "udp";
                    let port_rules = [rules.rendering(2), rules.rendering(3)];
                    let actual = chain_jump
                        && port_rules.iter().any(|rule| {
                            if rule.contains("--dport 41100 ") {
                                destination == 41100
                            } else if rule.contains("--sport 41100 ") {
                                source == 41100
                            } else {
                                false
                            }
                        });
                    assert_eq!(actual, reference);
                }
            }
        }
    }
    fs::remove_dir_all(directory).unwrap();
}
