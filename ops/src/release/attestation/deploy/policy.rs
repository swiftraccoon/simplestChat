//! Original rendered runtime/configuration policy; no YAML parser or secret output.

use crate::json::Json;
use std::collections::{BTreeMap, BTreeSet};

pub(super) type Result<T> = std::result::Result<T, &'static str>;
pub(super) fn ensure(value: bool, message: &'static str) -> Result<()> {
    if value { Ok(()) } else { Err(message) }
}
pub(super) fn decode(data: &[u8]) -> Result<Json> {
    crate::json::decode_limited(data, 2 * 1024 * 1024).map_err(|error| match error {
        crate::Error::AmbiguousJson => "DuplicateJsonError",
        crate::Error::Json => "JSONDecodeError",
        _ => "ValueError",
    })
}
pub(super) fn decode_text(data: &[u8]) -> Result<Json> {
    let text = std::str::from_utf8(data).map_err(|_| "UnicodeDecodeError")?;
    if text.starts_with('\u{feff}') || text.contains('\0') {
        return Err("JSONDecodeError");
    }
    decode(data)
}
pub(super) fn object(value: &Json) -> Result<&BTreeMap<String, Json>> {
    value.object().ok_or("ValueError")
}
pub(super) fn object_mut(value: &mut Json) -> Result<&mut BTreeMap<String, Json>> {
    if let Json::Object(value, _) = value {
        Ok(value)
    } else {
        Err("ValueError")
    }
}
pub(super) fn member<'a>(value: &'a Json, name: &str) -> Result<&'a Json> {
    object(value)?.get(name).ok_or("KeyError")
}
pub(super) fn string(value: &Json) -> Result<&str> {
    value.string().ok_or("ValueError")
}
pub(super) fn text<'a>(value: &'a Json, name: &str) -> Result<&'a str> {
    string(member(value, name)?)
}
pub(super) fn array(value: &Json) -> Result<&[Json]> {
    value.array().ok_or("ValueError")
}
pub(super) fn fixed(value: serde_json::Value) -> Result<Json> {
    decode(&serde_json::to_vec(&value).map_err(|_| "OSError")?)
}
pub(super) fn exact_hex(value: &str, count: usize) -> bool {
    value.len() == count
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
pub(super) fn image_id(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|value| exact_hex(value, 64))
}
fn null_or_empty(value: Option<&Json>) -> bool {
    matches!(value, None | Some(Json::Null))
        || value.and_then(Json::array).is_some_and(<[Json]>::is_empty)
}
fn strings(value: Option<&Json>, expected: &[&str]) -> bool {
    value.and_then(Json::array).is_some_and(|values| {
        values.len() == expected.len()
            && values
                .iter()
                .zip(expected)
                .all(|(value, expected)| value.string() == Some(*expected))
    })
}
fn environment_names<'a>(names: impl Iterator<Item = &'a str>) -> Result<()> {
    let mut seen = BTreeSet::new();
    for name in names {
        ensure(seen.insert(name), "RuntimeProfileError")?;
        ensure(
            !name.is_empty()
                && name.bytes().enumerate().all(|(at, byte)| {
                    byte == b'_' || byte.is_ascii_alphabetic() || at > 0 && byte.is_ascii_digit()
                })
                && !name.starts_with("LD_")
                && !name.starts_with("OPENSSL_")
                && !["GLIBC_TUNABLES", "GCONV_PATH", "LOCPATH"].contains(&name),
            "RuntimeProfileError",
        )?;
    }
    Ok(())
}
pub(super) fn compose(config: &Json, socket: &str, image: &str) -> Result<()> {
    const FIELDS: &[&str] = &[
        "build",
        "cap_drop",
        "command",
        "cpus",
        "depends_on",
        "entrypoint",
        "environment",
        "healthcheck",
        "image",
        "init",
        "labels",
        "logging",
        "mem_limit",
        "network_mode",
        "networks",
        "pids_limit",
        "ports",
        "profiles",
        "read_only",
        "restart",
        "security_opt",
        "stop_grace_period",
        "tmpfs",
        "ulimits",
        "user",
        "volumes",
        "working_dir",
    ];
    ensure(image_id(image), "RuntimeProfileError")?;
    let services = member(config, "services")?;
    for name in ["simplestchat", "migrate"] {
        let service = member(services, name)?;
        let fields = object(service)?;
        ensure(
            text(service, "image")? == image
                && fields.keys().all(|key| FIELDS.contains(&key.as_str())),
            "RuntimeProfileError",
        )?;
        ensure(
            text(service, "user")? == "10001:10001"
                && (matches!(service.get("working_dir"), None | Some(Json::Null))
                    || service.get("working_dir").and_then(Json::string) == Some("/app")),
            "RuntimeProfileError",
        )?;
        ensure(
            (matches!(service.get("command"), None | Some(Json::Null))
                || strings(service.get("command"), &["/app/simplestChat"]))
                && null_or_empty(service.get("entrypoint"))
                && matches!(service.get("healthcheck"), None | Some(Json::Null)),
            "RuntimeProfileError",
        )?;
        ensure(
            service.get("read_only").and_then(Json::boolean) == Some(true)
                && service.get("init").and_then(Json::boolean) == Some(true)
                && strings(service.get("cap_drop"), &["ALL"])
                && strings(service.get("security_opt"), &["no-new-privileges:true"]),
            "RuntimeProfileError",
        )?;
        let environment = object(member(service, "environment")?)?;
        environment_names(environment.keys().map(String::as_str))?;
        ensure(
            environment.values().all(|value| value.string().is_some())
                && environment.get("RUN_MIGRATIONS").and_then(Json::string)
                    == Some(if name == "migrate" { "true" } else { "false" }),
            "RuntimeProfileError",
        )?;
        let mounts = array(member(service, "volumes")?)?;
        ensure(mounts.len() == 1, "RuntimeProfileError")?;
        let mount = &mounts[0];
        ensure(
            object(mount)?.keys().all(|name| {
                ["type", "source", "target", "read_only", "bind"].contains(&name.as_str())
            }) && text(mount, "type")? == "bind"
                && text(mount, "source")? == socket
                && text(mount, "target")? == "/run/simplestchat-postgres"
                && mount.get("read_only").and_then(Json::boolean) == Some(true),
            "RuntimeProfileError",
        )?;
        if let Some(bind) = mount.get("bind") {
            ensure(
                bind.object().is_some_and(BTreeMap::is_empty)
                    || bind.semantic_eq(&fixed(serde_json::json!({"create_host_path":true}))?),
                "RuntimeProfileError",
            )?;
        }
        let tmpfs = format!(
            "/tmp:rw,nosuid,nodev,noexec,size={},mode=1777",
            if name == "migrate" { "32m" } else { "64m" }
        );
        ensure(
            strings(service.get("tmpfs"), &[&tmpfs]),
            "RuntimeProfileError",
        )?;
    }
    Ok(())
}
pub(super) fn browser_mount(proxy: &Json, store: &str) -> std::result::Result<(), String> {
    let empty = Json::Array(Vec::new());
    let mounts = array(proxy.get("volumes").unwrap_or(&empty))?;
    let mut selected = Vec::new();
    for mount in mounts {
        if object(mount)?.get("target").and_then(Json::string)
            == Some("/run/simplestchat-browser-assets")
        {
            selected.push(mount);
        }
    }
    ensure(
        !selected.is_empty(),
        "Browser assets need --maintenance --browser-assets-upgrade before app-only releases",
    )?;
    ensure(selected.len() == 1, "Ambiguous browser asset proxy mount")?;
    let mount = selected[0];
    let expected = fixed(serde_json::json!({
        "type":"bind","source":store,"target":"/run/simplestchat-browser-assets","read_only":true
    }))?;
    let actual = object(mount)?
        .iter()
        .filter(|(name, _)| name.as_str() != "bind")
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect::<BTreeMap<_, _>>();
    let expected = object(&expected)?;
    // dict.get() treats an absent key and an explicit null identically here.
    // Unreviewed key bytes may themselves be private; never project them into
    // the fixed report. Reviewed mismatch names retain the original ordering.
    if actual
        .iter()
        .any(|(name, value)| !expected.contains_key(name) && !value.semantic_eq(&Json::Null))
    {
        return Err("Browser asset proxy mount contains unreviewed fields".into());
    }
    let mut differing = expected
        .iter()
        .filter(|(name, value)| !actual.get(*name).unwrap_or(&Json::Null).semantic_eq(value))
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>();
    if mount
        .get("bind")
        .is_some_and(|bind| bind.object().is_none())
    {
        differing.push("bind");
    }
    if differing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "Browser asset proxy mount differs from reviewed readonly store: {}",
            differing.join(", ")
        ))
    }
}
pub(super) fn ledger(data: &[u8]) -> Result<BTreeMap<String, String>> {
    let text = std::str::from_utf8(data).map_err(|_| "UnicodeDecodeError")?;
    let mut result = BTreeMap::new();
    for line in super::super::stage::python_lines(text) {
        let fields = line
            .split(super::super::stage::python_space)
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>();
        ensure(fields.len() == 3, "Invalid migration ledger")?;
        let version = fields[0];
        ensure(
            version
                .as_bytes()
                .first()
                .is_some_and(|v| (b'1'..=b'9').contains(v))
                && version.bytes().all(|v| v.is_ascii_digit())
                && fields[1] == "t"
                && exact_hex(fields[2], 96)
                && !result.contains_key(version),
            "Invalid migration ledger",
        )?;
        result.insert(version.into(), fields[2].into());
    }
    ensure(!result.is_empty(), "Empty migration ledger")?;
    Ok(result)
}
pub(super) fn stable(container: &Json) -> Result<Json> {
    Ok(Json::Array(vec![
        member(container, "id")?.clone(),
        member(container, "image")?.clone(),
        member(member(container, "state")?, "StartedAt")?.clone(),
        member(container, "restarts")?.clone(),
    ]))
}

/// Pinned Python int(token), including Nd digits and separators. The existing
/// 4300-digit decimal conversion bound counts digits, not underscores.
pub(super) fn metrics_integer(value: &str) -> Option<Json> {
    let (negative, digits) = match value.as_bytes().first() {
        Some(b'-') => (true, &value[1..]),
        Some(b'+') => (false, &value[1..]),
        _ => (false, value),
    };
    let mut normalized = String::new();
    let mut previous_digit = false;
    for character in digits.chars() {
        if character == '_' {
            if !previous_digit {
                return None;
            }
            previous_digit = false;
        } else {
            let digit = crate::release::manifest::decimal(character)?;
            normalized.push(char::from(b'0' + u8::try_from(digit).ok()?));
            if normalized.len() > 4300 {
                return None;
            }
            previous_digit = true;
        }
    }
    if !previous_digit {
        return None;
    }
    let significant = normalized.trim_start_matches('0');
    Some(Json::Integer(if significant.is_empty() {
        "0".into()
    } else if negative {
        format!("-{significant}")
    } else {
        significant.into()
    }))
}

#[cfg(test)]
mod browser_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mount_mismatch_lists_the_original_sorted_keys_and_bind_last() {
        let proxy = fixed(json!({"volumes":[{
            "type":"volume","source":"PRIVATE_SOURCE_VALUE",
            "target":"/run/simplestchat-browser-assets","read_only":false,
            "bind":null
        }]}))
        .unwrap();
        let refused = browser_mount(&proxy, "/reviewed/browser-assets");
        assert_eq!(
            refused,
            Err(concat!(
                "Browser asset proxy mount differs from reviewed readonly store: ",
                "read_only, source, type, bind"
            )
            .into())
        );
        let missing = fixed(json!({"volumes":[{
            "type":"bind","source":"/reviewed/browser-assets",
            "target":"/run/simplestchat-browser-assets"
        }]}))
        .unwrap();
        assert_eq!(
            browser_mount(&missing, "/reviewed/browser-assets"),
            Err(concat!(
                "Browser asset proxy mount differs from reviewed readonly store: ",
                "read_only"
            )
            .into())
        );
    }

    #[test]
    fn mount_mismatch_never_projects_an_unknown_key_or_value() {
        let proxy = fixed(json!({"volumes":[{
            "type":"bind","source":"/reviewed/browser-assets",
            "target":"/run/simplestchat-browser-assets","read_only":false,
            "PRIVATE_UNKNOWN_KEY":"PRIVATE_UNKNOWN_VALUE"
        }]}))
        .unwrap();
        assert_eq!(
            browser_mount(&proxy, "/reviewed/browser-assets"),
            Err("Browser asset proxy mount contains unreviewed fields".into())
        );
    }

    #[test]
    fn mount_comparison_preserves_null_missing_and_compose_bind_semantics() {
        for bind in [None, Some(json!({"propagation":"rprivate"}))] {
            let mut mount = json!({
                "type":"bind","source":"/reviewed/browser-assets",
                "target":"/run/simplestchat-browser-assets","read_only":1,"extra":null
            });
            if let Some(bind) = bind {
                mount["bind"] = bind;
            }
            let proxy = fixed(json!({"volumes":[mount]})).unwrap();
            assert_eq!(browser_mount(&proxy, "/reviewed/browser-assets"), Ok(()));
        }
    }
}
