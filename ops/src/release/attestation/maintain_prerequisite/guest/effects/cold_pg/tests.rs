//! Controlled captured histories only; no daemon, clock or host is exercised.
use super::*;
use sha2::{Digest, Sha256};

const HEALTH: &str = "/bin/sh -c fixed-health";
const WORK: &str = "psql fixed-bootstrap";
fn id(byte: char) -> String {
    std::iter::repeat_n(byte, 64).collect()
}
fn event(action: &str, exec: char, exit: &str) -> Value {
    json!({"Type":"container","Action":action,"Actor":{"ID":id('a'),
        "Attributes":{"execID":id(exec),"exitCode":exit}}})
}
fn trace(command: &str, exec: char) -> Vec<Value> {
    vec![
        event(&format!("exec_create: {command}"), exec, "0"),
        event(&format!("exec_start: {command}"), exec, "0"),
        event("exec_die", exec, "0"),
    ]
}
fn work() -> Vec<Value> {
    trace(WORK, 'b')
}
fn commands() -> BTreeMap<String, usize> {
    BTreeMap::from([(WORK.into(), 1)])
}
fn admit(prefix: &[Value], body: &[Value]) -> (Result<ColdPg>, Option<Value>) {
    let opening = Opening::capture(prefix, prefix.len() + body.len(), &id('a'), HEALTH).unwrap();
    let rows = body
        .iter()
        .filter(|row| row["Type"] == "container" && row["Actor"]["ID"] == id('a'))
        .cloned()
        .collect::<Vec<_>>();
    let mut audit = Auditor::new(body);
    audit.begin_cold_pg();
    let result = ColdPg::admit(opening, &rows, &commands(), &mut audit);
    (result, audit.diagnostic())
}
fn gap(
    state: &mut ColdPg,
    prefix: &[Value],
    body: &[Value],
    rows: &[Value],
) -> (Result<()>, Option<Value>) {
    let all = [prefix, body, rows].concat();
    let pending = Opening::capture(&all, all.len(), &id('a'), HEALTH)
        .unwrap()
        .boundary
        .seeds;
    let mut audit = Auditor::new(&all);
    audit.begin_cold_pg();
    let result = state.reconcile(&all, &pending, &mut audit);
    (result, audit.diagnostic())
}
fn finish(state: &mut ColdPg, rows: &[Value]) -> (Result<()>, Option<Value>) {
    let mut audit = Auditor::new(rows);
    audit.begin_cold_pg();
    let result = state.finish(rows, &mut audit);
    (result, audit.diagnostic())
}
fn evidence(state: &ColdPg, expected: [usize; 4]) {
    assert_eq!(
        state.evidence(),
        json!({"openingHealthSeeded":expected[0],
        "trailingHealthDeferred":expected[1],"coldHealthObligations":expected[2],
        "coldHealthCompletions":expected[3]})
    );
}
fn refused(result: Result<ColdPg>, diagnostic: Option<Value>, reason: &str) -> Value {
    assert!(matches!(result, Err(Error::Refused)));
    let diagnostic = diagnostic.unwrap();
    assert_eq!(diagnostic["stage"], "daemon-pg-history");
    assert_eq!(diagnostic["failureClass"], reason);
    assert_eq!(diagnostic.as_object().unwrap().len(), 10);
    let raw = serde_json::to_string(&diagnostic).unwrap();
    assert!(raw.len() <= 1024 && !raw.contains("PRIVATE") && !raw.contains(HEALTH));
    diagnostic
}

#[test]
fn complete_baseline_without_edges_retains_zero_counters_and_strict_legacy_success() {
    let body = work();
    assert_eq!(policy::pg_exec_history(&body, &commands(), HEALTH), Ok(()));
    let (state, diagnostic) = admit(&[], &body);
    let mut state = state.unwrap();
    assert!(diagnostic.is_none() && state.complete());
    evidence(&state, [0, 0, 0, 0]);
    assert_eq!(gap(&mut state, &[], &body, &[]).0, Ok(()));
    assert_eq!(finish(&mut state, &[]).0, Ok(()));
    evidence(&state, [0, 0, 0, 0]);
}

#[test]
fn witnessed_opening_create_or_start_admits_only_its_exact_health_continuation() {
    let health = trace(HEALTH, 'c');
    for cut in [1, 2] {
        let prefix = &health[..cut];
        let body = [health[cut..].to_vec(), work()].concat();
        assert_eq!(
            policy::pg_exec_history(&body, &commands(), HEALTH),
            Err(Error::Refused)
        );
        let (state, diagnostic) = admit(prefix, &body);
        let mut state = state.unwrap();
        assert!(diagnostic.is_none());
        evidence(&state, [1, 0, 1, 1]);
        assert_eq!(gap(&mut state, prefix, &body, &[]).0, Ok(()));
        assert_eq!(finish(&mut state, &[]).0, Ok(()));
        evidence(&state, [1, 0, 1, 1]);
    }
}

#[test]
fn trailing_health_must_complete_in_gap_or_final_epoch_without_a_new_wait() {
    let health = trace(HEALTH, 'c');
    for cut in [1, 2] {
        let body = [work(), health[..cut].to_vec()].concat();
        assert_eq!(
            policy::pg_exec_history(&body, &commands(), HEALTH),
            Err(Error::Refused)
        );
        let (state, _) = admit(&[], &body);
        let mut state = state.unwrap();
        assert!(!state.complete());
        evidence(&state, [0, 1, 1, 0]);
        assert_eq!(gap(&mut state, &[], &body, &health[cut..]).0, Ok(()));
        evidence(&state, [0, 1, 1, 1]);
        assert_eq!(finish(&mut state, &[]).0, Ok(()));
        let (state, _) = admit(&[], &body);
        let mut state = state.unwrap();
        assert_eq!(gap(&mut state, &[], &body, &[]).0, Ok(()));
        evidence(&state, [0, 1, 1, 0]);
        assert_eq!(finish(&mut state, &health[cut..]).0, Ok(()));
        evidence(&state, [0, 1, 1, 1]);
    }
}

#[test]
fn shared_and_distinct_seed_tail_identities_have_exact_union_counters() {
    let health = trace(HEALTH, 'c');
    let prefix = &health[..1];
    let body = [work(), health[1..2].to_vec()].concat();
    let (state, _) = admit(prefix, &body);
    let mut state = state.unwrap();
    evidence(&state, [1, 1, 1, 0]);
    assert_eq!(gap(&mut state, prefix, &body, &[]).0, Ok(()));
    assert_eq!(finish(&mut state, &health[2..]).0, Ok(()));
    evidence(&state, [1, 1, 1, 1]);
    let next = trace(HEALTH, 'd');
    let prefix = &health[..2];
    let body = [health[2..].to_vec(), work(), next[..1].to_vec()].concat();
    let (state, _) = admit(prefix, &body);
    let mut state = state.unwrap();
    evidence(&state, [1, 1, 2, 1]);
    assert_eq!(gap(&mut state, prefix, &body, &next[1..]).0, Ok(()));
    assert_eq!(finish(&mut state, &[]).0, Ok(()));
    evidence(&state, [1, 1, 2, 2]);
}

#[test]
fn absent_ambiguous_completed_wrong_actor_or_wrong_command_prefix_never_seeds_health() {
    let health = trace(HEALTH, 'c');
    let mut wrong_actor = health[..1].to_vec();
    wrong_actor[0]["Actor"]["ID"] = json!(id('f'));
    let mut nonzero = health.clone();
    nonzero[2]["Actor"]["Attributes"]["exitCode"] = json!("1");
    let mut malformed = health[..1].to_vec();
    malformed.push(event("exec_pause", 'c', "0"));
    let mut wrong_start = health[..1].to_vec();
    wrong_start.push(event("exec_start: PRIVATE_OTHER", 'c', "0"));
    for prefix in [
        Vec::new(),
        wrong_actor,
        health[1..2].to_vec(),
        health.clone(),
        [health[..1].to_vec(), health[..1].to_vec()].concat(),
        [health.clone(), health[..1].to_vec()].concat(),
        nonzero,
        malformed,
        wrong_start,
        [vec![event("exec_pause", 'c', "0")], health[..1].to_vec()].concat(),
        [health[1..2].to_vec(), health[..1].to_vec()].concat(),
    ] {
        let body = [health[1..].to_vec(), work()].concat();
        let (result, diagnostic) = admit(&prefix, &body);
        refused(result, diagnostic, "pg-missing-create");
    }
    let other = trace("PRIVATE_OTHER", 'c');
    let prefix = &other[..2];
    let body = [vec![event("exec_die", 'c', "0")], work()].concat();
    let (result, diagnostic) = admit(prefix, &body);
    let diagnostic = refused(result, diagnostic, "pg-missing-create");
    assert_eq!(
        diagnostic["expectedCommandSha256"],
        Sha256::digest(b"PRIVATE_OTHER")
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    assert!(diagnostic["observedCommandSha256"].is_null());
}

#[test]
fn completed_or_poisoned_pre_cut_identifiers_cannot_be_created_again_in_the_baseline() {
    let health = trace(HEALTH, 'c');
    let mut malformed = event("exec_pause", 'c', "0");
    malformed["Action"] = Value::Null;
    for prefix in [
        health.clone(),
        trace("PRIVATE_OTHER", 'c'),
        vec![event("exec_pause", 'c', "0")],
        vec![malformed],
    ] {
        let body = [health.clone(), work()].concat();
        let (result, diagnostic) = admit(&prefix, &body);
        refused(result, diagnostic, "pg-duplicate-create");
    }
    let prefix = trace("PRIVATE_OTHER", 'b');
    let (result, diagnostic) = admit(&prefix, &work());
    refused(result, diagnostic, "pg-duplicate-create");
}

#[test]
fn capture_and_gap_coordinates_are_bounded_by_the_same_retained_raw_slices() {
    let health = trace(HEALTH, 'c');
    assert!(Opening::capture(&health[..1], 0, &id('a'), HEALTH).is_err());
    assert!(Opening::capture(&[], 2049, &id('a'), HEALTH).is_err());
    let body = work();
    let opening = Opening::capture(&[], body.len(), &id('a'), HEALTH).unwrap();
    let mut audit = Auditor::new(&[]);
    audit.begin_cold_pg();
    let result = ColdPg::admit(opening, &body, &commands(), &mut audit);
    refused(result, audit.diagnostic(), "pg-incomplete-sequence");
    let (state, _) = admit(&[], &body);
    let mut state = state.unwrap();
    let mut audit = Auditor::new(&body);
    audit.begin_cold_pg();
    assert_eq!(
        state.reconcile(&body[..1], &BTreeMap::new(), &mut audit),
        Err(Error::Refused)
    );
    evidence(&state, [0, 0, 0, 0]);
}

#[test]
fn bootstrap_commands_cannot_be_deferred_omitted_duplicated_or_replaced() {
    let good = work();
    for (body, reason) in [
        (good[..2].to_vec(), "pg-incomplete-sequence"),
        (Vec::new(), "pg-command-count"),
        (trace("PRIVATE_OTHER", 'c'), "pg-unknown-command"),
    ] {
        let (result, diagnostic) = admit(&[], &body);
        refused(result, diagnostic, reason);
    }
    let duplicate = [work(), trace(WORK, 'd')].concat();
    let (result, diagnostic) = admit(&[], &duplicate);
    refused(result, diagnostic, "pg-command-count");
    let health = trace(HEALTH, 'c');
    let body = [
        work(),
        health[..1].to_vec(),
        trace(HEALTH, 'd')[..1].to_vec(),
    ]
    .concat();
    let (result, diagnostic) = admit(&[], &body);
    refused(result, diagnostic, "pg-incomplete-sequence");
    let prefix = [health[..1].to_vec(), trace(HEALTH, 'd')[..1].to_vec()].concat();
    let (result, diagnostic) = admit(&prefix, &work());
    refused(result, diagnostic, "pg-incomplete-sequence");
    let body = health[..1].to_vec();
    let opening = Opening::capture(&[], body.len(), &id('a'), HEALTH).unwrap();
    let mut audit = Auditor::new(&body);
    audit.begin_cold_pg();
    let result = ColdPg::admit(
        opening,
        &body,
        &BTreeMap::from([(HEALTH.into(), 1)]),
        &mut audit,
    );
    refused(result, audit.diagnostic(), "pg-incomplete-sequence");
}

#[test]
fn gap_and_final_keep_identity_order_nonzero_and_reuse_refusals() {
    let health = trace(HEALTH, 'c');
    let body = [work(), health[..1].to_vec()].concat();
    let mut wrong_actor = health[1..].to_vec();
    wrong_actor[0]["Actor"]["ID"] = json!(id('f'));
    for (rows, reason) in [
        (vec![health[2].clone()], "pg-die-order"),
        (
            vec![event("exec_start: PRIVATE_OTHER", 'c', "0")],
            "pg-die-action",
        ),
        (vec![health[0].clone()], "pg-duplicate-create"),
        (wrong_actor, "pg-exec-id"),
        (
            [health[1..2].to_vec(), vec![event("exec_die", 'c', "1")]].concat(),
            "pg-nonzero-exit",
        ),
        (vec![health[1].clone(), health[1].clone()], "pg-start-order"),
    ] {
        for in_gap in [true, false] {
            let (state, _) = admit(&[], &body);
            let mut state = state.unwrap();
            let (result, diagnostic) = if in_gap {
                gap(&mut state, &[], &body, &rows)
            } else {
                assert_eq!(gap(&mut state, &[], &body, &[]).0, Ok(()));
                finish(&mut state, &rows)
            };
            assert_eq!(result, Err(Error::Refused));
            assert_eq!(diagnostic.unwrap()["failureClass"], reason);
            evidence(&state, [0, 1, 1, 0]);
        }
    }
}

#[test]
fn cold_pending_cannot_hide_under_final_generic_health_prefix_allowance() {
    let health = trace(HEALTH, 'c');
    let body = [work(), health[..2].to_vec()].concat();
    let (state, _) = admit(&[], &body);
    let mut state = state.unwrap();
    assert_eq!(gap(&mut state, &[], &body, &[]).0, Ok(()));
    let (result, diagnostic) = finish(&mut state, &[]);
    assert_eq!(result, Err(Error::Refused));
    assert_eq!(
        diagnostic.unwrap()["failureClass"],
        "pg-incomplete-sequence"
    );
    evidence(&state, [0, 1, 1, 0]);
    assert_eq!(finish(&mut state, &health[2..]).0, Err(Error::Refused));
}

#[test]
fn gap_handoff_must_preserve_exact_phase_and_completion_does_not_authorize_reuse() {
    let health = trace(HEALTH, 'c');
    let body = [work(), health[..2].to_vec()].concat();
    for pending in [BTreeMap::new(), BTreeMap::from([(id('c'), 1)])] {
        let (state, _) = admit(&[], &body);
        let mut state = state.unwrap();
        let mut audit = Auditor::new(&body);
        audit.begin_cold_pg();
        assert_eq!(
            state.reconcile(&body, &pending, &mut audit),
            Err(Error::Refused)
        );
        assert_eq!(
            audit.diagnostic().unwrap()["failureClass"],
            "pg-incomplete-sequence"
        );
    }
    let (state, _) = admit(&[], &body);
    let mut state = state.unwrap();
    assert_eq!(gap(&mut state, &[], &body, &health[2..]).0, Ok(()));
    evidence(&state, [0, 1, 1, 1]);
    let (result, diagnostic) = finish(&mut state, &health[..1]);
    assert_eq!(result, Err(Error::Refused));
    assert_eq!(diagnostic.unwrap()["failureClass"], "pg-duplicate-create");
    evidence(&state, [0, 1, 1, 1]);
}

#[test]
fn final_audit_retains_actual_partial_completions_and_ignores_new_unrelated_health_prefix() {
    let health = trace(HEALTH, 'c');
    let body = [work(), health[..2].to_vec()].concat();
    let (state, _) = admit(&[], &body);
    let mut state = state.unwrap();
    assert_eq!(gap(&mut state, &[], &body, &[]).0, Ok(()));
    let final_rows = [health[2..].to_vec(), health[..1].to_vec()].concat();
    assert_eq!(finish(&mut state, &final_rows).0, Err(Error::Refused));
    evidence(&state, [0, 1, 1, 1]);
    let (state, _) = admit(&[], &work());
    let mut state = state.unwrap();
    assert_eq!(gap(&mut state, &[], &work(), &[]).0, Ok(()));
    assert_eq!(finish(&mut state, &trace(HEALTH, 'd')[..1]).0, Ok(()));
    evidence(&state, [0, 0, 0, 0]);
}

#[test]
fn captured_positions_survive_prefix_noise_and_gap_offset_and_one_shot_guards() {
    let health = trace(HEALTH, 'c');
    let body = [work(), health[..2].to_vec()].concat();
    let (state, _) = admit(&[], &body);
    let mut state = state.unwrap();
    let noise = json!({"Type":"network","Actor":{"ID":"PRIVATE_NOISE"}});
    let rows = vec![noise, event("exec_die", 'c', "1")];
    let (result, diagnostic) = gap(&mut state, &[], &body, &rows);
    assert_eq!(result, Err(Error::Refused));
    let diagnostic = diagnostic.unwrap();
    assert_eq!(diagnostic["rowIndex"], body.len() + 1);
    assert_eq!(diagnostic["rows"], body.len() + rows.len());
    assert_eq!(gap(&mut state, &[], &body, &[]).0, Err(Error::Refused));
    evidence(&state, [0, 1, 1, 0]);
}

#[test]
fn actual_migration_admission_and_terminal_uncertainty_keep_cold_and_mutating_debts_separate() {
    use super::super::super::{
        host::Baseline,
        protocol::{self, Request},
        sources,
    };
    use super::super::{Fixture, migration_upgrade::admitted};
    let pin = json!({"sha256":"c".repeat(64),"bytes":123});
    let source_pins = sources::guest_names()
        .into_iter()
        .map(|name| (name, json!("a".repeat(64))))
        .collect::<serde_json::Map<_, _>>();
    let helper_pins = sources::helper_names()
        .into_iter()
        .map(|name| (name, json!("b".repeat(64))))
        .collect::<serde_json::Map<_, _>>();
    let request = Request::decode_host(&serde_json::to_vec(&json!({
        "schemaVersion":1,"scope":"PreparedHostMaintenanceRequest",
        "runId":"d".repeat(32),"revision":"e".repeat(40),"scenario":"host-migration-upgrade",
        "setupSeconds":1560,"bodySeconds":3660,"endSeconds":3780,
        "binary":pin,"manifest":pin,"archive":pin,"sourceBundle":pin,"sourceTree":"f".repeat(40),
        "sourcePins":source_pins,"helperPins":helper_pins,
        "postgresImage":format!("postgres@sha256:{}","a".repeat(64)),
        "caddyImage":protocol::CADDY_IMAGE,"engineVersion":"29.8.2"
    })).unwrap()).unwrap();
    let mut fixture = Fixture::new(&request);
    fixture.pg = Some(id('a'));
    fixture.image = format!("sha256:{}", id('e'));
    let baseline = Baseline {
        pg: id('a'),
        app: id('c'),
        proxy: id('d'),
        image: fixture.image.clone(),
        caddy_image: format!("sha256:{}", id('f')),
        network: id('e'),
        none_network: id('f'),
        health: HEALTH.into(),
        health_pending: BTreeMap::new(),
        shell_pg_commands: commands(),
        shell_attempt: None,
        app_row: Value::Null,
        proxy_row: Value::Null,
        attempts: BTreeSet::new(),
    };
    let health = trace(HEALTH, 'c');
    let body = [work(), health[..2].to_vec()].concat();
    let (state, _) = admit(&[], &body);
    fixture.cold_pg = Some(state.unwrap());
    fixture.unknown_effect = true;
    assert!(fixture.daemon_effects_uncertain());
    assert_eq!(
        admitted(&fixture, &request, None, &baseline),
        Err(Error::Refused)
    );
    // The complete baseline audit clears only mutational uncertainty.
    fixture.unknown_effect = false;
    assert_eq!(admitted(&fixture, &request, None, &baseline), Ok(()));
    assert!(fixture.daemon_effects_uncertain());
    // A preparation's independently proved global clear cannot erase cold debt.
    fixture.unknown_effect = true;
    assert_eq!(
        admitted(&fixture, &request, None, &baseline),
        Err(Error::Refused)
    );
    fixture.unknown_effect = false;
    assert!(fixture.daemon_effects_uncertain());
    assert_eq!(admitted(&fixture, &request, None, &baseline), Ok(()));
    // Completing cold health has no authority over a separate unknown mutation.
    fixture.unknown_effect = true;
    let cold = fixture.cold_pg.as_mut().unwrap();
    assert_eq!(gap(cold, &[], &body, &health[2..]).0, Ok(()));
    assert_eq!(finish(cold, &[]).0, Ok(()));
    evidence(cold, [0, 1, 1, 1]);
    assert!(fixture.unknown_effect && fixture.daemon_effects_uncertain());
    assert_eq!(
        admitted(&fixture, &request, None, &baseline),
        Err(Error::Refused)
    );
    fixture.unknown_effect = false;
    fixture.create_uncertain = true;
    assert!(fixture.daemon_effects_uncertain());
    assert_eq!(
        admitted(&fixture, &request, None, &baseline),
        Err(Error::Refused)
    );
    fixture.create_uncertain = false;
    assert!(!fixture.daemon_effects_uncertain());
    assert_eq!(admitted(&fixture, &request, None, &baseline), Ok(()));
    assert_eq!(
        admitted(&fixture, &request, Some(Error::Refused), &baseline),
        Err(Error::Refused)
    );
    fixture.cold_pg = None;
    assert!(!fixture.daemon_effects_uncertain());
    fixture.unknown_effect = true;
    assert!(fixture.daemon_effects_uncertain());
}
