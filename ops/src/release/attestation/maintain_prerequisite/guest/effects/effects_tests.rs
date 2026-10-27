use super::{Error, policy};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

fn id(byte: char) -> String {
    std::iter::repeat_n(byte, 64).collect()
}
fn endpoint_rows() -> (Value, Value) {
    let container = id('a');
    let network = id('b');
    let endpoint = id('c');
    let row = json!({"Id":container,"NetworkSettings":{"Networks":{"sc0123456789ab":{
        "NetworkID":network,"IPAddress":"172.20.0.2","IPPrefixLen":16,"Gateway":"172.20.0.1","EndpointID":endpoint}}},
        "State":{"Running":true,"Restarting":false,"Paused":false,"Dead":false,"OOMKilled":false,"Status":"running","Pid":123}});
    let network = json!({"Id":id('b'),"Name":"sc0123456789ab","Driver":"bridge","Scope":"local","Internal":true,"EnableIPv6":false,
        "Options":{"com.docker.network.bridge.name":"sc0123456789ab"},"IPAM":{"Config":[{"Subnet":"172.20.0.0/16","Gateway":"172.20.0.1"}]},
        "Containers":{(container):{"EndpointID":id('c'),"IPv4Address":"172.20.0.2/16"}}});
    (row, network)
}
fn endpoint(row: &Value, network: &Value) -> super::Result<policy::Endpoint> {
    policy::endpoint(
        &id('a'),
        &BTreeSet::from([id('a')]),
        row,
        network,
        &id('b'),
        "sc0123456789ab",
    )
}

#[test]
fn strict_daemon_json_and_raw_zero_identifiers_refuse_ambiguous_or_partial_input() {
    assert!(policy::one(br#"[{"Id":"a"}]"#).is_ok());
    for bytes in [
        br#"[{"Id":"a","Id":"b"}]"#.as_slice(),
        b"[]",
        b"[{},{}]",
        b"[NaN]",
        b"[{}]\n{}",
    ] {
        assert!(policy::one(bytes).is_err());
    }
    assert_eq!(
        policy::created_id(format!("{}\n", id('a')).as_bytes()).unwrap(),
        id('a')
    );
    for bytes in [
        id('a').into_bytes(),
        format!("{}\nnoise", id('a')).into_bytes(),
        format!("{}\n", id('A')).into_bytes(),
    ] {
        assert!(policy::created_id(&bytes).is_err());
    }
    assert!(policy::ids(format!("{}\n{}\n", id('a'), id('a')).as_bytes()).is_err());
}

#[test]
fn endpoint_joins_private_ipam_and_the_entire_observed_peer_inventory() {
    let (row, network) = endpoint_rows();
    let selected = endpoint(&row, &network).unwrap();
    assert_eq!(selected.address.to_string(), "172.20.0.2");
    assert_eq!(selected.prefix, 16);
    for (pointer, value) in [
        (
            "/NetworkSettings/Networks/sc0123456789ab/IPAddress",
            json!("172.20.0.1"),
        ),
        (
            "/NetworkSettings/Networks/sc0123456789ab/IPAddress",
            json!("172.20.255.255"),
        ),
        (
            "/NetworkSettings/Networks/sc0123456789ab/IPAddress",
            json!("172.020.0.2"),
        ),
        (
            "/NetworkSettings/Networks/sc0123456789ab/IPPrefixLen",
            json!(true),
        ),
        (
            "/NetworkSettings/Networks/sc0123456789ab/NetworkID",
            json!(id('f')),
        ),
        ("/State/Paused", json!(true)),
        ("/State/Pid", json!(0)),
    ] {
        let mut invalid = row.clone();
        *invalid.pointer_mut(pointer).unwrap() = value;
        assert!(endpoint(&invalid, &network).is_err(), "{pointer}");
    }
    let mut unknown = network.clone();
    unknown["Containers"][id('d')] = json!({"EndpointID":id('e'),"IPv4Address":"172.20.0.3/16"});
    assert!(endpoint(&row, &unknown).is_err());
    let mut invalid = network.clone();
    invalid["IPAM"]["Config"][0]["Subnet"] = json!("172.20.1.0/16");
    assert!(endpoint(&row, &invalid).is_err());
    let mut public = network.clone();
    public["IPAM"]["Config"][0]["Subnet"] = json!("192.0.2.0/24");
    assert!(endpoint(&row, &public).is_err());
}

#[test]
fn fixed_firewall_accepts_the_owned_maintenance_table_and_only_control_egress() {
    let rules = policy::firewall("sc_maintain_0123456789ab", "ens3", "sc0123456789ab").unwrap();
    assert!(rules.contains("table inet sc_maintain_0123456789ab"));
    assert!(rules.contains("table netdev sc_maintain_0123456789ab"));
    assert!(rules.contains("tcp sport 22 ct state established counter name ssh_control accept"));
    assert!(!rules.contains("ct state established accept"));
    assert_eq!(rules.matches("policy drop").count(), 3);
    for (table, interface, bridge) in [
        ("sc_caddy_0123456789ab", "ens3", "sc0123456789ab"),
        ("sc_maintain_0123456789ab", "ens3\naccept", "sc0123456789ab"),
        ("sc_maintain_0123456789ab", "ens3", "sc0123456789ab;"),
    ] {
        assert!(policy::firewall(table, interface, bridge).is_err());
    }
}

#[test]
fn blocked_counter_requires_one_exact_owned_unsigned_counter() {
    let value = json!({"nftables":[{"counter":{"name":"blocked_output","family":"inet","table":"sc_maintain_0123456789ab","packets":u64::MAX,"bytes":u64::MAX}}]});
    let encoded = serde_json::to_vec(&value).unwrap();
    assert_eq!(
        policy::blocked_counter(&encoded, "sc_maintain_0123456789ab").unwrap(),
        (u64::MAX, u64::MAX)
    );
    for bad in [json!(true), json!(-1), json!(1.0), json!("1")] {
        let mut invalid = value.clone();
        invalid["nftables"][0]["counter"]["packets"] = bad;
        assert!(
            policy::blocked_counter(
                &serde_json::to_vec(&invalid).unwrap(),
                "sc_maintain_0123456789ab"
            )
            .is_err()
        );
    }
    let mut duplicate = value.clone();
    duplicate["nftables"]
        .as_array_mut()
        .unwrap()
        .push(value["nftables"][0].clone());
    assert!(
        policy::blocked_counter(
            &serde_json::to_vec(&duplicate).unwrap(),
            "sc_maintain_0123456789ab"
        )
        .is_err()
    );
    assert!(policy::blocked_counter(&encoded, "sc_maintain_ffffffffffff").is_err());
}

fn execution(command: &str, exit: &str) -> Vec<Value> {
    let attrs = json!({"execID":id('a'),"exitCode":exit});
    [
        format!("exec_create: {command}"),
        format!("exec_start: {command}"),
        "exec_die".into(),
    ]
    .into_iter()
    .map(|action| json!({"Action":action,"Actor":{"Attributes":attrs}}))
    .collect()
}
#[test]
fn postgres_exec_history_joins_create_start_die_and_exact_command_counts() {
    let commands = BTreeMap::from([("psql fixed-query".to_owned(), 1)]);
    let good = execution("psql fixed-query", "0");
    assert!(policy::pg_exec_history(&good, &commands, "/bin/sh -c ready").is_ok());
    let mut health = good.clone();
    health.push(json!({"Action":"health_status: healthy"}));
    assert!(policy::pg_exec_history(&health, &commands, "/bin/sh -c ready").is_ok());
    for bad in [
        good[..2].to_vec(),
        execution("psql arbitrary-query", "0"),
        execution("psql fixed-query", "1"),
        execution("psql fixed-query", "00"),
    ] {
        assert_eq!(
            policy::pg_exec_history(&bad, &commands, "/bin/sh -c ready"),
            Err(Error::Refused)
        );
    }
    let mut duplicate = good.clone();
    duplicate.extend(good.clone());
    assert!(policy::pg_exec_history(&duplicate, &commands, "/bin/sh -c ready").is_err());
    let mut wrong_order = good;
    wrong_order.swap(0, 1);
    assert!(policy::pg_exec_history(&wrong_order, &commands, "/bin/sh -c ready").is_err());
}

#[test]
fn natural_caddy_arguments_keep_normal_mounts_and_coordinated_owned_identity() {
    let args = policy::normal_arguments(
        "caddy@sha256:fixed",
        "sc0123456789ab",
        "0123456789abcdef0123456789abcdef",
    );
    let joined = args.join("\n");
    assert!(joined.contains("simplestchat.maintain-prerequisite=0123456789abcdef0123456789abcdef"));
    assert!(joined.contains("type=bind,src=/srv/simplestchat-public/browser-assets,dst=/run/simplestchat-browser-assets,readonly"));
    assert!(!joined.contains("src=/etc/simplestchat-public/browser-assets"));
    assert!(joined.contains("10001:10001"));
    assert!(joined.contains("no-new-privileges"));
    assert_eq!(args.iter().filter(|v| v.as_str() == "--mount").count(), 4);
    assert_eq!(args.last().unwrap(), "caddy@sha256:fixed");
}

#[test]
fn event_stream_keeps_partial_records_and_refuses_duplicate_or_oversized_neighbors() {
    let mut rows = Vec::new();
    let mut pending = Vec::new();
    let mut consumed = 0;
    super::actors::append_events(&mut rows, &mut pending, &mut consumed, b"{\"Type\":\"net")
        .unwrap();
    assert!(rows.is_empty());
    assert!(!pending.is_empty());
    super::actors::append_events(
        &mut rows,
        &mut pending,
        &mut consumed,
        b"work\"}\n{\"Type\":\"container\"}\n",
    )
    .unwrap();
    assert_eq!(rows.len(), 2);
    assert!(pending.is_empty());
    assert_eq!(rows[0]["Type"], "network");
    for bytes in [
        b"{\"Type\":1,\"Type\":2}\n".to_vec(),
        b"[]\n".to_vec(),
        b"\n".to_vec(),
        vec![b'x'; 131073],
    ] {
        assert!(
            super::actors::append_events(&mut Vec::new(), &mut Vec::new(), &mut 0, &bytes).is_err()
        );
    }
    let mut rows = vec![json!({}); 2048];
    assert!(super::actors::append_events(&mut rows, &mut Vec::new(), &mut 0, b"{}\n").is_err());
    assert_eq!(rows.len(), 2048);
}

fn proxy_row() -> Value {
    json!({"Id":id('a'),"Image":format!("sha256:{}",id('b')),
        "Config":{"User":"10001:10001","Image":"caddy@sha256:fixed"},
        "HostConfig":{"ReadonlyRootfs":true,"CapDrop":["ALL"],"CapAdd":["NET_BIND_SERVICE"],"Init":true,
            "Memory":268435456,"PidsLimit":128,"NanoCpus":500000000,"SecurityOpt":["no-new-privileges"],
            "NetworkMode":"sc0123456789ab","RestartPolicy":{"Name":"unless-stopped","MaximumRetryCount":0},
            "PortBindings":{"80/tcp":[{"HostIp":"127.0.0.1","HostPort":"80"}],
                "443/tcp":[{"HostIp":"127.0.0.1","HostPort":"443"}],"443/udp":[{"HostIp":"127.0.0.1","HostPort":"443"}]}},
        "Mounts":[{"Type":"bind","Destination":"/etc/caddy/Caddyfile","Source":"/etc/simplestchat-public/Caddyfile","RW":false},
            {"Type":"bind","Destination":"/data","Source":"/srv/simplestchat-public/caddy-data","RW":true},
            {"Type":"bind","Destination":"/config","Source":"/srv/simplestchat-public/caddy-config","RW":true},
            {"Type":"bind","Destination":"/run/simplestchat-browser-assets","Source":"/srv/simplestchat-public/browser-assets","RW":false}]})
}

#[test]
fn proxy_profile_requires_normal_split_mounts_restrictions_and_loopback_ports() {
    let good = proxy_row();
    let image = format!("sha256:{}", id('b'));
    let check = |row: &Value| {
        policy::proxy_profile(
            row,
            &id('a'),
            &image,
            "caddy@sha256:fixed",
            "sc0123456789ab",
        )
    };
    assert!(check(&good).is_ok());
    for (pointer, bad) in [
        ("/Config/User", json!("0:0")),
        ("/HostConfig/ReadonlyRootfs", json!(false)),
        (
            "/HostConfig/CapAdd",
            json!(["NET_BIND_SERVICE", "SYS_ADMIN"]),
        ),
        ("/HostConfig/Memory", json!(268435456.0)),
        ("/Mounts/0/RW", json!(true)),
        (
            "/Mounts/3/Source",
            json!("/etc/simplestchat-public/browser-assets"),
        ),
        (
            "/HostConfig/PortBindings/443~1tcp/0/HostIp",
            json!("0.0.0.0"),
        ),
    ] {
        let mut row = good.clone();
        *row.pointer_mut(pointer).unwrap() = bad;
        assert!(check(&row).is_err(), "{pointer}");
    }
    let mut extra = good.clone();
    extra["Mounts"]
        .as_array_mut()
        .unwrap()
        .push(good["Mounts"][0].clone());
    assert!(check(&extra).is_err());
}
