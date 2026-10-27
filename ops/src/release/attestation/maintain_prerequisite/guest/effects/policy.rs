//! Fixed daemon observations and containment policy for the owned fixture.

use super::super::{Error, Result};
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    net::Ipv4Addr,
};

pub(super) const LABEL: &str = "simplestchat.maintain-prerequisite";
pub(super) const PROJECT: &str = "simplestchat-public";
pub(super) const CONFIG: &str = "/etc/simplestchat-public";
pub(super) const PUBLIC: &str = "/srv/simplestchat-public";

pub(super) fn require(value: bool) -> Result<()> {
    if value { Ok(()) } else { Err(Error::Refused) }
}
pub(super) fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub(super) fn object(value: &Value) -> Result<&Map<String, Value>> {
    value.as_object().ok_or(Error::Refused)
}
pub(super) fn string(value: &Value) -> Result<&str> {
    value.as_str().ok_or(Error::Refused)
}
pub(super) fn decode(bytes: &[u8]) -> Result<Value> {
    crate::json::decode_limited(bytes, 2 * 1024 * 1024).map_err(|_| Error::Refused)?;
    serde_json::from_slice(bytes).map_err(|_| Error::Refused)
}
pub(super) fn one(bytes: &[u8]) -> Result<Value> {
    let value = decode(bytes)?;
    let rows = value.as_array().ok_or(Error::Refused)?;
    require(rows.len() == 1)?;
    object(&rows[0])?;
    Ok(rows[0].clone())
}
pub(super) fn ids(bytes: &[u8]) -> Result<BTreeSet<String>> {
    let text = std::str::from_utf8(bytes).map_err(|_| Error::Refused)?;
    let mut values = BTreeSet::new();
    for line in text.lines() {
        require(digest(line) && values.insert(line.to_owned()))?;
    }
    Ok(values)
}
pub(super) fn created_id(bytes: &[u8]) -> Result<String> {
    require(bytes.len() == 65 && bytes[64] == b'\n')?;
    let id = std::str::from_utf8(&bytes[..64]).map_err(|_| Error::Refused)?;
    require(digest(id))?;
    Ok(id.to_owned())
}

/// Passive identity of Docker's predefined null network; grants no cleanup authority.
pub(super) struct NoneNetwork {
    id: String,
    profile: Value,
}
impl NoneNetwork {
    pub fn admit(row: &Value, postgres: &str) -> Result<Self> {
        let id = string(&row["Id"])?;
        require(digest(id) && digest(postgres))?;
        require(
            row["Name"] == "none"
                && row["Driver"] == "null"
                && row["Scope"] == "local"
                && row["EnableIPv4"] == true
                && row["EnableIPv6"] == false
                && row["Internal"] == false
                && row["Attachable"] == false
                && row["Ingress"] == false
                && row["ConfigOnly"] == false
                && row["ConfigFrom"] == json!({"Network":""})
                && row["IPAM"] == json!({"Driver":"default","Options":null,"Config":null})
                && row["Options"] == json!({})
                && row["Labels"] == json!({}),
        )?;
        chrono::DateTime::parse_from_rfc3339(string(&row["Created"])?)
            .map_err(|_| Error::Refused)?;
        let members = object(&row["Containers"])?;
        require(members.len() == 1 && members.contains_key(postgres))?;
        let member = &members[postgres];
        object(member)?;
        require(
            member["Name"] == "simplestchat-public-postgres-1"
                && digest(string(&member["EndpointID"])?),
        )?;
        Ok(Self {
            id: id.to_owned(),
            profile: row.clone(),
        })
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn check(&self, row: &Value) -> Result<()> {
        require(row == &self.profile)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NoneConnection {
    Connected,
    Disconnected,
}

/// Only the launcher's validator and one migration may transiently use null networking.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct NoneHistory(BTreeMap<String, NoneConnection>);
impl NoneHistory {
    pub fn observe(
        &mut self,
        row: &Value,
        network: &NoneNetwork,
        validation: Option<&str>,
        services: &BTreeMap<String, String>,
        traces: &BTreeMap<String, Vec<String>>,
    ) -> Result<()> {
        let attrs = &row["Actor"]["Attributes"];
        let container = string(&attrs["container"])?;
        let migration = services.get(container).is_some_and(|s| s == "migrate")
            && services
                .values()
                .filter(|s| s.as_str() == "migrate")
                .count()
                == 1;
        let validator = validation == Some(container) && !services.contains_key(container);
        require(
            row["Type"] == "network"
                && row["Actor"]["ID"] == network.id
                && object(attrs)?.len() == 3
                && attrs["name"] == "none"
                && attrs["type"] == "null"
                && digest(container)
                && (migration || validator),
        )?;
        let phases = traces.get(container).ok_or(Error::Refused)?;
        let following = match string(&row["Action"])? {
            "connect" => {
                require(
                    phases == &["create"] && !self.0.contains_key(container) && self.0.len() < 2,
                )?;
                NoneConnection::Connected
            }
            "disconnect" => {
                require(
                    phases == &["create", "start"]
                        && self.0.get(container) == Some(&NoneConnection::Connected),
                )?;
                NoneConnection::Disconnected
            }
            _ => return Err(Error::Refused),
        };
        self.0.insert(container.into(), following);
        Ok(())
    }
    pub fn complete(&self, validation: &str, services: &BTreeMap<String, String>) -> Result<()> {
        let migrations = services
            .iter()
            .filter(|(_, service)| service.as_str() == "migrate")
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>();
        require(
            digest(validation)
                && migrations.len() == 1
                && digest(migrations[0])
                && validation != migrations[0]
                && self.0.len() == 2
                && [validation, migrations[0]]
                    .iter()
                    .all(|id| self.0.get(*id) == Some(&NoneConnection::Disconnected)),
        )
    }
}

pub(super) fn validator_role(attributes: &Value, expected_name: &str) -> Result<()> {
    let run = expected_name
        .strip_prefix("scpub-")
        .and_then(|name| name.strip_suffix("-validate"))
        .ok_or(Error::Refused)?;
    require(
        run.len() == 32
            && run
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            && attributes["name"] == expected_name
            && attributes["simplestchat.public.validation"] == run,
    )
}
pub(super) fn server_image(
    row: &Value,
    archive: &crate::release::archive::VerifiedArchive,
    revision: &str,
) -> Result<String> {
    let id = string(&row["Id"])?;
    let config = &row["Config"];
    let saved = decode(archive.config_bytes())?;
    require(
        archive.accepts_runtime_id(id)
            && row["Os"] == "linux"
            && row["Architecture"] == "amd64"
            && config["User"] == "10001:10001"
            && config["Cmd"] == json!(["/app/simplestChat"])
            && (config["Entrypoint"].is_null() || config["Entrypoint"] == json!([]))
            && config["WorkingDir"] == "/app"
            && !truthy(&config["Healthcheck"])
            && config["Labels"]["org.opencontainers.image.revision"] == revision
            && row["RootFS"]["Type"] == "layers"
            && row["RootFS"]["Layers"] == saved["rootfs"]["diff_ids"],
    )?;
    let env = config["Env"].as_array().ok_or(Error::Refused)?;
    let mut names = BTreeSet::new();
    for assignment in env {
        let text = string(assignment)?;
        let name = text.split('=').next().ok_or(Error::Refused)?;
        require(
            !name.is_empty()
                && names.insert(name)
                && name.bytes().enumerate().all(|(i, b)| {
                    b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit())
                })
                && !name.starts_with("LD_")
                && !name.starts_with("OPENSSL_")
                && !["GLIBC_TUNABLES", "GCONV_PATH", "LOCPATH"].contains(&name),
        )?;
    }
    Ok(id.to_owned())
}
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::Number(v) => v.as_f64().is_none_or(|n| n != 0.0),
        Value::String(v) => !v.is_empty(),
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
    }
}
pub(super) fn bridge(row: &Value, id: &str, name: &str) -> Result<()> {
    require(
        digest(id)
            && row["Id"] == id
            && row["Name"] == name
            && row["Driver"] == "bridge"
            && row["Scope"] == "local"
            && row["Internal"] == false
            && row["EnableIPv6"] == false
            && row["Options"]
                == json!({
                    "com.docker.network.bridge.name":name,
                    "com.docker.network.bridge.enable_ip_masquerade":"false"
                }),
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Endpoint {
    pub id: String,
    pub peers: BTreeSet<String>,
    pub address: Ipv4Addr,
    pub prefix: u8,
    pub endpoint_id: String,
    pub subnet: String,
    pub gateway: Ipv4Addr,
}
fn canonical_ip(value: &Value) -> Result<Ipv4Addr> {
    let text = string(value)?;
    let address = text.parse::<Ipv4Addr>().map_err(|_| Error::Refused)?;
    require(address.to_string() == text)?;
    Ok(address)
}
pub(super) fn endpoint(
    id: &str,
    peers: &BTreeSet<String>,
    row: &Value,
    network: &Value,
    network_id: &str,
    bridge_name: &str,
) -> Result<Endpoint> {
    bridge(network, network_id, bridge_name)?;
    require(peers.contains(id) && digest(id) && row["Id"] == id)?;
    let memberships = object(&row["NetworkSettings"]["Networks"])?;
    require(memberships.len() == 1 && memberships.contains_key(bridge_name))?;
    let member = &memberships[bridge_name];
    require(member["NetworkID"] == network_id)?;
    let configs = network["IPAM"]["Config"].as_array().ok_or(Error::Refused)?;
    require(configs.len() == 1)?;
    let subnet = string(&configs[0]["Subnet"])?;
    let (base, prefix_text) = subnet.split_once('/').ok_or(Error::Refused)?;
    let prefix = prefix_text.parse::<u8>().map_err(|_| Error::Refused)?;
    require(prefix <= 32 && prefix.to_string() == prefix_text)?;
    let base = base.parse::<Ipv4Addr>().map_err(|_| Error::Refused)?;
    let mask = u32::MAX.checked_shl(u32::from(32 - prefix)).unwrap_or(0);
    let network_number = u32::from(base);
    require(network_number & mask == network_number && format!("{base}/{prefix}") == subnet)?;
    let private = [
        (0x0a00_0000_u32, 8_u8),
        (0xac10_0000, 12),
        (0xc0a8_0000, 16),
    ]
    .iter()
    .any(|&(start, bits)| prefix >= bits && network_number >> (32 - bits) == start >> (32 - bits));
    let address = canonical_ip(&member["IPAddress"])?;
    let gateway = canonical_ip(&configs[0]["Gateway"])?;
    require(
        private
            && member["IPPrefixLen"].as_u64() == Some(u64::from(prefix))
            && u32::from(address) & mask == network_number
            && u32::from(gateway) & mask == network_number
            && address != base
            && u32::from(address) != (network_number | !mask)
            && address != gateway
            && !address.is_loopback()
            && !address.is_link_local()
            && !address.is_multicast()
            && !address.is_unspecified()
            && address != Ipv4Addr::new(10, 0, 2, 2),
    )?;
    require(canonical_ip(&member["Gateway"])? == gateway)?;
    let actual = object(&network["Containers"])?;
    require(actual.keys().cloned().collect::<BTreeSet<_>>() == *peers)?;
    let endpoint_id = string(&member["EndpointID"])?;
    require(
        digest(endpoint_id)
            && actual[id]["EndpointID"] == endpoint_id
            && actual[id]["IPv4Address"] == format!("{address}/{prefix}"),
    )?;
    let state = &row["State"];
    require(
        state["Running"] == true
            && state["Restarting"] == false
            && state["Paused"] == false
            && state["Dead"] == false
            && state["OOMKilled"] == false
            && state["Status"] == "running"
            && state["Pid"]
                .as_u64()
                .is_some_and(|pid| (1..=2_147_483_647).contains(&pid)),
    )?;
    Ok(Endpoint {
        id: id.to_owned(),
        peers: peers.clone(),
        address,
        prefix,
        endpoint_id: endpoint_id.to_owned(),
        subnet: subnet.to_owned(),
        gateway,
    })
}

pub(super) fn proxy_profile(
    row: &Value,
    id: &str,
    image_id: &str,
    image_ref: &str,
    bridge: &str,
) -> Result<()> {
    let host = &row["HostConfig"];
    let config = &row["Config"];
    require(
        row["Id"] == id
            && row["Image"] == image_id
            && config["User"] == "10001:10001"
            && config["Image"] == image_ref
            && host["ReadonlyRootfs"] == true
            && host["CapDrop"] == json!(["ALL"])
            && host["CapAdd"] == json!(["CAP_NET_BIND_SERVICE"])
            && host["Init"] == true
            && host["Memory"].as_u64() == Some(256 * 1024 * 1024)
            && host["PidsLimit"].as_u64() == Some(128)
            && host["NanoCpus"].as_u64() == Some(500_000_000),
    )?;
    require(
        host["SecurityOpt"]
            .as_array()
            .ok_or(Error::Refused)?
            .iter()
            .any(|v| v == "no-new-privileges"),
    )?;
    let expected = BTreeMap::from([
        (
            "/etc/caddy/Caddyfile",
            (format!("{CONFIG}/Caddyfile"), false),
        ),
        ("/data", (format!("{PUBLIC}/caddy-data"), true)),
        ("/config", (format!("{PUBLIC}/caddy-config"), true)),
        (
            "/run/simplestchat-browser-assets",
            (format!("{PUBLIC}/browser-assets"), false),
        ),
    ]);
    let mounts = row["Mounts"].as_array().ok_or(Error::Refused)?;
    require(mounts.len() == expected.len())?;
    let mut seen = BTreeSet::new();
    for mount in mounts {
        let destination = string(&mount["Destination"])?;
        let (source, writable) = expected.get(destination).ok_or(Error::Refused)?;
        require(
            seen.insert(destination)
                && mount["Type"] == "bind"
                && mount["Source"] == source.as_str()
                && mount["RW"].as_bool() == Some(*writable),
        )?;
    }
    require(
        host["NetworkMode"] == bridge
            && host["RestartPolicy"] == json!({"Name":"unless-stopped","MaximumRetryCount":0})
            && host["PortBindings"]
                == json!({
                    "80/tcp":[{"HostIp":"127.0.0.1","HostPort":"80"}],
                    "443/tcp":[{"HostIp":"127.0.0.1","HostPort":"443"}],
                    "443/udp":[{"HostIp":"127.0.0.1","HostPort":"443"}]
                }),
    )
}

pub(super) fn firewall(table: &str, interface: &str, bridge: &str) -> Result<String> {
    let suffix = table.strip_prefix("sc_maintain_").ok_or(Error::Refused)?;
    require(
        suffix.len() == 12
            && suffix
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
    )?;
    for name in [interface, bridge] {
        require(
            !name.is_empty()
                && name.len() <= 15
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)),
        )?;
    }
    Ok(format!(
        concat!(
            "table inet {table} {{\n counter blocked_output {{ }}\n",
            " counter blocked_forward {{ }}\n counter ssh_control {{ }}\n",
            " chain output {{ type filter hook output priority -1; policy drop;\n",
            "  oifname \"lo\" accept\n  oifname \"{bridge}\" accept\n",
            "  oifname \"{interface}\" ip daddr 10.0.2.2 tcp sport 22 ct state established ",
            "counter name ssh_control accept\n  counter name blocked_output drop\n }}\n",
            " chain forward {{ type filter hook forward priority -1; policy drop;\n",
            "  iifname \"{bridge}\" oifname \"{bridge}\" accept\n",
            "  counter name blocked_forward drop\n }}\n}}\n",
            "table netdev {table} {{\n counter blocked_link {{ }}\n",
            " chain egress {{ type filter hook egress device \"{interface}\" ",
            "priority 0; policy drop;\n  ip daddr 10.0.2.2 tcp sport 22 accept\n",
            "  arp daddr ip 10.0.2.2 accept\n  counter name blocked_link drop\n }}\n}}\n",
        ),
        table = table,
        interface = interface,
        bridge = bridge
    ))
}
pub(super) fn blocked_counter(bytes: &[u8], table: &str) -> Result<(u64, u64)> {
    let value = decode(bytes)?;
    let rows = value["nftables"].as_array().ok_or(Error::Refused)?;
    let counters = rows
        .iter()
        .filter_map(|r| r.get("counter"))
        .filter(|r| r["name"] == "blocked_output")
        .collect::<Vec<_>>();
    require(counters.len() == 1)?;
    let row = counters[0];
    require(row["family"] == "inet" && row["table"] == table)?;
    Ok((
        row["packets"].as_u64().ok_or(Error::Refused)?,
        row["bytes"].as_u64().ok_or(Error::Refused)?,
    ))
}

pub(super) fn pg_exec_history(
    rows: &[Value],
    commands: &BTreeMap<String, usize>,
    health: &str,
) -> Result<()> {
    let mut executions: BTreeMap<String, (String, Vec<&str>)> = BTreeMap::new();
    for row in rows {
        let action = string(&row["Action"])?;
        if action == "health_status: healthy" {
            continue;
        }
        let attrs = &row["Actor"]["Attributes"];
        let id = string(&attrs["execID"])?;
        require(digest(id))?;
        if let Some(command) = action.strip_prefix("exec_create: ") {
            require(
                !executions.contains_key(id)
                    && (commands.contains_key(command) || command == health),
            )?;
            executions.insert(id.to_owned(), (command.to_owned(), vec!["create"]));
        } else {
            let (command, phases) = executions.get_mut(id).ok_or(Error::Refused)?;
            if action == format!("exec_start: {command}") {
                require(phases == &["create"])?;
                phases.push("start");
            } else {
                require(
                    action == "exec_die"
                        && phases == &["create", "start"]
                        && attrs["exitCode"] == "0",
                )?;
                phases.push("die");
            }
        }
    }
    require(
        executions
            .values()
            .all(|(_, phases)| phases == &["create", "start", "die"]),
    )?;
    for (command, count) in commands {
        require(
            executions
                .values()
                .filter(|(actual, _)| actual == command)
                .count()
                == *count,
        )?;
    }
    Ok(())
}

pub(super) fn normal_arguments(image: &str, bridge: &str, run_id: &str) -> Vec<String> {
    [
        "--label".into(),
        format!("{LABEL}={run_id}"),
        "--pull".into(),
        "never".into(),
        "--network".into(),
        bridge.into(),
        "--user".into(),
        "10001:10001".into(),
        "--init".into(),
        "--restart".into(),
        "unless-stopped".into(),
        "--stop-timeout".into(),
        "30".into(),
        "--read-only".into(),
        "--cap-drop".into(),
        "ALL".into(),
        "--cap-add".into(),
        "NET_BIND_SERVICE".into(),
        "--security-opt".into(),
        "no-new-privileges".into(),
        "--memory".into(),
        "256m".into(),
        "--cpus".into(),
        "0.5".into(),
        "--pids-limit".into(),
        "128".into(),
        "--sysctl".into(),
        "net.ipv4.ip_unprivileged_port_start=0".into(),
        "--log-driver".into(),
        "local".into(),
        "--log-opt".into(),
        "max-size=10m".into(),
        "--log-opt".into(),
        "max-file=3".into(),
        "--env-file".into(),
        format!("{CONFIG}/proxy.env"),
        "--mount".into(),
        format!("type=bind,src={CONFIG}/Caddyfile,dst=/etc/caddy/Caddyfile,readonly"),
        "--mount".into(),
        format!("type=bind,src={PUBLIC}/caddy-data,dst=/data"),
        "--mount".into(),
        format!("type=bind,src={PUBLIC}/caddy-config,dst=/config"),
        "--mount".into(),
        format!(
            "type=bind,src={PUBLIC}/browser-assets,dst=/run/simplestchat-browser-assets,readonly"
        ),
        "--tmpfs".into(),
        "/tmp:rw,nosuid,nodev,noexec,size=32m,mode=1777".into(),
        "--publish".into(),
        "127.0.0.1:443:443/tcp".into(),
        "--publish".into(),
        "127.0.0.1:443:443/udp".into(),
        "--publish".into(),
        "127.0.0.1:80:80/tcp".into(),
        image.into(),
    ]
    .into()
}
