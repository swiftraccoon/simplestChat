//! Exact old-proxy validation lane and complete replacement refinement.
//! Validate the one exact native Caddy validation exec before projecting it
//! away for the existing container lifecycle parser. Unknown rows stay visible.
use super::super::super::{Error, Result, protocol::Request};
use super::{Baseline, NATIVE_DUMP, completion, digest, require, text};
use serde_json::Value;

const VALIDATE: &str = "caddy validate --config /dev/stdin --adapter caddyfile";
const PROXY: &str = "simplestchat-public-caddy-1";
const LABEL: &str = "simplestchat.maintain-prerequisite";

pub(super) fn project(
    rows: &[Value],
    baseline: &Baseline,
    request: &Request,
) -> Result<Vec<Value>> {
    require(request.is_host() && request.scenario == "host-proxy-upgrade" && rows.len() <= 2048)?;
    let mut phase = 0;
    let mut exec = None;
    let mut completed = None;
    let mut result = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        if row["Type"] != "container"
            || row["Actor"]["ID"] != baseline.proxy
            || !row["Action"]
                .as_str()
                .is_some_and(|s| s.starts_with("exec_"))
        {
            result.push(row.clone());
            continue;
        }
        let attrs = &row["Actor"]["Attributes"];
        require(
            attrs["name"] == PROXY
                && attrs[LABEL] == request.run_id
                && attrs["com.docker.compose.project"] == "simplestchat-public"
                && attrs["com.docker.compose.service"] == "caddy"
                && attrs["com.docker.compose.container-number"] == "1"
                && (attrs["image"] == baseline.caddy_image
                    || attrs["image"] == request.caddy_image),
        )?;
        let id = text(&attrs["execID"])?;
        require(digest(id))?;
        match (phase, text(&row["Action"])?) {
            (0, action) if action == format!("exec_create: {VALIDATE}") => {
                exec = Some(id.to_owned());
                phase = 1;
            }
            (1, action) if action == format!("exec_start: {VALIDATE}") => {
                require(exec.as_deref() == Some(id))?;
                phase = 2;
            }
            (2, "exec_die") => {
                require(exec.as_deref() == Some(id) && attrs["exitCode"] == "0")?;
                phase = 3;
                completed = Some(index);
            }
            _ => return Err(Error::Refused),
        }
    }
    // Native preflight validates candidate Caddy using the actual old live
    // proxy, then takes the listed backup. It cannot validate after stopping.
    let dump_started = rows
        .iter()
        .position(|row| {
            row["Type"] == "container"
                && row["Actor"]["ID"] == baseline.pg
                && row["Action"] == format!("exec_create: {NATIVE_DUMP}")
        })
        .ok_or(Error::Refused)?;
    require(
        phase == 3
            && completed.ok_or(Error::Refused)? < dump_started
            && dump_started < completion(rows, &baseline.pg, NATIVE_DUMP)?,
    )?;
    Ok(result)
}

/// Refinement after the existing full finite lifecycle/network parser succeeds.
/// It already joins replacement labels/rename lineage, old stop/die/destroy and
/// final new create/start plus all database, validator, migration and app actors.
pub(super) fn require_replacement(
    rows: &[Value],
    baseline: &Baseline,
    request: &Request,
    proxy: &Value,
) -> Result<()> {
    require(request.is_host() && request.scenario == "host-proxy-upgrade")?;
    let current = text(&proxy["Id"])?;
    require(
        digest(current) && current != baseline.proxy && proxy["Image"] == baseline.caddy_image,
    )?;
    let count = |id: &str, action: &str| {
        rows.iter()
            .filter(|row| {
                row["Type"] == "container" && row["Actor"]["ID"] == id && row["Action"] == action
            })
            .count()
    };
    require(
        count(&baseline.proxy, "start") == 0
            && count(&baseline.proxy, "kill") == 1
            && count(&baseline.proxy, "die") == 1
            && count(&baseline.proxy, "stop") == 1
            && count(&baseline.proxy, "destroy") == 1
            && count(current, "create") == 1
            && count(current, "start") == 1
            && count(current, "die") == 0
            && count(current, "destroy") == 0,
    )?;
    let mounts = proxy["Mounts"].as_array().ok_or(Error::Refused)?;
    require(
        mounts.len() == 4
            && mounts
                .iter()
                .filter(|m| {
                    m["Type"] == "bind"
                        && m["Source"] == "/srv/simplestchat-public/browser-assets"
                        && m["Destination"] == "/run/simplestchat-browser-assets"
                        && m["RW"] == false
                })
                .count()
                == 1,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::{BTreeMap, BTreeSet};

    fn fixture() -> (Vec<Value>, Baseline, Request) {
        // Finite event-policy inputs, not a Docker or signature proof.
        let request = serde_json::from_value(json!({
            "schemaVersion":1,"scope":"PreparedHostMaintenanceRequest",
            "runId":"a".repeat(32),"revision":"b".repeat(40),
            "scenario":"host-proxy-upgrade","setupSeconds":1560,
            "bodySeconds":3660,"endSeconds":3780,
            "binary":{"bytes":1,"sha256":"1".repeat(64)},
            "manifest":{"bytes":1,"sha256":"1".repeat(64)},
            "archive":{"bytes":1,"sha256":"1".repeat(64)},
            "sourceBundle":{"bytes":1,"sha256":"1".repeat(64)},
            "sourceTree":"b".repeat(40),"sourcePins":{},"helperPins":{},
            "postgresImage":"postgres","caddyImage":"caddy","engineVersion":"29.0.0"
        }))
        .unwrap();
        let baseline = Baseline {
            pg: "1".repeat(64),
            app: "2".repeat(64),
            proxy: "3".repeat(64),
            image: format!("sha256:{}", "6".repeat(64)),
            caddy_image: format!("sha256:{}", "7".repeat(64)),
            network: "8".repeat(64),
            none_network: "9".repeat(64),
            health: "health probe".into(),
            health_pending: BTreeMap::new(),
            shell_pg_commands: BTreeMap::new(),
            shell_attempt: None,
            app_row: Value::Null,
            proxy_row: Value::Null,
            attempts: BTreeSet::new(),
        };
        let mut rows = Vec::new();
        for action in [
            format!("exec_create: {VALIDATE}"),
            format!("exec_start: {VALIDATE}"),
            "exec_die".into(),
        ] {
            rows.push(json!({"Type":"container","Action":action,"Actor":{
                "ID":baseline.proxy,"Attributes":{
                    "name":PROXY,"image":baseline.caddy_image,"execID":"4".repeat(64),
                    "exitCode":"0",(LABEL):"a".repeat(32),
                    "com.docker.compose.project":"simplestchat-public",
                    "com.docker.compose.service":"caddy",
                    "com.docker.compose.container-number":"1"
                }
            }}));
        }
        for action in [
            format!("exec_create: {NATIVE_DUMP}"),
            format!("exec_start: {NATIVE_DUMP}"),
            "exec_die".into(),
        ] {
            rows.push(json!({"Type":"container","Action":action,"Actor":{
                "ID":baseline.pg,"Attributes":{"execID":"5".repeat(64),"exitCode":"0"}
            }}));
        }
        (rows, baseline, request)
    }

    #[test]
    fn exact_validation_lane_is_removed_and_all_other_rows_remain_visible() {
        let (mut rows, baseline, request) = fixture();
        let unknown = json!({"Type":"container","Action":"exec_die","Actor":{
            "ID":"f".repeat(64),"Attributes":{"execID":"e".repeat(64)}
        }});
        rows.insert(1, unknown.clone());
        let remaining = project(&rows, &baseline, &request).unwrap();
        assert_eq!(remaining.len(), 4);
        assert_eq!(remaining[0], unknown);
        assert_eq!(remaining[1..], rows[4..]);
    }

    #[test]
    fn incomplete_foreign_duplicate_nonzero_or_late_validation_never_projects() {
        let (rows, baseline, request) = fixture();
        let mut invalid = rows.clone();
        invalid.remove(1);
        assert!(project(&invalid, &baseline, &request).is_err());
        let mut invalid = rows.clone();
        invalid.insert(1, rows[0].clone());
        assert!(project(&invalid, &baseline, &request).is_err());
        let mut invalid = rows.clone();
        invalid[2]["Actor"]["Attributes"]["exitCode"] = "1".into();
        assert!(project(&invalid, &baseline, &request).is_err());
        let mut invalid = rows.clone();
        invalid[1]["Actor"]["Attributes"]["execID"] = "e".repeat(64).into();
        assert!(project(&invalid, &baseline, &request).is_err());
        let mut invalid = rows.clone();
        invalid[0]["Action"] = "exec_create: caddy reload".into();
        assert!(project(&invalid, &baseline, &request).is_err());
        let mut invalid = rows.clone();
        invalid.rotate_left(3);
        assert!(project(&invalid, &baseline, &request).is_err());
        let mut invalid = rows.clone();
        invalid.swap(2, 3);
        assert!(project(&invalid, &baseline, &request).is_err());
        for field in [
            "name",
            "image",
            LABEL,
            "com.docker.compose.project",
            "com.docker.compose.service",
            "com.docker.compose.container-number",
        ] {
            let mut invalid = rows.clone();
            invalid[0]["Actor"]["Attributes"][field] = "foreign".into();
            assert!(project(&invalid, &baseline, &request).is_err(), "{field}");
        }
        let (_, _, mut foreign) = fixture();
        foreign.scenario = "host-success".into();
        assert!(project(&rows, &baseline, &foreign).is_err());
    }

    fn replacement(baseline: &Baseline) -> (Vec<Value>, Value) {
        let new = "c".repeat(64);
        let mut rows = Vec::new();
        for action in ["kill", "die", "stop", "destroy"] {
            rows.push(json!({"Type":"container","Action":action,
                "Actor":{"ID":baseline.proxy}}));
        }
        for action in ["create", "start"] {
            rows.push(json!({"Type":"container","Action":action,"Actor":{"ID":new}}));
        }
        let row = json!({"Id":new,"Image":baseline.caddy_image,"Mounts":[
            {"Type":"bind","Source":"/etc/simplestchat-public/Caddyfile",
                "Destination":"/etc/caddy/Caddyfile","RW":false},
            {"Type":"bind","Source":"/srv/simplestchat-public/caddy-data",
                "Destination":"/data","RW":true},
            {"Type":"bind","Source":"/srv/simplestchat-public/caddy-config",
                "Destination":"/config","RW":true},
            {"Type":"bind","Source":"/srv/simplestchat-public/browser-assets",
                "Destination":"/run/simplestchat-browser-assets","RW":false}
        ]});
        (rows, row)
    }

    #[test]
    fn replacement_refinement_rejects_restart_missing_retirement_and_writable_assets() {
        let (_, baseline, request) = fixture();
        let (rows, row) = replacement(&baseline);
        assert!(require_replacement(&rows, &baseline, &request, &row).is_ok());
        let mut invalid = row.clone();
        invalid["Id"] = baseline.proxy.clone().into();
        assert!(require_replacement(&rows, &baseline, &request, &invalid).is_err());
        let mut invalid = row.clone();
        invalid["Mounts"][3]["RW"] = true.into();
        assert!(require_replacement(&rows, &baseline, &request, &invalid).is_err());
        let mut invalid = rows.clone();
        invalid.remove(3);
        assert!(require_replacement(&invalid, &baseline, &request, &row).is_err());
        let mut invalid = rows.clone();
        invalid.push(json!({"Type":"container","Action":"start",
            "Actor":{"ID":baseline.proxy}}));
        assert!(require_replacement(&invalid, &baseline, &request, &row).is_err());
        let mut invalid = rows.clone();
        invalid.push(rows[4].clone());
        assert!(require_replacement(&invalid, &baseline, &request, &row).is_err());
    }
}
