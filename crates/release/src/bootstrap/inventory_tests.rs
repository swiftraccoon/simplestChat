//! The inventory contract against a disposable checkout layout.

use super::super::files::tests::Scratch;
use super::super::files::write_record;
use super::super::tests::Recorded;
use super::*;
use std::path::PathBuf;

const REVISION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const PREVIOUS: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

/// A checkout with `ops/ansible`, and a private directory beside it holding
/// the key and the trust file.
struct Layout {
    checkout: Scratch,
    private: Scratch,
}

impl Layout {
    fn new() -> Self {
        let checkout = Scratch::new();
        std::fs::create_dir_all(checkout.0.join("ops/ansible")).unwrap();
        let private = Scratch::new();
        private.file("key", b"private fixture\n", 0o600);
        private.file("known_hosts", b"host key\n", 0o600);
        Self { checkout, private }
    }

    fn target(&self) -> Target {
        Target {
            host: "test.example.invalid".to_owned(),
            user: "debian".to_owned(),
            port: 22,
            identity: self.private.0.join("key"),
            known_hosts: self.private.0.join("known_hosts"),
        }
    }

    fn path(&self) -> PathBuf {
        self.checkout.0.join("ops/ansible/inventory.local.test.yml")
    }

    fn prepare(&self, status: i32) -> Result<Map<String, Value>> {
        let places = Places {
            root: &self.checkout.0,
            home: &self.private.0,
            current: &self.checkout.0,
        };
        let runner = Recorded::new(&[(status, "")]);
        prepare_inventory(
            &runner,
            &self.path(),
            &self.target(),
            "test_vps",
            REVISION,
            places,
        )
    }

    /// Replaces the stored inventory with `value`.
    fn store(&self, value: &Value) {
        let _ = std::fs::remove_file(self.path());
        write_record(&self.path(), value).unwrap();
    }

    fn generated(&self) -> Value {
        inventory_value(&self.target(), "test_vps", REVISION)
    }
}

fn code(result: Result<Map<String, Value>>) -> &'static str {
    result.unwrap_err().0
}

/// From `test_inventory_creation_then_revision_override_preserves_bytes_and_extras`:
/// source advancement is an extra variable, never an inventory rewrite.
#[test]
fn inventory_creation_then_later_runs_preserve_bytes_and_extras() {
    let layout = Layout::new();
    let created = layout.prepare(0).unwrap();
    assert_eq!(created["ansible_host"], "test.example.invalid");
    assert_eq!(created["scbench_upgrade_packages"], false);
    assert_eq!(created["scbench_reboot"], false);
    let stored: Value = serde_json::from_slice(&std::fs::read(layout.path()).unwrap()).unwrap();
    assert_eq!(stored, layout.generated());
    let mut edited = stored;
    let host = &mut edited["benchmark_hosts"]["hosts"]["test_vps"];
    host["scbench_revision"] = json!(PREVIOUS);
    host["scpub_port_mbps"] = json!(2000);
    host["scpub_transfer_allowance_tb"] = json!(0);
    std::fs::write(layout.path(), serde_json::to_vec(&edited).unwrap()).unwrap();
    let previous = std::fs::read(layout.path()).unwrap();
    let kept = layout.prepare(0).unwrap();
    assert_eq!(std::fs::read(layout.path()).unwrap(), previous);
    assert_eq!(kept["scpub_port_mbps"], 2000);
    // The selected revision replaces the stored one for this run only.
    assert_eq!(kept["scbench_revision"], REVISION);
}

/// The inventory must be the ignored local file under `ops/ansible`.
#[test]
fn inventory_must_be_an_ignored_local_file() {
    let layout = Layout::new();
    assert_eq!(code(layout.prepare(1)), "inventory_not_ignored");
    assert!(!layout.path().exists());
    let places = Places {
        root: &layout.checkout.0,
        home: &layout.private.0,
        current: &layout.checkout.0,
    };
    for path in [
        layout.checkout.0.join("ops/ansible/inventory.yml"),
        layout.checkout.0.join("inventory.local.test.yml"),
        layout.private.0.join("inventory.local.test.yml"),
    ] {
        let silent = Recorded::new(&[]);
        let refused = prepare_inventory(
            &silent,
            &path,
            &layout.target(),
            "test_vps",
            REVISION,
            places,
        );
        assert_eq!(code(refused), "inventory_must_use_ignored_local_name");
        assert!(silent.calls().is_empty());
    }
}

/// From `test_inventory_transport_override_and_wrong_host_rejected`: an
/// existing inventory cannot redirect the target or the SSH executable.
#[test]
fn inventory_transport_overrides_and_a_wrong_host_are_rejected() {
    let layout = Layout::new();
    for (key, value, expected) in [
        (
            "ansible_host",
            json!("different.invalid"),
            "inventory_identity_mismatch",
        ),
        ("ansible_user", json!("root"), "inventory_identity_mismatch"),
        ("ansible_port", json!(2222), "inventory_port_mismatch"),
        (
            "ansible_ssh_private_key_file",
            json!("/other/key"),
            "inventory_key_mismatch",
        ),
        (
            "scbench_ssh_known_hosts_file",
            json!("/other/known_hosts"),
            "inventory_known_hosts_mismatch",
        ),
        (
            "ansible_ssh_common_args",
            json!("-o StrictHostKeyChecking=no"),
            "unsupported_inventory_ssh_args",
        ),
        (
            "ansible_connection",
            json!("local"),
            "unsupported_inventory_connection",
        ),
        (
            "ansible_ssh_executable",
            json!("/untrusted/ssh"),
            "invalid_inventory",
        ),
        (
            "ansible_ssh_extra_args",
            json!("-o StrictHostKeyChecking=no"),
            "invalid_inventory",
        ),
        ("ansible_password", json!("secret"), "invalid_inventory"),
        ("scbench_reboot", json!(true), "invalid_inventory"),
        ("scpub_enabled", json!(true), "invalid_inventory"),
        (
            "scbench_root",
            json!("{{ lookup('pipe', 'id') }}"),
            "invalid_inventory",
        ),
    ] {
        let mut data = layout.generated();
        data["benchmark_hosts"]["hosts"]["test_vps"][key] = value;
        layout.store(&data);
        assert_eq!(code(layout.prepare(0)), expected, "{key}");
    }
    // Ansible's own defaults for the port and the connection are accepted.
    let mut data = layout.generated();
    let host = data["benchmark_hosts"]["hosts"]["test_vps"]
        .as_object_mut()
        .unwrap();
    host.remove("ansible_port");
    host.remove("ansible_ssh_common_args");
    layout.store(&data);
    layout.prepare(0).unwrap();
}

/// From `test_inventory_groups_and_group_variables_are_rejected`: inherited
/// group data cannot bypass the one-host transport boundary.
#[test]
fn inventory_groups_group_variables_and_loose_files_are_rejected() {
    let layout = Layout::new();
    let mut extra_group = layout.generated();
    extra_group["all"] = json!({"vars": {"ansible_ssh_extra_args": "untrusted"}});
    let mut group_variables = layout.generated();
    group_variables["benchmark_hosts"]["vars"] = json!({"ansible_user": "other"});
    let mut second_host = layout.generated();
    second_host["benchmark_hosts"]["hosts"]["other"] = json!({"ansible_host": "x"});
    for data in [extra_group, group_variables, second_host] {
        layout.store(&data);
        assert_eq!(code(layout.prepare(0)), "invalid_inventory");
    }
    layout.store(&layout.generated());
    let readable = std::os::unix::fs::PermissionsExt::from_mode(0o644);
    std::fs::set_permissions(layout.path(), readable).unwrap();
    assert_eq!(code(layout.prepare(0)), "invalid_private_file_mode");
}
