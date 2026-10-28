//! Source-bound schema and data generation for the owned SQL-upgrade trial.
//!
//! The only SQL mutation builds one controlled
//! pre-035 generation in the disposable owned fixture. It never runs during the
//! maintenance operation's monitored epoch or against a production database.

use super::super::{Error, Result};
use serde_json::{Value, json};
use sha2::{Digest, Sha256, Sha384};
use std::collections::BTreeMap;

const USER: &str = "00000000-0000-4000-8000-000000000035";
const INVITE: &str = "00000000-0000-4000-8000-000000000036";
// A lookup digest with no generated or retained invitation capability.
const INVITE_HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const CURRENT_035: &str = concat!(
    "5fbb9935b5024a14ece58af557e582fd123973fbcfdcae0fef8392",
    "d780ca339338cc3275d94c98b2c51e93df4ebe2eda"
);
const CURRENT_036: &str = concat!(
    "57601e8850e77c13f5fd250ff0a30a366c8489827a7be08941c316",
    "b09b687fb28c635090e0a104d9c0a5e74137b0f69c"
);

#[derive(Clone, Copy)]
pub(in super::super) enum Generation {
    Before035,
    After036,
}
impl Generation {
    fn last(self) -> u64 {
        match self {
            Self::Before035 => 34,
            Self::After036 => 36,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Before035 => "pre035",
            Self::After036 => "post036",
        }
    }
    fn role(self) -> u64 {
        match self {
            Self::Before035 => 1,
            Self::After036 => 2,
        }
    }
    fn definition(self) -> &'static str {
        match self {
            Self::Before035 => "CHECK (((role >= 1) AND (role <= 3)))",
            Self::After036 => "CHECK (((role >= 2) AND (role <= 4)))",
        }
    }
}

/// Construct only from the existing checked source bytes held for this run.
/// In particular, a supplied sourcePins hash is not a substitute for the bytes.
pub(in super::super) struct SourceLedger(BTreeMap<u64, String>);
impl SourceLedger {
    pub(in super::super) fn from_sources(sources: &BTreeMap<String, Vec<u8>>) -> Result<Self> {
        let mut rows = BTreeMap::new();
        for (path, bytes) in sources {
            let Some(name) = path.strip_prefix("migrations/") else {
                continue;
            };
            let (prefix, _) = name.split_once('_').ok_or(Error::Refused)?;
            if name.contains('/')
                || !name.ends_with(".sql")
                || bytes.is_empty()
                || bytes.len() > 1024 * 1024
            {
                return Err(Error::Refused);
            }
            let version = prefix.parse::<u64>().map_err(|_| Error::Refused)?;
            let hash = Sha384::digest(bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            if rows.insert(version, hash).is_some() {
                return Err(Error::Refused);
            }
        }
        if rows.keys().copied().collect::<Vec<_>>() != (1..=36).collect::<Vec<_>>()
            || rows.get(&35).map(String::as_str) != Some(CURRENT_035)
            || rows.get(&36).map(String::as_str) != Some(CURRENT_036)
            || !sources.contains_key("migrations/035_role_numbering.sql")
            || !sources.contains_key("migrations/036_validate_role_numbering.sql")
        {
            return Err(Error::Refused);
        }
        Ok(Self(rows))
    }

    /// Exactly one bounded psql --command, original database CID and uid999.
    /// Explicit BEGIN/COMMIT joins seed data, schema reversal and the two ledger
    /// deletions. ON_ERROR_STOP and a closed connection roll back a failed txn.
    pub(in super::super) fn preparation_sql(&self) -> String {
        let values = self
            .0
            .iter()
            .map(|(version, hash)| format!("({version},decode('{hash}','hex'))"))
            .collect::<Vec<_>>()
            .join(",");
        PREPARE
            .replace("@EXPECTED@", &values)
            .replace("@USER@", USER)
            .replace("@INVITE@", INVITE)
            .replace("@HASH@", INVITE_HASH)
    }

    /// Same separate read-only ledger query used by host_ledger. Parsing its
    /// successful output is independent of the preparation command's status.
    fn verify_ledger(&self, bytes: &[u8], generation: Generation) -> Result<()> {
        let text = std::str::from_utf8(bytes).map_err(|_| Error::Refused)?;
        let mut actual = BTreeMap::new();
        for line in text.lines() {
            let fields = line.split('|').collect::<Vec<_>>();
            if fields.len() != 3 || fields[1] != "t" {
                return Err(Error::Refused);
            }
            let version = fields[0].parse::<u64>().map_err(|_| Error::Refused)?;
            if actual.insert(version, fields[2].to_owned()).is_some() {
                return Err(Error::Refused);
            }
        }
        let expected = self
            .0
            .iter()
            .filter(|(version, _)| **version <= generation.last())
            .map(|(version, hash)| (*version, hash.clone()))
            .collect::<BTreeMap<_, _>>();
        if actual == expected {
            Ok(())
        } else {
            Err(Error::Refused)
        }
    }

    /// Validate the separate read-only command's actual data, checks and ledger.
    pub(in super::super) fn observe(&self, value: &Value, generation: Generation) -> Result<Value> {
        if !exact(value, &["snapshot", "ledger"]) {
            return Err(Error::Refused);
        }
        verify_snapshot(&value["snapshot"], generation)?;
        let ledger = value["ledger"].as_str().ok_or(Error::Refused)?;
        if ledger.len() > 8192 {
            return Err(Error::Bounds);
        }
        self.verify_ledger(ledger.as_bytes(), generation)?;
        let hash = Sha256::digest(ledger.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        Ok(json!({
            "schemaVersion":1,"scope":"PreparedHostRoleNumberingObservation",
            "generation":generation.name(),"snapshot":value["snapshot"],
            "ledgerRows":generation.last(),"ledgerSha256":hash,
            "source035Sha384":CURRENT_035,"source036Sha384":CURRENT_036
        }))
    }
}

// Every check uses fixed identifiers. No passwords, invitation tokens, owner
// credentials, environment settings or arbitrary caller SQL enter this block.
const PREPARE: &str = r#"
BEGIN;
SET LOCAL lock_timeout='2s';
SET LOCAL statement_timeout='10s';
DO $fixture$
DECLARE
    fixture_owner uuid;
    affected bigint;
BEGIN
    LOCK TABLE public.users, public.rooms, public.room_roles, public.invites,
        public.invite_redemptions, public._sqlx_migrations IN ACCESS EXCLUSIVE MODE;
    IF EXISTS (
        WITH expected(version,checksum) AS (VALUES @EXPECTED@)
        SELECT 1 FROM expected e FULL JOIN public._sqlx_migrations a USING(version)
        WHERE e.version IS NULL OR a.version IS NULL
            OR a.success IS DISTINCT FROM true OR a.checksum IS DISTINCT FROM e.checksum
    ) THEN RAISE EXCEPTION 'Fixture refused'; END IF;
    IF (
        SELECT count(*) FROM pg_catalog.pg_constraint c
        WHERE c.connamespace='public'::regnamespace
            AND c.contype='c' AND c.convalidated AND NOT c.connoinherit
            AND pg_get_constraintdef(c.oid,false)='CHECK (((role >= 2) AND (role <= 4)))'
            AND ((c.conrelid='public.room_roles'::regclass
                    AND c.conname='room_roles_role_check')
                OR (c.conrelid='public.invites'::regclass
                    AND c.conname='invites_role_check'))
    ) <> 2 THEN RAISE EXCEPTION 'Fixture refused'; END IF;
    SELECT id INTO STRICT fixture_owner FROM public.users;
    IF (SELECT count(*) FROM public.rooms) <> 1
        OR NOT EXISTS (SELECT 1 FROM public.rooms WHERE id='lobby' AND owner_id=fixture_owner)
        OR EXISTS (SELECT 1 FROM public.room_roles)
        OR EXISTS (SELECT 1 FROM public.invites)
        OR EXISTS (SELECT 1 FROM public.invite_redemptions)
    THEN RAISE EXCEPTION 'Fixture refused'; END IF;

    INSERT INTO public.users(id,email,display_name,password_hash)
        VALUES('@USER@','migration-fixture@maintain.invalid','Migration fixture',NULL);
    INSERT INTO public.room_roles(room_id,user_id,role,granted_by)
        VALUES('lobby','@USER@',2,fixture_owner);
    INSERT INTO public.invites(code_hash,id,kind,room_id,role,created_by,uses_left,expires_at)
        VALUES('@HASH@','@INVITE@','room','lobby',2,fixture_owner,1,'2099-01-01T00:00:00Z');
    INSERT INTO public.invite_redemptions(invite_hash,user_id,granted_role)
        VALUES('@HASH@','@USER@',2);

    ALTER TABLE public.room_roles DROP CONSTRAINT room_roles_role_check;
    UPDATE public.room_roles SET role=role-1;
    GET DIAGNOSTICS affected=ROW_COUNT;
    IF affected<>1 THEN RAISE EXCEPTION 'Fixture refused'; END IF;
    ALTER TABLE public.room_roles ADD CONSTRAINT room_roles_role_check
        CHECK(role BETWEEN 1 AND 3);
    ALTER TABLE public.invites DROP CONSTRAINT invites_role_check;
    UPDATE public.invites SET role=role-1;
    GET DIAGNOSTICS affected=ROW_COUNT;
    IF affected<>1 THEN RAISE EXCEPTION 'Fixture refused'; END IF;
    ALTER TABLE public.invites ADD CONSTRAINT invites_role_check CHECK(role BETWEEN 1 AND 3);
    DELETE FROM public._sqlx_migrations WHERE version IN (35,36);
    GET DIAGNOSTICS affected=ROW_COUNT;
    IF affected<>2 THEN RAISE EXCEPTION 'Fixture refused'; END IF;
END
$fixture$;
COMMIT;
"#;

/// Independently run before GO and after the tested epoch collector is frozen.
pub(in super::super) fn observation_sql() -> String {
    OBSERVE
        .replace("@USER@", USER)
        .replace("@INVITE@", INVITE)
        .replace("@HASH@", INVITE_HASH)
}
const OBSERVE: &str = r#"
SELECT json_build_object('snapshot',json_build_object(
    'schemaVersion',1,
    'scope','PreparedHostRoleNumberingFixture',
    'usersCount',(SELECT count(*) FROM public.users),
    'lobbyCount',(SELECT count(*) FROM public.rooms WHERE id='lobby'),
    'fixtureUserCount',(SELECT count(*) FROM public.users WHERE id='@USER@'
        AND email='migration-fixture@maintain.invalid' AND password_hash IS NULL),
    'roomRolesCount',(SELECT count(*) FROM public.room_roles),
    'invitesCount',(SELECT count(*) FROM public.invites),
    'redemptionsCount',(SELECT count(*) FROM public.invite_redemptions),
    'memberRole',(SELECT role FROM public.room_roles
        WHERE room_id='lobby' AND user_id='@USER@'),
    'inviteRole',(SELECT role FROM public.invites
        WHERE code_hash='@HASH@' AND id='@INVITE@' AND kind='room' AND room_id='lobby'
            AND uses_left=1 AND expires_at='2099-01-01T00:00:00Z'),
    'redemptionRole',(SELECT granted_role FROM public.invite_redemptions
        WHERE invite_hash='@HASH@' AND user_id='@USER@'),
    'constraints',(
        SELECT json_agg(json_build_object(
            'table',CASE WHEN c.conrelid='public.invites'::regclass
                THEN 'invites' ELSE 'room_roles' END,
            'name',c.conname,
            'validated',c.convalidated,
            'check',pg_get_constraintdef(c.oid,false)
        ) ORDER BY c.conname)
        FROM pg_catalog.pg_constraint c
        WHERE c.connamespace='public'::regnamespace AND c.contype='c' AND NOT c.connoinherit
            AND ((c.conrelid='public.room_roles'::regclass
                    AND c.conname='room_roles_role_check')
                OR (c.conrelid='public.invites'::regclass
                    AND c.conname='invites_role_check'))
    )
),'ledger',(
    SELECT coalesce(string_agg(coalesce(version::text,'NULL') || '|' ||
        CASE WHEN success IS TRUE THEN 't' ELSE 'f' END || '|' ||
        coalesce(encode(checksum,'hex'),'NULL'),
        E'\n' ORDER BY version),'') FROM public._sqlx_migrations
))::text
"#;

fn exact(value: &Value, names: &[&str]) -> bool {
    value.as_object().is_some_and(|object| {
        object.len() == names.len() && names.iter().all(|name| object.contains_key(*name))
    })
}
fn verify_snapshot(value: &Value, generation: Generation) -> Result<()> {
    let fields = [
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
    ];
    if !exact(value, &fields)
        || value["schemaVersion"].as_u64() != Some(1)
        || value["scope"] != "PreparedHostRoleNumberingFixture"
        || value["usersCount"].as_u64() != Some(2)
        || [
            "lobbyCount",
            "fixtureUserCount",
            "roomRolesCount",
            "invitesCount",
            "redemptionsCount",
        ]
        .iter()
        .any(|field| value[*field].as_u64() != Some(1))
        || value["memberRole"].as_u64() != Some(generation.role())
        || value["inviteRole"].as_u64() != Some(generation.role())
        || value["redemptionRole"].as_u64() != Some(2)
    {
        return Err(Error::Refused);
    }
    let constraints = value["constraints"].as_array().ok_or(Error::Refused)?;
    if constraints.len() != 2 {
        return Err(Error::Refused);
    }
    for (row, table, name) in [
        (&constraints[0], "invites", "invites_role_check"),
        (&constraints[1], "room_roles", "room_roles_role_check"),
    ] {
        if !exact(row, &["table", "name", "validated", "check"])
            || row["table"] != table
            || row["name"] != name
            || row["validated"] != true
            || row["check"] != generation.definition()
        {
            return Err(Error::Refused);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn snapshot(generation: Generation) -> Value {
        json!({
            "schemaVersion":1,"scope":"PreparedHostRoleNumberingFixture",
            "usersCount":2,"lobbyCount":1,"fixtureUserCount":1,
            "roomRolesCount":1,"invitesCount":1,"redemptionsCount":1,
            "memberRole":generation.role(),"inviteRole":generation.role(),"redemptionRole":2,
            "constraints":[
                {"table":"invites","name":"invites_role_check",
                    "validated":true,"check":generation.definition()},
                {"table":"room_roles","name":"room_roles_role_check",
                    "validated":true,"check":generation.definition()}
            ]
        })
    }
    fn current_sources() -> BTreeMap<String, Vec<u8>> {
        let mut sources = (1..=34)
            .map(|version| {
                (
                    format!("migrations/{version:03}_fixture.sql"),
                    b"SELECT 1;".to_vec(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        sources.insert(
            "migrations/035_role_numbering.sql".into(),
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../migrations/035_role_numbering.sql"
            ))
            .to_vec(),
        );
        sources.insert(
            "migrations/036_validate_role_numbering.sql".into(),
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../migrations/036_validate_role_numbering.sql"
            ))
            .to_vec(),
        );
        sources
    }
    #[test]
    fn reconstruction_refuses_unknown_source_or_partial_migration_inventory() {
        let sources = current_sources();
        SourceLedger::from_sources(&sources).unwrap();
        for name in [
            "migrations/035_role_numbering.sql",
            "migrations/036_validate_role_numbering.sql",
        ] {
            let mut changed = sources.clone();
            changed.get_mut(name).unwrap().push(b' ');
            assert!(SourceLedger::from_sources(&changed).is_err());
            let mut missing = sources.clone();
            missing.remove(name);
            assert!(SourceLedger::from_sources(&missing).is_err());
        }
        let mut additional = sources;
        additional.insert(
            "migrations/037_unreviewed.sql".into(),
            b"SELECT 1;".to_vec(),
        );
        assert!(SourceLedger::from_sources(&additional).is_err());
    }
    #[test]
    fn closed_observation_joins_actual_snapshot_and_complete_same_generation_ledger() {
        let source = SourceLedger::from_sources(&current_sources()).unwrap();
        for generation in [Generation::Before035, Generation::After036] {
            let ledger = source
                .0
                .iter()
                .filter(|(version, _)| **version <= generation.last())
                .map(|(version, hash)| format!("{version}|t|{hash}"))
                .collect::<Vec<_>>()
                .join("\n");
            let observed = json!({"snapshot":snapshot(generation),"ledger":ledger});
            let receipt = source.observe(&observed, generation).unwrap();
            assert_eq!(receipt.as_object().unwrap().len(), 8);
            assert_eq!(receipt["generation"], generation.name());
            assert_eq!(receipt["ledgerRows"], generation.last());
            assert_eq!(receipt["ledgerSha256"].as_str().unwrap().len(), 64);
            assert_eq!(receipt["source035Sha384"], CURRENT_035);
            assert_eq!(receipt["source036Sha384"], CURRENT_036);
            let mut extra = observed.clone();
            extra["mutationSucceeded"] = true.into();
            assert!(source.observe(&extra, generation).is_err());
            let mut changed = observed.clone();
            changed["snapshot"]["inviteRole"] = 3.into();
            assert!(source.observe(&changed, generation).is_err());
            let mut duplicate = observed;
            duplicate["ledger"] = format!("{ledger}\n{}", ledger.lines().next().unwrap()).into();
            assert!(source.observe(&duplicate, generation).is_err());
        }
    }
    #[test]
    fn both_real_columns_and_validation_state_are_mandatory() {
        for generation in [Generation::Before035, Generation::After036] {
            let row = snapshot(generation);
            verify_snapshot(&row, generation).unwrap();
            for field in ["memberRole", "inviteRole", "redemptionRole"] {
                let mut changed = row.clone();
                changed[field] = 3.into();
                assert!(verify_snapshot(&changed, generation).is_err());
            }
            let mut missing_validation = row.clone();
            missing_validation["constraints"][1]["validated"] = false.into();
            assert!(verify_snapshot(&missing_validation, generation).is_err());
            let other = match generation {
                Generation::Before035 => Generation::After036,
                Generation::After036 => Generation::Before035,
            };
            assert!(verify_snapshot(&row, other).is_err());
        }
    }
    #[test]
    fn prefix_ledger_requires_all_current_source_checksums_and_no_holes() {
        let source = SourceLedger((1..=36).map(|version| (version, "c".repeat(96))).collect());
        for generation in [Generation::Before035, Generation::After036] {
            let rows = (1..=generation.last())
                .map(|version| format!("{version}|t|{}\n", "c".repeat(96)))
                .collect::<String>();
            source.verify_ledger(rows.as_bytes(), generation).unwrap();
            let missing = rows.lines().skip(1).collect::<Vec<_>>().join("\n");
            assert!(
                source
                    .verify_ledger(missing.as_bytes(), generation)
                    .is_err()
            );
            assert!(
                source
                    .verify_ledger(rows.replacen("|t|", "|f|", 1).as_bytes(), generation)
                    .is_err()
            );
            let extra = format!("{rows}37|t|{}\n", "c".repeat(96));
            assert!(source.verify_ledger(extra.as_bytes(), generation).is_err());
        }
    }
}
