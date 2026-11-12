//! Admission decisions of a migration that need no I/O: which hosts and
//! addresses a move may use, what the two machines reported, and what the
//! destination, the name service and GitHub answered.

use serde_json::Value;
use simplestchat_toolkit::tooling::{Failure, Result};
use std::{
    collections::BTreeSet,
    net::{IpAddr, Ipv6Addr},
};

pub(super) fn refusal(code: &'static str) -> Failure {
    Failure::new(1, code)
}

/// The fixed diagnostic a failure carries, or the one name for every failure
/// whose cause stays in private evidence.
pub(super) fn class(error: &Failure) -> &str {
    let code = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_';
    match error.code {
        130 | 143 => "interrupted",
        _ if !error.message.is_empty() && error.message.bytes().all(code) => &error.message,
        _ => "migration_step_failed",
    }
}

/// A plain, usable IP literal in its canonical spelling, never a name that
/// could resolve differently later. An IPv4 address mapped into IPv6 is
/// judged, and returned, as the IPv4 address it is.
pub(super) fn address(value: &str) -> Result<String> {
    if let Some((literal, zone)) = value.split_once('%') {
        // A zone names an interface of one machine, not a reachable host.
        let scoped = literal.parse::<Ipv6Addr>().is_ok() && !zone.is_empty();
        return Err(refusal(match scoped {
            true => "migration_unusable_address",
            false => "migration_literal_addresses_required",
        }));
    }
    let parsed = value.parse::<IpAddr>();
    let parsed = parsed.map_err(|_| refusal("migration_literal_addresses_required"))?;
    let selected = match parsed {
        IpAddr::V6(wide) => wide.to_ipv4_mapped().map_or(parsed, IpAddr::V4),
        IpAddr::V4(_) => parsed,
    };
    let local = match selected {
        IpAddr::V4(narrow) => narrow.is_link_local(),
        IpAddr::V6(wide) => wide.is_unicast_link_local(),
    };
    if selected.is_loopback() || selected.is_unspecified() || selected.is_multicast() || local {
        return Err(refusal("migration_unusable_address"));
    }
    Ok(selected.to_string())
}

/// The addresses a destination is verified at while its name still points
/// at the source, and that the name must resolve to afterwards.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Transport {
    pub address: String,
    pub ipv6: Option<String>,
}

impl Transport {
    /// How curl is told to reach `host` at this destination while the name
    /// still resolves to the source: an IPv6 address goes in brackets.
    pub(super) fn pin(&self, host: &str) -> String {
        match self.address.contains(':') {
            true => format!("{host}:443:[{}]", self.address),
            false => format!("{host}:443:{}", self.address),
        }
    }

    /// Exactly the addresses the public name has once DNS was cut over.
    pub(super) fn addresses(&self) -> BTreeSet<String> {
        self.ipv6.iter().chain([&self.address]).cloned().collect()
    }
}

/// Two inventories that reach one host cannot be the ends of a move.
pub(super) fn distinct(source: &Value, target: &Value) -> Result<()> {
    let reached = |variables: &Value| {
        let host = variables.get("ansible_host").and_then(Value::as_str)?;
        Some(host.to_lowercase().trim_end_matches('.').to_owned())
    };
    match (reached(source), reached(target)) {
        (Some(old), Some(new)) if old != new => Ok(()),
        _ => Err(refusal("migration_distinct_hosts_required")),
    }
}

/// The optional services the destination's own inventory switches on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Services {
    pub relay: bool,
    pub backup: bool,
    pub monitoring: bool,
}

pub(super) fn services(target: &Value) -> Result<Services> {
    let switch = |name: &str| match target.get(name) {
        None => Ok(false),
        Some(Value::Bool(enabled)) => Ok(*enabled),
        Some(_) => Err(refusal("migration_service_opt_in_invalid")),
    };
    Ok(Services {
        relay: switch("scpub_turn_enabled")?,
        backup: switch("scpub_backup_enabled")?,
        monitoring: switch("scmon_enabled")?,
    })
}

fn text<'a>(variables: &'a Value, name: &str) -> Result<&'a str> {
    let missing = || refusal("migration_literal_addresses_required");
    variables
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(missing)
}

/// Keep the hosts' identities and the announced media address independent
/// of DNS: two different literal addresses, and a destination that announces
/// the address it is reached at.
pub(super) fn transport(source: &Value, target: &Value) -> Result<Transport> {
    let old = address(text(source, "ansible_host")?)?;
    let new = address(text(target, "ansible_host")?)?;
    if old == new {
        return Err(refusal("migration_distinct_hosts_required"));
    }
    if address(text(target, "scpub_announce_ip")?)? != new {
        return Err(refusal("migration_target_media_address_mismatch"));
    }
    let ipv6 = match target.get("scpub_announce_ipv6") {
        None => "",
        Some(_) => text(target, "scpub_announce_ipv6")?,
    };
    let ipv6 = match ipv6 {
        "" => None,
        literal => Some(address(literal)?),
    };
    if ipv6.as_ref().is_some_and(|wide| !wide.contains(':')) {
        return Err(refusal("migration_invalid_ipv6_address"));
    }
    Ok(Transport { address: new, ipv6 })
}

/// The addresses one `dig +short` answer lists for one record type. Names a
/// CNAME chain passes through may come first; anything else is not an answer.
pub(super) fn answers(output: &[u8], wide: bool) -> Result<BTreeSet<String>> {
    let invalid = || refusal("migration_dns_response_invalid");
    let text = std::str::from_utf8(output)
        .ok()
        .filter(|text| text.is_ascii());
    let mut found = BTreeSet::new();
    for line in text.ok_or_else(invalid)?.lines() {
        let Ok(parsed) = line.parse::<IpAddr>() else {
            let alias = line.strip_suffix('.').map(str::to_ascii_lowercase);
            if found.is_empty() && alias.is_some_and(|name| hostname(&name)) {
                continue;
            }
            return Err(invalid());
        };
        if parsed.is_ipv6() != wide {
            return Err(refusal("migration_dns_address_family_mismatch"));
        }
        found.insert(parsed.to_string());
    }
    Ok(found)
}

/// A lower-case DNS name of at least two labels ending in a letter-only one.
fn hostname(name: &str) -> bool {
    let plain = |byte: &u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    let label = |label: &&str| {
        let bytes = label.as_bytes();
        (1..=63).contains(&bytes.len())
            && bytes.iter().all(|byte| plain(byte) || *byte == b'-')
            && bytes.first().is_some_and(plain)
            && bytes.last().is_some_and(plain)
    };
    let labels: Vec<&str> = name.split('.').collect();
    let Some((last, others)) = labels.split_last() else {
        return false;
    };
    name.len() <= 253
        && !others.is_empty()
        && others.iter().all(label)
        && (2..=63).contains(&last.len())
        && last.bytes().all(|byte| byte.is_ascii_lowercase())
}

fn hexadecimal(value: Option<&Value>, length: usize) -> Option<&str> {
    let text = value.and_then(Value::as_str)?;
    let digits = text
        .bytes()
        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
    (text.len() == length && digits).then_some(text)
}

/// What both machines reported before anything changed: two different
/// machines, the source serving the source's name and the destination none
/// or its own, a deployed revision, and a passkey identity both names
/// belong to. Returns that identity, which the destination must keep.
pub(super) fn relying_party(
    inspected: &Value,
    source_host: &str,
    target_host: &str,
) -> Result<String> {
    let unreadable = || refusal("migration_step_failed");
    let source = inspected.get("source").filter(|side| side.is_object());
    let target = inspected.get("target").filter(|side| side.is_object());
    let (source, target) = (
        source.ok_or_else(unreadable)?,
        target.ok_or_else(unreadable)?,
    );
    let old = hexadecimal(source.get("machineId"), 32);
    let new = hexadecimal(target.get("machineId"), 32);
    if old.is_none() || new.is_none() || old == new {
        return Err(refusal("migration_distinct_machines_required"));
    }
    let serving = |side: &Value| {
        side.get("domain")
            .filter(|domain| !domain.is_null())
            .cloned()
    };
    let free = serving(target).is_none_or(|domain| domain == target_host);
    if serving(source).is_none_or(|domain| domain != source_host) || !free {
        return Err(refusal("migration_remote_domain_mismatch"));
    }
    if hexadecimal(source.get("revision"), 40).is_none() {
        return Err(refusal("migration_source_revision"));
    }
    let identity = source.get("rpId").and_then(Value::as_str);
    let identity = identity.filter(|identity| hostname(identity));
    let identity = identity.ok_or_else(|| refusal("migration_source_rp_id"))?;
    let within = |host: &str| host == identity || host.ends_with(&format!(".{identity}"));
    if !within(source_host) || !within(target_host) {
        return Err(refusal("migration_passkey_domain_incompatible"));
    }
    Ok(identity.to_owned())
}

/// What the controller's own verification left for the later steps: the
/// directory of the signed originals it checked for exactly this revision.
pub(super) fn application(bytes: &[u8], revision: &str) -> Result<String> {
    let invalid = || refusal("migration_application_record");
    let record: Value = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    let fields = record.as_object().ok_or_else(invalid)?;
    let digest = |name: &str| hexadecimal(fields.get(name), 64).is_some();
    let directory = fields.get("directory").and_then(Value::as_str);
    let directory = directory.filter(|path| path.starts_with('/') && path.len() <= 4096);
    let exact = fields.len() == 4
        && fields.get("revision").and_then(Value::as_str) == Some(revision)
        && digest("image.tar")
        && digest("release.json");
    directory
        .filter(|_| exact)
        .map(str::to_owned)
        .ok_or_else(invalid)
}

/// One anonymous reply of the destination: curl's body and, on its own last
/// line, the status. Readiness, the home page and the served revision each
/// have their own refusal.
pub(super) fn reply(bytes: &[u8], path: &str, revision: &str) -> Result<()> {
    let text = std::str::from_utf8(bytes)
        .ok()
        .filter(|_| bytes.len() <= MAX_REPLY + 4);
    let parts = text.and_then(|text| text.rsplit_once('\n'));
    let Some((body, "200")) = parts.filter(|(body, _)| !body.is_empty()) else {
        return Err(refusal("migration_https_response"));
    };
    if path == "/" {
        let opening: String = body.chars().take(256).collect();
        let page = opening.to_ascii_lowercase().contains("<!doctype html>");
        return if page {
            Ok(())
        } else {
            Err(refusal("migration_https_homepage"))
        };
    }
    let record: Value = serde_json::from_str(body).map_err(|_| refusal("migration_https_json"))?;
    let fields = record
        .as_object()
        .ok_or_else(|| refusal("migration_https_json"))?;
    let field = |name: &str| fields.get(name).and_then(Value::as_str);
    match path {
        "/health" if field("status") != Some("ok") => Err(refusal("migration_https_readiness")),
        "/ready" if field("status") != Some("ready") => Err(refusal("migration_https_readiness")),
        "/app-version.json" if field("revision") != Some(revision) => {
            Err(refusal("migration_https_revision"))
        }
        _ => Ok(()),
    }
}

/// The largest body one anonymous check reads.
pub(super) const MAX_REPLY: usize = 128 * 1024;

/// The repository's canary variable, which must exist and name `origin`.
pub(super) fn canary(bytes: &[u8], origin: &str) -> Result<()> {
    let record: Value = serde_json::from_slice(bytes).unwrap_or(Value::Null);
    let fields = record.as_object();
    let fields = fields.ok_or_else(|| refusal("migration_canary_variable_invalid"))?;
    let field = |name: &str| fields.get(name).and_then(Value::as_str);
    if field("name") == Some("CANARY_ORIGIN") && field("value") == Some(origin) {
        Ok(())
    } else {
        Err(refusal("migration_canary_origin_mismatch"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn code<T: std::fmt::Debug>(result: Result<T>) -> String {
        result.unwrap_err().message
    }

    #[test]
    fn a_failure_reports_its_own_code_or_the_one_name_for_private_causes() {
        assert_eq!(class(&refusal("ci_failed")), "ci_failed");
        assert_eq!(
            class(&refusal("migration_https_revision")),
            "migration_https_revision"
        );
        for private in [
            "Routine deployment command failed.",
            "",
            "Mixed_case",
            "two words",
        ] {
            assert_eq!(
                class(&Failure::new(1, private)),
                "migration_step_failed",
                "{private:?}"
            );
        }
        assert_eq!(class(&Failure::child_exit(2)), "migration_step_failed");
        assert_eq!(class(&Failure::new(124, "ci_failed")), "ci_failed");
        for signal in [130, 143] {
            assert_eq!(class(&Failure::new(signal, "ci_failed")), "interrupted");
        }
    }

    #[test]
    fn two_ends_of_a_move_are_reached_differently_and_switch_services_with_booleans() {
        let host = |address: &str| json!({"ansible_host": address});
        distinct(&host("192.0.2.10"), &host("192.0.2.11")).unwrap();
        distinct(&host("old.example.test"), &host("new.example.test")).unwrap();
        for (old, new) in [
            ("192.0.2.10", "192.0.2.10"),
            ("Host.Example.Test", "host.example.test."),
            ("host.example.test..", "HOST.example.test"),
        ] {
            let refused = code(distinct(&host(old), &host(new)));
            assert_eq!(refused, "migration_distinct_hosts_required", "{old} {new}");
        }
        for (old, new) in [
            (json!({}), host("a")),
            (host("a"), json!({"ansible_host": 7})),
        ] {
            assert_eq!(
                code(distinct(&old, &new)),
                "migration_distinct_hosts_required"
            );
        }
        let none = Services {
            relay: false,
            backup: false,
            monitoring: false,
        };
        assert_eq!(services(&json!({})).unwrap(), none);
        let chosen = json!({"scpub_turn_enabled": true, "scpub_backup_enabled": false,
            "scmon_enabled": true});
        let expected = Services {
            relay: true,
            backup: false,
            monitoring: true,
        };
        assert_eq!(services(&chosen).unwrap(), expected);
        for name in [
            "scpub_turn_enabled",
            "scpub_backup_enabled",
            "scmon_enabled",
        ] {
            for value in [json!("yes"), json!(1), json!(null), json!([true])] {
                let refused = code(services(&json!({name: value})));
                assert_eq!(refused, "migration_service_opt_in_invalid", "{name}");
            }
        }
    }

    #[test]
    fn a_name_service_answer_is_addresses_of_one_family_after_any_aliases() {
        let set = |values: &[&str]| values.iter().map(|value| (*value).to_owned()).collect();
        assert_eq!(answers(b"", false).unwrap(), BTreeSet::new());
        assert_eq!(
            answers(b"192.0.2.11\n", false).unwrap(),
            set(&["192.0.2.11"])
        );
        let chained = b"Edge.Example.NET.\nlb-1.example.net.\n192.0.2.11\n192.0.2.12\n";
        assert_eq!(
            answers(chained, false).unwrap(),
            set(&["192.0.2.11", "192.0.2.12"])
        );
        let wide = b"2001:0DB8:0::11\n2001:db8::12\n";
        assert_eq!(
            answers(wide, true).unwrap(),
            set(&["2001:db8::11", "2001:db8::12"])
        );
        for (output, family) in [(&b"192.0.2.11\n"[..], true), (b"2001:db8::11\n", false)] {
            let refused = code(answers(output, family));
            assert_eq!(refused, "migration_dns_address_family_mismatch");
        }
        for output in [
            &b"192.0.2.11\nlate.example.net.\n"[..],
            b"alias.example.net\n192.0.2.11\n",
            b"192.0.2.011\n",
            b";; connection timed out; no servers could be reached\n",
            b"-bad.example.net.\n",
            b"\n",
            b"192.0.2.11\n\xff\n",
            "192.0.2.11\n\u{e9}.example.net.\n".as_bytes(),
        ] {
            assert_eq!(
                code(answers(output, false)),
                "migration_dns_response_invalid",
                "{output:?}"
            );
        }
        let both = Transport {
            address: "192.0.2.11".into(),
            ipv6: Some("2001:db8::11".into()),
        };
        assert_eq!(both.addresses(), set(&["192.0.2.11", "2001:db8::11"]));
        let one = Transport {
            address: "192.0.2.11".into(),
            ipv6: None,
        };
        assert_eq!(one.addresses(), set(&["192.0.2.11"]));
        assert_eq!(
            both.pin("chat.example.test"),
            "chat.example.test:443:192.0.2.11"
        );
        let wide = Transport {
            address: "2001:db8::11".into(),
            ipv6: None,
        };
        assert_eq!(
            wide.pin("chat.example.test"),
            "chat.example.test:443:[2001:db8::11]"
        );
    }

    #[test]
    fn the_controller_record_names_the_verified_originals_of_exactly_this_revision() {
        let revision = "a".repeat(40);
        let record = |change: &dyn Fn(&mut Value)| {
            let mut value = json!({"directory": "/private/results/verify.1/artifact",
                "image.tar": "c".repeat(64), "release.json": "d".repeat(64),
                "revision": revision});
            change(&mut value);
            serde_json::to_vec(&value).unwrap()
        };
        let directory = application(&record(&|_| {}), &revision).unwrap();
        assert_eq!(directory, "/private/results/verify.1/artifact");
        let changes: [&dyn Fn(&mut Value); 9] = [
            &|value| value["revision"] = json!("b".repeat(40)),
            &|value| value["directory"] = json!("relative/artifact"),
            &|value| value["directory"] = json!(7),
            &|value| value["image.tar"] = json!("c".repeat(63)),
            &|value| value["release.json"] = json!("D".repeat(64)),
            &|value| value["extra"] = json!(true),
            &|value| drop(value.as_object_mut().unwrap().remove("image.tar")),
            &|value| *value = json!([]),
            &|value| value["directory"] = json!(format!("/{}", "a".repeat(4096))),
        ];
        for (index, change) in changes.iter().enumerate() {
            let refused = code(application(&record(change), &revision));
            assert_eq!(refused, "migration_application_record", "{index}");
        }
        assert_eq!(
            code(application(b"not json", &revision)),
            "migration_application_record"
        );
    }

    #[test]
    fn each_anonymous_reply_is_a_success_with_the_content_its_path_promises() {
        let revision = "a".repeat(40);
        let reply_of = |body: &str, status: &str| format!("{body}\n{status}").into_bytes();
        let served = format!("{{\"revision\":\"{revision}\"}}");
        for (path, body) in [
            ("/health", "{\"status\":\"ok\"}"),
            ("/ready", "{\"status\": \"ready\", \"database\": true}"),
            ("/", "\n <!DOCTYPE HTML><html></html>"),
            ("/api/capabilities", "{\"auth\": true}"),
            ("/app-version.json", served.as_str()),
        ] {
            reply(&reply_of(body, "200"), path, &revision).unwrap();
            for status in ["302", "204", "20", "200 ", ""] {
                let refused = code(reply(&reply_of(body, status), path, &revision));
                assert_eq!(refused, "migration_https_response", "{path} {status}");
            }
            assert_eq!(
                code(reply(body.as_bytes(), path, &revision)),
                "migration_https_response"
            );
            assert_eq!(
                code(reply(b"\n200", path, &revision)),
                "migration_https_response"
            );
        }
        let late = format!("{}<!doctype html>", " ".repeat(256));
        for body in ["<html></html>", late.as_str()] {
            let refused = code(reply(&reply_of(body, "200"), "/", &revision));
            assert_eq!(refused, "migration_https_homepage");
        }
        for path in [
            "/health",
            "/ready",
            "/api/capabilities",
            "/app-version.json",
        ] {
            for body in ["<!doctype html>", "[]", "\"ok\"", "{\"status\": \"ok\""] {
                let refused = code(reply(&reply_of(body, "200"), path, &revision));
                assert_eq!(refused, "migration_https_json", "{path} {body}");
            }
        }
        for (path, body) in [
            ("/health", "{\"status\":\"ready\"}"),
            ("/health", "{}"),
            ("/ready", "{\"status\":\"ok\"}"),
            ("/ready", "{\"status\":true}"),
        ] {
            let refused = code(reply(&reply_of(body, "200"), path, &revision));
            assert_eq!(refused, "migration_https_readiness", "{path} {body}");
        }
        let other = format!("{{\"revision\":\"{}\"}}", "b".repeat(40));
        let two = format!("{{\"revision\":[\"{revision}\"]}}");
        for body in [
            other.as_str(),
            two.as_str(),
            "{}",
            "{\"revision\":\"development\"}",
        ] {
            let refused = code(reply(
                &reply_of(body, "200"),
                "/app-version.json",
                &revision,
            ));
            assert_eq!(refused, "migration_https_revision", "{body}");
        }
        // The largest body the request itself admits is read; one byte more is not.
        let largest = format!("<!doctype html>{}", "x".repeat(MAX_REPLY - 15));
        reply(&reply_of(&largest, "200"), "/", &revision).unwrap();
        let beyond = format!("{largest}x");
        assert_eq!(
            code(reply(&reply_of(&beyond, "200"), "/", &revision)),
            "migration_https_response"
        );
        assert_eq!(
            code(reply(b"\xff\n200", "/", &revision)),
            "migration_https_response"
        );
    }

    #[test]
    fn the_canary_variable_exists_and_names_exactly_the_expected_origin() {
        let origin = "https://chat.example.test";
        let variable = |name: &str, value: Value| {
            serde_json::to_vec(&json!({"name": name, "value": value, "updated_at": "now"})).unwrap()
        };
        canary(&variable("CANARY_ORIGIN", json!(origin)), origin).unwrap();
        for record in [
            variable("CANARY_ORIGIN", json!("https://other.example.test")),
            variable("CANARY_ORIGIN", json!(format!("{origin}/"))),
            variable("OTHER_ORIGIN", json!(origin)),
            variable("CANARY_ORIGIN", json!([origin])),
            b"{}".to_vec(),
        ] {
            assert_eq!(
                code(canary(&record, origin)),
                "migration_canary_origin_mismatch"
            );
        }
        for record in [&b"[]"[..], b"not json", b"", b"null"] {
            assert_eq!(
                code(canary(record, origin)),
                "migration_canary_variable_invalid"
            );
        }
    }

    /// Thirty inspection reports with the Python controller's verdicts.
    #[test]
    fn inspections_are_admitted_as_recorded() {
        let recorded: Value = serde_json::from_str(include_str!("recorded_vectors.json")).unwrap();
        let inspections = recorded["inspections"].as_array().unwrap();
        assert_eq!(inspections.len(), 30);
        let mut admitted = 0;
        for vector in inspections {
            let source = vector["sourceHost"].as_str().unwrap();
            let target = vector["targetHost"].as_str().unwrap();
            match relying_party(&vector["inspection"], source, target) {
                Ok(identity) => {
                    assert_eq!(vector["rpId"], identity, "{vector}");
                    admitted += 1;
                }
                Err(error) => assert_eq!(vector["refused"], error.message, "{vector}"),
            }
        }
        assert_eq!(admitted, 4);
    }

    /// Twenty-nine spellings and eleven pairs of hosts with the Python
    /// controller's verdicts. A destination without a media address was an
    /// unnamed fault there and is a named refusal here.
    #[test]
    fn addresses_and_host_pairs_are_admitted_as_recorded() {
        let recorded: Value = serde_json::from_str(include_str!("recorded_vectors.json")).unwrap();
        let addresses = recorded["addresses"].as_array().unwrap();
        assert_eq!(addresses.len(), 29);
        for vector in addresses {
            let value = vector["value"].as_str().unwrap();
            match address(value) {
                Ok(canonical) => assert_eq!(vector["address"], canonical, "{value:?}"),
                Err(error) => assert_eq!(vector["refused"], error.message, "{value:?}"),
            }
        }
        let pairs = recorded["pairs"].as_array().unwrap();
        assert_eq!(pairs.len(), 11);
        let mut admitted = 0;
        for vector in pairs {
            let (source, target) = (&vector["source"], &vector["target"]);
            match transport(source, target) {
                Ok(selected) => {
                    assert!(vector["refused"].is_null(), "{target}");
                    let reached = address(target["ansible_host"].as_str().unwrap()).unwrap();
                    let wide = target.get("scpub_announce_ipv6").and_then(Value::as_str);
                    let wide = wide
                        .filter(|literal| !literal.is_empty())
                        .map(str::to_owned);
                    assert_eq!(
                        selected.addresses(),
                        wide.iter().chain([&reached]).cloned().collect()
                    );
                    assert_eq!(
                        selected,
                        Transport {
                            address: reached,
                            ipv6: wide
                        }
                    );
                    admitted += 1;
                }
                Err(error) if vector["refused"] == "ValueError" => {
                    assert_eq!(
                        error.message, "migration_literal_addresses_required",
                        "{target}"
                    );
                }
                Err(error) => assert_eq!(vector["refused"], error.message, "{target}"),
            }
        }
        assert_eq!(admitted, 5);
    }
}
