//! The pinned OpenSSH transport: one explicit endpoint, identity and host
//! trust file, with no configuration file, agent, forwarding or connection
//! sharing inherited from the controller.

pub(super) use crate::ops_artifact::capacity_vps::inventory::{Target, quote};

/// How one connection authenticates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Authentication {
    /// The enrolled key only; nothing can prompt.
    Key,
    /// One password prompt and no key, for the first enrollment.
    Password,
}

/// Direct transport, identity, host trust and bounded liveness.
fn options(target: &Target) -> Vec<String> {
    let mut arguments = vec![
        "-F".to_owned(),
        "/dev/null".to_owned(),
        "-p".to_owned(),
        target.port.to_string(),
        "-i".to_owned(),
        target.identity.display().to_string(),
    ];
    let known_hosts = format!("UserKnownHostsFile={}", target.known_hosts.display());
    for option in [
        "IdentitiesOnly=yes",
        "IdentityAgent=none",
        "ForwardAgent=no",
        "ClearAllForwardings=yes",
        "ControlMaster=no",
        "ControlPath=none",
        "StrictHostKeyChecking=yes",
        "UpdateHostKeys=no",
        "GlobalKnownHostsFile=/dev/null",
        known_hosts.as_str(),
        "ConnectTimeout=15",
        "ConnectionAttempts=1",
        "ServerAliveInterval=10",
        "ServerAliveCountMax=3",
    ] {
        arguments.extend(["-o".to_owned(), option.to_owned()]);
    }
    arguments
}

/// One direct `ssh` invocation of `remote`, a single command string the
/// remote shell receives unchanged. `tty` asks for a remote terminal.
pub(super) fn command(
    target: &Target,
    remote: &str,
    authentication: Authentication,
    tty: bool,
) -> Vec<String> {
    let chosen: &[&str] = match authentication {
        Authentication::Key => &[
            "BatchMode=yes",
            "PreferredAuthentications=publickey",
            "PasswordAuthentication=no",
            "KbdInteractiveAuthentication=no",
        ],
        Authentication::Password => &[
            "BatchMode=no",
            "PreferredAuthentications=password",
            "PubkeyAuthentication=no",
            "KbdInteractiveAuthentication=no",
            "NumberOfPasswordPrompts=1",
        ],
    };
    let mut arguments = vec!["/usr/bin/ssh".to_owned()];
    arguments.extend(options(target));
    for option in chosen {
        arguments.extend(["-o".to_owned(), (*option).to_owned()]);
    }
    arguments.push(if tty { "-tt" } else { "-T" }.to_owned());
    arguments.push(format!("{}@{}", target.user, target.host));
    arguments.push(remote.to_owned());
    arguments
}

/// OpenSSH's `known_hosts` selector for the chosen port.
pub(super) fn host_token(target: &Target) -> String {
    if target.port == 22 {
        target.host.clone()
    } else {
        format!("[{}]:{}", target.host, target.port)
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::path::PathBuf;

    pub(in super::super) fn target(port: u16) -> Target {
        Target {
            host: "192.0.2.10".to_owned(),
            user: "debian".to_owned(),
            port,
            identity: PathBuf::from("/keys/bench key"),
            known_hosts: PathBuf::from("/keys/known_hosts"),
        }
    }

    /// From `test_private_transport_is_explicit_and_password_free`.
    #[test]
    fn key_transport_is_explicit_and_cannot_prompt() {
        let argv = command(&target(2222), "true", Authentication::Key, false);
        assert_eq!(argv[0], "/usr/bin/ssh");
        assert_eq!(
            argv[1..7],
            ["-F", "/dev/null", "-p", "2222", "-i", "/keys/bench key"]
        );
        assert_eq!(argv[argv.len() - 3..], ["-T", "debian@192.0.2.10", "true"]);
        let options = argv.iter().filter(|word| word.contains('='));
        let options = options.map(String::as_str).collect::<Vec<_>>();
        for expected in [
            "IdentitiesOnly=yes",
            "IdentityAgent=none",
            "ForwardAgent=no",
            "ClearAllForwardings=yes",
            "ControlMaster=no",
            "ControlPath=none",
            "StrictHostKeyChecking=yes",
            "UpdateHostKeys=no",
            "GlobalKnownHostsFile=/dev/null",
            "UserKnownHostsFile=/keys/known_hosts",
            "ConnectTimeout=15",
            "ConnectionAttempts=1",
            "BatchMode=yes",
            "PreferredAuthentications=publickey",
            "PasswordAuthentication=no",
            "KbdInteractiveAuthentication=no",
        ] {
            assert!(options.contains(&expected), "{expected}");
        }
        assert!(
            !options
                .iter()
                .any(|option| option.starts_with("PubkeyAuthentication"))
        );
        // Every option follows its own `-o`.
        for (index, word) in argv.iter().enumerate() {
            if word.contains('=') {
                assert_eq!(argv[index - 1], "-o", "{word}");
            }
        }
    }

    /// The password transport allows exactly one prompt, offers no key and
    /// asks for a remote terminal, which a forced password change needs.
    #[test]
    fn password_transport_prompts_once_without_a_key() {
        let argv = command(&target(22), "sh -c x", Authentication::Password, true);
        for expected in [
            "BatchMode=no",
            "PreferredAuthentications=password",
            "PubkeyAuthentication=no",
            "KbdInteractiveAuthentication=no",
            "NumberOfPasswordPrompts=1",
        ] {
            assert!(argv.iter().any(|word| word == expected), "{expected}");
        }
        assert!(!argv.iter().any(|word| word == "BatchMode=yes"));
        assert_eq!(
            argv[argv.len() - 3..],
            ["-tt", "debian@192.0.2.10", "sh -c x"]
        );
    }

    /// The trust selector and the inventory's SSH arguments follow the
    /// endpoint; a path with a space is quoted for Ansible's shell split.
    #[test]
    fn host_token_and_inventory_arguments_follow_the_endpoint() {
        assert_eq!(host_token(&target(22)), "192.0.2.10");
        assert_eq!(host_token(&target(2222)), "[192.0.2.10]:2222");
        let mut spaced = target(22);
        spaced.known_hosts = PathBuf::from("/keys/known hosts");
        let common = spaced.common();
        assert!(common.starts_with("-F /dev/null -o StrictHostKeyChecking=yes -o "));
        assert!(common.contains("'UserKnownHostsFile=/keys/known hosts'"));
        assert!(common.ends_with("-o ControlMaster=no -o ControlPath=none"));
        assert_eq!(quote("it's"), "'it'\"'\"'s'");
        assert_eq!(quote(""), "''");
        assert_eq!(quote("plain_value-1.2:/x"), "plain_value-1.2:/x");
    }
}
