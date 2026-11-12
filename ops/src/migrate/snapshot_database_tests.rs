//! The snapshot and the owner's rename against real PostgreSQL: a dump and
//! a restore that change the physical layout leave the evidence equal, and
//! every logical drift changes it. These run only inside the disposable
//! cluster that `simplestchat-ci database-check migration` owns and selects;
//! anywhere else, a broad `--include-ignored` run included, they return at
//! once.

use super::super::snapshot_sql::CONSTRAINT_DEFINITION;
use super::super::target::owner_sql;
use super::*;
use crate::host::System;
use crate::host::command::{Commands, Local, Request};
use crate::host::state::tests::Scratch;
use crate::host::tests::paths;
use crate::json::python_dump;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const SELECTOR: &str = "MIGRATION_DATABASE_CHECK";
const OLD: &str = "https://old.example.test";
const NEW: &str = "https://new.example.test";

/// The owning check's database, or nothing anywhere else.
fn database() -> Option<String> {
    let set = |name: &str| std::env::var(name).is_ok_and(|value| value == "1");
    if !(set(SELECTOR) && set("DISPOSABLE_TEST_DATABASE")) {
        return None;
    }
    Some(std::env::var("TEST_DATABASE_URL").expect("the owning check's database"))
}

/// Where an installed PostgreSQL tool is.
fn installed(name: &str) -> PathBuf {
    let path = std::env::var_os("PATH").unwrap();
    let found = std::env::split_paths(&path).map(|directory| directory.join(name));
    let found = found.into_iter().find(|candidate| candidate.is_file());
    found.unwrap_or_else(|| panic!("{name} is required"))
}

/// Run one PostgreSQL tool with `source` on its standard input: what it
/// printed, or what it complained of when it failed.
fn tool(name: &str, arguments: &[&str], source: &[u8]) -> std::result::Result<String, String> {
    let mut command = Command::new(installed(name));
    command.args(arguments).stdin(Stdio::piped());
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(source).unwrap();
    let output = child.wait_with_output().unwrap();
    if output.status.success() {
        Ok(String::from_utf8(output.stdout).unwrap())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned())
    }
}

/// Statements run in one database of the owned cluster.
fn attempted(url: &str, source: &str) -> std::result::Result<String, String> {
    let arguments = ["-X", "-qAt", "--set", "ON_ERROR_STOP=1", url];
    tool("psql", &arguments, source.as_bytes())
}

fn sql(url: &str, source: &str) -> String {
    attempted(url, source).unwrap_or_else(|complaint| panic!("{complaint}"))
}

/// A machine whose database client is the installed one, on the fixture's
/// own connection instead of a container's socket.
struct Adapter<'a>(&'a str);

impl Commands for Adapter<'_> {
    fn run(&self, request: &Request<'_>) -> Result<Vec<u8>> {
        panic!("a snapshot runs no other command: {:?}", request.argv);
    }

    fn converse<'a>(
        &'a self,
        argv: &[String],
        environment: &[(&str, &str)],
    ) -> Result<Box<dyn Dialogue + 'a>> {
        // The production client's own options, up to where it names the
        // container's socket.
        let client = argv.iter().position(|word| word == "psql").unwrap();
        let socket = argv.iter().rposition(|word| word == "--host").unwrap();
        let mut local = vec![installed("psql").to_str().unwrap().to_owned()];
        local.extend_from_slice(&argv[client + 1..socket]);
        local.push(self.0.to_owned());
        Local.converse(&local, environment)
    }
}

/// What a snapshot compares, of one fixture database: every table's rows,
/// every sequence's state and the schema's description.
fn evidence(url: &str) -> (Vec<Json>, Vec<Json>, Json) {
    let adapter = Adapter(url);
    let clock = System::new();
    let scratch = std::env::temp_dir();
    let host = Host {
        commands: &adapter,
        clock: &clock,
        paths: paths(&scratch),
        owner: 0,
    };
    let mut session = Session::open(&host, &"1".repeat(64), BOUNDS).unwrap();
    let taken = (|| {
        session.silent(SETUP)?;
        let catalog = session.value(CATALOG)?;
        let tables = tables(&mut session, &catalog)?;
        let sequences = sequences(&mut session, &catalog)?;
        let metadata = session.digest(METADATA, BOUNDS.section)?;
        Ok::<_, Failure>((tables, sequences, metadata))
    })();
    session.close();
    taken.unwrap()
}

fn spelled(values: &[Json]) -> String {
    python_dump(&Json::Array(values.to_vec()), false)
}

/// The digest of the users table once the managed owner is renamed, taken
/// from the stream without changing a row.
fn renamed(url: &str) -> Json {
    let adapter = Adapter(url);
    let clock = System::new();
    let scratch = std::env::temp_dir();
    let host = Host {
        commands: &adapter,
        clock: &clock,
        paths: paths(&scratch),
        owner: 0,
    };
    let container = "1".repeat(64);
    renamed_users(
        &host,
        &container,
        "owner@old.example.test",
        "owner@new.example.test",
    )
    .unwrap()
}

/// From `test_enum_check_normalization_preserves_every_other_expression`:
/// only the proven lossless relocation of the array cast is made canonical.
#[test]
#[ignore = "runs only in the cluster `database-check migration` owns"]
fn only_the_lossless_enum_cast_relocation_is_made_canonical() {
    let Some(url) = database() else { return };
    let canonical_of = |definition: &str, kind: &str| {
        let literal = format!("'{}'", definition.replace('\'', "''"));
        let answer = sql(
            &url,
            &format!(
                "BEGIN READ ONLY; SET search_path=pg_catalog; SELECT {CONSTRAINT_DEFINITION} \
                 FROM (VALUES ('{kind}')) c(contype) \
                 CROSS JOIN (VALUES ({literal})) d(definition); ROLLBACK;"
            ),
        );
        answer.strip_suffix('\n').unwrap_or(&answer).to_owned()
    };
    let original = "CHECK (kind::text = ANY (ARRAY['registration'::character varying, \
                    'room'::character varying]::text[]))";
    let canonical = "CHECK (kind::text = ANY (ARRAY['registration'::character varying::text, \
                     'room'::character varying::text]))";
    assert_eq!(canonical_of(original, "c"), canonical);
    assert_eq!(canonical_of(canonical, "c"), canonical);
    assert_eq!(canonical_of(original, "f"), original);
    for expression in [
        original.replace("character varying", "character varying(1)"),
        original.replace(" = ANY ", " <> ALL "),
        original.replace("kind::text", "lower(kind::text)"),
        original.replace("'room'::character varying", "NULL::character varying"),
        original.replace("'room'", "'room''s'"),
        original.replace("'room'", "'room::character varying'"),
        original.replace("'room'::character varying", "room::character varying"),
        format!("{original} NOT VALID"),
        format!("prefix {original}"),
    ] {
        assert_eq!(canonical_of(&expression, "c"), expression);
    }
    for changed in [
        original.replace("kind::text", "other::text"),
        original.replace("'room'", "'private'"),
        original.replace("'registration'", "'room'").replace(
            "'room'::character varying]",
            "'registration'::character varying]",
        ),
    ] {
        assert_ne!(canonical_of(&changed, "c"), canonical, "{changed}");
    }
}

/// The databases a test made in the owned cluster, dropped when it ends.
struct Made<'a> {
    base: &'a str,
    names: Vec<String>,
}

impl Drop for Made<'_> {
    fn drop(&mut self) {
        for name in &self.names {
            let _ = attempted(self.base, &format!("DROP DATABASE {name};"));
        }
    }
}

/// The address of another database of the same cluster.
fn beside(base: &str, name: &str) -> String {
    let (address, options) = match base.split_once('?') {
        Some((address, options)) => (address, format!("?{options}")),
        None => (base, String::new()),
    };
    let (cluster, _) = address.rsplit_once('/').unwrap();
    format!("{cluster}/{name}{options}")
}

const FIXTURE: &str = "SET ROLE postgres;
CREATE SCHEMA operations;
REVOKE ALL ON SCHEMA operations FROM PUBLIC;
CREATE TABLE public.users(id uuid PRIMARY KEY,email text UNIQUE NOT NULL,password_hash text,
 auth_version bigint NOT NULL,removed text, payload jsonb,
 chat_style varchar(16) NOT NULL CONSTRAINT users_chat_style_value
 CHECK (chat_style IN ('accent','text','bubble')));
ALTER TABLE public.users DROP COLUMN removed;
CREATE TABLE public.rooms(id text PRIMARY KEY,owner_id uuid REFERENCES public.users(id));
CREATE TABLE public.partitioned(id int,payload text) PARTITION BY RANGE(id);
CREATE TABLE public.first_partition PARTITION OF public.partitioned FOR VALUES FROM(0) TO(10);
CREATE TABLE public.second_partition PARTITION OF public.partitioned FOR VALUES FROM(10) TO(20);
CREATE TABLE operations.events(id bigserial PRIMARY KEY,payload text);
GRANT SELECT,UPDATE ON public.users TO simplestchat_app;
INSERT INTO public.users VALUES('11111111-1111-4111-8111-111111111111',
 'owner@old.example.test','fixture-password-hash',9,'{\"text\":\"line\\nnext\\tpart\"}','accent');
INSERT INTO public.rooms VALUES('lobby','11111111-1111-4111-8111-111111111111');
INSERT INTO public.partitioned VALUES(1,E'a\\nb'),(11,'last');
INSERT INTO operations.events(payload) VALUES('fixture incident');";

/// The guarded rename refuses another cluster and changes exactly the
/// owner's address, which the stream's own digest predicted.
fn rename_changes_only_the_owner(destination: &str, original: &(Vec<Json>, Vec<Json>, Json)) {
    let cluster = sql(
        destination,
        "SELECT system_identifier FROM pg_control_system();",
    );
    let cluster = cluster.trim();
    sql(
        destination,
        "GRANT EXECUTE ON FUNCTION pg_control_system() TO postgres;",
    );
    let guarded = |cluster: &str| {
        let statement = owner_sql(OLD, NEW, cluster).unwrap();
        attempted(
            destination,
            &format!("BEGIN; SET ROLE postgres; {statement} COMMIT;"),
        )
    };
    assert!(guarded("1").is_err());
    let unchanged = evidence(destination);
    assert_eq!(spelled(&unchanged.0), spelled(&original.0));
    assert_eq!(spelled(&unchanged.1), spelled(&original.1));
    assert_eq!(
        python_dump(&unchanged.2, false),
        python_dump(&original.2, false)
    );
    let expected = renamed(destination);
    guarded(cluster).unwrap();
    let changed = evidence(destination);
    let users = changed.0.iter().find(|table| {
        table.get("name").and_then(Json::string) == Some("users")
            && table.get("schema").and_then(Json::string) == Some("public")
    });
    let users = users.unwrap();
    for name in ["rows", "copyBytes", "sha256"] {
        assert!(
            users
                .get(name)
                .unwrap()
                .semantic_eq(expected.get(name).unwrap()),
            "{name}"
        );
    }
    assert_ne!(spelled(&changed.0), spelled(&original.0));
    assert_eq!(spelled(&changed.1), spelled(&original.1));
    assert_eq!(
        python_dump(&changed.2, false),
        python_dump(&original.2, false)
    );
    let kept = sql(
        destination,
        "SELECT password_hash||':'||auth_version::text FROM public.users;",
    );
    assert_eq!(kept, "fixture-password-hash:9\n");
    sql(
        destination,
        "UPDATE public.users SET email='owner@old.example.test';",
    );
    assert_eq!(spelled(&evidence(destination).0), spelled(&original.0));
}

/// A changed allowed value of an enum check is seen although every stored
/// row is identical.
fn an_enum_change_is_seen(destination: &str, original: &(Vec<Json>, Vec<Json>, Json)) {
    for allowed in ["balloon", "bubble"] {
        sql(
            destination,
            &format!(
                "ALTER TABLE public.users DROP CONSTRAINT users_chat_style_value; \
                 ALTER TABLE public.users ADD CONSTRAINT users_chat_style_value \
                 CHECK (chat_style IN ('accent','text','{allowed}'));"
            ),
        );
        let changed = evidence(destination);
        assert_eq!(spelled(&changed.0), spelled(&original.0));
        let same = python_dump(&changed.2, false) == python_dump(&original.2, false);
        assert_eq!(same, allowed == "bubble");
    }
}

/// From `test_real_restore_preserves_rows_partitions_acl_and_schema_digest`:
/// a real dump and restore, with a dropped column's hole, partitions,
/// grants and data that holds line breaks, leave the evidence equal; no row
/// content reaches it; and a changed row or a changed grant is seen.
#[test]
#[ignore = "runs only in the cluster `database-check migration` owns"]
fn a_real_restore_keeps_rows_partitions_grants_and_the_schema_digest() {
    let Some(url) = database() else { return };
    let base = url;
    let mut random = [0_u8; 16];
    getrandom::fill(&mut random).unwrap();
    // The guarded rename fixes the runtime database's name: making it must
    // succeed before this test owns it, and may later drop it.
    let names = [
        format!("migration_{}", hexadecimal_text(&random)),
        "simplestchat".to_owned(),
    ];
    sql(
        &base,
        "DO $$ BEGIN
IF NOT EXISTS(SELECT FROM pg_roles WHERE rolname='postgres') THEN
 CREATE ROLE postgres NOLOGIN; END IF;
IF NOT EXISTS(SELECT FROM pg_roles WHERE rolname='simplestchat_app') THEN
 CREATE ROLE simplestchat_app NOLOGIN; END IF;
IF NOT EXISTS(SELECT FROM pg_roles WHERE rolname='simplestchat_migrate') THEN
 CREATE ROLE simplestchat_migrate NOLOGIN; END IF;
END $$;",
    );
    let mut made = Made {
        base: &base,
        names: Vec::new(),
    };
    for name in &names {
        sql(&base, &format!("CREATE DATABASE {name} OWNER postgres;"));
        made.names.push(name.clone());
    }
    let (source, destination) = (beside(&base, &names[0]), beside(&base, &names[1]));
    sql(&source, FIXTURE);
    let original = evidence(&source);
    let rows = |name: &str| {
        let table = original.0.iter();
        let mut table =
            table.filter(|table| table.get("name").and_then(Json::string) == Some(name));
        table.next().unwrap().get("rows").unwrap().integer()
    };
    assert_eq!(rows("partitioned"), Some(0));
    assert_eq!(rows("first_partition"), Some(1));
    assert_eq!(rows("second_partition"), Some(1));
    let whole = format!(
        "{}{}{}",
        spelled(&original.0),
        spelled(&original.1),
        python_dump(&original.2, false)
    );
    assert!(!whole.contains("fixture-password-hash"));
    let scratch = Scratch::new();
    let archive = scratch.path().join("database.dump");
    let archive = archive.to_str().unwrap();
    tool(
        "pg_dump",
        &["--format=custom", "--file", archive, &source],
        b"",
    )
    .unwrap();
    let restoring = [
        "--exit-on-error",
        "--single-transaction",
        "--dbname",
        &destination,
        archive,
    ];
    tool("pg_restore", &restoring, b"").unwrap();
    let restored = evidence(&destination);
    assert_eq!(spelled(&restored.0), spelled(&original.0));
    assert_eq!(spelled(&restored.1), spelled(&original.1));
    if python_dump(&restored.2, false) != python_dump(&original.2, false) {
        // The schema records themselves say where the two differ.
        let described = format!("SET search_path=pg_catalog; {METADATA}");
        let (before, after) = (sql(&source, &described), sql(&destination, &described));
        let differing = before
            .lines()
            .zip(after.lines())
            .find(|(one, other)| one != other);
        panic!("the restored schema differs: {differing:?}");
    }
    rename_changes_only_the_owner(&destination, &original);
    an_enum_change_is_seen(&destination, &original);
    sql(&destination, "UPDATE public.users SET auth_version=10;");
    assert_ne!(spelled(&evidence(&destination).0), spelled(&original.0));
    sql(&destination, "UPDATE public.users SET auth_version=9;");
    sql(
        &destination,
        "GRANT DELETE ON public.users TO simplestchat_app;",
    );
    let changed = evidence(&destination);
    assert_eq!(spelled(&changed.0), spelled(&original.0));
    assert_ne!(
        python_dump(&changed.2, false),
        python_dump(&original.2, false)
    );
    drop(made);
}
