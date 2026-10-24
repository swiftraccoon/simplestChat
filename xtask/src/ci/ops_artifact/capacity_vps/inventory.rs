//! One static protected inventory. Mature YAML token/scalar handling preserves
//! the reference's small maps/scalars domain without executing Ansible inventory.

use super::super::namespace::{Budget, Chain};
use super::{Result, failure, files::Original, worker::Checkpoint};
use serde::{
    Deserialize, Deserializer,
    de::{self, MapAccess, Visitor},
};
use serde_json::{Map, Value};
use std::{
    collections::BTreeMap,
    fmt,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

#[derive(Clone)]
pub(super) struct Target {
    pub host: String,
    pub user: String,
    pub port: u16,
    pub identity: PathBuf,
    pub known_hosts: PathBuf,
}
impl fmt::Debug for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PrivateVpsSshTarget")
    }
}
pub(super) struct Inventory {
    pub target: Target,
    pub snapshot: Vec<u8>,
    originals: Vec<Original>,
    budget: Budget,
}

enum Node {
    Nonfinite,
    String(String),
    Scalar(Value),
    Object(BTreeMap<String, Node>),
}
struct NodeVisitor;
impl<'de> Visitor<'de> for NodeVisitor {
    type Value = Node;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("bounded inventory scalar or mapping")
    }
    fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Node, E> {
        Ok(Node::String(v.into()))
    }
    fn visit_string<E: de::Error>(self, v: String) -> std::result::Result<Node, E> {
        Ok(Node::String(v))
    }
    fn visit_bool<E: de::Error>(self, v: bool) -> std::result::Result<Node, E> {
        Ok(Node::Scalar(Value::Bool(v)))
    }
    fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<Node, E> {
        Ok(Node::Scalar(v.into()))
    }
    fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Node, E> {
        Ok(Node::Scalar(v.into()))
    }
    fn visit_f64<E: de::Error>(self, v: f64) -> std::result::Result<Node, E> {
        Ok(serde_json::Number::from_f64(v)
            .map_or(Node::Nonfinite, |v| Node::Scalar(Value::Number(v))))
    }
    fn visit_unit<E: de::Error>(self) -> std::result::Result<Node, E> {
        Ok(Node::Scalar(Value::Null))
    }
    fn visit_none<E: de::Error>(self) -> std::result::Result<Node, E> {
        Ok(Node::Scalar(Value::Null))
    }
    fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> std::result::Result<Node, M::Error> {
        let mut result = BTreeMap::new();
        while let Some(key) = map.next_key::<Node>()? {
            let Node::String(key) = key else {
                return Err(de::Error::custom("nonstring inventory key"));
            };
            let value = map.next_value()?;
            if result.insert(key, value).is_some() {
                return Err(de::Error::custom("duplicate inventory key"));
            }
        }
        Ok(Node::Object(result))
    }
}
impl<'de> Deserialize<'de> for Node {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        d.deserialize_any(NodeVisitor)
    }
}
impl Node {
    fn object(&self) -> Result<&BTreeMap<String, Node>> {
        if let Self::Object(v) = self {
            Ok(v)
        } else {
            Err(failure(1))
        }
    }
    fn check(&self, depth: usize) -> Result<()> {
        if depth > 4 {
            return Err(failure(1));
        }
        match self {
            Self::String(v) if ["{{", "{%", "{#"].iter().any(|marker| v.contains(marker)) => {
                Err(failure(1))
            }
            Self::Object(v) => {
                for child in v.values() {
                    child.check(depth + 1)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
    fn value(&self, depth: usize) -> Result<Value> {
        if depth > 4 {
            return Err(failure(1));
        }
        match self {
            Self::String(v) if !["{{", "{%", "{#"].iter().any(|marker| v.contains(marker)) => {
                Ok(Value::String(v.clone()))
            }
            Self::Scalar(v) => Ok(v.clone()),
            Self::Object(v) => v
                .iter()
                .map(|(k, v)| Ok((k.clone(), v.value(depth + 1)?)))
                .collect::<Result<Map<_, _>>>()
                .map(Value::Object),
            _ => Err(failure(1)),
        }
    }
}
pub(super) fn decode(bytes: &[u8], name: &str, revision: &str) -> Result<Map<String, Value>> {
    if bytes.is_empty() || bytes.len() > 65536 {
        return Err(failure(1));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| failure(1))?;
    use serde_saphyr::granit_parser::{Scanner, StrInput, TokenType};
    for token in Scanner::new(StrInput::new(text)) {
        let token = token.map_err(|_| failure(1))?;
        if matches!(
            token.token_type(),
            TokenType::Alias(_) | TokenType::Anchor(_) | TokenType::Tag(_, _)
        ) {
            return Err(failure(1));
        }
    }
    let config = serde_saphyr::options! {
        duplicate_keys:serde_saphyr::DuplicateKeyPolicy::Error,
        merge_keys:serde_saphyr::MergeKeyPolicy::Error,
        strict_booleans:false,reject_unsupported_tags:true,
        non_finite_float_policy:serde_saphyr::NonFiniteFloatPolicy::PassThrough,
        budget:serde_saphyr::budget!{
            max_reader_input_bytes:Some(65536),max_events:16384,max_nodes:8192,max_depth:8,
            flow_nesting_limit:8,max_aliases:0,max_anchors:0,max_recorded_anchor_events:0,
            max_recorded_anchor_bytes:0,max_total_scalar_bytes:65536,max_merge_keys:0,
            max_documents:1,max_inclusion_depth:0,
        },
    };
    let node: Node =
        serde_saphyr::from_slice_with_options(bytes, config).map_err(|_| failure(1))?;
    node.check(0)?;
    let outer = node.object()?;
    if outer.len() != 1 {
        return Err(failure(1));
    }
    let group = outer
        .get("benchmark_hosts")
        .ok_or_else(|| failure(1))?
        .object()?;
    if group.len() != 1 {
        return Err(failure(1));
    }
    let hosts = group.get("hosts").ok_or_else(|| failure(1))?.object()?;
    if hosts.len() != 1 {
        return Err(failure(1));
    }
    let selected = hosts.get(name).ok_or_else(|| failure(1))?.object()?;
    let mut host = Map::new();
    for (key, value) in selected {
        // The exact revision is already an immutable explicit --extra-vars
        // override. Retain operator bytes separately; only this generated
        // execution inventory field is canonicalized before JSON conversion.
        host.insert(
            key.clone(),
            if key == "scbench_revision" {
                Value::String(revision.into())
            } else {
                value.value(4)?
            },
        );
    }
    const ALLOWED: &[&str] = &[
        "ansible_host",
        "ansible_user",
        "ansible_port",
        "ansible_connection",
        "ansible_ssh_private_key_file",
        "ansible_ssh_common_args",
        "scbench_ssh_known_hosts_file",
        "scbench_revision",
        "scbench_root",
        "scbench_upgrade_packages",
        "scbench_reboot",
        "scpub_enabled",
        "scpub_port_mbps",
        "scpub_transfer_allowance_tb",
    ];
    if host.keys().any(|key| !ALLOWED.contains(&key.as_str())) {
        return Err(failure(1));
    }
    for name in [
        "scpub_enabled",
        "scbench_upgrade_packages",
        "scbench_reboot",
    ] {
        if host
            .get(name)
            .is_some_and(|value| value != &Value::Bool(false))
        {
            return Err(failure(1));
        }
    }
    for name in ["scpub_port_mbps", "scpub_transfer_allowance_tb"] {
        if host.get(name).is_some_and(|v| {
            v.as_f64()
                .is_none_or(|v| !v.is_finite() || !(0.0..=1_000_000.0).contains(&v))
        }) {
            return Err(failure(1));
        }
    }
    Ok(host)
}
fn selected(path: &str, caller: &Path, home: &Path) -> Result<PathBuf> {
    let path = if let Some(tail) = path.strip_prefix("~/") {
        home.join(tail)
    } else {
        caller.join(path)
    };
    if path
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        return Err(failure(1));
    }
    let path = path.canonicalize().map_err(|_| failure(1))?;
    if !super::options::path(&path) {
        return Err(failure(1));
    }
    Ok(path)
}
fn protected(
    path: &Path,
    secret: bool,
    budget: &Budget,
    checkpoint: &Checkpoint,
) -> Result<Original> {
    let directory = Chain::open(path.parent().ok_or_else(|| failure(1))?, budget)?;
    let admitted = (|| {
        let meta = directory.file().metadata().map_err(|_| failure(1))?;
        if meta.uid() != rustix::process::getuid().as_raw() || meta.mode() & 0o022 != 0 {
            return Err(failure(1));
        }
        let (original, _) = Original::open(path, 65536, budget, checkpoint)?;
        if secret {
            original.require_private()?;
        }
        // Public host trust still cannot be group/world-writable.
        let meta = std::fs::symlink_metadata(path).map_err(|_| failure(1))?;
        if meta.uid() != rustix::process::getuid().as_raw() || meta.mode() & 0o022 != 0 {
            return Err(failure(1));
        }
        Ok(original)
    })();
    let closed = directory.finish();
    admitted.and_then(|v| closed.map(|()| v))
}
impl Inventory {
    pub fn open(
        path: &Path,
        name: &str,
        revision: &str,
        caller: &Path,
        home: &Path,
        budget: &Budget,
        checkpoint: &Checkpoint,
    ) -> Result<Self> {
        let inventory = protected(path, true, budget, checkpoint)?;
        let bytes = inventory.read(budget, checkpoint)?;
        let mut variables = decode(&bytes, name, revision)?;
        let text = |key: &str, default: Option<&str>| -> Result<String> {
            variables.get(key).map_or_else(
                || default.map(str::to_owned).ok_or_else(|| failure(1)),
                |v| v.as_str().map(str::to_owned).ok_or_else(|| failure(1)),
            )
        };
        let host = text("ansible_host", Some(name))?;
        let user = text("ansible_user", None)?;
        let port = variables
            .get("ansible_port")
            .map_or(Some(22), Value::as_u64)
            .filter(|n| (1..=65535).contains(n))
            .ok_or_else(|| failure(1))?;
        if host.is_empty()
            || host.len() > 253
            || !host.as_bytes()[0].is_ascii_alphanumeric()
            || !host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".:-".contains(&b))
            || user.is_empty()
            || user.len() > 32
            || !matches!(user.as_bytes()[0], b'a'..=b'z' | b'_')
            || !user
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b))
            || text("ansible_connection", Some("ssh"))? != "ssh"
            || text("scbench_root", Some("/srv/simplestchat-bench"))? != "/srv/simplestchat-bench"
        {
            return Err(failure(1));
        }
        let identity = selected(&text("ansible_ssh_private_key_file", None)?, caller, home)?;
        if identity.starts_with(caller) {
            return Err(failure(1));
        }
        let default_known = home.join(".ssh/known_hosts");
        let known_hosts = selected(
            &text("scbench_ssh_known_hosts_file", default_known.to_str())?,
            caller,
            home,
        )?;
        let target = Target {
            host,
            user,
            port: port as u16,
            identity,
            known_hosts,
        };
        let common = target.common();
        if variables
            .get("ansible_ssh_common_args")
            .is_some_and(|value| value.as_str().is_none_or(|v| !v.is_empty() && v != common))
        {
            return Err(failure(1));
        }
        let key = protected(&target.identity, true, budget, checkpoint)?;
        let trust = protected(&target.known_hosts, false, budget, checkpoint)?;
        variables.insert("ansible_host".into(), target.host.clone().into());
        variables.insert("ansible_user".into(), target.user.clone().into());
        variables.insert("ansible_port".into(), (target.port as u64).into());
        variables.insert(
            "ansible_ssh_private_key_file".into(),
            target.identity.to_string_lossy().into_owned().into(),
        );
        variables.insert("ansible_ssh_common_args".into(), common.into());
        let snapshot =
            serde_json::to_vec(&serde_json::json!({"benchmark_hosts":{"hosts":{name:variables}}}))
                .map_err(|_| failure(1))?;
        Ok(Self {
            target,
            snapshot,
            originals: vec![inventory, key, trust],
            budget: budget.clone(),
        })
    }
    pub fn check(&self, checkpoint: &Checkpoint) -> Result<()> {
        for original in &self.originals {
            original.check(&self.budget, checkpoint)?;
        }
        Ok(())
    }
}
pub(super) fn quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(&b))
    {
        return value.into();
    }
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}
impl Target {
    pub fn common(&self) -> String {
        [
            "-F".into(),
            "/dev/null".into(),
            "-o".into(),
            "StrictHostKeyChecking=yes".into(),
            "-o".into(),
            format!("UserKnownHostsFile={}", self.known_hosts.display()),
            "-o".into(),
            "GlobalKnownHostsFile=/dev/null".into(),
            "-o".into(),
            "IdentityAgent=none".into(),
            "-o".into(),
            "ForwardAgent=no".into(),
            "-o".into(),
            "IdentitiesOnly=yes".into(),
            "-o".into(),
            "PasswordAuthentication=no".into(),
            "-o".into(),
            "KbdInteractiveAuthentication=no".into(),
            "-o".into(),
            "BatchMode=yes".into(),
            "-o".into(),
            "ControlMaster=no".into(),
            "-o".into(),
            "ControlPath=none".into(),
        ]
        .iter()
        .map(|v| quote(v))
        .collect::<Vec<_>>()
        .join(" ")
    }
    pub fn ssh(&self, bootstrap: &str) -> Vec<std::ffi::OsString> {
        let remote = if self.user == "root" {
            format!("/usr/bin/python3 -I -B -c {}", quote(bootstrap))
        } else {
            format!("sudo -n -- /usr/bin/python3 -I -B -c {}", quote(bootstrap))
        };
        let mut args = vec![
            "-F".into(),
            "/dev/null".into(),
            "-p".into(),
            self.port.to_string(),
            "-i".into(),
            self.identity.to_string_lossy().into_owned(),
        ];
        for value in [
            "IdentitiesOnly=yes",
            "IdentityAgent=none",
            "ForwardAgent=no",
            "ClearAllForwardings=yes",
            "ControlMaster=no",
            "ControlPath=none",
            "StrictHostKeyChecking=yes",
            "UpdateHostKeys=no",
            "GlobalKnownHostsFile=/dev/null",
            "ConnectTimeout=15",
            "ConnectionAttempts=1",
            "ServerAliveInterval=10",
            "ServerAliveCountMax=3",
            "BatchMode=yes",
            "PreferredAuthentications=publickey",
            "PasswordAuthentication=no",
            "KbdInteractiveAuthentication=no",
        ] {
            args.extend(["-o".into(), value.into()]);
        }
        args.extend([
            "-o".into(),
            format!("UserKnownHostsFile={}", self.known_hosts.display()),
            "-T".into(),
            format!("{}@{}", self.user, self.host),
            remote,
        ]);
        args.into_iter().map(Into::into).collect()
    }
}
