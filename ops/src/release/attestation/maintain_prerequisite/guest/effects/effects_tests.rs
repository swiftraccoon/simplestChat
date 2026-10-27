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
    let network = json!({"Id":id('b'),"Name":"sc0123456789ab","Driver":"bridge","Scope":"local","Internal":false,"EnableIPv6":false,
        "Options":{"com.docker.network.bridge.name":"sc0123456789ab",
            "com.docker.network.bridge.enable_ip_masquerade":"false"},
        "IPAM":{"Config":[{"Subnet":"172.20.0.0/16","Gateway":"172.20.0.1"}]},
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
fn bridge_requires_one_fixed_ordinary_profile_without_masquerading() {
    let (_, network) = endpoint_rows();
    let check = |row: &Value| policy::bridge(row, &id('b'), "sc0123456789ab");
    assert_eq!(check(&network), Ok(()));
    for value in [json!(true), Value::Null, json!("false"), json!(0)] {
        let mut invalid = network.clone();
        invalid["Internal"] = value;
        assert_eq!(check(&invalid), Err(Error::Refused));
    }
    let mut missing = network.clone();
    assert!(
        missing
            .as_object_mut()
            .unwrap()
            .remove("Internal")
            .is_some()
    );
    assert_eq!(check(&missing), Err(Error::Refused));
    for value in [
        json!("true"),
        json!(false),
        json!(true),
        Value::Null,
        json!("FALSE"),
        json!("0"),
        json!(""),
    ] {
        let mut invalid = network.clone();
        invalid["Options"]["com.docker.network.bridge.enable_ip_masquerade"] = value;
        assert_eq!(check(&invalid), Err(Error::Refused));
    }
    for name in [
        "com.docker.network.bridge.name",
        "com.docker.network.bridge.enable_ip_masquerade",
    ] {
        let mut missing = network.clone();
        assert!(
            missing["Options"]
                .as_object_mut()
                .unwrap()
                .remove(name)
                .is_some()
        );
        assert_eq!(check(&missing), Err(Error::Refused));
    }
    for value in [Value::Null, json!([]), json!({})] {
        let mut invalid = network.clone();
        invalid["Options"] = value;
        assert_eq!(check(&invalid), Err(Error::Refused));
    }
    for (name, value) in [
        (
            "com.docker.network.bridge.gateway_mode_ipv4",
            json!("routed"),
        ),
        ("com.docker.network.bridge.enable_icc", json!("true")),
    ] {
        let mut invalid = network.clone();
        invalid["Options"][name] = value;
        assert_eq!(check(&invalid), Err(Error::Refused));
    }
    let mut wrong_name = network.clone();
    wrong_name["Options"]["com.docker.network.bridge.name"] = json!("scffffffffffff");
    assert_eq!(check(&wrong_name), Err(Error::Refused));
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
fn endpoint_requires_its_canonical_gateway_to_equal_the_admitted_ipam_gateway() {
    let (row, network) = endpoint_rows();
    assert_eq!(
        endpoint(&row, &network).unwrap().gateway.to_string(),
        "172.20.0.1"
    );
    for value in [
        json!(""),
        json!("172.20.0.3"),
        json!("172.020.0.1"),
        Value::Null,
        json!(false),
    ] {
        let mut invalid = row.clone();
        invalid["NetworkSettings"]["Networks"]["sc0123456789ab"]["Gateway"] = value;
        assert_eq!(endpoint(&invalid, &network), Err(Error::Refused));
    }
    let mut missing = row.clone();
    assert!(
        missing["NetworkSettings"]["Networks"]["sc0123456789ab"]
            .as_object_mut()
            .unwrap()
            .remove("Gateway")
            .is_some()
    );
    assert_eq!(endpoint(&missing, &network), Err(Error::Refused));
    let mut different = network.clone();
    different["IPAM"]["Config"][0]["Gateway"] = json!("172.20.0.4");
    assert_eq!(endpoint(&row, &different), Err(Error::Refused));
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

fn classic_capture(ether_type: [u8; 2], payload: &[u8], little: bool) -> Vec<u8> {
    let mut bytes = vec![0; 40];
    let encode16 = |v: u16| {
        if little {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        }
    };
    let encode32 = |v: u32| {
        if little {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        }
    };
    bytes[..4].copy_from_slice(&if little {
        [0xd4, 0xc3, 0xb2, 0xa1]
    } else {
        [0xa1, 0xb2, 0xc3, 0xd4]
    });
    bytes[4..6].copy_from_slice(&encode16(2));
    bytes[6..8].copy_from_slice(&encode16(4));
    bytes[16..20].copy_from_slice(&encode32(128));
    bytes[20..24].copy_from_slice(&encode32(1));
    let count = u32::try_from(14 + payload.len()).unwrap();
    bytes[32..36].copy_from_slice(&encode32(count));
    bytes[36..40].copy_from_slice(&encode32(count));
    bytes.extend_from_slice(&[0; 12]);
    bytes.extend_from_slice(&ether_type);
    bytes.extend_from_slice(payload);
    bytes
}

#[test]
fn capture_header_classes_never_admit_a_nonempty_capture_or_change_empty_rule() {
    use super::actors::{CaptureStage, PacketClass, capture_output, first_packet_class};
    let mut advertisement = vec![0; 56];
    advertisement[0] = 0x60;
    advertisement[4..6].copy_from_slice(&16u16.to_be_bytes());
    advertisement[6] = 58;
    advertisement[40] = 134;
    for (kind, payload, class) in [
        ([0x08, 0x06], vec![], PacketClass::Arp),
        ([0x08, 0x00], vec![0x45], PacketClass::Ipv4),
        ([0x86, 0xdd], vec![0x60; 40], PacketClass::Ipv6),
        (
            [0x86, 0xdd],
            advertisement.clone(),
            PacketClass::Icmpv6RouterAdvertisement,
        ),
        ([0x81, 0x00], vec![], PacketClass::Other),
    ] {
        for little in [true, false] {
            let mut bytes = classic_capture(kind, &payload, little);
            for nanoseconds in [false, true] {
                if nanoseconds {
                    bytes[..4].copy_from_slice(&if little {
                        [0x4d, 0x3c, 0xb2, 0xa1]
                    } else {
                        [0xa1, 0xb2, 0x3c, 0x4d]
                    });
                }
                assert_eq!(first_packet_class(&bytes), class);
                assert_eq!(
                    capture_output(0, &bytes, &[]),
                    Some(CaptureStage::Data(class))
                );
            }
        }
    }
    // Preserve the existing byte-count rule; this diagnostic adds no positive parser gate.
    assert_eq!(capture_output(0, &[0; 24], &[0; 8192]), None);
    assert_eq!(capture_output(1, &[0; 24], &[]), Some(CaptureStage::Status));
    assert_eq!(capture_output(0, &[0; 23], &[]), Some(CaptureStage::Short));
    assert_eq!(
        capture_output(0, &[0; 24], &[0; 8193]),
        Some(CaptureStage::Bounds)
    );
    assert_eq!(
        capture_output(0, &vec![0; 1024 * 1024 + 1], &[]),
        Some(CaptureStage::Bounds)
    );
    for (offset, value) in [(0, 0x40), (6, 0), (40, 133), (41, 1)] {
        let mut changed = advertisement.clone();
        changed[offset] = value;
        let bytes = classic_capture([0x86, 0xdd], &changed, true);
        assert_eq!(first_packet_class(&bytes), PacketClass::Ipv6);
        assert_eq!(
            capture_output(0, &bytes, &[]),
            Some(CaptureStage::Data(PacketClass::Ipv6))
        );
    }
}

#[test]
fn malformed_capture_headers_and_truncated_first_records_remain_unknown_refusals() {
    use super::actors::{CaptureStage, PacketClass, capture_output, first_packet_class};
    let bytes = classic_capture([0x08, 0x06], &[], true);
    for end in 0..bytes.len() {
        assert_eq!(
            first_packet_class(&bytes[..end]),
            PacketClass::Unknown,
            "{end}"
        );
        if end > 24 {
            assert_eq!(
                capture_output(0, &bytes[..end], &[]),
                Some(CaptureStage::Data(PacketClass::Unknown))
            );
        }
    }
    for (offset, value) in [
        (0, 0u32),
        (4, 3),
        (16, 0),
        (16, 129),
        (20, 101),
        (20, 113),
        (32, 0),
        (32, 13),
        (32, 129),
        (32, u32::MAX),
        (36, 13),
    ] {
        let mut changed = bytes.clone();
        changed[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        assert_eq!(
            first_packet_class(&changed),
            PacketClass::Unknown,
            "{offset}"
        );
        assert_eq!(
            capture_output(0, &changed, &[]),
            Some(CaptureStage::Data(PacketClass::Unknown))
        );
    }
    let mut oversized = bytes;
    oversized.resize(1024 * 1024 + 1, 0);
    assert_eq!(first_packet_class(&oversized), PacketClass::Unknown);
}

#[test]
fn capture_failure_retains_first_error_stage_through_later_close_or_clock_failure() {
    use super::actors::{CaptureFailure, CaptureStage, PacketClass};
    let mut failure = CaptureFailure::default();
    assert_eq!(
        failure.observe(
            CaptureStage::Data(PacketClass::Arp),
            Err::<(), _>(Error::Refused)
        ),
        Err(Error::Refused)
    );
    assert_eq!(
        failure.observe(CaptureStage::Close, Err::<(), _>(Error::Close)),
        Err(Error::Close)
    );
    assert_eq!(
        failure.observe(CaptureStage::Live, Err::<(), _>(Error::Deadline)),
        Err(Error::Deadline)
    );
    assert_eq!(
        failure.diagnostic(),
        Some((Error::Refused, "capture-data-arp"))
    );
    assert_eq!(failure.observe(CaptureStage::Live, Ok(())), Ok(()));
    assert_eq!(
        failure.diagnostic(),
        Some((Error::Refused, "capture-data-arp"))
    );
    let mut bounds = CaptureFailure::default();
    assert_eq!(
        bounds.observe(CaptureStage::Live, Err::<(), _>(Error::Bounds)),
        Err(Error::Bounds)
    );
    assert_eq!(bounds.diagnostic(), Some((Error::Bounds, "capture-bounds")));
}

#[test]
fn capture_checkpoint_cannot_replace_an_earlier_body_or_cleanup_failure() {
    use super::super::{Control, local_checkpoint};
    use std::time::Instant;
    let previous = "source-observe";
    let watch = Some((Error::Refused, "capture-data-arp"));
    assert_eq!(
        local_checkpoint(
            previous,
            None,
            Err(Error::Refused),
            watch,
            Some(Error::Refused)
        ),
        "capture-data-arp"
    );
    for earlier in [Error::Io, Error::Refused, Error::Deadline, Error::Terminate] {
        let mut owner = Control::new(Instant::now()).unwrap();
        owner.record(earlier);
        let original = owner.first;
        owner.record(Error::Refused);
        owner.begin_settling();
        let checkpoint =
            local_checkpoint(previous, original, Err(Error::Refused), watch, owner.first);
        let ended = owner.finish();
        assert_eq!(ended, Err(earlier));
        assert_eq!(owner.first, Some(earlier));
        assert_eq!(checkpoint, previous);
    }
    for (local, watch, first) in [
        (Ok(()), watch, Some(Error::Refused)),
        (Err(Error::Io), watch, Some(Error::Io)),
        (Err(Error::Refused), None, Some(Error::Refused)),
        (Err(Error::Refused), watch, Some(Error::Deadline)),
        (Err(Error::Refused), watch, None),
    ] {
        assert_eq!(
            local_checkpoint(previous, None, local, watch, first),
            previous
        );
    }
}

struct ReplacementHistory {
    row: Value,
    services: BTreeMap<String, String>,
    traces: BTreeMap<String, Vec<String>>,
}

impl ReplacementHistory {
    fn new() -> Self {
        let retired = ["create", "start", "die", "destroy"]
            .map(String::from)
            .to_vec();
        Self {
            // Synthetic identities; the field shape matches a stopped Moby rename event.
            row: json!({"Type":"container","Action":"rename","Actor":{
                "ID":id('a'),"Attributes":{
                    "name":"simplestchat-public-simplestchat-1",
                    "oldName":"/bbbbbbbbbbbb_simplestchat-public-simplestchat-1",
                    "image":format!("sha256:{}",id('f')),
                    "simplestchat.maintain-prerequisite":"0123456789abcdef0123456789abcdef",
                    "com.docker.compose.project":"simplestchat-public",
                    "com.docker.compose.service":"simplestchat",
                    "com.docker.compose.container-number":"1",
                    "com.docker.compose.replace":"simplestchat-1"}}}),
            services: BTreeMap::from([
                (id('a'), "simplestchat".into()),
                (id('b'), "simplestchat".into()),
                (id('c'), "migrate".into()),
                (id('d'), "caddy".into()),
            ]),
            traces: BTreeMap::from([
                (id('a'), vec!["create".into()]),
                (id('b'), retired.clone()),
                (id('c'), retired.clone()),
                (id('d'), vec!["create".into(), "start".into()]),
                (id('e'), retired),
            ]),
        }
    }

    fn check(&self) -> super::Result<()> {
        self.check_identity(&id('a'), "/simplestchat-public-simplestchat-1")
    }

    fn check_identity(&self, app: &str, name: &str) -> super::Result<()> {
        super::observation::app_replacement_rename(
            &self.row,
            app,
            name,
            "0123456789abcdef0123456789abcdef",
            &format!("sha256:{}", id('f')),
            &self.services,
            &self.traces,
        )
    }

    fn complete(&self) -> super::Result<()> {
        super::observation::expected_daemon_traces(
            &id('e'),
            &id('a'),
            &id('d'),
            &self.services,
            &self.traces,
        )
    }
}

#[test]
fn replacement_rename_joins_retired_identity_before_mandatory_trace_completion() {
    let mut history = ReplacementHistory::new();
    let before = history.traces.clone();
    assert_eq!(history.check(), Ok(()));
    assert_eq!(history.traces, before);
    assert_eq!(history.complete(), Err(Error::Refused));
    history
        .traces
        .get_mut(&id('a'))
        .unwrap()
        .push("rename".into());
    assert_eq!(history.complete(), Err(Error::Refused));
    history
        .traces
        .get_mut(&id('a'))
        .unwrap()
        .push("start".into());
    assert_eq!(history.complete(), Ok(()));
}

#[test]
fn replacement_rename_refuses_unjoined_event_fields_without_changing_trace() {
    for (pointer, value) in [
        ("/Type", json!("network")),
        ("/Action", json!("start")),
        ("/Actor/ID", json!(id('b'))),
        ("/Actor/ID", Value::Null),
        ("/Actor/Attributes", Value::Null),
        (
            "/Actor/Attributes/image",
            json!(format!("sha256:{}", id('e'))),
        ),
        (
            "/Actor/Attributes/simplestchat.maintain-prerequisite",
            json!("ffffffffffffffffffffffffffffffff"),
        ),
        (
            "/Actor/Attributes/com.docker.compose.project",
            json!("another-project"),
        ),
        (
            "/Actor/Attributes/com.docker.compose.service",
            json!("migrate"),
        ),
        (
            "/Actor/Attributes/com.docker.compose.container-number",
            json!("2"),
        ),
        (
            "/Actor/Attributes/com.docker.compose.replace",
            json!("another-1"),
        ),
        (
            "/Actor/Attributes/name",
            json!("/simplestchat-public-simplestchat-1"),
        ),
        (
            "/Actor/Attributes/name",
            json!("bbbbbbbbbbbb_simplestchat-public-simplestchat-1"),
        ),
        (
            "/Actor/Attributes/oldName",
            json!("bbbbbbbbbbbb_simplestchat-public-simplestchat-1"),
        ),
        (
            "/Actor/Attributes/oldName",
            json!("/aaaaaaaaaaaa_simplestchat-public-simplestchat-1"),
        ),
        (
            "/Actor/Attributes/oldName",
            json!("/bbbbbbbbbbbb_simplestchat-public-simplestchat-2"),
        ),
    ] {
        let mut history = ReplacementHistory::new();
        *history.row.pointer_mut(pointer).unwrap() = value;
        let before = history.traces.clone();
        assert_eq!(history.check(), Err(Error::Refused), "{pointer}");
        assert_eq!(history.traces, before, "{pointer}");
    }
    for name in [
        "image",
        "oldName",
        "name",
        "com.docker.compose.container-number",
        "com.docker.compose.replace",
    ] {
        let mut history = ReplacementHistory::new();
        history.row["Actor"]["Attributes"]
            .as_object_mut()
            .unwrap()
            .remove(name);
        assert_eq!(history.check(), Err(Error::Refused), "{name}");
    }
}

#[test]
fn replacement_rename_requires_canonical_distinct_ids_and_fresh_inspected_name() {
    let history = ReplacementHistory::new();
    for name in [
        "simplestchat-public-simplestchat-1",
        "//simplestchat-public-simplestchat-1",
        "/simplestchat-public-simplestchat-2",
        "/bbbbbbbbbbbb_simplestchat-public-simplestchat-1",
        "",
    ] {
        assert_eq!(history.check_identity(&id('a'), name), Err(Error::Refused));
    }
    for invalid in [String::new(), "a".into(), id('A'), id('é')] {
        let mut history = ReplacementHistory::new();
        history.services.remove(&id('a'));
        history
            .services
            .insert(invalid.clone(), "simplestchat".into());
        history.row["Actor"]["ID"] = json!(invalid);
        assert_eq!(
            history.check_identity(&invalid, "/simplestchat-public-simplestchat-1"),
            Err(Error::Refused)
        );
        let mut history = ReplacementHistory::new();
        history.services.remove(&id('b'));
        history.services.insert(invalid, "simplestchat".into());
        assert_eq!(history.check(), Err(Error::Refused));
    }
    let mut missing = ReplacementHistory::new();
    missing.services.remove(&id('b'));
    assert_eq!(missing.check(), Err(Error::Refused));
    let mut foreign = ReplacementHistory::new();
    foreign.services.insert(id('f'), "simplestchat".into());
    assert_eq!(foreign.check(), Err(Error::Refused));
    let mut wrong_role = ReplacementHistory::new();
    wrong_role.services.insert(id('b'), "migrate".into());
    assert_eq!(wrong_role.check(), Err(Error::Refused));
}

#[test]
fn replacement_rename_refuses_duplicate_late_or_unretired_histories() {
    for phases in [
        vec![],
        vec!["start"],
        vec!["create", "create"],
        vec!["create", "rename"],
        vec!["create", "start"],
        vec!["create", "rename", "start"],
    ] {
        let mut history = ReplacementHistory::new();
        history
            .traces
            .insert(id('a'), phases.into_iter().map(String::from).collect());
        let before = history.traces.clone();
        assert_eq!(history.check(), Err(Error::Refused));
        assert_eq!(history.traces, before);
    }
    for phases in [
        vec![],
        vec!["create", "start"],
        vec!["create", "start", "die"],
        vec!["create", "start", "destroy", "die"],
        vec!["create", "start", "die", "destroy", "destroy"],
    ] {
        let mut history = ReplacementHistory::new();
        history
            .traces
            .insert(id('b'), phases.into_iter().map(String::from).collect());
        assert_eq!(history.check(), Err(Error::Refused));
    }
    let mut missing = ReplacementHistory::new();
    missing.traces.remove(&id('b'));
    assert_eq!(missing.check(), Err(Error::Refused));
}

#[test]
fn completed_daemon_trace_requires_one_rename_and_preserves_peer_lifecycles() {
    for phases in [
        vec!["create", "start"],
        vec!["rename", "create", "start"],
        vec!["create", "start", "rename"],
        vec!["create", "rename", "rename", "start"],
        vec!["create", "rename", "start", "die"],
    ] {
        let mut history = ReplacementHistory::new();
        history
            .traces
            .insert(id('a'), phases.into_iter().map(String::from).collect());
        assert_eq!(history.complete(), Err(Error::Refused));
    }
    for actor in ['b', 'c', 'd', 'e'] {
        let mut history = ReplacementHistory::new();
        history.traces.insert(
            id('a'),
            vec!["create".into(), "rename".into(), "start".into()],
        );
        assert_eq!(history.complete(), Ok(()));
        history
            .traces
            .get_mut(&id(actor))
            .unwrap()
            .push("rename".into());
        assert_eq!(history.complete(), Err(Error::Refused));
    }
}

fn proxy_row() -> Value {
    json!({"Id":id('a'),"Image":format!("sha256:{}",id('b')),
        "Config":{"User":"10001:10001","Image":"caddy@sha256:fixed"},
        "HostConfig":{"ReadonlyRootfs":true,"CapDrop":["ALL"],"CapAdd":["CAP_NET_BIND_SERVICE"],"Init":true,
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
            json!(["CAP_NET_BIND_SERVICE", "CAP_SYS_ADMIN"]),
        ),
        ("/HostConfig/CapAdd", json!(["NET_BIND_SERVICE"])),
        ("/HostConfig/CapAdd", json!(["CAP_SYS_ADMIN"])),
        ("/HostConfig/CapAdd", json!([])),
        ("/HostConfig/CapAdd", Value::Null),
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
