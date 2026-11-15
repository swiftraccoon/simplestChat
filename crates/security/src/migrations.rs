//! The lint of new migrations.
//!
//! SQLx runs a migration in a transaction unless its first line is the
//! marker `-- no-transaction`, and the scanner is told which of the two it
//! reads. The marker is taken only where SQLx takes it, and only before the
//! one statement that needs it: a single concurrent index with plain,
//! unquoted names. Whatever else must run outside a transaction needs a
//! change of this policy, not a marker. A concurrent index runs with the
//! timeouts the server sets when it opens its maintenance connection, which
//! no migration can set for itself, so the scanner reads them before the
//! migration, as they are written in the server's source. Every rule of the
//! scanner is on, and no finding is reviewed away.

use crate::{
    Fault, Result,
    context::{Context, Outcome, Run},
    fast::Programs,
    files, findings,
    policy::space,
    record, require,
};
use std::{
    ffi::OsStr,
    fs::DirBuilder,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
};

/// The first line of a migration that runs outside a transaction.
const MARKER: &str = "-- no-transaction";
/// The version of PostgreSQL the migrations are written for.
const VERSION: &str = "18.6";
/// Where the server opens its maintenance connection.
const SOURCE: &str = "src/db.rs";
/// What a migration that is not one concurrent index is refused as.
const ONE: Fault = Fault("migration_requires_one_concurrent_index");
/// The timeouts the maintenance connection must set, each once.
const TIMEOUTS: [&str; 2] = ["statement_timeout", "lock_timeout"];

/// A letter as the reference's patterns compared it without regard to
/// case: an ASCII letter in lowercase, and the four letters outside ASCII
/// that they took for `i`, `k` and `s`.
fn folded(letter: char) -> char {
    match letter {
        '\u{130}' | '\u{131}' => 'i',
        '\u{17f}' => 's',
        '\u{212a}' => 'k',
        other => other.to_ascii_lowercase(),
    }
}

/// What follows `word` at the start of `text`, whatever the case of its
/// letters.
fn word<'a>(text: &'a str, word: &str) -> Option<&'a str> {
    let mut rest = text.chars();
    let begun = word
        .chars()
        .all(|letter| rest.next().map(folded) == Some(letter));
    begun.then_some(rest.as_str())
}

/// What follows the white space at the start of `text`, when there is some.
fn spaced(text: &str) -> Option<&str> {
    let rest = text.trim_start_matches(space);
    (rest.len() < text.len()).then_some(rest)
}

/// What follows the plain name at the start of `text`: letters, digits and
/// underscores, not beginning with a digit.
fn named(text: &str) -> Option<&str> {
    let letter = |part: char| folded(part).is_ascii_lowercase() || part == '_';
    let rest = text.strip_prefix(letter)?;
    Some(rest.trim_start_matches(|part: char| letter(part) || part.is_ascii_digit()))
}

/// Whether a line of `sql` begins, after white space, with two dashes and
/// the marker's word. The white space may hold line ends, and the word ends
/// before anything but an ASCII letter, a digit or an underscore.
fn misplaced(sql: &str) -> bool {
    let later = sql.match_indices('\n').map(|(end, _)| end + 1);
    std::iter::once(0).chain(later).any(|start| {
        let line = sql[start..].trim_start_matches(space);
        let said = line
            .strip_prefix("--")
            .map(|rest| rest.trim_start_matches(space));
        let ended = said.and_then(|rest| word(rest, "no-transaction"));
        let longer = |next: char| next.is_ascii_alphanumeric() || next == '_';
        ended.is_some_and(|rest| !rest.starts_with(longer))
    })
}

/// The statements of `body` without what cannot hold one: every text as an
/// empty one, and no comment. A text that never ends is not read.
fn code(body: &str) -> Option<String> {
    let (mut code, mut rest) = (String::new(), body);
    while let Some(next) = rest.chars().next() {
        if next == '\'' {
            // A quote written twice in a text reads as two texts side by
            // side, which hold as little.
            let end = rest[1..].find('\'')?;
            code.push_str("''");
            rest = &rest[end + 2..];
        } else if rest.starts_with("--") {
            rest = &rest[rest.find('\n').unwrap_or(rest.len())..];
        } else {
            code.push(next);
            rest = &rest[next.len_utf8()..];
        }
    }
    Some(code)
}

/// Whether `code` is one statement that creates one concurrent index, with
/// the word `unique` when `unique` and the words `if not exists` when
/// `guarded`: plain names for the index and its table, then the columns
/// and whatever follows them up to the one statement end.
fn index(code: &str, unique: bool, guarded: bool) -> Option<()> {
    let mut rest = spaced(word(code, "create")?)?;
    if unique {
        rest = spaced(word(rest, "unique")?)?;
    }
    rest = spaced(word(rest, "index")?)?;
    rest = spaced(word(rest, "concurrently")?)?;
    if guarded {
        for part in ["if", "not", "exists"] {
            rest = spaced(word(rest, part)?)?;
        }
    }
    rest = spaced(named(rest)?)?;
    rest = spaced(word(rest, "on")?)?;
    let columns = named(rest)?.trim_start_matches(space).strip_prefix('(')?;
    let (inside, after) = columns.split_once(';')?;
    (!inside.is_empty() && after.is_empty()).then_some(())
}

/// Whether SQLx runs the migration `sql` in a transaction. It does unless
/// the first line is exactly the marker, and then the rest must be one
/// concurrent index and nothing that could hide another statement: no
/// block comment, backslash, dollar sign, null byte, double quote or
/// carriage return. A quoted name may hold a single quote, which would read
/// as the start of a text here and hide what follows, and the database
/// ends a comment at a carriage return, where one ends here only at a line
/// feed. The marker's word after two dashes at the start of any other line
/// is refused, because SQLx would not take it.
pub fn transactional(sql: &str) -> Result<bool> {
    let Some(rest) = sql.strip_prefix(MARKER) else {
        require(!misplaced(sql), "migration_transaction_marker_position")?;
        return Ok(true);
    };
    let body = rest.strip_prefix('\n');
    require(
        body.is_some() || rest.is_empty(),
        "migration_transaction_marker_format",
    )?;
    let body = body.unwrap_or_default();
    let hiding = ["/*", "*/", "\\", "$", "\0", "\"", "\r"];
    require(
        !hiding.iter().any(|token| body.contains(token)),
        "migration_concurrent_index_syntax",
    )?;
    let code = code(body).ok_or(ONE)?;
    let code = code.trim_matches(space);
    let forms = [(true, true), (true, false), (false, true), (false, false)];
    let one = |(unique, guarded)| index(code, unique, guarded).is_some();
    require(forms.into_iter().any(one), ONE.0)?;
    Ok(false)
}

/// The mode of the migration `content`, which is text.
fn mode(content: &[u8]) -> Result<bool> {
    let sql = std::str::from_utf8(content).map_err(|_| Fault("migration_not_text"))?;
    transactional(sql)
}

/// What the maintenance connection in `source` is opened with: what stands
/// between the brackets of the first options set after the connection's
/// own address is read.
fn declared(source: &str) -> Option<&str> {
    let opened = |(at, binding): (usize, &str)| {
        let rest = source[at + binding.len()..].trim_start_matches(space);
        let rest = rest.strip_prefix('=')?.trim_start_matches(space);
        let rest = rest.strip_prefix("PgConnectOptions::from_str(")?;
        rest.strip_prefix('&').unwrap_or(rest).strip_prefix("url)")
    };
    let after = source.match_indices("let maintenance").find_map(opened)?;
    let (_, options) = after.split_once(".options([")?;
    Some(options.split_once("]);")?.0)
}

/// One timeout at the start of `text`, written as a pair of two literals:
/// its name, its value, and what follows the pair. A value is a whole
/// number that does not begin with a nought, in milliseconds, seconds or
/// minutes.
fn timeout(text: &str) -> Option<(&'static str, &str, &str)> {
    let rest = text.strip_prefix("(\"")?;
    let name = TIMEOUTS.into_iter().find(|name| rest.starts_with(name))?;
    let rest = rest[name.len()..].strip_prefix("\",")?;
    let rest = rest.trim_start_matches(space).strip_prefix('"')?;
    let number = rest.trim_start_matches(|digit: char| digit.is_ascii_digit());
    let digits = rest.len() - number.len();
    let ended = |unit: &str| number.strip_prefix(unit)?.strip_prefix("\")");
    let (unit, after) = ["ms", "s", "min"]
        .into_iter()
        .find_map(|unit| Some((unit, ended(unit)?)))?;
    let value = &rest[..digits + unit.len()];
    (digits > 0 && !value.starts_with('0')).then_some((name, value, after))
}

/// The statements that set the maintenance connection's timeouts as
/// `source` sets them, for a scanner that reads only SQL. The connection
/// must be opened with exactly the two timeouts, each a literal, and it
/// must be the one the migrations run on.
pub fn timeouts(source: &str) -> Result<String> {
    let declared = declared(source).ok_or(Fault("migration_timeout_configuration_missing"))?;
    require(
        source.contains("connect_with(&maintenance)"),
        "migration_timeout_connection_missing",
    )?;
    let (mut set, mut rest, mut other) = (Vec::new(), declared, false);
    while let Some(next) = rest.chars().next() {
        if let Some((name, value, after)) = timeout(rest) {
            set.push((name, value));
            rest = after;
        } else {
            other |= !matches!(next, ' ' | ',' | '\t' | '\r' | '\n');
            rest = &rest[next.len_utf8()..];
        }
    }
    let both = matches!(set.as_slice(), [first, second] if first.0 != second.0);
    require(both && !other, "migration_timeout_literals_required")?;
    let statement = |(name, value): &(&str, &str)| format!("SET {name} = '{value}';\n");
    Ok(set.iter().map(statement).collect())
}

/// The timeouts of the maintenance connection that the source below `root`
/// opens, read whole and of at most `limit` bytes.
fn prefix(root: &Path, limit: u64) -> Result<String> {
    let source = files::bounded(&root.join(SOURCE), limit, false)?;
    let missing = Fault("migration_timeout_configuration_missing");
    timeouts(std::str::from_utf8(&source).map_err(|_| missing)?)
}

/// Run the scanner over `paths` as migrations that run in a transaction,
/// or as ones that do not, with every rule on, and refuse any finding.
fn scanned(
    context: &mut Context,
    programs: &dyn Programs,
    transactional: bool,
    paths: &[PathBuf],
) -> Outcome<()> {
    let name = match transactional {
        true => "squawk-transactional",
        false => "squawk-concurrent-index",
    };
    let config = context.output().join(format!("{name}.toml"));
    let settings = format!(
        "excluded_rules = []\nincluded_rules = []\nexcluded_paths = []\n\
         assume_in_transaction = {transactional}\npg_version = \"{VERSION}\"\n"
    );
    files::write_private(&config, settings.as_bytes(), 0o600)?;
    let program = programs.program("squawk")?;
    let version = format!("--pg-version={VERSION}");
    let mut argv = vec![
        program.as_os_str(),
        OsStr::new("--config"),
        config.as_os_str(),
        OsStr::new(&version),
    ];
    argv.extend(transactional.then_some(OsStr::new("--assume-in-transaction")));
    argv.push(OsStr::new("--reporter=json"));
    argv.extend(paths.iter().map(|path| path.as_os_str()));
    let home = context.output().join("home");
    let how = Run {
        directory: Some(&home),
        ..Run::default()
    };
    let (_, report) = context.run(name, &argv, &how)?;
    let report = record::document(&report)?;
    require(
        findings::list(&report)?.is_empty(),
        "migration_policy_findings",
    )?;
    Ok(())
}

/// Lint the new `migrations`, named from the sources' root, each in the
/// mode SQLx runs it in. The ones that run in a transaction are read where
/// they are. The others are read from private copies that begin with the
/// maintenance connection's timeouts.
pub fn lint(context: &mut Context, programs: &dyn Programs, migrations: &[String]) -> Outcome<()> {
    let limit = context.limits().report;
    let (mut inside, mut outside) = (Vec::new(), Vec::new());
    for name in migrations {
        let path = context.root().join(name);
        let content = files::bounded(&path, limit, false)?;
        if mode(&content)? {
            inside.push(path);
        } else {
            outside.push((path, content));
        }
    }
    if !inside.is_empty() {
        scanned(context, programs, true, &inside)?;
    }
    if outside.is_empty() {
        return Ok(());
    }
    let prefix = prefix(context.root(), limit)?;
    let projected = context.output().join("migration-context");
    let made = DirBuilder::new().mode(0o700).create(&projected);
    made.map_err(|error| files::unusable(&error))?;
    let mut copies = Vec::new();
    for (path, content) in outside {
        let copy = projected.join(path.file_name().unwrap_or_default());
        files::write_private(&copy, &[prefix.as_bytes(), &content].concat(), 0o600)?;
        copies.push(copy);
    }
    scanned(context, programs, false, &copies)
}

#[cfg(test)]
mod tests;
