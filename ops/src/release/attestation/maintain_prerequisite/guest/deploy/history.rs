//! Passive native-actor traces joined to retained stdout identities and signed images.
//! The caller separately owns retained name-file anchors and original raw-byte custody.

use super::{Error, Result};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct Binding<'a> {
    pub current_images: &'a BTreeSet<String>,
    pub previous_images: &'a BTreeSet<String>,
    pub postgres_id: &'a str,
    pub postgres_image: &'a str,
    pub postgres_reference: &'a str,
    pub token: &'a str,
    pub label: &'a str,
}
struct Actor {
    name: String,
    image: String,
    kind: &'static str,
    actions: Vec<&'static str>,
}
fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value.get(key).and_then(Value::as_str).ok_or(Error::Input)
}
fn hex(value: &str, count: usize) -> bool {
    super::super::protocol::digest(value, count)
}
fn object_row(value: &Value) -> Result<()> {
    if value.is_object() {
        Ok(())
    } else {
        Err(Error::Input)
    }
}
fn service_event(
    event: &Value,
    attributes: &Value,
    identifier: &str,
    binding: &Binding<'_>,
) -> Result<()> {
    if attributes
        .get("com.docker.compose.project")
        .and_then(Value::as_str)
        != Some("simplestchat-public")
        || attributes.get(binding.label).and_then(Value::as_str) != Some(binding.token)
    {
        return Err(Error::Refused);
    }
    match attributes
        .get("com.docker.compose.service")
        .and_then(Value::as_str)
    {
        Some("postgres") => {
            let action = text(event, "Action")?
                .split(':')
                .next()
                .ok_or(Error::Input)?;
            let image = text(attributes, "image")?;
            if identifier != binding.postgres_id
                || !["exec_create", "exec_start", "exec_die"].contains(&action)
                || ![binding.postgres_reference, binding.postgres_image].contains(&image)
            {
                return Err(Error::Refused);
            }
        }
        Some("simplestchat") => {}
        _ => return Err(Error::Refused),
    }
    Ok(())
}

/// Preserve the frozen four-actor policy; these observations never grant cleanup authority.
pub(super) fn native_history(
    rows: &[Value],
    candidates: &BTreeSet<String>,
    binding: &Binding<'_>,
) -> Result<Value> {
    if rows.is_empty()
        || rows.len() >= 256
        || serde_json::to_vec(rows).map_err(|_| Error::Input)?.len() > 2 * 1024 * 1024
    {
        return Err(Error::Bounds);
    }
    let mut actors = BTreeMap::<String, Actor>::new();
    let mut order = Vec::<String>::new();
    for event in rows {
        object_row(event)?;
        if event.get("Type").and_then(Value::as_str) != Some("container") {
            continue;
        }
        let actor = event.get("Actor").ok_or(Error::Input)?;
        let attributes = actor.get("Attributes").ok_or(Error::Input)?;
        let name = text(attributes, "name")?;
        let identifier = text(actor, "ID")?;
        if !hex(identifier, 64) {
            return Err(Error::Input);
        }
        let kind = if let Some(suffix) = name.strip_prefix("scpub-release-validate-") {
            if !hex(suffix, 32) {
                return Err(Error::Refused);
            }
            "validator"
        } else if let Some(suffix) = name.strip_prefix("scpub-release-assets-") {
            if !hex(suffix, 32) {
                return Err(Error::Refused);
            }
            "assets"
        } else {
            service_event(event, attributes, identifier, binding)?;
            continue;
        };
        let image = text(attributes, "image")?;
        if !candidates.contains(identifier)
            || !(binding.current_images.contains(image) || binding.previous_images.contains(image))
        {
            return Err(Error::Refused);
        }
        let current = actors.entry(identifier.into()).or_insert_with(|| Actor {
            name: name.into(),
            image: image.into(),
            kind,
            actions: Vec::new(),
        });
        if current.name != name || current.image != image {
            return Err(Error::Refused);
        }
        let observed = event.get("Action");
        let action = match observed.and_then(Value::as_str) {
            Some("create") => "create",
            Some("start") => "start",
            Some("die") => "die",
            Some("destroy") => "destroy",
            _ if observed.is_some_and(|value| value.is_array() || value.is_object()) => {
                return Err(Error::Input);
            }
            _ => continue,
        };
        if action == "create" {
            order.push(identifier.into());
        }
        if action == "die" && attributes.get("exitCode").and_then(Value::as_str) != Some("0") {
            return Err(Error::Refused);
        }
        current.actions.push(action);
    }
    if actors.len() != 4 || order.len() != 4 {
        return Err(Error::Refused);
    }
    let kinds = order.iter().map(|id| actors[id].kind).collect::<Vec<_>>();
    if kinds != ["validator", "validator", "assets", "assets"] {
        return Err(Error::Refused);
    }
    for actor in actors.values() {
        let expected: &[&str] = if actor.kind == "validator" {
            &["create", "start", "die", "destroy"]
        } else {
            &["create", "destroy"]
        };
        if actor.actions != expected {
            return Err(Error::Refused);
        }
    }
    Ok(json!({
        "actorIds": order,
        "validatorCount": 2,
        "assetCaptureCount": 2,
        "allCreatedStartedAsRequiredExitedZeroAndRemoved": true,
        "historyRows": rows.len(),
        "nativeIsolationInspected": false,
    }))
}

/// Require the exact stopped candidate's observed lifecycle without granting cleanup authority.
pub(super) fn injected_sequence(
    rows: &[Value],
    id: &str,
    image: &str,
    token: &str,
    label: &str,
) -> Result<()> {
    let mut positions = [None; 4];
    let mut stopped = false;
    let mut repeated = false;
    for (index, event) in rows.iter().enumerate() {
        object_row(event)?;
        if event.get("Type").and_then(Value::as_str) != Some("container") {
            continue;
        }
        let actor = event
            .get("Actor")
            .and_then(Value::as_object)
            .ok_or(Error::Input)?;
        if actor.get("ID").and_then(Value::as_str) != Some(id) {
            continue;
        }
        let attributes = actor
            .get("Attributes")
            .and_then(Value::as_object)
            .ok_or(Error::Input)?;
        if attributes.get("image").and_then(Value::as_str) != Some(image)
            || attributes.get(label).and_then(Value::as_str) != Some(token)
        {
            return Err(Error::Refused);
        }
        let action = event.get("Action").unwrap_or(&Value::Null);
        if action.is_array() || action.is_object() {
            return Err(Error::Input);
        }
        let role = match action.as_str() {
            Some("create") => 0,
            Some("start") => 1,
            Some("die") => 2,
            Some("destroy") => 3,
            Some("stop") => {
                stopped = true;
                continue;
            }
            _ => continue,
        };
        repeated |= positions[role].replace(index).is_some();
    }
    if repeated
        || !stopped
        || positions.iter().any(Option::is_none)
        || !positions.windows(2).all(|pair| pair[0] < pair[1])
    {
        return Err(Error::Refused);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn id(byte: char) -> String {
        std::iter::repeat_n(byte, 64).collect()
    }
    fn image(byte: char) -> String {
        format!("sha256:{}", id(byte))
    }
    struct Fixture {
        current: BTreeSet<String>,
        previous: BTreeSet<String>,
        candidates: BTreeSet<String>,
        postgres: String,
        postgres_image: String,
        token: String,
    }
    impl Fixture {
        fn new() -> Self {
            Self {
                current: BTreeSet::from([image('a')]),
                previous: BTreeSet::from([image('b')]),
                candidates: ['1', '2', '3', '4'].into_iter().map(id).collect(),
                postgres: id('5'),
                postgres_image: image('6'),
                token: "7".repeat(32),
            }
        }
        fn binding(&self) -> Binding<'_> {
            Binding {
                current_images: &self.current,
                previous_images: &self.previous,
                postgres_id: &self.postgres,
                postgres_image: &self.postgres_image,
                postgres_reference: "postgres:pinned",
                token: &self.token,
                label: "simplestchat.maintain-prerequisite",
            }
        }
        fn admit(&self, rows: &[Value]) -> Result<Value> {
            native_history(rows, &self.candidates, &self.binding())
        }
        fn service(&self, service: &str, action: &str) -> Value {
            json!({"Type":"container", "Action":action, "Actor":{"ID":self.postgres,
                "Attributes":{"name":"owned-service", "image":self.postgres_image,
                    "com.docker.compose.project":"simplestchat-public",
                    "com.docker.compose.service":service,
                    "simplestchat.maintain-prerequisite":self.token}}})
        }
    }
    fn rows() -> Vec<Value> {
        let mut result = Vec::new();
        for (byte, kind, image_byte) in [
            ('1', "validate", 'a'),
            ('2', "validate", 'b'),
            ('3', "assets", 'a'),
            ('4', "assets", 'b'),
        ] {
            let actions: &[&str] = if kind == "validate" {
                &["create", "start", "die", "destroy"]
            } else {
                &["create", "destroy"]
            };
            for action in actions {
                result.push(json!({"Type":"container", "Action":action,
                    "Actor":{"ID":id(byte), "Attributes":{
                        "name":format!("scpub-release-{kind}-{}", byte.to_string().repeat(32)),
                        "image":image(image_byte), "exitCode":"0"}}}));
            }
        }
        result
    }
    #[test]
    fn complete_distinct_native_traces_project_only_the_original_six_fields() {
        let fixture = Fixture::new();
        let value = fixture.admit(&rows()).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 6);
        assert_eq!(value["actorIds"], json!(['1', '2', '3', '4'].map(id)));
        assert_eq!(value["validatorCount"], 2);
        assert_eq!(value["assetCaptureCount"], 2);
        assert_eq!(value["nativeIsolationInspected"], false);
        assert_eq!(value["historyRows"], 12);
    }
    #[test]
    fn missing_repeated_reordered_and_nonzero_traces_refuse() {
        let fixture = Fixture::new();
        for index in 0..rows().len() {
            let mut changed = rows();
            changed.remove(index);
            assert!(fixture.admit(&changed).is_err(), "missing {index}");
            let mut changed = rows();
            changed.insert(index, changed[index].clone());
            assert!(fixture.admit(&changed).is_err(), "duplicate {index}");
        }
        let mut changed = rows();
        changed.swap(0, 1);
        assert!(fixture.admit(&changed).is_err());
        let mut changed = rows();
        changed[2]["Actor"]["Attributes"]["exitCode"] = json!("1");
        assert!(fixture.admit(&changed).is_err());
        let mut changed = rows();
        changed.rotate_left(8);
        assert!(fixture.admit(&changed).is_err());
    }
    #[test]
    fn actual_stdout_candidate_and_signed_graph_joins_cannot_be_skipped() {
        let mut fixture = Fixture::new();
        fixture.candidates.remove(&id('2'));
        assert!(fixture.admit(&rows()).is_err());
        let fixture = Fixture::new();
        for (field, hostile) in [
            ("name", json!("scpub-release-validate-INERT_PRIVATE")),
            ("image", json!("sha256:unbound")),
        ] {
            let mut changed = rows();
            changed[1]["Actor"]["Attributes"][field] = hostile;
            assert!(fixture.admit(&changed).is_err());
        }
        let mut changed = rows();
        changed[1]["Actor"]["ID"] = json!(id('8'));
        assert!(fixture.admit(&changed).is_err());
    }
    #[test]
    fn unrelated_container_rows_require_the_exact_service_binding() {
        let fixture = Fixture::new();
        for action in ["exec_create: health", "exec_start: health", "exec_die"] {
            let mut changed = rows();
            changed.push(fixture.service("postgres", action));
            fixture.admit(&changed).unwrap();
        }
        let mut changed = rows();
        changed.push(fixture.service("simplestchat", "start"));
        fixture.admit(&changed).unwrap();
        for (field, hostile) in [
            ("com.docker.compose.project", "another"),
            ("com.docker.compose.service", "caddy"),
            ("simplestchat.maintain-prerequisite", "INERT_PRIVATE"),
            ("image", "postgres:unknown"),
        ] {
            let mut extra = fixture.service("postgres", "exec_die");
            extra["Actor"]["Attributes"][field] = json!(hostile);
            let mut changed = rows();
            changed.push(extra);
            assert!(fixture.admit(&changed).is_err(), "{field}");
        }
        for action in ["create", "start", "destroy", "exec_other", "exec_died"] {
            let mut changed = rows();
            changed.push(fixture.service("postgres", action));
            assert!(fixture.admit(&changed).is_err(), "{action}");
        }
    }
    #[test]
    fn old_label_is_explicitly_selectable_and_wrong_postgres_identity_refuses() {
        let fixture = Fixture::new();
        let mut extra = fixture.service("postgres", "exec_die");
        let attributes = extra["Actor"]["Attributes"].as_object_mut().unwrap();
        let token = attributes
            .remove("simplestchat.maintain-prerequisite")
            .unwrap();
        attributes.insert("simplestchat.release-fixture".into(), token);
        let mut changed = rows();
        changed.push(extra.clone());
        assert!(fixture.admit(&changed).is_err());
        let mut binding = fixture.binding();
        binding.label = "simplestchat.release-fixture";
        native_history(&changed, &fixture.candidates, &binding).unwrap();
        extra["Actor"]["ID"] = json!(id('9'));
        changed.pop();
        changed.push(extra);
        assert!(native_history(&changed, &fixture.candidates, &binding).is_err());
    }
    #[test]
    fn row_and_byte_budgets_are_closed_and_private_fields_are_not_projected() {
        let fixture = Fixture::new();
        assert_eq!(fixture.admit(&[]), Err(Error::Bounds));
        let mut changed = rows();
        changed.resize(255, json!({"Type":"network"}));
        fixture.admit(&changed).unwrap();
        changed.push(json!({"Type":"network"}));
        assert_eq!(fixture.admit(&changed), Err(Error::Bounds));
        let mut changed = rows();
        changed[0]["private"] = json!("INERT_PRIVATE");
        assert!(
            !fixture
                .admit(&changed)
                .unwrap()
                .to_string()
                .contains("INERT_PRIVATE")
        );
        changed[0]["private"] = json!("x".repeat(2 * 1024 * 1024));
        assert_eq!(fixture.admit(&changed), Err(Error::Bounds));
    }
    #[test]
    fn frozen_reference_and_its_native_isolation_limit_remain_source_bound() {
        use sha2::{Digest, Sha256};
        let source = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../build/security_vm_deploy_guest.py"
        ));
        assert_eq!(source.len(), 129071);
        let hash = Sha256::digest(source)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(
            hash,
            "55bc7b486b61c2200a2b17f0b4d4a25dff8416eddda50022a632f9e4a030a7bd"
        );
    }
    #[test]
    fn every_event_is_an_object_before_type_filtering_and_actor_actions_keep_frozen_shapes() {
        let fixture = Fixture::new();
        for row in [
            Value::Null,
            json!([]),
            json!([{}]),
            json!("container"),
            json!(1),
            json!(true),
        ] {
            let mut changed = rows();
            changed.insert(0, row);
            assert_eq!(fixture.admit(&changed), Err(Error::Input));
        }
        for row in [
            json!({}),
            json!({"Type":"network"}),
            json!({"Type":"unknown"}),
        ] {
            let mut changed = rows();
            changed.insert(0, row);
            assert!(fixture.admit(&changed).is_ok());
        }
        for action in [json!([]), json!({})] {
            let mut changed = rows();
            let mut extra = changed[0].clone();
            extra["Action"] = action;
            changed.push(extra);
            assert_eq!(fixture.admit(&changed), Err(Error::Input));
        }
        for action in [Value::Null, json!(false), json!(7), json!("unknown")] {
            let mut changed = rows();
            let mut extra = changed[0].clone();
            extra["Action"] = action;
            changed.push(extra);
            assert!(fixture.admit(&changed).is_ok());
        }
    }
    #[test]
    fn injected_candidate_requires_each_lifecycle_once_and_at_least_one_unordered_stop() {
        let label = "simplestchat.maintain-prerequisite";
        let candidate = id('8');
        let selected = image('b');
        let token = "7".repeat(32);
        let event = |action: &str| {
            json!({"Type":"container", "Action":action,
            "Actor":{"ID":candidate, "Attributes":{"image":selected,
                (label):token, "exitCode":"137"}}})
        };
        let original = ["create", "start", "stop", "die", "destroy"].map(event);
        let check = |rows: &[Value]| injected_sequence(rows, &candidate, &selected, &token, label);
        assert!(check(&original).is_ok());
        for row in [
            Value::Null,
            json!([]),
            json!("container"),
            json!(1),
            json!(false),
        ] {
            let mut changed = original.to_vec();
            changed.insert(0, row);
            assert_eq!(check(&changed), Err(Error::Input));
        }
        for at in 0..original.len() {
            let mut missing = original.to_vec();
            missing.remove(at);
            assert!(check(&missing).is_err());
            let mut duplicate = original.to_vec();
            duplicate.insert(at, original[at].clone());
            assert_eq!(check(&duplicate).is_ok(), at == 2);
        }
        for (left, right) in [(0, 1), (1, 3), (3, 4)] {
            let mut changed = original.clone();
            changed.swap(left, right);
            assert!(check(&changed).is_err());
        }
        // The frozen predicate places no order requirement on stop, nor a zero-exit guard.
        for at in 0..=4 {
            let mut moved = original.to_vec();
            let stop = moved.remove(2);
            moved.insert(at, stop);
            assert!(check(&moved).is_ok());
        }
        let mut noisy = original.to_vec();
        noisy.insert(0, json!({"Type":"network"}));
        noisy.push(json!({"Type":"container", "Actor":{"ID":id('9')}}));
        noisy.push(event("health_status: healthy"));
        assert!(check(&noisy).is_ok());
        for key in ["image", label] {
            let mut changed = noisy.clone();
            changed.last_mut().unwrap()["Actor"]["Attributes"][key] = "wrong".into();
            assert!(check(&changed).is_err());
        }
        let mut changed = original.to_vec();
        changed[0]["Action"] = json!({});
        assert!(check(&changed).is_err());
    }
}
