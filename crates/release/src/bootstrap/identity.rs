//! The dedicated automation key: created once, unencrypted, outside the
//! checkout and never replaced.

use super::files::{outside_checkout, private_parent, protected_file, sync_file};
use super::ssh::Target;
use super::{Result, Runner, require};
use regex::Regex;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The public half's path: the identity's name with `.pub` appended.
fn public_path(identity: &Path) -> PathBuf {
    let mut name = identity.as_os_str().to_owned();
    name.push(".pub");
    PathBuf::from(name)
}

/// Creates the key when it is absent, verifies the files either way, and
/// returns the public key line that OpenSSH derives from the private key.
pub(super) fn ensure_identity(
    runner: &dyn Runner,
    target: &Target,
    name: &str,
    root: &Path,
) -> Result<String> {
    outside_checkout(&target.identity, root)?;
    private_parent(&target.identity)?;
    let identity = target.identity.display().to_string();
    let public = public_path(&target.identity);
    let present = |path: &Path| path.symlink_metadata().is_ok();
    if !present(&target.identity) {
        // A public half without its private key belongs to something else.
        require(!present(&public), "orphan_public_key_exists")?;
        let comment = format!("simplestchat-bootstrap:{name}");
        let generate = [
            "/usr/bin/ssh-keygen",
            "-q",
            "-t",
            "ed25519",
            "-N",
            "",
            "-f",
            &identity,
            "-C",
            &comment,
        ];
        let generated = runner.run(&generate.map(String::from), Duration::from_secs(30))?;
        require(generated.status == 0, "identity_generation_failed")?;
    }
    protected_file(&target.identity, true)?;
    sync_file(&target.identity)?;
    if present(&public) {
        protected_file(&public, false)?;
        sync_file(&public)?;
    }
    let derive = ["/usr/bin/ssh-keygen", "-y", "-P", "", "-f", &identity];
    let derived = runner.run(&derive.map(String::from), Duration::from_secs(30))?;
    let shape = Regex::new(r"^ssh-ed25519 [A-Za-z0-9+/=]+(?: [^\r\n]+)?$").unwrap();
    let supported = derived.status == 0 && shape.is_match(&derived.text);
    require(supported, "unsupported_identity")?;
    Ok(derived.text)
}

#[cfg(test)]
mod tests {
    use super::super::files::tests::Scratch;
    use super::super::tests::{PUBLIC, Recorded};
    use super::super::{Finished, Refusal};
    use super::*;
    use std::cell::RefCell;

    fn target(scratch: &Scratch) -> Target {
        Target {
            host: "test.example.invalid".to_owned(),
            user: "debian".to_owned(),
            port: 22,
            identity: scratch.0.join("key"),
            known_hosts: scratch.0.join("known_hosts"),
        }
    }

    /// A runner that behaves like `ssh-keygen`: generation writes both
    /// halves, derivation prints the public line.
    struct Keygen {
        calls: RefCell<Vec<Vec<String>>>,
    }

    impl Runner for Keygen {
        fn run(&self, argv: &[String], _: Duration) -> Result<Finished> {
            self.calls.borrow_mut().push(argv.to_vec());
            if argv.contains(&"-t".to_owned()) {
                let path = PathBuf::from(&argv[7]);
                std::fs::write(&path, b"private fixture\n").unwrap();
                let private = std::os::unix::fs::PermissionsExt::from_mode(0o600);
                std::fs::set_permissions(&path, private).unwrap();
                std::fs::write(public_path(&path), format!("{PUBLIC} comment\n")).unwrap();
            }
            let text = format!("{PUBLIC} simplestchat-bootstrap:test_vps");
            Ok(Finished { status: 0, text })
        }
    }

    /// A missing key is generated once with an empty passphrase and a comment
    /// naming the host; an existing key is only verified.
    #[test]
    fn a_missing_key_is_generated_once_and_an_existing_key_is_kept() {
        let scratch = Scratch::new();
        let checkout = Scratch::new();
        let target = target(&scratch);
        let keygen = Keygen {
            calls: RefCell::new(Vec::new()),
        };
        let public = ensure_identity(&keygen, &target, "test_vps", &checkout.0).unwrap();
        assert_eq!(public, format!("{PUBLIC} simplestchat-bootstrap:test_vps"));
        let identity = target.identity.display().to_string();
        let calls = keygen.calls.borrow().clone();
        assert_eq!(
            calls[0],
            [
                "/usr/bin/ssh-keygen",
                "-q",
                "-t",
                "ed25519",
                "-N",
                "",
                "-f",
                &identity,
                "-C",
                "simplestchat-bootstrap:test_vps"
            ]
        );
        assert_eq!(
            calls[1],
            ["/usr/bin/ssh-keygen", "-y", "-P", "", "-f", &identity]
        );
        let before = std::fs::read(&target.identity).unwrap();
        ensure_identity(&keygen, &target, "test_vps", &checkout.0).unwrap();
        assert_eq!(keygen.calls.borrow().len(), 3);
        assert_eq!(std::fs::read(&target.identity).unwrap(), before);
    }

    /// A key inside the checkout, an orphan public half, a readable private
    /// key, a failed generation and an unsupported key type are refused.
    #[test]
    fn unsafe_or_unsupported_identities_are_refused() {
        let scratch = Scratch::new();
        let checkout = Scratch::new();
        let target = target(&scratch);
        let silent = Recorded::new(&[]);
        let inside = ensure_identity(&silent, &target, "test_vps", &scratch.0);
        assert_eq!(
            inside.unwrap_err(),
            Refusal("credential_path_inside_checkout")
        );
        scratch.file("key.pub", b"ssh-ed25519 AAAA\n", 0o644);
        let orphan = ensure_identity(&silent, &target, "test_vps", &checkout.0);
        assert_eq!(orphan.unwrap_err(), Refusal("orphan_public_key_exists"));
        assert!(silent.calls().is_empty());
        std::fs::remove_file(scratch.0.join("key.pub")).unwrap();
        let failed = Recorded::new(&[(1, "")]);
        let failed = ensure_identity(&failed, &target, "test_vps", &checkout.0);
        assert_eq!(failed.unwrap_err(), Refusal("identity_generation_failed"));
        scratch.file("key", b"private fixture\n", 0o644);
        let readable = ensure_identity(&silent, &target, "test_vps", &checkout.0);
        assert_eq!(readable.unwrap_err(), Refusal("invalid_private_file_mode"));
        scratch.file("key", b"private fixture\n", 0o600);
        for answer in [
            (1, PUBLIC),
            (0, "ssh-rsa AAAA comment"),
            (0, "ssh-ed25519 not*base64"),
        ] {
            let derived = Recorded::new(&[answer]);
            let refused = ensure_identity(&derived, &target, "test_vps", &checkout.0);
            assert_eq!(refused.unwrap_err(), Refusal("unsupported_identity"));
        }
    }
}
