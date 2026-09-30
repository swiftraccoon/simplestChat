# Maintained static-analysis rules

This pack checks a small set of concrete source patterns with the authenticated
standalone Semgrep engine. It is part of the fast security gate. The engine and
its required native libraries are installed from checksum-pinned archives in
`build/security-tools.lock.json`. The Python Semgrep CLI and its dependency graph
are not installed or invoked. Rules are local JSON; there is no registry fetch,
remote ruleset, auto-update, autofix or executable Python rule predicate.

Run the check after installing the pinned security tools:

```sh
python3 build/security_semgrep.py check \
  --root "$PWD" --output "$PWD/target/semgrep-review"
```

The output directory must not exist. Its parent must already exist. Output inside
the checkout must live under the ignored `target` or `results` directory. A separate installed
tool directory can be selected with `--tools-directory`; its receipt and every
bundled engine/library hash still have to match the maintained tool lock.

## Scan scope and evidence

The shared `security_context.Context` takes a bounded copy of Git-tracked and
nonignored source files. It rejects symbolic links and special files and records
each copied file's SHA-256. This pack selects `.rs`, `.py`, `.ts` and `.js` files
under these maintained production paths:

- `src/`
- `build/`
- `ops/ansible/files/`
- `ops/ansible/callback_plugins/`
- `ops/ansible/filter_plugins/`
- `web/src/`

Rust tests embedded in these modules are included. Standalone Python/browser test
suites and this pack's intentionally unsafe examples are outside the production
scan. Vendor sources, C/C++, shell, workflow YAML, SQL migrations, dependencies and
image contents have separate gates. An unsupported language or a newly added
production directory requires a deliberate scope/rule update; this pack does not
claim to scan it automatically.

Before scanning production sources, the same engine scans every golden fixture.
For both runs the wrapper requires the exact selected target set, all expected
rule IDs, the locked engine version, zero scanner errors, no skipped rules and no
taint fixpoint timeouts. A missing report field is an error. The process runs with
two workers, a five-second per-rule/file timeout, a 2,048 MiB engine memory budget,
an overall 180-second deadline, and independent 16 MiB stream ceilings. The shared
process boundary terminates its owned process group on timeout or output overflow.

The new output directory is private (`0700`). Target inventories, fixture results,
the source manifest and full engine stdout/stderr remain in it. Full scanner
output can contain source excerpts, credential values and local paths; do not
upload those files as public CI artifacts. `report.json` and CLI stdout contain
the rule/tool/source-manifest hashes, assertion/file counts and findings reduced
to a fixed rule ID, canonical relative path and line number. Finding messages,
metavariables, matched strings and raw exception details are never printed.
Scanner execution or golden failures return a fixed failure code. A completed
scan with findings writes a failed report and returns a nonzero exit status.

## Rules and reviewed limits

All rule IDs start with `simplestchat.`. Rule matching is conservative and local;
these checks supplement code review and compiler/tests.

| Rule suffix | Checked behavior | Limits |
| --- | --- | --- |
| `python-credential-url` | Literal URL user/password fields passed to `urllib.request.urlopen` or `requests.get/post` | Does not resolve arbitrary URL builders or every HTTP library. |
| `javascript-credential-url` | Literal credential-bearing URLs passed to `fetch` | Covers JavaScript/TypeScript call syntax, not all client wrappers. |
| `rust-credential-url` | Literal credential-bearing URLs passed to `reqwest::get` | Does not model every client builder. |
| `python-secret-log` | Known credential variable/property names passed to common logger methods | Names are explicit; renamed or transformed secrets require review. |
| `javascript-secret-log` | Known credential variable/property names passed to `console` methods | Does not model every logging abstraction. |
| `rust-secret-log` | Known credential arguments in logging macros and named credential fields inside those macros | Tracing field syntax is additionally matched inside the parsed macro range; it is not a whole-program secret-flow model. |
| `python-network-timeout` | Missing or explicit `None` timeout on supported `urllib`/`requests` calls | Does not prove a variable timeout is positive or cap response sizes. |
| `python-process-timeout` | Missing or explicit `None` timeout on `run`, `check_call` and `check_output` | `Popen` ownership, output limits and descendant cleanup need separate review/tests. |
| `python-request-to-process` | Local data flow from `request.args/form.get` or `request.json[...]` to process execution | Known Python request shapes only; arbitrary aliases, frameworks and cross-function flows are not proved. |
| `python-request-to-file` | The same request shapes reaching supported file-read/open paths | A sanitizer must be explicitly reviewed before the rule is expanded to recognize it. |
| `python-archive-extractall` | Calls to bulk archive extraction | Reviewed bounded readers inspect members without filesystem extraction. |
| `javascript-location-to-html` | Local flow from location search/hash to `innerHTML` or `document.write` | Other input sources and sinks need additional rules and fixtures. |
| `rust-assert-sql-safe` | `sqlx::AssertSqlSafe` outside the two reviewed files | Does not prove query parameterization by itself. |
| `rust-assert-sql-safe-db` | The assertion outside `verify_schema` in exactly `src/db.rs` | Changes within that function still require SQL/data-flow review. |
| `rust-assert-sql-safe-room-settings` | The assertion outside `update_room_settings` in exactly `src/room/settings.rs` | Changes within that function still require SQL/data-flow review. |

The two SQL exceptions are explicit path/function combinations, not directory or
function-name allowlists. The database function builds a schema check from fixed
identifiers; the room-settings function builds fixed column assignments while
binding caller values separately. A function with the same name in another file
is covered by the general rule. No rule proves route authentication, tenant
isolation, permission correctness, native memory safety or complete absence of
credential leakage. Semgrep's parser and internal optimizations also do not
replace compiling the selected source.

## Changing and testing the pack

`rules.json` is the reviewable rule source. Fixtures live in `fixtures/` and end in
`.fixture` so ordinary application linters and test discovery do not execute or
import the intentionally unsafe snippets. The harness stages their exact bytes
under the filename without that suffix. Fixture-relative paths are passed as
repository-root paths, which tests the same SQL path restrictions as production.

Each rule must have at least one `ruleid:` positive annotation and one `ok:`
negative annotation immediately before the relevant statement. Every actual
finding must exactly equal an expected `(rule ID, path, line)` coordinate. An
unexpected match fails even when no `ok:` annotation names it. Unknown IDs,
malformed directives, unattached annotations, omitted positive/negative coverage,
lost positives and false positives all fail. Fixtures use public synthetic values
and `example.invalid`; they make no network requests and are never executed.

Run all wrapper tests, including the installed engine's regression cases:

```sh
SIMPLESTCHAT_SEMGREP_ENGINE_TESTS=1 python3 -m unittest discover \
  -s ops/ansible/tests -p test_security_semgrep.py
```

Ordinary offline unittest discovery omits only the installed-engine regression
class. The `check` command always runs the actual golden engine scan and has no
skip option. Installed-engine regression tests additionally mutate a rule to
lose positives, mutate a negative fixture to trigger a finding, and supply
malformed relevant source to confirm those failures cannot report success.

When updating the engine lock, run the real golden scan and full source check on
supported CI platforms. Review intentional rule changes together with new
positive and negative examples. Keep the scope and limits above accurate instead
of broadening exceptions to make the gate green.

Primary references: [Semgrep rule syntax](https://semgrep.dev/docs/writing-rules/rule-syntax),
[rule testing conventions](https://semgrep.dev/docs/writing-rules/testing-rules),
and the [upstream core interface definitions](https://github.com/semgrep/semgrep-interfaces).
