//! An operator's initialization of missing recovery keys for named accounts.
//! It is a mode of the server binary, run where the database is, never a
//! public endpoint, and it never replaces a key: every named account must
//! exist and have none, or nothing changes.
//!
//! The keys are written out before the database is asked to store their
//! digests, because an answer can be lost after a commit: whoever ran the
//! command then still holds the only copy.

use super::account::{generate_recovery_key, recovery_hash};
use serde::Serialize;
use sqlx::PgPool;
use std::io::Write;

/// The most accounts one run initializes.
const MAX_ACCOUNTS: usize = 20;
const MAX_EMAIL_BYTES: usize = 255;

/// How a run that issued nothing, or may have, ended.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// The named accounts are not a selection this command acts on.
    Selection(&'static str),
    /// Nothing changed: no key could be written out, an account is missing
    /// or already has a key, or the database failed before any commit.
    Unchanged,
    /// The commit's outcome is unknown. The keys already written out may be
    /// the accounts' only copy.
    Unconfirmed,
}

/// The one line that tells the operator how a run ended, and its exit status.
pub(crate) fn ending(outcome: &Result<(), Refusal>) -> (&'static str, u8) {
    match outcome {
        Ok(()) => ("account_recovery_issued", 0),
        Err(Refusal::Selection(_) | Refusal::Unchanged) => {
            ("account_recovery_refused_nothing_changed", 1)
        }
        Err(Refusal::Unconfirmed) => (
            "account_recovery_unconfirmed_keep_the_keys_no_automatic_retry",
            1,
        ),
    }
}

/// An exact account address: lower case, one `@` between two parts, no blank
/// and no control character.
fn exact(email: &str) -> bool {
    let plain = |part: &str| {
        let ordinary = |character: char| !character.is_whitespace() && !character.is_control();
        // No `@` either: an address has exactly one.
        !part.is_empty() && part.chars().all(ordinary)
    };
    let parts = email.split_once('@');
    email.len() <= MAX_EMAIL_BYTES
        && email.to_lowercase() == email
        && parts.is_some_and(|(name, domain)| plain(name) && plain(domain) && !domain.contains('@'))
}

/// Require one to twenty different exact addresses.
pub(crate) fn selection(emails: &[String]) -> Result<(), &'static str> {
    if emails.is_empty() || emails.len() > MAX_ACCOUNTS {
        return Err("invalid_account_count");
    }
    let different: std::collections::BTreeSet<&String> = emails.iter().collect();
    if different.len() != emails.len() {
        return Err("duplicate_account");
    }
    match emails.iter().all(|email| exact(email)) {
        true => Ok(()),
        false => Err("invalid_email"),
    }
}

#[derive(Serialize)]
struct Account<'a> {
    email: &'a str,
    #[serde(rename = "recoveryKey")]
    recovery: String,
}

#[derive(Serialize)]
struct Document<'a> {
    accounts: Vec<Account<'a>>,
}

/// New keys for the named accounts: the document that hands them over and
/// the digests the database stores, in the accounts' order.
fn generate(emails: &[String]) -> Option<(String, Vec<String>)> {
    let (mut accounts, mut digests) = (Vec::new(), Vec::new());
    for email in emails {
        let recovery = generate_recovery_key().ok()?;
        digests.push(recovery_hash(&recovery).ok()?);
        accounts.push(Account { email, recovery });
    }
    let document = serde_json::to_string_pretty(&Document { accounts }).ok()?;
    Some((format!("{document}\n"), digests))
}

/// Store every digest or none. One statement sets the digest of each named
/// account that exists and has no key; unless that is every one of them,
/// nothing is committed.
async fn store(pool: &PgPool, emails: &[String], digests: &[String]) -> Result<(), Refusal> {
    let unchanged = |_| Refusal::Unchanged;
    let mut transaction = pool.begin().await.map_err(unchanged)?;
    let changed = sqlx::query(concat!(
        "UPDATE users AS u SET recovery_key_hash = r.digest, updated_at = now() ",
        "FROM unnest($1::text[], $2::text[]) AS r(email, digest) ",
        "WHERE u.email = r.email AND u.recovery_key_hash IS NULL",
    ))
    .bind(emails)
    .bind(digests)
    .execute(&mut *transaction)
    .await
    .map_err(unchanged)?;
    if usize::try_from(changed.rows_affected()) != Ok(emails.len()) {
        return Err(Refusal::Unchanged);
    }
    transaction.commit().await.map_err(|_| Refusal::Unconfirmed)
}

/// Initialize the named accounts' recovery keys. The keys go to `output`,
/// completely, before the database is asked to store anything.
pub(crate) async fn issue(
    pool: &PgPool,
    emails: &[String],
    output: &mut impl Write,
) -> Result<(), Refusal> {
    selection(emails).map_err(Refusal::Selection)?;
    let (document, digests) = generate(emails).ok_or(Refusal::Unchanged)?;
    let written = output
        .write_all(document.as_bytes())
        .and_then(|()| output.flush());
    written.map_err(|_| Refusal::Unchanged)?;
    store(pool, emails, &digests).await
}

#[cfg(test)]
#[path = "recovery_issuance_tests.rs"]
mod tests;
