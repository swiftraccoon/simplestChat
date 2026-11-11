//! Host trust: an existing strict `known_hosts` entry, or one scanned
//! Ed25519 key that equals a fingerprint the operator verified independently.
//! A scanned key alone is never a trust decision.

use super::files::{metadata, private_parent, protected_file, write_new};
use super::ssh::{Target, host_token};
use super::{Refusal, Result, Runner, require};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use rustix::fs::OFlags;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::time::Duration;

/// The largest public key blob accepted.
const MAX_KEY_BYTES: usize = 4096;

/// OpenSSH's SHA256 fingerprint of one base64 public key blob.
pub(super) fn key_fingerprint(encoded: &str) -> Result<String> {
    let blob = STANDARD.decode(encoded);
    let blob = blob.map_err(|_| Refusal("invalid_host_key"))?;
    require(blob.len() <= MAX_KEY_BYTES, "invalid_host_key")?;
    let digest = STANDARD_NO_PAD.encode(Sha256::digest(&blob));
    Ok(format!("SHA256:{digest}"))
}

/// The whitespace-separated fields of every line that is not a comment.
fn entries(text: &str) -> impl Iterator<Item = Vec<&str>> {
    let lines = text.lines().filter(|line| !line.starts_with('#'));
    lines.map(|line| line.split_whitespace().collect())
}

/// Appends one entry to an existing trust file without following a link.
fn append(target: &Target, entry: &[u8]) -> Result<()> {
    let nofollow = i32::try_from(OFlags::NOFOLLOW.bits()).unwrap();
    let mut options = OpenOptions::new();
    let file = options.append(true).custom_flags(nofollow);
    let mut file = file.open(&target.known_hosts).map_err(metadata)?;
    file.write_all(entry).map_err(metadata)?;
    file.sync_all().map_err(metadata)
}

/// Uses existing strict trust, or appends only a scanned Ed25519 key that
/// equals `fingerprint`. Returns what the trust rests on.
pub(super) fn host_trust(
    runner: &dyn Runner,
    target: &Target,
    fingerprint: Option<&str>,
) -> Result<String> {
    private_parent(&target.known_hosts)?;
    let known = target.known_hosts.display().to_string();
    let token = host_token(target);
    if target.known_hosts.exists() {
        protected_file(&target.known_hosts, false)?;
        let lookup = ["/usr/bin/ssh-keygen", "-F", &token, "-f", &known];
        let found = runner.run(&lookup.map(String::from), Duration::from_secs(30))?;
        if found.status == 0 {
            let mut hashes = BTreeSet::new();
            for fields in entries(&found.text).filter(|fields| fields.len() >= 3) {
                hashes.insert(key_fingerprint(fields[2])?);
            }
            require(!hashes.is_empty(), "invalid_known_host_entry")?;
            let pinned = fingerprint.is_none_or(|wanted| hashes.contains(wanted));
            require(pinned, "host_fingerprint_mismatch")?;
            return Ok(fingerprint.unwrap_or("existing_known_hosts").to_owned());
        }
    }
    let wanted = fingerprint.ok_or(Refusal("verified_host_fingerprint_required"))?;
    let port = target.port.to_string();
    let scan = [
        "/usr/bin/ssh-keyscan",
        "-T",
        "10",
        "-p",
        &port,
        "-t",
        "ed25519",
        &target.host,
    ];
    let scanned = runner.run(&scan.map(String::from), Duration::from_secs(30))?;
    require(scanned.status == 0, "host_key_scan_failed")?;
    let candidates = entries(&scanned.text).filter(|fields| fields.len() == 3);
    let candidates = candidates.map(|fields| (fields[1], fields[2]));
    let candidates = candidates.collect::<BTreeSet<_>>();
    require(candidates.len() == 1, "ambiguous_host_key_scan")?;
    let (kind, encoded) = candidates.into_iter().next().unwrap();
    let matches = kind == "ssh-ed25519" && key_fingerprint(encoded)? == wanted;
    require(matches, "host_fingerprint_mismatch")?;
    let entry = format!("\n{token} {kind} {encoded}\n");
    if target.known_hosts.exists() {
        append(target, entry.as_bytes())?;
    } else {
        write_new(&target.known_hosts, entry.as_bytes())?;
    }
    Ok(wanted.to_owned())
}

#[cfg(test)]
mod tests {
    use super::super::files::tests::Scratch;
    use super::super::tests::{PUBLIC, Recorded};
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn target(scratch: &Scratch) -> Target {
        Target {
            host: "test.example.invalid".to_owned(),
            user: "debian".to_owned(),
            port: 22,
            identity: scratch.0.join("key"),
            known_hosts: scratch.0.join("known_hosts"),
        }
    }

    /// The fingerprint is OpenSSH's own: unpadded base64 of the blob's
    /// SHA-256, and a malformed or oversized blob is refused.
    #[test]
    fn fingerprints_are_unpadded_sha256_of_a_strict_blob() {
        let encoded = PUBLIC.split(' ').nth(1).unwrap();
        let fingerprint = key_fingerprint(encoded).unwrap();
        assert!(fingerprint.starts_with("SHA256:") && fingerprint.len() == 50);
        assert!(!fingerprint.ends_with('='));
        for malformed in ["not base64!", "AAAA=A", "A"] {
            let refused = key_fingerprint(malformed).unwrap_err();
            assert_eq!(refused, Refusal("invalid_host_key"), "{malformed}");
        }
        let oversized = STANDARD.encode(vec![0u8; MAX_KEY_BYTES + 1]);
        assert_eq!(
            key_fingerprint(&oversized).unwrap_err(),
            Refusal("invalid_host_key")
        );
    }

    /// From `test_host_scan_requires_independent_matching_pin`: scanned keys
    /// are candidates, never an implicit trust decision.
    #[test]
    fn host_scan_requires_an_independent_matching_pin() {
        let scratch = Scratch::new();
        let target = target(&scratch);
        let fingerprint = key_fingerprint(PUBLIC.split(' ').nth(1).unwrap()).unwrap();
        let silent = Recorded::new(&[]);
        let unpinned = host_trust(&silent, &target, None).unwrap_err();
        assert_eq!(unpinned, Refusal("verified_host_fingerprint_required"));
        assert!(silent.calls().is_empty());
        let scan = format!("# test.example.invalid:22 SSH-2.0-OpenSSH\nhost {PUBLIC}");
        let scanned = Recorded::new(&[(0, &scan), (0, &scan)]);
        let wrong = format!("SHA256:{}", "x".repeat(43));
        let mismatch = host_trust(&scanned, &target, Some(&wrong)).unwrap_err();
        assert_eq!(mismatch, Refusal("host_fingerprint_mismatch"));
        assert!(!target.known_hosts.exists());
        let trusted = host_trust(&scanned, &target, Some(&fingerprint)).unwrap();
        assert_eq!(trusted, fingerprint);
        let written = std::fs::read_to_string(&target.known_hosts).unwrap();
        assert_eq!(written, format!("\ntest.example.invalid {PUBLIC}\n"));
        let calls = scanned.calls();
        assert_eq!(calls[0][0], "/usr/bin/ssh-keyscan");
        assert_eq!(
            calls[0][1..],
            [
                "-T",
                "10",
                "-p",
                "22",
                "-t",
                "ed25519",
                "test.example.invalid"
            ]
        );
    }

    /// A scan answering two keys, another key type or a failure is refused.
    #[test]
    fn ambiguous_failed_or_foreign_scans_are_refused() {
        let scratch = Scratch::new();
        let target = target(&scratch);
        let fingerprint = key_fingerprint(PUBLIC.split(' ').nth(1).unwrap()).unwrap();
        let two = format!("host {PUBLIC}\nhost ssh-ed25519 AAAA");
        let rsa = "host ssh-rsa AAAA".to_owned();
        for (status, text, code) in [
            (1, String::new(), "host_key_scan_failed"),
            (0, String::new(), "ambiguous_host_key_scan"),
            (0, two, "ambiguous_host_key_scan"),
            (0, rsa, "host_fingerprint_mismatch"),
        ] {
            let scanned = Recorded::new(&[(status, &text)]);
            let refused = host_trust(&scanned, &target, Some(&fingerprint)).unwrap_err();
            assert_eq!(refused, Refusal(code));
            assert!(!target.known_hosts.exists());
        }
    }

    /// From `test_existing_known_hosts_can_be_publicly_readable_and_is_preserved`:
    /// trusted existing entries need no new scan or fingerprint.
    #[test]
    fn existing_known_hosts_can_be_publicly_readable_and_is_preserved() {
        let scratch = Scratch::new();
        let target = target(&scratch);
        let original = format!("other.invalid {PUBLIC}\ntest.example.invalid {PUBLIC}\n");
        scratch.file("known_hosts", original.as_bytes(), 0o644);
        let found =
            format!("# Host test.example.invalid found: line 2\ntest.example.invalid {PUBLIC}");
        let lookup = Recorded::new(&[(0, &found)]);
        let trusted = host_trust(&lookup, &target, None).unwrap();
        assert_eq!(trusted, "existing_known_hosts");
        let calls = lookup.calls();
        assert_eq!(calls.len(), 1);
        let known = target.known_hosts.display().to_string();
        assert_eq!(
            calls[0],
            [
                "/usr/bin/ssh-keygen",
                "-F",
                "test.example.invalid",
                "-f",
                &known
            ]
        );
        assert_eq!(
            std::fs::read_to_string(&target.known_hosts).unwrap(),
            original
        );
        let fingerprint = key_fingerprint(PUBLIC.split(' ').nth(1).unwrap()).unwrap();
        let pinned = host_trust(&Recorded::new(&[(0, &found)]), &target, Some(&fingerprint));
        assert_eq!(pinned.unwrap(), fingerprint);
        let wrong = format!("SHA256:{}", "x".repeat(43));
        let refused = host_trust(&Recorded::new(&[(0, &found)]), &target, Some(&wrong));
        assert_eq!(refused.unwrap_err(), Refusal("host_fingerprint_mismatch"));
        let empty = host_trust(&Recorded::new(&[(0, "# only a comment")]), &target, None);
        assert_eq!(empty.unwrap_err(), Refusal("invalid_known_host_entry"));
    }

    /// An unknown host in an existing file is scanned, pinned and appended;
    /// the existing entries stay.
    #[test]
    fn an_unknown_host_is_appended_to_an_existing_file() {
        let scratch = Scratch::new();
        let target = target(&scratch);
        let original = format!("other.invalid {PUBLIC}\n");
        scratch.file("known_hosts", original.as_bytes(), 0o600);
        let fingerprint = key_fingerprint(PUBLIC.split(' ').nth(1).unwrap()).unwrap();
        let scan = format!("host {PUBLIC}");
        let answers = Recorded::new(&[(1, ""), (0, &scan)]);
        host_trust(&answers, &target, Some(&fingerprint)).unwrap();
        let written = std::fs::read_to_string(&target.known_hosts).unwrap();
        assert_eq!(
            written,
            format!("{original}\ntest.example.invalid {PUBLIC}\n")
        );
    }

    /// From `test_known_hosts_requires_a_nonreplaceable_parent`.
    #[test]
    fn known_hosts_requires_a_nonreplaceable_parent() {
        let scratch = Scratch::new();
        let target = target(&scratch);
        scratch.file("known_hosts", b"test.example.invalid key\n", 0o600);
        let shared = std::fs::Permissions::from_mode(0o770);
        std::fs::set_permissions(&scratch.0, shared).unwrap();
        let silent = Recorded::new(&[]);
        let refused = host_trust(&silent, &target, None).unwrap_err();
        assert_eq!(refused, Refusal("unsafe_private_parent_mode"));
        assert!(silent.calls().is_empty());
    }
}
