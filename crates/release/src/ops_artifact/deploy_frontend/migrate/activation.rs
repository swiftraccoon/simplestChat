//! Replacing the operator's own source inventory after a move: only the one
//! explicitly selected, only while it is a private ignored file, and only
//! while it still holds the bytes the move started from.

use super::super::failure;
use super::checks::refusal;
use serde_json::{Map, Value, json};
use simplestchat_toolkit::tooling::Result;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
};

const MAXIMUM: u64 = 1024 * 1024;

/// Fresh lower-case hexadecimal text from the operating system.
pub(super) fn entropy(bytes: usize) -> Result<String> {
    let mut drawn = vec![0_u8; bytes];
    let read = File::open("/dev/urandom").and_then(|mut source| source.read_exact(&mut drawn));
    read.map_err(|_| failure(1))?;
    Ok(drawn.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// Admit the inventory a move may replace afterwards: the source inventory
/// itself, inside the checkout, an ordinary private file of this user.
pub(super) fn admit(root: &Path, source: &Path, selected: &Path) -> Result<()> {
    if selected != source || !selected.starts_with(root) {
        return Err(refusal("migration_activation_requires_source_inventory"));
    }
    let private = fs::symlink_metadata(selected).is_ok_and(|stamp| {
        stamp.is_file()
            && stamp.uid() == rustix::process::getuid().as_raw()
            && stamp.mode() & 0o7777 == 0o600
            && (1..=MAXIMUM).contains(&stamp.len())
    });
    if private {
        Ok(())
    } else {
        Err(refusal(
            "migration_activation_inventory_private_file_required",
        ))
    }
}

/// The inventory that selects the destination from now on: its alias and
/// own variables with the moved revision, repository and passkey identity.
pub(super) fn selected(
    alias: &str,
    variables: &Value,
    moved: &Map<String, Value>,
) -> Result<Vec<u8>> {
    let mut host = variables.as_object().cloned().ok_or_else(|| failure(1))?;
    for (name, from) in [
        ("scbench_revision", "scmig_revision"),
        ("scbench_repository", "scbench_repository"),
        ("scpub_release_revision", "scmig_revision"),
        ("scpub_webauthn_rp_id", "scpub_webauthn_rp_id"),
    ] {
        let value = moved.get(from).filter(|value| value.is_string());
        host.insert(name.into(), value.cloned().ok_or_else(|| failure(1))?);
    }
    host.insert("scpub_release_deploy".into(), json!(true));
    let inventory = json!({"benchmark_hosts": {"hosts": {alias: host}}});
    let mut bytes = serde_json::to_vec_pretty(&inventory).map_err(|_| failure(1))?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Put `bytes` in place of the inventory through a private sibling, asking
/// `unchanged` to vouch for the original before writing and again before
/// the rename. Returns whether the inventory was replaced, beside the
/// outcome of the whole step: a replacement that could not be made durable
/// is still a replacement.
pub(super) fn replace(
    path: &Path,
    bytes: &[u8],
    unchanged: impl Fn() -> Result<()>,
) -> (bool, Result<()>) {
    let sibling = path.parent().zip(entropy(8).ok());
    let Some((parent, suffix)) = sibling else {
        return (false, Err(failure(1)));
    };
    let temporary = parent.join(format!(".migration-inventory-{suffix}"));
    let written = (|| {
        unchanged()?;
        let mut options = OpenOptions::new();
        let created = options
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary);
        let mut file = created.map_err(|_| failure(1))?;
        let stored = file.write_all(bytes).and_then(|()| file.sync_all());
        stored.map_err(|_| failure(1))?;
        unchanged()?;
        fs::rename(&temporary, path).map_err(|_| failure(1))
    })();
    if let Err(error) = written {
        let _ = fs::remove_file(&temporary);
        return (false, Err(error));
    }
    let durable = File::open(parent).and_then(|directory| directory.sync_all());
    (true, durable.map_err(|_| failure(1)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn directory(name: &str) -> std::path::PathBuf {
        let unique = format!("migration-activation-{name}-{}", std::process::id());
        let path = std::env::temp_dir().canonicalize().unwrap().join(unique);
        let _ = fs::remove_dir_all(&path);
        fs::create_dir(&path).unwrap();
        path
    }

    fn private(path: &Path, bytes: &[u8], mode: u32) {
        let _ = fs::remove_file(path);
        fs::write(path, bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn only_the_private_source_inventory_inside_the_checkout_is_admitted() {
        let root = directory("admit");
        let source = root.join("source.yml");
        private(&source, b"all: {}\n", 0o600);
        admit(&root, &source, &source).unwrap();
        let other = root.join("other.yml");
        private(&other, b"all: {}\n", 0o600);
        let outside = root.parent().unwrap().join("source.yml");
        for (base, selected) in [(&root, &other), (&root.join("nested"), &source)] {
            let error = admit(base, &source, selected).unwrap_err();
            assert_eq!(
                error.message,
                "migration_activation_requires_source_inventory"
            );
        }
        let error = admit(&root, &outside, &outside).unwrap_err();
        assert_eq!(
            error.message,
            "migration_activation_requires_source_inventory"
        );
        for mode in [0o644, 0o640, 0o400, 0o700] {
            private(&source, b"all: {}\n", mode);
            let error = admit(&root, &source, &source).unwrap_err();
            assert_eq!(
                error.message, "migration_activation_inventory_private_file_required",
                "{mode:o}"
            );
        }
        private(&source, b"", 0o600);
        assert!(admit(&root, &source, &source).is_err());
        fs::remove_file(&source).unwrap();
        std::os::unix::fs::symlink(&other, &source).unwrap();
        assert!(admit(&root, &source, &source).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_new_inventory_keeps_the_alias_and_opt_ins_and_names_what_was_moved() {
        let variables = json!({"ansible_host": "192.0.2.11", "scpub_turn_enabled": true,
            "scbench_revision": "0".repeat(40), "scpub_release_deploy": false});
        let moved = json!({"scmig_revision": "a".repeat(40), "scpub_webauthn_rp_id": "example.test",
            "scbench_repository": "https://github.com/swiftraccoon/simplestChat.git",
            "scmig_run_id": "f".repeat(32)});
        let bytes = selected("next_vps", &variables, moved.as_object().unwrap()).unwrap();
        assert!(bytes.ends_with(b"\n}\n") && bytes.starts_with(b"{\n  \"benchmark_hosts\""));
        let inventory: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            inventory,
            json!({"benchmark_hosts": {"hosts": {"next_vps": {
            "ansible_host": "192.0.2.11", "scpub_turn_enabled": true,
            "scbench_revision": "a".repeat(40), "scpub_release_revision": "a".repeat(40),
            "scbench_repository": "https://github.com/swiftraccoon/simplestChat.git",
            "scpub_webauthn_rp_id": "example.test", "scpub_release_deploy": true}}}})
        );
        let mut partial = moved.as_object().unwrap().clone();
        partial.remove("scpub_webauthn_rp_id");
        assert!(selected("next_vps", &variables, &partial).is_err());
        assert!(selected("next_vps", &json!([]), moved.as_object().unwrap()).is_err());
    }

    #[test]
    fn an_inventory_is_replaced_whole_or_left_exactly_as_it_was() {
        let root = directory("replace");
        let path = root.join("source.yml");
        private(&path, b"original\n", 0o600);
        let listed = || {
            let mut names: Vec<_> = fs::read_dir(&root)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            names.sort();
            names
        };
        let (replaced, outcome) = replace(&path, b"switched\n", || Ok(()));
        assert!(replaced && outcome.is_ok());
        assert_eq!(fs::read(&path).unwrap(), b"switched\n");
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o7777, 0o600);
        assert_eq!(listed(), ["source.yml"]);
        // An original that changed before the write, or between the write and
        // the rename, stays; the private sibling does not.
        for refused_at in [1, 2] {
            let calls = std::cell::Cell::new(0);
            let (replaced, outcome) = replace(&path, b"again\n", || {
                calls.set(calls.get() + 1);
                if calls.get() == refused_at {
                    return Err(refusal("migration_activation_inventory_changed"));
                }
                Ok(())
            });
            assert!(!replaced, "{refused_at}");
            assert_eq!(
                outcome.unwrap_err().message,
                "migration_activation_inventory_changed"
            );
            assert_eq!(
                (calls.get(), fs::read(&path).unwrap()),
                (refused_at, b"switched\n".to_vec())
            );
            assert_eq!(listed(), ["source.yml"]);
        }
        assert_eq!(entropy(16).unwrap().len(), 32);
        assert_ne!(entropy(16).unwrap(), entropy(16).unwrap());
        fs::remove_dir_all(root).unwrap();
    }
}
