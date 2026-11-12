//! The selection rule against the judgments recorded from the helper this
//! command replaced, the document against the digests, and the transaction
//! against a migrated disposable `TEST_DATABASE_URL`.

use super::*;
use uuid::Uuid;

fn list(emails: &[&str]) -> Vec<String> {
    emails.iter().map(|email| (*email).to_owned()).collect()
}

/// Fifty-one selections, admitted or refused as the earlier helper judged
/// them when its own validation was recorded. One differs on purpose: a
/// control character above the ASCII range is refused like the others.
#[test]
fn a_selection_is_admitted_where_the_earlier_helper_admitted_it() {
    let numbered = |count: usize| -> Vec<String> {
        (0..count)
            .map(|index| format!("user{index}@example.test"))
            .collect()
    };
    let long = |letter: &str, count: usize| vec![format!("{}@example.test", letter.repeat(count))];
    const COUNT: Result<(), &str> = Err("invalid_account_count");
    const DUPLICATE: Result<(), &str> = Err("duplicate_account");
    const EMAIL: Result<(), &str> = Err("invalid_email");
    let cases: Vec<(Vec<String>, Result<(), &str>)> = vec![
        (list(&["owner@example.test"]), Ok(())),
        (list(&["a@b"]), Ok(())),
        (list(&["owner@example.test", "other@example.test"]), Ok(())),
        (list(&[]), COUNT),
        (
            list(&["owner@example.test", "owner@example.test"]),
            DUPLICATE,
        ),
        (numbered(20), Ok(())),
        (numbered(21), COUNT),
        (list(&["Owner@example.test"]), EMAIL),
        (list(&["owner@Example.test"]), EMAIL),
        (list(&["owner"]), EMAIL),
        (list(&["@example.test"]), EMAIL),
        (list(&["owner@"]), EMAIL),
        (list(&["owner@@example.test"]), EMAIL),
        (list(&["owner@example@test"]), EMAIL),
        (list(&["owner @example.test"]), EMAIL),
        (list(&["owner@exam ple.test"]), EMAIL),
        (list(&[" owner@example.test"]), EMAIL),
        (list(&["owner@example.test "]), EMAIL),
        (list(&["owner@example.test\n"]), EMAIL),
        (list(&["owner\t@example.test"]), EMAIL),
        (list(&["owner\0@example.test"]), EMAIL),
        (list(&["owner\u{1f}@example.test"]), EMAIL),
        (list(&["owner\u{7f}@example.test"]), EMAIL),
        (list(&["owner\u{a0}@example.test"]), EMAIL),
        (list(&["owner\u{2028}@example.test"]), EMAIL),
        (list(&["ünïcode@example.test"]), Ok(())),
        (list(&["Ünïcode@example.test"]), EMAIL),
        (list(&["owner@exämple.test"]), Ok(())),
        (list(&["straße@example.test"]), Ok(())),
        (list(&["STRASSE@example.test"]), EMAIL),
        (list(&["ǆ@example.test"]), Ok(())),
        (list(&["ǅ@example.test"]), EMAIL),
        (list(&["İ@example.test"]), EMAIL),
        (list(&["ı@example.test"]), Ok(())),
        (long("a", 242), Ok(())),
        (long("a", 243), EMAIL),
        (long("é", 121), Ok(())),
        (long("é", 122), EMAIL),
        (list(&["owner+tag@example.test"]), Ok(())),
        (list(&["o'wner@example.test"]), Ok(())),
        (list(&["o\"wner@example.test"]), Ok(())),
        (list(&["owner;drop@example.test"]), Ok(())),
        (list(&["owner@example.test", "Owner@example.test"]), EMAIL),
        (list(&["a@b", "a@b"]), DUPLICATE),
        (list(&["a@b", "a@c", "a@b"]), DUPLICATE),
        (list(&[""]), EMAIL),
        (list(&["@"]), EMAIL),
        (list(&["a@b@"]), EMAIL),
        (list(&["\u{b}owner@example.test"]), EMAIL),
        (list(&["owner\u{85}@example.test"]), EMAIL),
        // The earlier helper admitted this one.
        (list(&["owner\u{80}@example.test"]), EMAIL),
    ];
    assert_eq!(cases.len(), 51);
    let mut admitted = 0;
    for (position, (emails, expected)) in cases.iter().enumerate() {
        assert_eq!(selection(emails), *expected, "selection {position}");
        admitted += usize::from(expected.is_ok());
    }
    assert_eq!(admitted, 15);
}

/// Each account is handed one new key of the application's own format, in a
/// document of exactly those accounts, and the digest kept for it is the one
/// the recovery flow later looks up.
#[test]
fn the_document_hands_over_the_keys_whose_digests_are_stored() {
    let emails = list(&["alpha@example.test", "beta@example.test"]);
    let (document, digests) = generate(&emails).unwrap();
    assert!(document.ends_with("}\n"));
    let value: serde_json::Value = serde_json::from_str(&document).unwrap();
    assert_eq!(value.as_object().unwrap().len(), 1);
    let accounts = value["accounts"].as_array().unwrap();
    assert_eq!((accounts.len(), digests.len()), (2, 2));
    for (position, account) in accounts.iter().enumerate() {
        assert_eq!(account.as_object().unwrap().len(), 2);
        assert_eq!(account["email"], emails[position].as_str());
        let handed = account["recoveryKey"].as_str().unwrap();
        // A key is never shown, only judged.
        let shaped = handed.len() == 55 && handed.starts_with("sc-recovery-");
        let stored = recovery_hash(handed).is_ok_and(|digest| digest == digests[position]);
        assert!(shaped && stored, "account {position}");
        assert!(digests[position].len() == 64, "account {position}");
    }
    assert_ne!(accounts[0]["recoveryKey"], accounts[1]["recoveryKey"]);
    assert_ne!(digests[0], digests[1]);
}

/// Only a confirmed commit ends well, and only an unknown one asks the
/// operator to keep the keys.
#[test]
fn a_run_ends_with_one_fixed_line_and_its_status() {
    assert_eq!(ending(&Ok(())), ("account_recovery_issued", 0));
    let refused = ("account_recovery_refused_nothing_changed", 1);
    assert_eq!(ending(&Err(Refusal::Selection("invalid_email"))), refused);
    assert_eq!(ending(&Err(Refusal::Unchanged)), refused);
    let unknown = "account_recovery_unconfirmed_keep_the_keys_no_automatic_retry";
    assert_eq!(ending(&Err(Refusal::Unconfirmed)), (unknown, 1));
}

/// A writer that fails, as a closed pipe would.
struct Closed;

impl Write for Closed {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::ErrorKind::BrokenPipe.into())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A writer that takes every byte and then cannot hand them on.
struct Unflushed;

impl Write for Unflushed {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Err(std::io::ErrorKind::BrokenPipe.into())
    }
}

async fn pool() -> PgPool {
    PgPool::connect(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"))
        .await
        .unwrap()
}

/// A new account with a password and, when given, a recovery digest.
async fn account(pool: &PgPool, digest: Option<&str>) -> String {
    account_at(pool, "recovery.invalid", digest).await
}

/// The same under another reserved domain.
async fn account_at(pool: &PgPool, domain: &str, digest: Option<&str>) -> String {
    let email = format!("{}@{domain}", Uuid::new_v4().simple());
    let created = "INSERT INTO users(email, display_name, password_hash, recovery_key_hash) \
                   VALUES($1, 'Recovery fixture', $2, $3)";
    sqlx::query(created)
        .bind(&email)
        .bind(format!("fixture-hash-{email}"))
        .bind(digest)
        .execute(pool)
        .await
        .unwrap();
    email
}

/// Everything of an account that issuing must leave alone, and its digest.
async fn stored(pool: &PgPool, email: &str) -> (Option<String>, i64, Option<String>) {
    let read = "SELECT password_hash, auth_version, recovery_key_hash FROM users WHERE email = $1";
    sqlx::query_as(read)
        .bind(email)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn remove(pool: &PgPool, emails: &[String]) {
    sqlx::query("DELETE FROM users WHERE email = ANY($1)")
        .bind(emails)
        .execute(pool)
        .await
        .unwrap();
}

/// Two accounts without a key each get the digest of the key written out
/// for them, and nothing else about them or about any other account
/// changes. A second run for the same accounts changes nothing, and a
/// document that cannot be written out stops the run before the database.
#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
async fn database_issuing_stores_exactly_the_digests_of_the_keys_written_out() {
    let pool = pool().await;
    let other = "c".repeat(64);
    let bystander = account(&pool, Some(&other)).await;
    let emails = vec![account(&pool, None).await, account(&pool, None).await];
    let before = [
        stored(&pool, &emails[0]).await,
        stored(&pool, &emails[1]).await,
    ];
    assert_eq!(
        issue(&pool, &emails, &mut Closed).await,
        Err(Refusal::Unchanged)
    );
    assert_eq!(
        issue(&pool, &emails, &mut Unflushed).await,
        Err(Refusal::Unchanged)
    );
    assert_eq!(stored(&pool, &emails[0]).await, before[0]);
    let mut output = Vec::new();
    assert_eq!(issue(&pool, &emails, &mut output).await, Ok(()));
    let document: serde_json::Value = serde_json::from_slice(&output).unwrap();
    for (position, email) in emails.iter().enumerate() {
        let handed = &document["accounts"][position];
        assert_eq!(handed["email"], email.as_str());
        let digest = recovery_hash(handed["recoveryKey"].as_str().unwrap()).unwrap();
        let (password, version, kept) = stored(&pool, email).await;
        assert!(kept == Some(digest), "account {position}");
        assert_eq!(
            (password, version),
            (before[position].0.clone(), before[position].1)
        );
    }
    assert_eq!(stored(&pool, &bystander).await.2, Some(other));
    // A key is never replaced.
    let after = [
        stored(&pool, &emails[0]).await,
        stored(&pool, &emails[1]).await,
    ];
    let mut second = Vec::new();
    assert_eq!(
        issue(&pool, &emails, &mut second).await,
        Err(Refusal::Unchanged)
    );
    assert!(!second.is_empty());
    assert_eq!(
        [
            stored(&pool, &emails[0]).await,
            stored(&pool, &emails[1]).await
        ],
        after
    );
    remove(&pool, &[emails, vec![bystander]].concat()).await;
    pool.close().await;
}

/// One account that is missing, or already has a key, keeps every other
/// named account unchanged too.
#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
async fn database_issuing_changes_nothing_unless_every_account_is_eligible() {
    let pool = pool().await;
    let eligible = account(&pool, None).await;
    let enabled = account(&pool, Some(&"d".repeat(64))).await;
    let absent = format!("{}@recovery.invalid", Uuid::new_v4().simple());
    let before = [
        stored(&pool, &eligible).await,
        stored(&pool, &enabled).await,
    ];
    for other in [&enabled, &absent] {
        for emails in [
            vec![eligible.clone(), other.clone()],
            vec![other.clone(), eligible.clone()],
        ] {
            let mut output = Vec::new();
            assert_eq!(
                issue(&pool, &emails, &mut output).await,
                Err(Refusal::Unchanged)
            );
            assert_eq!(stored(&pool, &eligible).await, before[0]);
            assert_eq!(stored(&pool, &enabled).await, before[1]);
        }
    }
    let invalid = vec![eligible.clone(), eligible.to_uppercase()];
    let refused = issue(&pool, &invalid, &mut Vec::new()).await;
    assert_eq!(refused, Err(Refusal::Selection("invalid_email")));
    assert_eq!(stored(&pool, &eligible).await, before[0]);
    remove(&pool, &[eligible, enabled]).await;
    pool.close().await;
}

/// The fixture's own triggers, in fixed text: each acts only on addresses
/// of its own reserved domain, and is created and removed by its test.
const SUPPRESSING: [&str; 5] = [
    "DROP TRIGGER IF EXISTS recovery_fixture_suppress ON users",
    "CREATE OR REPLACE FUNCTION recovery_fixture_suppress() RETURNS trigger LANGUAGE plpgsql AS $$ \
     BEGIN IF OLD.email LIKE '%@suppressed.recovery.invalid' THEN RETURN NULL; END IF; \
     RETURN NEW; END; $$",
    "CREATE TRIGGER recovery_fixture_suppress BEFORE UPDATE ON users \
     FOR EACH ROW EXECUTE FUNCTION recovery_fixture_suppress()",
    "DROP TRIGGER recovery_fixture_suppress ON users",
    "DROP FUNCTION recovery_fixture_suppress()",
];
const REFUSING: [&str; 5] = [
    "DROP TRIGGER IF EXISTS recovery_fixture_refuse ON users",
    "CREATE OR REPLACE FUNCTION recovery_fixture_refuse() RETURNS trigger LANGUAGE plpgsql AS $$ \
     BEGIN IF NEW.email LIKE '%@refused.recovery.invalid' THEN RAISE EXCEPTION 'fixture'; END IF; \
     RETURN NEW; END; $$",
    "CREATE CONSTRAINT TRIGGER recovery_fixture_refuse AFTER UPDATE ON users \
     DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION recovery_fixture_refuse()",
    "DROP TRIGGER recovery_fixture_refuse ON users",
    "DROP FUNCTION recovery_fixture_refuse()",
];

async fn run(pool: &PgPool, statements: &[&'static str]) {
    for statement in statements {
        sqlx::query(*statement).execute(pool).await.unwrap();
    }
}

/// An update that does not reach every named account is not committed:
/// here a trigger silently keeps one row as it was.
#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
async fn database_issuing_rolls_back_an_update_that_misses_an_account() {
    let pool = pool().await;
    let kept = account_at(&pool, "suppressed.recovery.invalid", None).await;
    let emails = vec![account(&pool, None).await, kept];
    run(&pool, &SUPPRESSING[..3]).await;
    let outcome = issue(&pool, &emails, &mut Vec::new()).await;
    run(&pool, &SUPPRESSING[3..]).await;
    assert_eq!(outcome, Err(Refusal::Unchanged));
    for email in &emails {
        assert_eq!(stored(&pool, email).await.2, None);
    }
    remove(&pool, &emails).await;
    pool.close().await;
}

/// A commit that fails leaves the outcome unknown to this command, which
/// says so: here a deferred constraint of the fixture's own refuses the
/// commit, so the keys written out were in fact not stored.
#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
async fn database_issuing_reports_a_failed_commit_as_unconfirmed() {
    let pool = pool().await;
    let emails = vec![account_at(&pool, "refused.recovery.invalid", None).await];
    run(&pool, &REFUSING[..3]).await;
    let mut output = Vec::new();
    let outcome = issue(&pool, &emails, &mut output).await;
    run(&pool, &REFUSING[3..]).await;
    assert_eq!(outcome, Err(Refusal::Unconfirmed));
    assert!(!output.is_empty());
    assert_eq!(stored(&pool, &emails[0]).await.2, None);
    remove(&pool, &emails).await;
    pool.close().await;
}
