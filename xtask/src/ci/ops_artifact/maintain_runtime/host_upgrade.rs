//! Closed passive observations for the two source-bound host upgrade fixtures.
//! These records describe guest checks; decoding them grants no SQL or Docker authority.

use super::{Error, Result, options, projection};
use serde_json::Value;

const SOURCE_035: &str = concat!(
    "5fbb9935b5024a14ece58af557e582fd123973fbcfdcae0fef8392",
    "d780ca339338cc3275d94c98b2c51e93df4ebe2eda"
);
const SOURCE_036: &str = concat!(
    "57601e8850e77c13f5fd250ff0a30a366c8489827a7be08941c316",
    "b09b687fb28c635090e0a104d9c0a5e74137b0f69c"
);
// Hashes of the checked source prefix: version|t|SHA384, LF-separated without
// a trailing LF. Both generations are fixed by the existing 36-file catalogue.
const LEDGER_BEFORE: &str = "1cc29ac8d0c6bf270c1587ac24b245e94a01afcb77b4b8c209ef89ae9efd3895";
const LEDGER_AFTER: &str = "fc0cd651753d635abbbf8b89290f376cbc69b66005bacd96e2c8db58b740f08f";
const LEGACY_CADDY: &str = "ea24dcb24b1285b4529e3aa70083550d25431c56cc3a762b8891a53f1306f3ab";

fn migration(value: &Value, after: bool) -> Result<()> {
    projection::exact(
        value,
        &[
            "schemaVersion",
            "scope",
            "generation",
            "snapshot",
            "ledgerRows",
            "ledgerSha256",
            "source035Sha384",
            "source036Sha384",
        ],
    )?;
    if value["schemaVersion"].as_u64() != Some(1)
        || value["scope"] != "PreparedHostRoleNumberingObservation"
        || value["generation"] != if after { "post036" } else { "pre035" }
        || value["ledgerRows"].as_u64() != Some(if after { 36 } else { 34 })
        || value["ledgerSha256"] != if after { LEDGER_AFTER } else { LEDGER_BEFORE }
        || value["source035Sha384"] != SOURCE_035
        || value["source036Sha384"] != SOURCE_036
    {
        return Err(Error::Refused);
    }
    let snapshot = &value["snapshot"];
    projection::exact(
        snapshot,
        &[
            "schemaVersion",
            "scope",
            "usersCount",
            "lobbyCount",
            "fixtureUserCount",
            "roomRolesCount",
            "invitesCount",
            "redemptionsCount",
            "memberRole",
            "inviteRole",
            "redemptionRole",
            "constraints",
        ],
    )?;
    if snapshot["schemaVersion"].as_u64() != Some(1)
        || snapshot["scope"] != "PreparedHostRoleNumberingFixture"
        || snapshot["usersCount"].as_u64() != Some(2)
        || [
            "lobbyCount",
            "fixtureUserCount",
            "roomRolesCount",
            "invitesCount",
            "redemptionsCount",
        ]
        .iter()
        .any(|name| snapshot[*name].as_u64() != Some(1))
        || snapshot["memberRole"].as_u64() != Some(if after { 2 } else { 1 })
        || snapshot["inviteRole"].as_u64() != Some(if after { 2 } else { 1 })
        || snapshot["redemptionRole"].as_u64() != Some(2)
    {
        return Err(Error::Refused);
    }
    let constraints = snapshot["constraints"].as_array().ok_or(Error::Refused)?;
    if constraints.len() != 2 {
        return Err(Error::Refused);
    }
    for (constraint, table, name) in [
        (&constraints[0], "invites", "invites_role_check"),
        (&constraints[1], "room_roles", "room_roles_role_check"),
    ] {
        projection::exact(constraint, &["table", "name", "validated", "check"])?;
        if constraint["table"] != table
            || constraint["name"] != name
            || constraint["validated"].as_bool() != Some(true)
            || constraint["check"]
                != if after {
                    "CHECK (((role >= 2) AND (role <= 4)))"
                } else {
                    "CHECK (((role >= 1) AND (role <= 3)))"
                }
        {
            return Err(Error::Refused);
        }
    }
    Ok(())
}

fn proxy(value: &Value, expected: &Value) -> Result<()> {
    projection::exact(
        value,
        &[
            "schemaVersion",
            "scope",
            "legacyCaddyfileSha256",
            "currentCaddyfileSha256",
            "legacyProxyId",
            "replacementProxyId",
        ],
    )?;
    if value["schemaVersion"].as_u64() != Some(1)
        || value["scope"] != "PreparedHostBrowserProxyUpgrade"
        || value["legacyCaddyfileSha256"] != LEGACY_CADDY
        || value["currentCaddyfileSha256"] != expected["sourcePins"]["Caddyfile"]
        || !value["currentCaddyfileSha256"]
            .as_str()
            .is_some_and(|hash| options::digest(hash, 64))
        || ["legacyProxyId", "replacementProxyId"].iter().any(|name| {
            !value[*name]
                .as_str()
                .is_some_and(|id| options::digest(id, 64))
        })
        || value["legacyProxyId"] == value["replacementProxyId"]
    {
        return Err(Error::Refused);
    }
    Ok(())
}

pub(super) fn validate(
    value: &Value,
    expected: &Value,
    positive: bool,
    observations: &Value,
) -> Result<()> {
    projection::exact(value, &["migrationBefore", "migrationAfter", "proxy"])?;
    let migration_case = expected["scenario"] == "host-migration-upgrade";
    let proxy_case = expected["scenario"] == "host-proxy-upgrade";
    for (name, after) in [("migrationBefore", false), ("migrationAfter", true)] {
        if value[name].is_null() {
            if migration_case && positive {
                return Err(Error::Refused);
            }
        } else {
            if !migration_case {
                return Err(Error::Refused);
            }
            migration(&value[name], after)?;
        }
    }
    if value["proxy"].is_null() {
        if proxy_case && positive {
            return Err(Error::Refused);
        }
    } else {
        if !proxy_case {
            return Err(Error::Refused);
        }
        proxy(&value["proxy"], expected)?;
    }
    for (name, applicable, complete) in [
        (
            "roleNumberingUpgradeObserved",
            migration_case,
            !value["migrationBefore"].is_null() && !value["migrationAfter"].is_null(),
        ),
        (
            "browserProxyUpgradeObserved",
            proxy_case,
            !value["proxy"].is_null(),
        ),
    ] {
        let observed = observations[name].as_bool().ok_or(Error::Refused)?;
        if observed && (!applicable || !complete) || positive && observed != applicable {
            return Err(Error::Refused);
        }
    }
    Ok(())
}

#[cfg(test)]
fn migration_fixture(after: bool) -> Value {
    use serde_json::json;
    let check = if after {
        "CHECK (((role >= 2) AND (role <= 4)))"
    } else {
        "CHECK (((role >= 1) AND (role <= 3)))"
    };
    json!({"schemaVersion":1,"scope":"PreparedHostRoleNumberingObservation",
        "generation":if after {"post036"} else {"pre035"},
        "snapshot":{"schemaVersion":1,"scope":"PreparedHostRoleNumberingFixture",
            "usersCount":2,"lobbyCount":1,"fixtureUserCount":1,"roomRolesCount":1,
            "invitesCount":1,"redemptionsCount":1,
            "memberRole":if after {2} else {1},"inviteRole":if after {2} else {1},
            "redemptionRole":2,"constraints":[
                {"table":"invites","name":"invites_role_check","validated":true,"check":check},
                {"table":"room_roles","name":"room_roles_role_check",
                    "validated":true,"check":check}]},
        "ledgerRows":if after {36} else {34},
        "ledgerSha256":if after {LEDGER_AFTER} else {LEDGER_BEFORE},
        "source035Sha384":SOURCE_035,"source036Sha384":SOURCE_036})
}

#[cfg(test)]
pub(super) fn fixture(scenario: &str, expected: &Value) -> Value {
    use serde_json::json;
    let mut value = json!({"migrationBefore":null,"migrationAfter":null,"proxy":null});
    match scenario {
        "host-migration-upgrade" => {
            value["migrationBefore"] = migration_fixture(false);
            value["migrationAfter"] = migration_fixture(true);
        }
        "host-proxy-upgrade" => {
            value["proxy"] = json!({"schemaVersion":1,"scope":"PreparedHostBrowserProxyUpgrade",
                "legacyCaddyfileSha256":LEGACY_CADDY,
                "currentCaddyfileSha256":expected["sourcePins"]["Caddyfile"],
                "legacyProxyId":"1".repeat(64),"replacementProxyId":"2".repeat(64)});
        }
        _ => {}
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Synthetic parser boundaries only: no SQL command, daemon, signature or
    // completed runtime observation is produced by these values.
    fn request(scenario: &str) -> Value {
        json!({"scenario":scenario,"sourcePins":{"Caddyfile":"c".repeat(64)}})
    }
    fn observations(scenario: &str) -> Value {
        json!({"roleNumberingUpgradeObserved":scenario == "host-migration-upgrade",
            "browserProxyUpgradeObserved":scenario == "host-proxy-upgrade"})
    }

    #[test]
    fn all_four_cases_require_only_their_exact_applicable_evidence() {
        for scenario in options::HOST_CASES {
            let expected = request(scenario);
            let evidence = fixture(scenario, &expected);
            let observed = observations(scenario);
            validate(&evidence, &expected, true, &observed).unwrap();
            for foreign in options::HOST_CASES
                .into_iter()
                .filter(|other| *other != scenario)
            {
                let changed = request(foreign);
                if foreign == "host-success" && scenario == "host-launcher-refusal"
                    || foreign == "host-launcher-refusal" && scenario == "host-success"
                {
                    continue;
                }
                assert!(validate(&evidence, &changed, true, &observed).is_err());
            }
            let mut extra = evidence;
            extra["unobserved"] = Value::Null;
            assert!(validate(&extra, &expected, false, &observed).is_err());
        }
    }

    #[test]
    fn role_numbering_rejects_counterfeit_data_constraints_ledger_and_source() {
        for after in [false, true] {
            let receipt = migration_fixture(after);
            migration(&receipt, after).unwrap();
            for (pointer, replacement) in [
                ("/schemaVersion", json!(true)),
                ("/schemaVersion", json!(1.0)),
                (
                    "/generation",
                    json!(if after { "pre035" } else { "post036" }),
                ),
                ("/ledgerRows", json!(if after { 34 } else { 36 })),
                ("/ledgerRows", json!(true)),
                ("/ledgerSha256", json!("a".repeat(64))),
                ("/source035Sha384", json!("a".repeat(96))),
                ("/source036Sha384", json!("A".repeat(96))),
                ("/snapshot/memberRole", json!(if after { 1 } else { 2 })),
                ("/snapshot/inviteRole", json!(if after { 1 } else { 2 })),
                ("/snapshot/redemptionRole", json!(1)),
                ("/snapshot/constraints/0/table", json!("room_roles")),
                ("/snapshot/constraints/1/name", json!("invites_role_check")),
                ("/snapshot/constraints/0/validated", json!(false)),
                ("/snapshot/constraints/1/validated", json!("true")),
                ("/snapshot/constraints/1/check", json!("CHECK (role >= 1)")),
            ] {
                let mut changed = receipt.clone();
                *changed.pointer_mut(pointer).unwrap() = replacement;
                assert!(migration(&changed, after).is_err(), "{pointer}");
            }
            for name in [
                "schemaVersion",
                "usersCount",
                "lobbyCount",
                "fixtureUserCount",
                "roomRolesCount",
                "invitesCount",
                "redemptionsCount",
                "memberRole",
                "inviteRole",
                "redemptionRole",
            ] {
                let mut changed = receipt.clone();
                changed["snapshot"][name] = true.into();
                assert!(migration(&changed, after).is_err(), "{name}");
            }
            let mut changed = receipt.clone();
            changed["snapshot"]["constraints"]
                .as_array_mut()
                .unwrap()
                .reverse();
            assert!(migration(&changed, after).is_err());
            let mut changed = receipt.clone();
            changed["snapshot"]["constraints"][0]["foreign"] = Value::Null;
            assert!(migration(&changed, after).is_err());
            let mut changed = receipt;
            changed["snapshot"]["foreign"] = Value::Null;
            assert!(migration(&changed, after).is_err());
        }
    }

    #[test]
    fn proxy_upgrade_joins_both_fixed_generations_and_distinct_real_id_shapes() {
        let expected = request("host-proxy-upgrade");
        let receipt = fixture("host-proxy-upgrade", &expected)["proxy"].clone();
        proxy(&receipt, &expected).unwrap();
        for (field, replacement) in [
            ("schemaVersion", json!(true)),
            ("legacyCaddyfileSha256", json!("a".repeat(64))),
            ("currentCaddyfileSha256", json!("d".repeat(64))),
            ("legacyProxyId", json!("2".repeat(64))),
            ("legacyProxyId", json!("A".repeat(64))),
            ("replacementProxyId", json!("2".repeat(63))),
            ("replacementProxyId", json!(true)),
            ("foreign", Value::Null),
        ] {
            let mut changed = receipt.clone();
            changed[field] = replacement;
            assert!(proxy(&changed, &expected).is_err(), "{field}");
        }
    }

    #[test]
    fn failed_guests_retain_validated_partials_without_completing_an_observation() {
        let scenario = "host-migration-upgrade";
        let expected = request(scenario);
        let mut evidence = fixture(scenario, &expected);
        evidence["migrationAfter"] = Value::Null;
        let observed = observations("host-success");
        validate(&evidence, &expected, false, &observed).unwrap();
        assert!(validate(&evidence, &expected, true, &observed).is_err());
        assert!(validate(&evidence, &expected, false, &observations(scenario)).is_err());
        evidence["migrationBefore"]["snapshot"]["memberRole"] = 2.into();
        assert!(validate(&evidence, &expected, false, &observed).is_err());
        for scenario in options::HOST_CASES {
            let expected = request(scenario);
            let empty = json!({"migrationBefore":null,"migrationAfter":null,"proxy":null});
            validate(&empty, &expected, false, &observed).unwrap();
        }
    }
}
