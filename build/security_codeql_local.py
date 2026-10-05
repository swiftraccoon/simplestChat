"""Run complete pinned CodeQL analysis locally without GitHub upload or ingestion claims."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import sys
import zipfile
from pathlib import Path
from typing import TYPE_CHECKING

import security_codeql
import security_codeql_cache as cache
import security_codeql_cargo as cargo_cache
import security_codeql_resources as resources
import security_codeql_tools as tools
import security_codeql_triage as triage
from security_context import ROOT, Context, executable
from security_source_scope import require_local_vendor
from security_tools import ToolError, bounded_file, require, write_private

# isort: split
from release_json import (
    JsonObject,
    array_value,
    decode_json,
    integer_value,
    object_value,
    string_value,
)

if TYPE_CHECKING:
    from collections.abc import Sequence

RUST_EXTRACTION_STATISTICS = frozenset(
    {
        "Elements extracted",
        "Elements unextracted",
        "Extraction errors",
        "Extraction warnings",
        "Files extracted - total",
        "Files extracted - total user",
        "Files extracted - with errors",
        "Files extracted - without errors",
        "Lines of user code extracted",
        "Macro calls - total",
        "Macro calls - resolved",
        "Macro calls - unresolved",
        "Taint edges - number of edges",
    }
)


def extraction_statistics(run: JsonObject, language: str) -> JsonObject:
    """Retain bounded original Rust coverage counters without source excerpts or cached verdicts."""
    result: JsonObject = {}
    if language != "rust":
        return result
    properties = object_value(run.get("properties", {}))
    for value in array_value(properties.get("metricResults", [])):
        metric = object_value(value)
        if metric.get("ruleId") != "rust/summary/summary-statistics":
            continue
        label = string_value(object_value(metric["message"])["text"])
        if label in RUST_EXTRACTION_STATISTICS:
            count = integer_value(metric["value"])
            require(count >= 0 and label not in result, "codeql_rust_extraction_metric")
            result[label] = count
    require(
        frozenset(result) == RUST_EXTRACTION_STATISTICS, "codeql_rust_extraction_metrics_missing"
    )
    return result


class Options(argparse.Namespace):
    """Explicit committed source, optional installed bundle and fresh output."""

    codeql: Path | None = None
    revision: str = ""
    output: Path = Path()
    language: str | None = None
    suite: str = "security"
    openssl_prefix: Path = ROOT / "target/openssl-4.0.3"
    database_cache: Path | None = None
    rust_cargo_cache: Path | None = None
    include_vendor: bool = False


def selected_languages(args: Options) -> Sequence[str]:
    """Keep optional native/vendor analysis outside every automated invocation."""
    require_local_vendor(include_vendor=args.include_vendor)
    require(args.language != "c-cpp" or args.include_vendor, "codeql_vendor_opt_in_required")
    if args.language:
        return [args.language]
    return tools.LANGUAGES if args.include_vendor else tools.AUTOMATED_LANGUAGES


def source_identity(context: Context, revision: str) -> None:
    """Bind results to the complete clean commit, including unpushed commits."""
    require(re.fullmatch(r"[a-f0-9]{40}", revision), "codeql_revision")
    _, head = context.run("codeql-source-head", [executable("git"), "rev-parse", "HEAD"])
    require(head.decode().strip() == revision, "codeql_source_head")
    _, status = context.run(
        "codeql-source-status",
        [executable("git"), "status", "--porcelain=v1", "--untracked-files=normal"],
    )
    require(not status.strip(), "codeql_source_not_committed")


def snapshot(context: Context) -> Path:
    """Exclude ignored artifacts while preserving committed executable bits."""
    source = context.snapshot()
    manifest = object_value(decode_json((context.output / "source-manifest.json").read_bytes()))
    for name in manifest:
        if (context.root / name).stat().st_mode & 0o111:
            (source / name).chmod(0o700)
    return source


def database_health(database: Path, language: str) -> None:
    """Reject unfinished, overlay, wrong-version or buildless native databases."""
    text = bounded_file(database / "codeql-database.yml", 65536).decode()
    selected = tools.language_pin(language)
    tools.scalar(text, "primaryLanguage", string_value(selected["extractor"]))
    tools.scalar(text, "cliVersion", string_value(tools.pin()["cliVersion"]))
    tools.scalar(text, "finalised", "true")
    tools.scalar(text, "overlayBaseDatabase", "false")
    tools.scalar(text, "overlayDatabase", "false")
    tools.scalar(text, "buildMode", "manual" if language == "c-cpp" else "none")


def report_health(path: Path, language: str, category: str) -> JsonObject:
    """Identify one complete full-scan report before the unchanged policy gate."""
    raw = object_value(decode_json(bounded_file(path, triage.MAX_REPORT)))
    require(raw.get("version") == "2.1.0", "codeql_sarif_version")
    runs = array_value(raw["runs"])
    require(len(runs) == 1, "codeql_local_report_runs")
    run = object_value(runs[0])
    invocations = array_value(run["invocations"])
    require(bool(invocations), "codeql_missing_execution")
    for invocation in invocations:
        details = object_value(invocation)
        require(details.get("executionSuccessful") is True, "codeql_execution_failed")
        for key in ("toolExecutionNotifications", "toolConfigurationNotifications"):
            require(
                all(
                    object_value(notice).get("level") not in {"error", "warning"}
                    for notice in array_value(details.get(key, []))
                ),
                "codeql_incomplete_execution",
            )
    automation = object_value(run["automationDetails"])
    require(
        string_value(automation["id"]).rstrip("/") == f"/language:{language}/{category}",
        "codeql_local_report_category",
    )
    require(
        not object_value(run.get("properties", {})).get("incrementalMode"),
        "codeql_local_incremental_report",
    )
    tool = object_value(run["tool"])
    driver = object_value(tool["driver"])
    require(driver.get("name") == "CodeQL", "codeql_sarif_tool")
    require(
        driver.get("semanticVersion", driver.get("version")) == tools.pin()["cliVersion"],
        "codeql_local_report_version",
    )
    components = [driver, *[object_value(v) for v in array_value(tool.get("extensions", []))]]
    count = sum(len(array_value(v.get("rules", []))) for v in components)
    require(count > 0, "codeql_local_empty_queries")
    return {
        "language": language,
        "category": category,
        "cliVersion": string_value(tools.pin()["cliVersion"]),
        "queriesVersion": string_value(tools.language_pin(language)["queriesVersion"]),
        "rules": count,
        "results": len(array_value(run["results"])),
        "extractionStatistics": extraction_statistics(run, language),
        "sarifSha256": hashlib.sha256(path.read_bytes()).hexdigest(),
    }


def complete_health(reports: Sequence[JsonObject], languages: Sequence[str], suite: str) -> None:
    """Reject missing, repeated or incomplete local language/suite evidence."""
    categories = ("security", "quality-advisory") if suite == "all" else ("security",)
    expected = {(language, category) for language in languages for category in categories}
    observed: set[tuple[str, str]] = set()
    for report in reports:
        identity = (string_value(report["language"]), string_value(report["category"]))
        require(identity in expected and identity not in observed, "codeql_local_report_inventory")
        require(report["policyPassed"] is True, "codeql_local_policy_failed")
        count = report["rules"]
        require(type(count) is int and count > 0, "codeql_local_empty_queries")
        if identity[0] == "c-cpp":
            count = report["nativeCompiledFiles"]
            require(type(count) is int and count > 0, "codeql_local_native_coverage")
        observed.add(identity)
    require(observed == expected, "codeql_local_incomplete_languages")


def upload_ready(output: Path, revision: str, category: str, language: str) -> bool:
    """Expose valid fresh SARIF even when current findings fail the policy gate."""
    require(language in tools.AUTOMATED_LANGUAGES, "codeql_vendor_upload_forbidden")
    report = output / (language + "-" + category + ".sarif")
    marker = report.with_suffix(".validated.json")
    if not marker.exists():
        return False
    expected = object_value(decode_json(bounded_file(marker, 4096)))
    identity = report_health(report, language, category)
    require(
        expected
        == {"revision": revision, "sarifSha256": identity["sarifSha256"], "includeVendor": False},
        "codeql_upload_identity",
    )
    triage.first_party_report(object_value(decode_json(bounded_file(report, triage.MAX_REPORT))))
    return True


def native_coverage(context: Context, codeql: Path, database: Path) -> int:
    """Retain the same real DTLS/STUN/SCTP/RTP compilation requirement as hosted CI."""
    output = context.output
    _ = context.run(
        "codeql-native-coverage-query",
        [
            str(codeql),
            "query",
            "run",
            str(ROOT / "security/codeql/native-coverage/compilations.ql"),
            "--database=" + str(database),
            "--output=" + str(output / "native-coverage.bqrs"),
        ],
        timeout=300,
    )
    _ = context.run(
        "codeql-native-coverage-decode",
        [
            str(codeql),
            "bqrs",
            "decode",
            "--format=json",
            str(output / "native-coverage.bqrs"),
            "--output=" + str(output / "native-coverage.json"),
        ],
    )
    return security_codeql.verify(output / "native-coverage.json")


def enforce_policy(context: Context, args: Options, identity: JsonObject) -> None:
    """Apply current exact reviews to every fresh or regenerated language report."""
    language = string_value(identity["language"])
    category = string_value(identity["category"])
    policy = context.output / (language + "-" + category + "-policy")
    command = [
        sys.executable,
        str(ROOT / "build/security_codeql_triage.py"),
        "sarif",
        "--revision",
        args.revision,
        "--input",
        str(context.output / (language + "-" + category + ".sarif")),
        "--output",
        str(policy),
    ]
    if language == "c-cpp":
        command.extend(["--source-cache", str(context.output / "vendor-cache")])
    if args.include_vendor:
        command.append("--include-vendor")
    _ = context.run("codeql-policy-" + language + "-" + category, command, timeout=600)
    verdict = object_value(decode_json(bounded_file(policy / "report.json", triage.MAX_REPORT)))
    require(verdict["passed"] is True, "codeql_local_policy_failed")
    identity["policyPassed"] = True


def create_native(
    context: Context,
    source: Path,
    codeql: Path,
    openssl_prefix: Path,
    budget: resources.Budget,
) -> None:
    """Record bounded traced compilation and database import as distinct timed checks."""
    database = context.output / "databases" / "c-cpp"
    _ = context.run(
        "codeql-init-c-cpp",
        [
            str(codeql),
            "database",
            "init",
            str(database),
            "--source-root=" + str(source),
            "--language=c-cpp",
            "--build-mode=manual",
        ],
        cwd=source,
    )
    _ = context.run(
        "codeql-trace-c-cpp",
        [
            str(codeql),
            "database",
            "trace-command",
            str(database),
            "--threads=" + str(budget.workers),
            "--ram=" + str(budget.ram_mib),
            "--",
            "bash",
            "build/codeql-native-build.sh",
        ],
        cwd=source,
        timeout=2100,
        env_updates={
            "OPENSSL_DIR": str(openssl_prefix),
            "CODEQL_BUILD_JOBS": str(budget.workers),
        },
    )
    _ = context.run(
        "codeql-finalize-c-cpp",
        [
            str(codeql),
            "database",
            "finalize",
            str(database),
            "--threads=" + str(budget.workers),
            "--ram=" + str(budget.ram_mib),
        ],
        cwd=source,
        timeout=600,
    )


def extraction_options(source: Path, language: str, *, include_vendor: bool) -> list[str]:
    """Exclude vendor during extraction while permitting Cargo to compile dependencies."""
    if include_vendor:
        return []
    options = ["--codescanning-config=" + str(source / tools.FIRST_PARTY_CONFIG)]
    if language == "rust":
        options.append("--extractor-option=extract_dependencies_as_source=false")
    return options


def validate_report_scope(report: Path, language: str, *, include_vendor: bool) -> None:
    """Require reports to match their declared source scope before cache publication."""
    require(include_vendor or language in tools.AUTOMATED_LANGUAGES, "codeql_vendor_scope")
    if not include_vendor:
        triage.first_party_report(
            object_value(decode_json(bounded_file(report, triage.MAX_REPORT)))
        )


def analyze_language(  # noqa: C901 -- Keep extraction, analysis and policy in their required order.
    context: Context, args: Options, source: Path, codeql: Path, language: str
) -> list[JsonObject]:
    """Analyze exact databases, regenerate full SARIF, and enforce current finding policy."""
    budget = resources.detect(os.environ.get("CODEQL_BUILD_JOBS"))
    database = context.output / "databases" / language
    ready = context.output / (language + "-cache-ready.json")
    require(not ready.exists() and not ready.is_symlink(), "codeql_cache_ready_exists")
    extraction: JsonObject | None = None
    cache_key: str | None = None
    if language == "c-cpp":
        require(sys.platform == "linux", "codeql_native_requires_linux")
        require(args.openssl_prefix.is_absolute(), "codeql_openssl_absolute")
        for name in ("lib/libssl.a", "lib/libcrypto.a"):
            require((args.openssl_prefix / name).is_file(), "codeql_static_openssl_missing")
        _ = context.run(
            "codeql-vendor-integrity",
            [
                sys.executable,
                str(ROOT / "build/security_vendor.py"),
                "verify",
                "--root",
                str(source),
                "--cache",
                str(context.output / "vendor-cache"),
                "--output",
                str(context.output / "vendor-integrity"),
            ],
            timeout=900,
        )
    if args.database_cache is not None:
        cache_key = cache.cache_key(
            context.root,
            source,
            args.openssl_prefix,
            language,
            args.suite,
            include_vendor=args.include_vendor,
        )
        extraction = cache.restore(context, codeql, args.database_cache, cache_key, database)
    if extraction is None and language == "c-cpp":
        create_native(context, source, codeql, args.openssl_prefix, budget)
    elif extraction is None:
        create = [
            str(codeql),
            "database",
            "create",
            str(database),
            "--source-root=" + str(source),
            "--language=" + language,
            "--build-mode=none",
            "--threads=" + str(budget.workers),
            "--ram=" + str(budget.ram_mib),
        ]
        create.extend(extraction_options(source, language, include_vendor=args.include_vendor))
        if language == "rust" and args.rust_cargo_cache is not None:
            target = cargo_cache.prepare(context.root, source, args.rust_cargo_cache)
            create.append("--extractor-option=cargo_target_dir=" + str(target))
        _ = context.run(
            "codeql-create-" + language,
            create,
            cwd=source,
            timeout=2100,
            env_updates={
                "OPENSSL_DIR": str(args.openssl_prefix),
                "CODEQL_BUILD_JOBS": str(budget.workers),
            },
        )
    database_health(database, language)
    if cache_key is not None:
        manifest = object_value(decode_json((context.output / "source-manifest.json").read_bytes()))
        cache.source_integrity(database, source, manifest, language)
    coverage = native_coverage(context, codeql, database) if language == "c-cpp" else None
    evaluation_revision = string_value(extraction["revision"]) if extraction else args.revision
    reports: list[JsonObject] = []
    categories = ("security", "quality-advisory") if args.suite == "all" else ("security",)
    for category in categories:
        report = context.output / (language + "-" + category + ".sarif")
        _ = context.run(
            "codeql-analyze-" + language + "-" + category,
            [
                str(codeql),
                "database",
                "analyze",
                *([] if extraction is not None else ["--rerun"]),
                str(database),
                str(tools.suite(codeql, language, category)),
                "--format=sarifv2.1.0",
                "--output=" + str(report),
                "--threads=" + str(budget.workers),
                "--ram=" + str(budget.query_ram_mib),
                "--sarif-category=/language:" + language + "/" + category,
                "--sarif-run-property=queryReuseEnabled=" + str(extraction is not None).lower(),
                "--sarif-run-property=originalEvaluationRevision=" + evaluation_revision,
            ],
            timeout=2100,
        )
        identity = report_health(report, language, category)
        validate_report_scope(report, language, include_vendor=args.include_vendor)
        write_private(
            report.with_suffix(".validated.json"),
            (
                json.dumps(
                    {
                        "revision": args.revision,
                        "sarifSha256": identity["sarifSha256"],
                        "includeVendor": args.include_vendor,
                    }
                )
                + "\n"
            ).encode(),
            0o600,
        )
        reports.append(
            {
                **identity,
                "policyPassed": False,
                "nativeCompiledFiles": coverage,
                "threads": budget.workers,
                "ramMiB": budget.query_ram_mib,
                "extraction": extraction,
                "queryReuse": {
                    "enabled": extraction is not None,
                    "originalRevision": evaluation_revision,
                },
            }
        )
    if cache_key is not None:
        source_identity(context, args.revision)
        require(
            cache_key
            == cache.cache_key(
                context.root,
                source,
                args.openssl_prefix,
                language,
                args.suite,
                include_vendor=args.include_vendor,
            ),
            "codeql_cache_inputs_changed",
        )
        if extraction is None and args.database_cache is not None:
            extraction = cache.save(
                context, codeql, args.database_cache, cache_key, args.revision, language=language
            )
            for report in reports:
                report["extraction"] = extraction
            write_private(
                ready,
                (json.dumps({"revision": args.revision, "key": cache_key}) + "\n").encode(),
                0o600,
            )
    for report in reports:
        enforce_policy(context, args, report)
    return reports


def execute(context: Context, args: Options) -> list[JsonObject]:
    """Produce complete current reports with fresh or exactly verified evaluated databases."""
    languages = selected_languages(args)
    source_identity(context, args.revision)
    source = snapshot(context)
    (context.output / "databases").mkdir(mode=0o700)
    codeql = args.codeql or tools.install(context)
    tools.verify(context, codeql, languages)
    reports: list[JsonObject] = []
    for language in languages:
        reports.extend(analyze_language(context, args, source, codeql, language))
    source_identity(context, args.revision)
    _ = context.snapshot("final-source")
    require(
        (context.output / "source-manifest.json").read_bytes()
        == (context.output / "final-source-manifest.json").read_bytes(),
        "codeql_source_changed",
    )
    complete_health(reports, languages, args.suite)
    return reports


def main() -> int:
    """Return failure for any missing result and retain bounded private evidence."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("--codeql", type=Path)
    _ = parser.add_argument("--revision", required=True)
    _ = parser.add_argument("--output", type=Path, required=True)
    _ = parser.add_argument("--language", choices=tools.LANGUAGES)
    _ = parser.add_argument("--suite", choices=("security", "all"), default="security")
    _ = parser.add_argument("--openssl-prefix", type=Path, default=ROOT / "target/openssl-4.0.3")
    _ = parser.add_argument("--database-cache", type=Path)
    _ = parser.add_argument("--rust-cargo-cache", type=Path)
    _ = parser.add_argument(
        "--include-vendor",
        action="store_true",
        help="Opt into local vendor/native analysis; unavailable in CI.",
    )
    args = parser.parse_args(namespace=Options())
    _ = os.umask(0o077)
    require(args.output.is_absolute(), "codeql_output_absolute")
    args.output.mkdir(mode=0o700)
    context = Context(ROOT, args.output)
    reports: list[JsonObject] = []
    passed = False
    failure: str | None = None
    try:
        reports = execute(context, args)
        passed = True
    except (ToolError, OSError, ValueError, KeyError, RuntimeError, zipfile.BadZipFile) as error:
        failure = str(error) if isinstance(error, ToolError) else type(error).__name__
    write_private(
        args.output / "summary.json",
        (
            json.dumps(
                {
                    "schemaVersion": 1,
                    "passed": passed,
                    "failure": failure,
                    "revision": args.revision,
                    "fullScan": True,
                    "includeVendor": args.include_vendor,
                    "excludedPaths": [] if args.include_vendor else ["vendor/**"],
                    "suite": args.suite,
                    "toolchainSha256": hashlib.sha256(tools.PIN.read_bytes()).hexdigest(),
                    "sourceManifestSha256": (
                        hashlib.sha256(
                            (args.output / "source-manifest.json").read_bytes()
                        ).hexdigest()
                        if (args.output / "source-manifest.json").is_file()
                        else None
                    ),
                    "languages": [args.language]
                    if args.language
                    else list(
                        tools.LANGUAGES if args.include_vendor else tools.AUTOMATED_LANGUAGES
                    ),
                    "reports": reports,
                    "checks": context.checks,
                },
                indent=2,
            )
            + "\n"
        ).encode(),
        0o600,
    )
    status = "passed" if passed else "failed"
    _ = sys.stdout.write(f"Local CodeQL: {status}; evidence: {args.output}\n")
    return 0 if passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
