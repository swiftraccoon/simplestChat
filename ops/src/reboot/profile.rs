//! The managed server's execution profile: what its image must declare and
//! what the resolved project may ask of it. Configuration names and values
//! never enter a diagnostic; a mismatch is one fixed kind of fault.

use crate::host::fault::{Fault, KEY, Result, VALUE, field, require, text};
use crate::host::image_id;
use crate::json::Json;
use crate::release::profile::{self, Refusal};

const COMMAND: &str = "/app/simplestChat";
/// An execution profile that is not the managed one.
const PROFILE: Fault = Fault::Kind("RuntimeProfileError");

fn is_text(value: Option<&Json>, expected: &str) -> bool {
    value.and_then(Json::string) == Some(expected)
}

/// Whether `value` is a list of exactly these texts.
fn is_list(value: Option<&Json>, expected: &[&str]) -> bool {
    value.and_then(Json::array).is_some_and(|items| {
        items.len() == expected.len()
            && items
                .iter()
                .zip(expected)
                .all(|(item, text)| item.string() == Some(*text))
    })
}

fn empty_list(value: &Json) -> bool {
    value.array().is_some_and(<[Json]>::is_empty)
}

/// A refusal of the launcher's rules, as the reboot helper saw it.
fn fault(refusal: Refusal) -> Fault {
    match refusal {
        Refusal::Code(_) => PROFILE,
        Refusal::Key => KEY,
        Refusal::Value => VALUE,
    }
}

/// Require the inspected image to be the reviewed runtime of `revision` and
/// return its identity.
pub(crate) fn image_identity(inspected: &Json, revision: &str) -> Result<String> {
    inspected.object().ok_or(VALUE)?;
    let identifier = text(field(inspected, "id")?)?;
    require(
        image_id(identifier),
        "Missing local content-addressed image ID",
    )?;
    for (name, expected) in [
        ("os", "linux"),
        ("architecture", "amd64"),
        ("user", "10001:10001"),
    ] {
        let matches = is_text(Some(field(inspected, name)?), expected);
        require(matches, "Unexpected image platform or runtime user")?;
    }
    let labels = field(inspected, "labels")?;
    labels.object().ok_or(VALUE)?;
    let label = labels.get("org.opencontainers.image.revision");
    require(is_text(label, revision), "Image revision mismatch")?;
    let command = is_list(Some(field(inspected, "cmd")?), &[COMMAND]);
    require(command, "Unexpected image entrypoint")?;
    let entrypoint = field(inspected, "entrypoint")?;
    let none = matches!(entrypoint, Json::Null) || empty_list(entrypoint);
    require(none, "Unexpected image entrypoint")?;
    let directory = is_text(Some(field(inspected, "workingDir")?), "/app");
    require(directory, "Unexpected image execution defaults")?;
    let unchecked = field(inspected, "hasHealthcheck")?.boolean() == Some(false);
    require(unchecked, "Unexpected image execution defaults")?;
    let names = field(inspected, "envNames")?.array().ok_or(VALUE)?;
    let names = names.iter().map(text).collect::<Result<Vec<&str>>>()?;
    profile::environment_names(&names).map_err(fault)?;
    Ok(identifier.to_owned())
}

/// Validate the resolved application service before it is stopped or
/// started, by the launcher's own rules for it: the selected image, only
/// reviewed fields, the fixed user and command, a read-only root with every
/// capability dropped, an environment of plain text without overrides, the
/// one socket mount and the bounded temporary filesystem.
pub(super) fn compose(resolved: &Json, socket: &str, image: &str) -> Result<()> {
    profile::compose(resolved, socket, image, false).map_err(fault)
}

/// Whether a fault is one a readiness poll retries: a refusal or a value of
/// the wrong shape, but never a deadline of the probe itself.
pub(super) fn retried(fault: &Fault) -> bool {
    matches!(fault, Fault::Refused(_)) || *fault == VALUE
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn judged(model: serde_json::Value, image: &str) -> Result<()> {
        let model = crate::host::state::decode(&serde_json::to_vec(&model).unwrap()).unwrap();
        compose(&model, "/srv/fixture/postgres-socket", image)
    }

    /// A reboot judges the application alone, by the launcher's rules, and
    /// names a refusal as the reboot helper saw it: one fixed kind for a
    /// requirement that is not met, and the kinds of a missing field and of
    /// a value of the wrong shape.
    #[test]
    fn the_application_alone_is_judged_and_a_refusal_keeps_its_kind() {
        let image = format!("sha256:{}", "a".repeat(64));
        let application = json!({"image":image,"user":"10001:10001","read_only":true,
            "init":true,"cap_drop":["ALL"],"security_opt":["no-new-privileges:true"],
            "environment":{"RUN_MIGRATIONS":"false"},
            "volumes":[{"type":"bind","source":"/srv/fixture/postgres-socket",
                "target":"/run/simplestchat-postgres","read_only":true}],
            "tmpfs":["/tmp:rw,nosuid,nodev,noexec,size=64m,mode=1777"]});
        let model = json!({"services":{"simplestchat":application}});
        assert_eq!(judged(model.clone(), &image), Ok(()));
        // The migration service is no part of a reboot, whatever it says.
        let mut with = model.clone();
        with["services"]["migrate"] = json!({"image":"anything","privileged":true});
        assert_eq!(judged(with, &image), Ok(()));
        let mut unmet = model.clone();
        unmet["services"]["simplestchat"]["read_only"] = json!(false);
        assert_eq!(judged(unmet, &image), Err(PROFILE));
        assert_eq!(judged(model.clone(), "latest"), Err(PROFILE));
        let mut absent = model.clone();
        absent["services"]["simplestchat"]
            .as_object_mut()
            .unwrap()
            .remove("volumes");
        assert_eq!(judged(absent, &image), Err(KEY));
        assert_eq!(judged(json!({}), &image), Err(KEY));
        let mut shaped = model;
        shaped["services"]["simplestchat"]["environment"] = json!([]);
        assert_eq!(judged(shaped, &image), Err(VALUE));
    }

    /// The names an image declares are judged by the same rule, and a name
    /// that is refused is the one fixed kind of fault.
    #[test]
    fn an_image_environment_is_judged_by_the_launchers_rule_for_names() {
        let image = format!("sha256:{}", "a".repeat(64));
        let inspected = |names: serde_json::Value| {
            let value = json!({"id":image,"os":"linux","architecture":"amd64",
                "user":"10001:10001","labels":{"org.opencontainers.image.revision":"r"},
                "cmd":["/app/simplestChat"],"entrypoint":null,"workingDir":"/app",
                "hasHealthcheck":false,"envNames":names});
            let value = crate::host::state::decode(&serde_json::to_vec(&value).unwrap()).unwrap();
            image_identity(&value, "r")
        };
        assert_eq!(inspected(json!(["PATH", "_PRIVATE1"])), Ok(image.clone()));
        for names in [
            json!(["PATH", "PATH"]),
            json!(["PATH", "LD_PRELOAD"]),
            json!(["OPENSSL_CONF"]),
            json!(["1BAD"]),
            json!([""]),
        ] {
            assert_eq!(inspected(names), Err(PROFILE));
        }
    }
}
