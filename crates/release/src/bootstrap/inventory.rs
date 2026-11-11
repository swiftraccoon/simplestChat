//! The one-host inventory: created without secrets when it is absent, kept
//! byte for byte when it exists, and refused when it could redirect the
//! selected endpoint, identity or host trust.

use super::files::{local_path, metadata, private_parent, protected_file, write_record};
use super::ssh::Target;
use super::{Refusal, Result, Runner, require};
use crate::ops_artifact::capacity_vps::inventory::decode;
use serde_json::{Map, Value, json};
use std::path::Path;
use std::time::Duration;

/// Where a stored path is resolved from.
#[derive(Clone, Copy)]
pub(super) struct Places<'a> {
    pub root: &'a Path,
    pub home: &'a Path,
    pub current: &'a Path,
}

/// The inventory written for a new host: no secrets, both OS-maintenance
/// flags off, and Ansible's SSH pinned to the same endpoint and trust as the
/// controller's own.
pub(super) fn inventory_value(target: &Target, name: &str, revision: &str) -> Value {
    let host = json!({
        "ansible_host": target.host,
        "ansible_user": target.user,
        "ansible_port": target.port,
        "ansible_ssh_private_key_file": target.identity.display().to_string(),
        "ansible_ssh_common_args": target.common(),
        "scbench_ssh_known_hosts_file": target.known_hosts.display().to_string(),
        "scbench_revision": revision,
        "scbench_upgrade_packages": false,
        "scbench_reboot": false,
    });
    json!({"benchmark_hosts": {"hosts": {name: host}}})
}

/// Creates the ignored inventory, or validates an existing one without
/// changing it, and returns the selected host's variables.
pub(super) fn prepare_inventory(
    runner: &dyn Runner,
    path: &Path,
    target: &Target,
    name: &str,
    revision: &str,
    places: Places<'_>,
) -> Result<Map<String, Value>> {
    let directory = places
        .root
        .canonicalize()
        .map_err(metadata)?
        .join("ops/ansible");
    let local = path.file_name().and_then(|name| name.to_str());
    let local = local.is_some_and(|name| name.starts_with("inventory.local."));
    let placed = local && path.parent() == Some(directory.as_path());
    require(placed, "inventory_must_use_ignored_local_name")?;
    let shown = path.display().to_string();
    let ignored = ["/usr/bin/git", "check-ignore", "-q", "--", &shown];
    let ignored = runner.run(&ignored.map(String::from), Duration::from_secs(30))?;
    require(ignored.status == 0, "inventory_not_ignored")?;
    if path.symlink_metadata().is_err() {
        write_record(path, &inventory_value(target, name, revision))?;
    }
    private_parent(path)?;
    let stored = protected_file(path, true)?;
    let host = decode(&stored, name, revision).map_err(|_| Refusal("invalid_inventory"))?;
    let text = |key: &str| host.get(key).and_then(Value::as_str);
    let same =
        text("ansible_host") == Some(&target.host) && text("ansible_user") == Some(&target.user);
    require(same, "inventory_identity_mismatch")?;
    let port = host.get("ansible_port").map_or(Some(22), Value::as_u64);
    require(
        port == Some(u64::from(target.port)),
        "inventory_port_mismatch",
    )?;
    let resolved = |stored: &str| local_path(stored, places.home, places.current).ok();
    let identity = text("ansible_ssh_private_key_file").and_then(resolved);
    require(
        identity.as_ref() == Some(&target.identity),
        "inventory_key_mismatch",
    )?;
    let known = match host.get("scbench_ssh_known_hosts_file") {
        None => resolved("~/.ssh/known_hosts"),
        Some(stored) => stored.as_str().and_then(resolved),
    };
    require(
        known.as_ref() == Some(&target.known_hosts),
        "inventory_known_hosts_mismatch",
    )?;
    let expected = target.common();
    let arguments = match host.get("ansible_ssh_common_args") {
        None => Some(""),
        Some(stored) => stored.as_str(),
    };
    let pinned = arguments.is_some_and(|stored| stored.is_empty() || stored == expected);
    require(pinned, "unsupported_inventory_ssh_args")?;
    let connection = match host.get("ansible_connection") {
        None => Some("ssh"),
        Some(stored) => stored.as_str(),
    };
    require(
        connection == Some("ssh"),
        "unsupported_inventory_connection",
    )?;
    Ok(host)
}

#[cfg(test)]
#[path = "inventory_tests.rs"]
mod tests;
