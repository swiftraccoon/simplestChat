//! Original rendered runtime/configuration policy; no YAML parser or secret output.

use crate::json::Json;
use std::collections::BTreeMap;

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
/// Require the application and the migration service of a resolved model
/// to run `image` as shipped. The rules are the launcher's own; a refusal
/// is named here as the release engine named it.
pub(super) fn compose(config: &Json, socket: &str, image: &str) -> Result<()> {
    use crate::release::profile::{self, Refusal};
    profile::compose(config, socket, image, true).map_err(|refusal| match refusal {
        Refusal::Code(_) => "RuntimeProfileError",
        Refusal::Key => "KeyError",
        Refusal::Value => "ValueError",
    })
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
mod profile_tests {
    use super::*;
    use serde_json::json;

    /// A resolved project whose two managed services run `image` as shipped.
    fn project(image: &str, sockets: &str) -> serde_json::Value {
        let service = |migration: bool| {
            let (switch, size) = if migration {
                ("true", "32m")
            } else {
                ("false", "64m")
            };
            json!({"image":image,"user":"10001:10001","read_only":true,"init":true,
                "cap_drop":["ALL"],"security_opt":["no-new-privileges:true"],
                "environment":{"RUN_MIGRATIONS":switch},
                "volumes":[{"type":"bind","source":sockets,
                    "target":"/run/simplestchat-postgres","read_only":true}],
                "tmpfs":[format!("/tmp:rw,nosuid,nodev,noexec,size={size},mode=1777")]})
        };
        json!({"services":{"simplestchat":service(false),"migrate":service(true)}})
    }

    /// A deployment judges the application and the migration service by
    /// the launcher's rules, at its own socket directory, and names a
    /// refusal as the release engine did: by the kind of the fault.
    #[test]
    fn a_project_is_judged_by_the_launchers_rules_and_refused_by_kind() {
        let image = format!("sha256:{}", "a".repeat(64));
        let sockets = "/srv/fixture/postgres-socket";
        let judged = |change: &dyn Fn(&mut serde_json::Value)| {
            let mut model = project(&image, sockets);
            change(&mut model);
            compose(&fixed(model).unwrap(), sockets, &image)
        };
        assert_eq!(judged(&|_| ()), Ok(()));
        let unmet: [&dyn Fn(&mut serde_json::Value); 5] = [
            &|model| {
                drop(
                    model["services"]["simplestchat"]
                        .as_object_mut()
                        .unwrap()
                        .remove("image"),
                )
            },
            &|model| {
                drop(
                    model["services"]["simplestchat"]
                        .as_object_mut()
                        .unwrap()
                        .remove("user"),
                )
            },
            &|model| model["services"]["migrate"]["read_only"] = json!(false),
            &|model| model["services"]["migrate"]["environment"]["RUN_MIGRATIONS"] = json!("false"),
            &|model| {
                model["services"]["simplestchat"]["volumes"][0]["source"] = json!("/srv/other")
            },
        ];
        for change in unmet {
            assert_eq!(judged(change), Err("RuntimeProfileError"));
        }
        let absent: [&dyn Fn(&mut serde_json::Value); 4] = [
            &|model| drop(model.as_object_mut().unwrap().remove("services")),
            &|model| drop(model["services"].as_object_mut().unwrap().remove("migrate")),
            &|model| {
                drop(
                    model["services"]["simplestchat"]
                        .as_object_mut()
                        .unwrap()
                        .remove("environment"),
                )
            },
            &|model| {
                drop(
                    model["services"]["migrate"]
                        .as_object_mut()
                        .unwrap()
                        .remove("volumes"),
                )
            },
        ];
        for change in absent {
            assert_eq!(judged(change), Err("KeyError"));
        }
        let shaped: [&dyn Fn(&mut serde_json::Value); 3] = [
            &|model| model["services"] = json!([]),
            &|model| model["services"]["simplestchat"] = json!("service"),
            &|model| model["services"]["migrate"]["volumes"] = json!({}),
        ];
        for change in shaped {
            assert_eq!(judged(change), Err("ValueError"));
        }
        // The services are looked for before the image is judged.
        let model = fixed(project(&image, sockets)).unwrap();
        assert_eq!(
            compose(&model, sockets, "latest"),
            Err("RuntimeProfileError")
        );
        assert_eq!(
            compose(&fixed(json!({})).unwrap(), sockets, "latest"),
            Err("KeyError")
        );
        // Another socket directory is another deployment's.
        assert_eq!(
            compose(&model, "/srv/other/postgres-socket", &image),
            Err("RuntimeProfileError")
        );
    }
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
