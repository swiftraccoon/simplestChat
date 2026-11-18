//! The analysis of one language: its extraction from the private copy of
//! the commit, fresh or restored by the cache tool and proved against that
//! copy, its traced native build, every suite's report regenerated from the
//! database and held to its claims, the database saved for the next run
//! once its inputs are proved unchanged, and the current reviews enforced
//! on every report by this program's own triage.

use super::{
    CACHE, CACHED, Host, Options, analyze_check, categories,
    command::source_identity,
    create_check, named, policy_check,
    reports::{database_health, extraction_options, report_health, validate_report_scope},
};
use crate::{
    budget::Budget,
    codeql::{
        cargo, coverage, tools,
        triage::{MAX_REPORT, bounded_file, decode, field, object, string, write_private},
    },
    context::{Context, Failure, Outcome, Run},
    document::Document,
    require,
};
use std::{
    ffi::{OsStr, OsString},
    path::Path,
    time::Duration,
};

/// How long a fresh extraction, a traced build or an analysis may take.
const EXTRACTION: Duration = Duration::from_secs(2100);
/// How long a database's import may take, and the policy of one report.
const IMPORT: Duration = Duration::from_secs(600);
/// How long the query of the native coverage may take.
const COVERAGE: Duration = Duration::from_secs(300);
/// How long the vendored sources' integrity may take.
const INTEGRITY: Duration = Duration::from_secs(900);
/// How long the cache tool may take to restore, prove or save a database.
const CACHING: Duration = Duration::from_secs(600);
/// The fields of a cache receipt that the cache tool answers, in the order
/// the reference's receipt gave them.
const RECEIPT: [&str; 6] = [
    "schemaVersion",
    "key",
    "revision",
    "bundleSha256",
    "bundleBytes",
    "reused",
];
/// The caller's variables that the key of an analyzed database binds: the
/// runner's image, local or hosted, and the trust of the run.
const KEYED: [&str; 6] = [
    "ACT",
    "LOCAL_CI_RUNNER_IMAGE",
    "ImageOS",
    "ImageVersion",
    "GITHUB_REF",
    "GITHUB_EVENT_NAME",
];
/// What an answer of the cache tool that is none of its answers is
/// refused as.
const UNANSWERED: Failure = Failure::Fixed("codeql_cache_tool_answer");

/// What a check runs for at most `within`, in the sources' root.
fn within(within: Duration) -> Run<'static> {
    Run {
        within,
        ..Run::default()
    }
}

/// `prefix` followed by `path`, as one argument.
fn prefixed(prefix: &str, path: &Path) -> OsString {
    let mut argument = OsString::from(prefix);
    argument.push(path);
    argument
}

/// Run `argv` as the check `name` of `context` once the host's witness has
/// been told of it.
pub(crate) fn checked(
    context: &mut Context,
    host: &Host<'_>,
    name: &'static str,
    argv: &[&OsStr],
    how: &Run<'_>,
) -> Outcome<(i32, Vec<u8>)> {
    (host.witness)(name, argv, how);
    context.run(name, argv, how)
}

/// The count of compiled files that the query of the native coverage finds
/// in `database`, decoded by `codeql`, once every protocol parser is among
/// them.
pub fn native_coverage(
    context: &mut Context,
    host: &Host<'_>,
    codeql: &Path,
    database: &Path,
) -> Outcome<usize> {
    let output = context.output().to_path_buf();
    let query = context
        .root()
        .join("security/codeql/native-coverage/compilations.ql");
    let (results, decoded) = (
        output.join("native-coverage.bqrs"),
        output.join("native-coverage.json"),
    );
    let (database, written) = (
        prefixed("--database=", database),
        prefixed("--output=", &results),
    );
    let argv = [
        codeql.as_os_str(),
        "query".as_ref(),
        "run".as_ref(),
        query.as_os_str(),
        &database,
        &written,
    ];
    checked(
        context,
        host,
        "codeql-native-coverage-query",
        &argv,
        &within(COVERAGE),
    )?;
    let written = prefixed("--output=", &decoded);
    let argv = [
        codeql.as_os_str(),
        "bqrs".as_ref(),
        "decode".as_ref(),
        "--format=json".as_ref(),
        results.as_os_str(),
        &written,
    ];
    checked(
        context,
        host,
        "codeql-native-coverage-decode",
        &argv,
        &Run::default(),
    )?;
    Ok(coverage::verify(&decoded)?)
}

/// Enforce the current reviews on the report that `identity` names, with
/// this program's `codeql triage sarif`, and mark it passed: a native
/// report with the vendored sources' cache, and every report of a vendor
/// scan as one.
pub fn enforce_policy(
    context: &mut Context,
    host: &Host<'_>,
    options: &Options,
    identity: &mut Document,
) -> Outcome<()> {
    let language = string(field(identity, "language")?)?.to_owned();
    let category = string(field(identity, "category")?)?.to_owned();
    let output = context.output().to_path_buf();
    let policy = output.join(format!("{language}-{category}-policy"));
    let report = output.join(format!("{language}-{category}.sarif"));
    let cache = output.join("vendor-cache");
    let mut argv: Vec<&OsStr> = vec![
        host.program.as_os_str(),
        "codeql".as_ref(),
        "triage".as_ref(),
        "sarif".as_ref(),
        "--revision".as_ref(),
        options.revision.as_ref(),
        "--input".as_ref(),
        report.as_os_str(),
        "--output".as_ref(),
        policy.as_os_str(),
    ];
    if language == "c-cpp" {
        argv.extend(["--source-cache".as_ref(), cache.as_os_str()]);
    }
    if options.include_vendor {
        argv.push("--include-vendor".as_ref());
    }
    let name = policy_check(&language, &category)?;
    checked(context, host, name, &argv, &within(IMPORT))?;
    let verdict = decode(&bounded_file(&policy.join("report.json"), MAX_REPORT)?)?;
    let passed = field(object(&verdict)?, "passed")? == &Document::Bool(true);
    require(passed, "codeql_local_policy_failed")?;
    identity.put("policyPassed", true.into());
    Ok(())
}

/// Extract the native sources at `source` into a new database with
/// `codeql` in three checks timed apart: its initialization, the traced
/// build within `budget` against the static OpenSSL at `openssl_prefix`,
/// and its import.
pub fn create_native(
    context: &mut Context,
    host: &Host<'_>,
    source: &Path,
    codeql: &Path,
    openssl_prefix: &Path,
    budget: &Budget,
) -> Outcome<()> {
    let database = context.output().join("databases").join("c-cpp");
    let root = prefixed("--source-root=", source);
    let threads = format!("--threads={}", budget.workers);
    let memory = format!("--ram={}", budget.ram_mib);
    let argv = [
        codeql.as_os_str(),
        "database".as_ref(),
        "init".as_ref(),
        database.as_os_str(),
        &root,
        "--language=c-cpp".as_ref(),
        "--build-mode=manual".as_ref(),
    ];
    let how = Run {
        directory: Some(source),
        ..Run::default()
    };
    checked(context, host, "codeql-init-c-cpp", &argv, &how)?;
    let argv = [
        codeql.as_os_str(),
        "database".as_ref(),
        "trace-command".as_ref(),
        database.as_os_str(),
        threads.as_ref(),
        memory.as_ref(),
        "--".as_ref(),
        "bash".as_ref(),
        "build/codeql-native-build.sh".as_ref(),
    ];
    let jobs = budget.workers.to_string();
    let variables = [
        ("OPENSSL_DIR", openssl_prefix.as_os_str()),
        ("CODEQL_BUILD_JOBS", jobs.as_ref()),
    ];
    let how = Run {
        directory: Some(source),
        within: EXTRACTION,
        variables: &variables,
        ..Run::default()
    };
    checked(context, host, "codeql-trace-c-cpp", &argv, &how)?;
    let argv = [
        codeql.as_os_str(),
        "database".as_ref(),
        "finalize".as_ref(),
        database.as_os_str(),
        threads.as_ref(),
        memory.as_ref(),
    ];
    let how = Run {
        directory: Some(source),
        within: IMPORT,
        ..Run::default()
    };
    checked(context, host, "codeql-finalize-c-cpp", &argv, &how)?;
    Ok(())
}

/// What the cache tool is asked, in the order of its checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operation {
    /// The key of a database.
    Key,
    /// A database restored.
    Restore,
    /// A database's sources held to the copy of the commit.
    Source,
    /// A database saved.
    Save,
}

/// The check of `operation` of the cache tool for `language`.
fn cache_check(language: &str, operation: Operation) -> Outcome<&'static str> {
    Ok(named(&CACHED, &CACHE, language)?[operation as usize])
}

/// A receipt the cache tool printed: one record of exactly the receipt's
/// fields, given here in the reference's order.
fn receipt(printed: &[u8]) -> Outcome<Document> {
    let answer = Document::read(printed).map_err(|_| UNANSWERED)?;
    require(
        answer.fields().is_some() && answer.names() == RECEIPT.into_iter().collect(),
        "codeql_cache_tool_answer",
    )?;
    let fields = RECEIPT.iter().filter_map(|name| {
        let value = answer.get(name)?;
        Some(((*name).to_owned(), value.clone()))
    });
    Ok(Document::Record(fields.collect()))
}

/// The cache tool `tool` asked as the check of `operation` of `language` with
/// `arguments`, as `how` says.
fn cache_tool(
    context: &mut Context,
    host: &Host<'_>,
    tool: &Path,
    (language, operation): (&str, Operation),
    arguments: &[&OsStr],
    how: &Run<'_>,
) -> Outcome<Vec<u8>> {
    let mut argv = vec![tool.as_os_str(), "codeql-cache".as_ref()];
    argv.extend_from_slice(arguments);
    let name = cache_check(language, operation)?;
    let (_, printed) = checked(context, host, name, &argv, how)?;
    Ok(printed)
}

/// The key of the analyzed database of `language` extracted from `source`,
/// the one line the cache tool `tool` prints for it: only the Rust and the
/// native databases are cached.
fn cache_key(
    context: &mut Context,
    host: &Host<'_>,
    tool: &Path,
    options: &Options,
    source: &Path,
    language: &str,
) -> Outcome<String> {
    require(CACHED.contains(&language), "codeql_cache_scope")?;
    let root = context.root().to_path_buf();
    let mut arguments: Vec<&OsStr> = vec![
        "key".as_ref(),
        "--source-root".as_ref(),
        source.as_os_str(),
        "--language".as_ref(),
        language.as_ref(),
        "--suite".as_ref(),
        options.suite.as_ref(),
        "--openssl-prefix".as_ref(),
        options.openssl_prefix.as_os_str(),
        "--root".as_ref(),
        root.as_os_str(),
    ];
    if options.include_vendor {
        arguments.push("--include-vendor".as_ref());
    }
    // The key binds the runner and the trust of the run, which the reference
    // read from the caller's environment and a check's own does not hold.
    let given: Vec<(&str, OsString)> = KEYED
        .iter()
        .filter_map(|name| Some((*name, (host.variable)(name)?)))
        .collect();
    let variables: Vec<(&str, &OsStr)> = given
        .iter()
        .map(|(name, value)| (*name, value.as_os_str()))
        .collect();
    let how = Run {
        variables: &variables,
        ..Run::default()
    };
    let asked = (language, Operation::Key);
    let printed = cache_tool(context, host, tool, asked, &arguments, &how)?;
    let line = String::from_utf8(printed).map_err(|_| UNANSWERED)?;
    let key = line
        .strip_suffix('\n')
        .filter(|key| !key.is_empty() && !key.contains('\n'));
    key.map(str::to_owned).ok_or(UNANSWERED)
}

/// Where the cache is, what keeps it, and the key of this run's database.
struct Cached<'a> {
    directory: &'a Path,
    tool: &'a Path,
    key: String,
}

/// Analyze `language` from the copy of the commit at `source` with
/// `codeql`: a database restored by the cache tool or extracted anew, held
/// to its claims and, when cached, to the copy; every suite's report
/// regenerated from it and marked validated; the database saved once its
/// inputs are proved unchanged; and every report's policy enforced.
pub fn analyze_language(
    context: &mut Context,
    host: &Host<'_>,
    options: &Options,
    source: &Path,
    codeql: &Path,
    language: &str,
) -> Outcome<Vec<Document>> {
    let jobs =
        (host.variable)("CODEQL_BUILD_JOBS").map(|given| given.into_string().unwrap_or_default());
    let budget = (host.budget)(jobs.as_deref())?;
    let root = context.root().to_path_buf();
    let output = context.output().to_path_buf();
    let database = output.join("databases").join(language);
    let ready = output.join(format!("{language}-cache-ready.json"));
    require(
        !ready.exists() && !ready.is_symlink(),
        "codeql_cache_ready_exists",
    )?;
    let mut extraction: Option<Document> = None;
    if language == "c-cpp" {
        require(host.linux, "codeql_native_requires_linux")?;
        require(
            options.openssl_prefix.is_absolute(),
            "codeql_openssl_absolute",
        )?;
        for name in ["lib/libssl.a", "lib/libcrypto.a"] {
            require(
                options.openssl_prefix.join(name).is_file(),
                "codeql_static_openssl_missing",
            )?;
        }
        let (cache, evidence) = (output.join("vendor-cache"), output.join("vendor-integrity"));
        let argv = [
            host.program.as_os_str(),
            "vendor".as_ref(),
            "verify".as_ref(),
            "--root".as_ref(),
            source.as_os_str(),
            "--cache".as_ref(),
            cache.as_os_str(),
            "--output".as_ref(),
            evidence.as_os_str(),
        ];
        checked(
            context,
            host,
            "codeql-vendor-integrity",
            &argv,
            &within(INTEGRITY),
        )?;
    }
    let mut cached = None;
    if let Some((directory, tool)) = options.cached()? {
        let key = cache_key(context, host, tool, options, source, language)?;
        let arguments = [
            "restore".as_ref(),
            "--directory".as_ref(),
            directory.as_os_str(),
            "--key".as_ref(),
            key.as_ref(),
            "--database".as_ref(),
            database.as_os_str(),
            "--codeql".as_ref(),
            codeql.as_os_str(),
            "--output".as_ref(),
            output.as_os_str(),
        ];
        let printed = cache_tool(
            context,
            host,
            tool,
            (language, Operation::Restore),
            &arguments,
            &within(CACHING),
        )?;
        if !printed.is_empty() {
            extraction = Some(receipt(&printed)?);
        }
        cached = Some(Cached {
            directory,
            tool,
            key,
        });
    }
    if extraction.is_none() && language == "c-cpp" {
        create_native(
            context,
            host,
            source,
            codeql,
            &options.openssl_prefix,
            &budget,
        )?;
    } else if extraction.is_none() {
        let mut argv: Vec<OsString> = vec![
            codeql.into(),
            "database".into(),
            "create".into(),
            database.clone().into(),
            prefixed("--source-root=", source),
            format!("--language={language}").into(),
            "--build-mode=none".into(),
            format!("--threads={}", budget.workers).into(),
            format!("--ram={}", budget.ram_mib).into(),
        ];
        argv.extend(extraction_options(source, language, options.include_vendor));
        if language == "rust"
            && let Some(directory) = &options.rust_cargo_cache
        {
            let builder = cargo::Host {
                variable: host.variable,
                system: host.system,
                machine: host.machine,
                search: host.search,
            };
            let target = cargo::prepare(&root, source, directory, &builder)?;
            argv.push(prefixed("--extractor-option=cargo_target_dir=", &target));
        }
        let jobs = budget.workers.to_string();
        let variables = [
            ("OPENSSL_DIR", options.openssl_prefix.as_os_str()),
            ("CODEQL_BUILD_JOBS", jobs.as_ref()),
        ];
        let how = Run {
            directory: Some(source),
            within: EXTRACTION,
            variables: &variables,
            ..Run::default()
        };
        let argv: Vec<&OsStr> = argv.iter().map(OsString::as_os_str).collect();
        checked(context, host, create_check(language)?, &argv, &how)?;
    }
    database_health(&root, &database, language)?;
    if let Some(cache) = &cached {
        let manifest = output.join("source-manifest.json");
        let arguments = [
            "validate-source".as_ref(),
            "--database".as_ref(),
            database.as_os_str(),
            "--source-root".as_ref(),
            source.as_os_str(),
            "--manifest".as_ref(),
            manifest.as_os_str(),
            "--language".as_ref(),
            language.as_ref(),
        ];
        cache_tool(
            context,
            host,
            cache.tool,
            (language, Operation::Source),
            &arguments,
            &within(CACHING),
        )?;
    }
    let coverage = if language == "c-cpp" {
        Some(native_coverage(context, host, codeql, &database)?)
    } else {
        None
    };
    let evaluation = match &extraction {
        Some(receipt) => string(field(receipt, "revision")?)?.to_owned(),
        None => options.revision.clone(),
    };
    let mut reports = Vec::new();
    for category in categories(&options.suite) {
        let report = output.join(format!("{language}-{category}.sarif"));
        let suite = tools::suite(&root, codeql, language, category)?;
        let mut argv: Vec<OsString> = vec![codeql.into(), "database".into(), "analyze".into()];
        if extraction.is_none() {
            argv.push("--rerun".into());
        }
        argv.extend([
            database.clone().into(),
            suite.into(),
            "--format=sarifv2.1.0".into(),
            prefixed("--output=", &report),
            format!("--threads={}", budget.workers).into(),
            format!("--ram={}", budget.query_ram_mib).into(),
            format!("--sarif-category=/language:{language}/{category}").into(),
            format!(
                "--sarif-run-property=queryReuseEnabled={}",
                extraction.is_some()
            )
            .into(),
            format!("--sarif-run-property=originalEvaluationRevision={evaluation}").into(),
        ]);
        let argv: Vec<&OsStr> = argv.iter().map(OsString::as_os_str).collect();
        let name = analyze_check(language, category)?;
        checked(context, host, name, &argv, &within(EXTRACTION))?;
        let mut identity = report_health(&root, &report, language, category)?;
        validate_report_scope(&report, language, options.include_vendor)?;
        let marker = Document::record([
            ("revision", options.revision.as_str().into()),
            ("sarifSha256", field(&identity, "sarifSha256")?.clone()),
            ("includeVendor", options.include_vendor.into()),
        ]);
        let marked = output.join(format!("{language}-{category}.validated.json"));
        write_private(&marked, format!("{}\n", marker.spaced()).as_bytes())?;
        identity.put("policyPassed", false.into());
        identity.put("nativeCompiledFiles", coverage.into());
        identity.put("threads", budget.workers.into());
        identity.put(
            "ramMiB",
            Document::Number(serde_json::Number::from(budget.query_ram_mib)),
        );
        identity.put("extraction", extraction.clone().unwrap_or(Document::Null));
        identity.put(
            "queryReuse",
            Document::record([
                ("enabled", extraction.is_some().into()),
                ("originalRevision", evaluation.as_str().into()),
            ]),
        );
        reports.push(identity);
    }
    if let Some(cache) = &cached {
        source_identity(context, host, &options.revision)?;
        let again = cache_key(context, host, cache.tool, options, source, language)?;
        require(cache.key == again, "codeql_cache_inputs_changed")?;
        if extraction.is_none() {
            let arguments = [
                "save".as_ref(),
                "--directory".as_ref(),
                cache.directory.as_os_str(),
                "--key".as_ref(),
                cache.key.as_ref(),
                "--revision".as_ref(),
                options.revision.as_ref(),
                "--language".as_ref(),
                language.as_ref(),
                "--codeql".as_ref(),
                codeql.as_os_str(),
                "--output".as_ref(),
                output.as_os_str(),
            ];
            let printed = cache_tool(
                context,
                host,
                cache.tool,
                (language, Operation::Save),
                &arguments,
                &within(CACHING),
            )?;
            let saved = receipt(&printed)?;
            for report in &mut reports {
                report.put("extraction", saved.clone());
            }
            let marker = Document::record([
                ("revision", options.revision.as_str().into()),
                ("key", cache.key.as_str().into()),
            ]);
            write_private(&ready, format!("{}\n", marker.spaced()).as_bytes())?;
        }
    }
    for report in &mut reports {
        enforce_policy(context, host, options, report)?;
    }
    Ok(reports)
}

#[cfg(test)]
#[path = "local_analysis_tests.rs"]
mod tests;
