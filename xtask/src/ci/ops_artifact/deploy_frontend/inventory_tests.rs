use super::*;
use serde_json::json;

const PRIVATE: &str = "PRIVATE_INVENTORY_SENTINEL";
fn original() -> Value {
    json!({"benchmark_hosts":{"hosts":["public"]},"_meta":{"hostvars":{"public":{
        "scpub_domain":"chat.example.test","ansible_host":"192.0.2.10","ansible_user":"debian",
        "ansible_ssh_private_key_file":"/private/controller key","private_value":PRIVATE
    }}}})
}
fn bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}
fn variables(value: &mut Value) -> &mut Value {
    &mut value["_meta"]["hostvars"]["public"]
}

#[test]
fn complete_selected_variables_are_preserved_for_drift_and_frozen_inventory_without_debug_leaks() {
    let source = original();
    let selected = selected(&bytes(&source), None, "chat.example.test").unwrap();
    assert_eq!(selected.host, "public");
    assert_eq!(selected.variables, source["_meta"]["hostvars"]["public"]);
    assert_eq!(format!("{selected:?}"), "PinnedReleaseFrontendTarget");
    assert!(!format!("{selected:?}").contains(PRIVATE));
    for (field, value) in [
        ("ansible_host", json!("192.0.2.20")),
        ("ansible_user", json!("other")),
        ("ansible_ssh_private_key_file", json!("/different/key")),
        ("ansible_port", json!(2222)),
        ("private_value", json!("another private value")),
    ] {
        let mut changed = source.clone();
        variables(&mut changed)[field] = value;
        let later = super::selected(&bytes(&changed), None, "chat.example.test").unwrap();
        assert_ne!(selected, later, "{field}");
    }
}

#[test]
fn exact_limit_cannot_expand_through_a_same_named_group_and_multiple_hosts_need_selection() {
    let mut value = original();
    value["benchmark_hosts"]["hosts"] = json!(["public", "second"]);
    value["public"] = json!({"hosts":["public","second"]});
    value["_meta"]["hostvars"]["second"] = value["_meta"]["hostvars"]["public"].clone();
    assert!(selected(&bytes(&value), None, "chat.example.test").is_err());
    assert_eq!(
        selected(&bytes(&value), Some("public"), "chat.example.test")
            .unwrap()
            .host,
        "public"
    );
    for limit in ["all", "public:second", "public*", "missing", "-option"] {
        assert!(
            selected(&bytes(&value), Some(limit), "chat.example.test").is_err(),
            "{limit}"
        );
    }
}

#[test]
fn graph_traversal_accepts_shared_children_but_refuses_cycles_duplicate_lists_and_missing_groups() {
    let mut valid = original();
    valid["benchmark_hosts"] = json!({"children":["left","right"]});
    valid["left"] = json!({"children":["_shared"]});
    valid["right"] = json!({"children":["_shared"]});
    valid["_shared"] = json!({"hosts":["public"]});
    assert!(selected(&bytes(&valid), None, "chat.example.test").is_ok());
    for (key, value) in [
        ("benchmark_hosts", json!({"children":["left","left"]})),
        ("left", json!({"children":["benchmark_hosts"]})),
        ("right", json!({"children":["missing"]})),
        ("_shared", json!({"hosts":["public","public"]})),
        ("_shared", json!({"hosts":[true]})),
        ("_shared", json!({"hosts":"public"})),
    ] {
        let mut changed = valid.clone();
        changed[key] = value;
        assert!(
            selected(&bytes(&changed), None, "chat.example.test").is_err(),
            "{key}"
        );
    }
    let mut deep = original();
    deep["benchmark_hosts"] = json!({"children":["group0"]});
    for n in 0..=DEPTH {
        deep[format!("group{n}")] = json!({"children":[format!("group{}",n+1)]});
    }
    deep[format!("group{}", DEPTH + 1)] = json!({"hosts":["public"]});
    assert!(selected(&bytes(&deep), None, "chat.example.test").is_err());
    let mut wide = original();
    wide["benchmark_hosts"]["hosts"] = json!(
        (0..=MEMBERS)
            .map(|n| format!("host{n}"))
            .collect::<Vec<_>>()
    );
    assert!(selected(&bytes(&wide), None, "chat.example.test").is_err());
}

#[test]
fn every_plain_ssh_boundary_refuses_coercion_credentials_and_argument_injection() {
    for (key, value) in [
        ("ansible_connection", json!("local")),
        ("ansible_connection", json!("paramiko")),
        ("ansible_connection", Value::Null),
        ("ansible_host", json!("-oProxyCommand=command")),
        ("ansible_host", json!("2001:db8::1")),
        ("ansible_host", json!("")),
        ("ansible_user", json!("")),
        ("ansible_user", json!("root;command")),
        ("ansible_ssh_private_key_file", json!("~/.ssh/key")),
        ("ansible_ssh_private_key_file", json!("/private/../key")),
        ("ansible_ssh_private_key_file", json!("/private//key")),
        (
            "ansible_ssh_private_key_file",
            json!("/private/control\u{0085}key"),
        ),
        ("ansible_port", json!(0)),
        ("ansible_port", json!(65536)),
        ("ansible_port", json!(true)),
        ("ansible_port", json!(22.0)),
        ("ansible_port", json!("022")),
        ("ansible_port", json!("22.0")),
        ("ansible_password", Value::Null),
        ("ansible_ssh_pass", json!("")),
        ("ansible_ssh_password", json!(PRIVATE)),
        ("ansible_become_password", json!(PRIVATE)),
        ("ansible_become_pass", json!(PRIVATE)),
        ("ansible_ssh_common_args", json!("-J jump.example.test")),
        ("ansible_ssh_extra_args", json!("-F private.conf")),
    ] {
        let mut changed = original();
        variables(&mut changed)[key] = value;
        let failure = selected(&bytes(&changed), None, "chat.example.test").unwrap_err();
        assert_eq!(failure.message, "unsupported_inventory_ssh", "{key}");
        assert!(!format!("{failure:?}").contains(PRIVATE));
    }
    for port in [json!(1), json!(65535), json!("2222")] {
        let mut changed = original();
        variables(&mut changed)["ansible_port"] = port;
        assert!(selected(&bytes(&changed), None, "chat.example.test").is_ok());
    }
}

#[test]
fn selected_domain_and_required_ssh_fields_are_checked_before_any_effect() {
    for key in [
        "ansible_host",
        "ansible_user",
        "ansible_ssh_private_key_file",
    ] {
        let mut changed = original();
        variables(&mut changed).as_object_mut().unwrap().remove(key);
        assert!(
            selected(&bytes(&changed), None, "chat.example.test").is_err(),
            "{key}"
        );
    }
    assert!(selected(&bytes(&original()), None, "other.example.test").is_err());
    assert!(selected(&bytes(&original()), None, "").is_err());
}

#[test]
fn duplicates_even_inside_private_hostvars_and_oversized_or_invalid_json_are_refused() {
    let valid = String::from_utf8(bytes(&original())).unwrap();
    let duplicate = valid.replacen(
        "\"private_value\":",
        "\"private_value\":null,\"private_value\":",
        1,
    );
    assert!(selected(duplicate.as_bytes(), None, "chat.example.test").is_err());
    let duplicate = valid.replacen(
        "\"benchmark_hosts\":",
        "\"benchmark_hosts\":null,\"benchmark_hosts\":",
        1,
    );
    assert!(selected(duplicate.as_bytes(), None, "chat.example.test").is_err());
    for input in [
        b"{\"unused\":NaN}".as_slice(),
        b"{\"unused\":1e999}",
        b"[]",
        b"null",
        b"{}",
    ] {
        assert!(selected(input, None, "chat.example.test").is_err());
    }
    assert!(selected(&vec![b' '; RESPONSE + 1], None, "chat.example.test").is_err());
}
