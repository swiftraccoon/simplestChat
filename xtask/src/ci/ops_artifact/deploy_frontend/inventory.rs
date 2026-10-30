//! Bounded inventory selection before any remote effect. The selected complete
//! host variables stay private so the caller can detect drift and freeze them.
//! API review: C-SERDE/C-VALIDATE reject duplicate inventory keys and ambiguous
//! destinations; C-COMMON-TRAITS deliberately redacts target Debug output.

use crate::tooling::{Failure, Result};
use serde::{
    Deserialize, Deserializer,
    de::{Error, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Map, Number, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    path::{Component, Path, PathBuf},
};

const RESPONSE: usize = 4 * 1024 * 1024;
const MEMBERS: usize = 1024;
const DEPTH: usize = 32;

#[derive(Clone, PartialEq)]
pub(super) struct Target {
    pub host: String,
    pub variables: Value,
}
impl fmt::Debug for Target {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PinnedReleaseFrontendTarget")
    }
}
fn refusal(code: &'static str) -> Failure {
    Failure::new(1, code)
}

/// Delegate JSON syntax, number grammar and recursion limits to serde_json;
/// only duplicate object-key rejection is added to its Value visitor.
struct Unique(Value);
impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct InventoryVisitor;
        impl<'de> Visitor<'de> for InventoryVisitor {
            type Value = Unique;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("inventory JSON without duplicate keys")
            }
            fn visit_bool<E: Error>(self, value: bool) -> std::result::Result<Unique, E> {
                Ok(Unique(Value::Bool(value)))
            }
            fn visit_i64<E: Error>(self, value: i64) -> std::result::Result<Unique, E> {
                Ok(Unique(Value::Number(value.into())))
            }
            fn visit_u64<E: Error>(self, value: u64) -> std::result::Result<Unique, E> {
                Ok(Unique(Value::Number(value.into())))
            }
            fn visit_f64<E: Error>(self, value: f64) -> std::result::Result<Unique, E> {
                Number::from_f64(value)
                    .map(|number| Unique(Value::Number(number)))
                    .ok_or_else(|| E::custom("invalid inventory number"))
            }
            fn visit_str<E: Error>(self, value: &str) -> std::result::Result<Unique, E> {
                Ok(Unique(Value::String(value.into())))
            }
            fn visit_string<E: Error>(self, value: String) -> std::result::Result<Unique, E> {
                Ok(Unique(Value::String(value)))
            }
            fn visit_unit<E: Error>(self) -> std::result::Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            fn visit_none<E: Error>(self) -> std::result::Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> std::result::Result<Unique, A::Error> {
                let mut values = Vec::new();
                while let Some(Unique(value)) = sequence.next_element()? {
                    values.push(value);
                }
                Ok(Unique(Value::Array(values)))
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Unique, A::Error> {
                let mut values = Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if values.contains_key(&key) {
                        return Err(A::Error::custom("duplicate inventory key"));
                    }
                    let Unique(value) = map.next_value()?;
                    values.insert(key, value);
                }
                Ok(Unique(Value::Object(values)))
            }
        }
        deserializer.deserialize_any(InventoryVisitor)
    }
}

fn object(value: Option<&Value>) -> Result<&Map<String, Value>> {
    value
        .and_then(Value::as_object)
        .ok_or_else(|| refusal("invalid_inventory_graph"))
}
fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
}
fn names(value: Option<&Value>, groups: bool) -> Result<Vec<&str>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let values = value
        .as_array()
        .ok_or_else(|| refusal("invalid_inventory_graph"))?;
    if values.len() > MEMBERS {
        return Err(refusal("invalid_inventory_graph"));
    }
    let mut unique = BTreeSet::new();
    values
        .iter()
        .map(|value| {
            let name = value
                .as_str()
                .ok_or_else(|| refusal("invalid_inventory_graph"))?;
            let valid = identifier(name)
                || (groups
                    && name.starts_with('_')
                    && name.len() <= 253
                    && name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte)));
            if !valid || !unique.insert(name) {
                return Err(refusal("invalid_inventory_graph"));
            }
            Ok(name)
        })
        .collect()
}

struct Graph<'a> {
    root: &'a Map<String, Value>,
    active: BTreeSet<String>,
    completed: BTreeMap<String, BTreeSet<String>>,
}
impl Graph<'_> {
    fn hosts(&mut self, name: &str, depth: usize) -> Result<BTreeSet<String>> {
        if depth > DEPTH || self.active.contains(name) {
            return Err(refusal("invalid_inventory_graph"));
        }
        if let Some(hosts) = self.completed.get(name) {
            return Ok(hosts.clone());
        }
        if self.active.len() + self.completed.len() >= MEMBERS {
            return Err(refusal("invalid_inventory_graph"));
        }
        let root = self.root;
        let group = object(root.get(name))?;
        let mut hosts = names(group.get("hosts"), false)?
            .into_iter()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        let children = names(group.get("children"), true)?;
        self.active.insert(name.into());
        for child in children {
            hosts.extend(self.hosts(child, depth + 1)?);
            if hosts.len() > MEMBERS {
                return Err(refusal("invalid_inventory_graph"));
            }
        }
        self.active.remove(name);
        self.completed.insert(name.into(), hosts.clone());
        Ok(hosts)
    }
}

fn identity_path(value: &str) -> bool {
    let path = Path::new(value);
    path.is_absolute()
        && path.file_name().is_some()
        && value.len() <= 4096
        && !value.chars().any(char::is_control)
        && path.components().count() <= 32
        && path.components().collect::<PathBuf>().as_os_str() == path.as_os_str()
        && path
            .components()
            .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
}
fn port(value: Option<&Value>) -> bool {
    let Some(value) = value else {
        return true;
    };
    let number = value.as_u64().or_else(|| {
        value.as_str().and_then(|text| {
            if text.is_empty()
                || text.len() > 5
                || text.as_bytes()[0] == b'0'
                || !text.bytes().all(|b| b.is_ascii_digit())
            {
                None
            } else {
                text.parse::<u64>().ok()
            }
        })
    });
    number.is_some_and(|number| (1..=65535).contains(&number))
}
fn ssh(variables: &Map<String, Value>) -> Result<()> {
    let host = variables.get("ansible_host").and_then(Value::as_str);
    let user = variables.get("ansible_user").and_then(Value::as_str);
    let identity = variables
        .get("ansible_ssh_private_key_file")
        .and_then(Value::as_str);
    let valid_user = user.is_some_and(|value| {
        !value.is_empty()
            && value.len() <= 32
            && (value.as_bytes()[0].is_ascii_lowercase() || value.as_bytes()[0] == b'_')
            && value.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_-".contains(&byte)
            })
    });
    if variables
        .get("ansible_connection")
        .is_some_and(|v| v.as_str() != Some("ssh"))
        || host.is_none_or(|value| !identifier(value))
        || !valid_user
        || identity.is_none_or(|value| !identity_path(value))
        || !port(variables.get("ansible_port"))
        || [
            "ansible_password",
            "ansible_ssh_pass",
            "ansible_ssh_password",
            "ansible_become_password",
            "ansible_become_pass",
        ]
        .iter()
        .any(|key| variables.contains_key(*key))
        || ["ansible_ssh_common_args", "ansible_ssh_extra_args"]
            .iter()
            .any(|key| {
                variables
                    .get(*key)
                    .is_some_and(|value| value.as_str() != Some(""))
            })
    {
        return Err(refusal("unsupported_inventory_ssh"));
    }
    Ok(())
}

/// Resolve only the benchmark-host closure, choosing one exact alias. The caller
/// repeats this operation after waiting and compares the complete returned value;
/// it must dispatch a private one-host inventory, never the original graph.
pub(super) fn selected(bytes: &[u8], limit: Option<&str>, origin_host: &str) -> Result<Target> {
    if bytes.is_empty() || bytes.len() > RESPONSE || limit.is_some_and(|value| !identifier(value)) {
        return Err(refusal("invalid_inventory_graph"));
    }
    let Unique(value) =
        serde_json::from_slice(bytes).map_err(|_| refusal("invalid_inventory_graph"))?;
    let root = object(Some(&value))?;
    let mut graph = Graph {
        root,
        active: BTreeSet::new(),
        completed: BTreeMap::new(),
    };
    let mut hosts = graph.hosts("benchmark_hosts", 0)?;
    if let Some(host) = limit {
        if !hosts.contains(host) {
            return Err(refusal("inventory_target_not_found"));
        }
        hosts = BTreeSet::from([host.into()]);
    }
    if hosts.len() != 1 {
        return Err(refusal("select_exactly_one_inventory_host"));
    }
    let host = hosts
        .into_iter()
        .next()
        .ok_or_else(|| refusal("select_exactly_one_inventory_host"))?;
    let meta = object(root.get("_meta"))?;
    let hostvars = object(meta.get("hostvars"))?;
    let variables = object(hostvars.get(&host))?;
    if origin_host.is_empty()
        || variables.get("scpub_domain").and_then(Value::as_str) != Some(origin_host)
    {
        return Err(refusal("inventory_smoke_origin_mismatch"));
    }
    ssh(variables)?;
    Ok(Target {
        host,
        variables: Value::Object(variables.clone()),
    })
}

#[cfg(test)]
#[path = "inventory_tests.rs"]
mod tests;
