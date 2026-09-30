# Exact CodeQL review and enforcement

This review covers the first 69 GitHub CodeQL alerts observed for revision
`407ec7e96ee9e57c593ac367142e1016e4dbeac5`, run `36709013559`, using CodeQL
`2.27.1`. Each alert was read with its surrounding source and relevant callers.
The machine-readable [review](codeql-review-2026-09-30.json) retains the precise
query, range, rendered-message digest and complete primary-file digest.
[Exceptions](exceptions.json) provide the owner, rationale, reachability and
2026-11-29 expiry for each reviewed false positive. These are individual reviews,
not exclusions for test files, upstream code or entire queries.

The initial disposition is 64 false positives, four findings requiring source
fixes, and one Medium upstream finding left open. Native C/C++ alerts
70–101 have a [separate exact-source review](codeql-native-review-2026-09-30.md); this document does not assert a clean repository
or a complete passing analysis. No alert was dismissed while preparing this
review.

## Decisions

The JavaScript race findings in private test directories exercise permission and
exclusive-creation assertions before reading synthetic data. Alert 1 calls a
stored predicate named `match`; it does not build or execute a regular expression.
The benchmark HTTP-to-file findings write owned loopback metrics into fixed
filenames, with private directories and exclusive creation. Remote data never
selects those output paths or becomes executable content.

The Python credential findings use conspicuous fake constants inside disposable
fixtures. The Jinja findings render SQL, shell, environment and YAML configuration
or evaluate local test guard expressions; HTML auto-escaping would be the wrong
encoding for those sinks. Alert 32's two public outputs are a fixture CA
certificate and an intentionally sanitized report. The certificate private key
and diagnostic captures use separate private files. Neither public output
contains a deployable credential.

All 37 initial Rust findings are inside `cfg(test)` modules. The hardcoded values
are validation boundaries, blocklist inputs, test-only account or room data, and
public known-answer vectors. Other findings concern numeric metrics or assertion
diagnostics from a test-owned loopback server. Production secret-bearing paths
remain scanned; this review does not exclude Rust tests as a category.

Alert 8 remains open: the upstream clang developer helper interpolates command
arguments into a shell command. Its developer-only role lowers the exposed
surface but does not make the reported operation a false positive. Alert 5's
bundle-reader file race and alerts 24–26's unspecified TLS minimum require source
changes and fresh analysis; no exception authorizes dismissing them.

## Maintained command contract

Run commands from the repository root with the documented Python environment.
Every output argument selects a **new directory**: the helper refuses an existing
directory, creates it with mode 0700, and writes mode-0600 evidence. Replace the
revision and paths below with exact reviewed values.

```sh
python3 build/security_codeql_triage.py sarif \
  --revision "$REVISION" --input "$ORIGINAL_CODEQL_SARIF" \
  --output "$NEW_SARIF_EVIDENCE_DIRECTORY"

python3 build/security_codeql_triage.py plan \
  --revision "$REVISION" --ref refs/heads/main \
  --output "$NEW_PLAN_DIRECTORY"

python3 build/security_codeql_triage.py check \
  --revision "$REVISION" --ref refs/heads/main \
  --output "$NEW_CHECK_DIRECTORY"
```

`sarif` is the token-free CI gate, including on fork pull requests. Supply the
original analyzer report, with successful invocation metadata and query rules.
The GitHub API's reconstructed SARIF omits that execution metadata and is not a
substitute. Suppressed results, incomplete execution, unknown security scores,
unknown locations and malformed data fail validation. A successful report with
no findings is accepted only when it still records the analyzer's successful
execution and nonempty query metadata. Each matrix job must run this gate;
aggregation must require every expected language job.

`plan` and `check` are read-only GitHub operations. They require current successful
analyses for all five security categories: Actions, JavaScript/TypeScript, Python,
Rust and C/C++. The live Git reference must equal the requested revision before
and after planning; historical revisions cannot be checked through current alert
state. The newest analysis for every category must match that revision. Once
those five categories are found, older analysis history is not required. All
alert-state pages are still read completely. An active finding manually
dismissed in GitHub is still assessed against repository policy; remote dismissal
is not an exclusion. Missing or stale analysis cannot produce a passing report.
`plan` exits successfully after producing a valid plan even if its `passed` field
is false, so a reviewer can inspect blocked findings. `check` and `sarif` exit
nonzero for unreviewed High/Critical findings. Medium, Low and nonsecurity results
remain in the report. Invalid or incomplete evidence always exits nonzero.

The helper limits each operation to 300 seconds, 20 pages of 100 entries, 2,000
SARIF findings, 64 MiB per input report and 4 MiB per source/API response. Each API
subprocess has a maximum 20-second deadline. Source identity requires the complete
tracked file to match the selected Git revision; changed files, symlinks and path
escapes fail. The fingerprint covers that file digest plus query, analyzer
version, range and rendered-message digest. It is not a proof that dependencies
or callers elsewhere never change: source review remains necessary when their
contracts change. Generated upstream source uses `--source-cache "$VERIFIED_VENDOR_CACHE"` and the
current `vendor/integrity.json`. The vendor gate must populate that private cache
before checking native reports or a complete API plan. This mode is offline: it
checks archive SHA256 before the bounded archive reader parses any member, maps
only declared Meson source roots, and verifies remote patch archives or tracked
packagefiles overlays against the maintained vendor delta. When an extracted
source exists, its bytes must match. The fingerprint additionally binds archive,
manifest, wrap and applicable overlay identities. Missing archives, unsupported
source and unexpected local changes fail rather than being skipped.

Raw SARIF stays private on the runner. CI should upload only the explicit policy
`report.json` or `failure.json`, which contain no source text or raw diagnostic
messages. Fixed validation codes identify rejected report shapes without exposing
report content.

## Explicitly authorized dismissal

Review the exact plan under the repository maintenance authorization before running:

```sh
python3 build/security_codeql_triage.py apply \
  --revision "$REVISION" --ref refs/heads/main \
  --input "$REVIEWED_PLAN_DIRECTORY/report.json" \
  --output "$NEW_APPLY_DIRECTORY" --authorize-dismissals
```

The apply operation requires repository, revision, reference, complete plan,
current policy and explicit-review digests, and each selected alert to remain unchanged. It re-fetches
the full plan and then each alert immediately before its PATCH. Only an open
alert with an exact unexpired exception **and** an exact false-positive review
record is eligible; risk acceptance alone cannot trigger dismissal. At most 128
alerts may be dismissed in one operation. No automatic retries or broad dismissal
endpoint is used. The API target is fixed to GitHub.com and the selected validated
owner/repository; credentials are never written into evidence.

Each action is journaled before the request and after confirmation. If an API
request fails ambiguously, a `dismissal_requested` record may exist without a
confirmation. Retain the private evidence, inspect the actual alert state, and
make a fresh plan; do not assume the action failed or replay the old plan blindly.
`failure.json` never claims an unfinished apply completed. The helper implements
the documented [GitHub code-scanning REST API](https://docs.github.com/en/rest/code-scanning/code-scanning?apiVersion=2022-11-28).

## Per-alert review index

Alerts 36 and 37 were re-reviewed from the actual `c107ca9dfb9df66701d2a8a1c54fe785ed39add8`
analysis after the test module gained its separate secret-canary submodule. Their
old fingerprints were replaced rather than retained as alternate exceptions.
Other linked source paths and lines describe the original revision above. Exact
fingerprints and full rationale are retained in the JSON record. A subsequent
source or location change must receive another explicit review; it does not
inherit this decision merely because the alert number is unchanged.

| Alert | Severity | Disposition | Reviewed source | Rationale |
| --- | --- | --- | --- | --- |
| <a id="alert-1"></a>[1](https://github.com/swiftraccoon/simplestChat/security/code-scanning/1) | high | false-positive | [`web/e2e/ui-stress.cjs:119`](../web/e2e/ui-stress.cjs#L119) | GuestPeer.request stores a caller-supplied predicate in waiter.match; message handling calls that function. Reviewed join/chat callsites pass fixed equality/type predicates. This is not String.match or RegExp construction. |
| <a id="alert-2"></a>[2](https://github.com/swiftraccoon/simplestChat/security/code-scanning/2) | high | false-positive | [`build/tests/canary-correlation.test.mjs:306`](../build/tests/canary-correlation.test.mjs#L306) | Read/assert operations in a private mkdtemp test fixture deliberately check 0600 permissions and then read synthetic report bytes, or re-read after asserting exclusive creation refused overwrite. This does not authorize a production file operation from a path check. |
| <a id="alert-3"></a>[3](https://github.com/swiftraccoon/simplestChat/security/code-scanning/3) | high | false-positive | [`build/tests/canary-correlation.test.mjs:311`](../build/tests/canary-correlation.test.mjs#L311) | Read/assert operations in a private mkdtemp test fixture deliberately check 0600 permissions and then read synthetic report bytes, or re-read after asserting exclusive creation refused overwrite. This does not authorize a production file operation from a path check. |
| <a id="alert-4"></a>[4](https://github.com/swiftraccoon/simplestChat/security/code-scanning/4) | high | false-positive | [`build/tests/test-server.test.mjs:62`](../build/tests/test-server.test.mjs#L62) | Read/assert operations in a private mkdtemp test fixture deliberately check 0600 permissions and then read synthetic report bytes, or re-read after asserting exclusive creation refused overwrite. This does not authorize a production file operation from a path check. |
| <a id="alert-5"></a>[5](https://github.com/swiftraccoon/simplestChat/security/code-scanning/5) | high | fix-pending | [`web/scripts/check-bundle.mjs:110`](../web/scripts/check-bundle.mjs#L110) | Assigned source remediation; this review authorizes no dismissal or exception. |
| <a id="alert-6"></a>[6](https://github.com/swiftraccoon/simplestChat/security/code-scanning/6) | high | false-positive | [`web/tests/performance-report.test.mjs:25`](../web/tests/performance-report.test.mjs#L25) | Read/assert operations in a private mkdtemp test fixture deliberately check 0600 permissions and then read synthetic report bytes, or re-read after asserting exclusive creation refused overwrite. This does not authorize a production file operation from a path check. |
| <a id="alert-7"></a>[7](https://github.com/swiftraccoon/simplestChat/security/code-scanning/7) | high | false-positive | [`web/tests/performance-report.test.mjs:57`](../web/tests/performance-report.test.mjs#L57) | Read/assert operations in a private mkdtemp test fixture deliberately check 0600 permissions and then read synthetic report bytes, or re-read after asserting exclusive creation refused overwrite. This does not authorize a production file operation from a path check. |
| <a id="alert-8"></a>[8](https://github.com/swiftraccoon/simplestChat/security/code-scanning/8) | medium | open-medium | [`vendor/mediasoup-sys-0.17.0/scripts/clang-scripts.mjs:367`](../vendor/mediasoup-sys-0.17.0/scripts/clang-scripts.mjs#L367) | The vendored developer clang helper interpolates local environment, tool and file inputs into execSync shell commands. It is outside the runtime path, but the interpolation is real. Retain the Medium finding instead of calling it a false positive. |
| <a id="alert-9"></a>[9](https://github.com/swiftraccoon/simplestChat/security/code-scanning/9) | medium | false-positive | [`load_tests/benchmark-podman.mjs:26`](../load_tests/benchmark-podman.mjs#L26) | The response is metrics/diagnostic data from the harness-owned loopback server. It is serialized as text/JSON in a newly created private evidence directory, using fixed filenames and exclusive writes. HTTP response data does not choose a pathname and is never executed or loaded as code. |
| <a id="alert-10"></a>[10](https://github.com/swiftraccoon/simplestChat/security/code-scanning/10) | medium | false-positive | [`load_tests/benchmark-local.mjs:23`](../load_tests/benchmark-local.mjs#L23) | The response is metrics/diagnostic data from the harness-owned loopback server. It is serialized as text/JSON in a newly created private evidence directory, using fixed filenames and exclusive writes. HTTP response data does not choose a pathname and is never executed or loaded as code. |
| <a id="alert-11"></a>[11](https://github.com/swiftraccoon/simplestChat/security/code-scanning/11) | medium | false-positive | [`load_tests/benchmark-podman.mjs:375`](../load_tests/benchmark-podman.mjs#L375) | The response is metrics/diagnostic data from the harness-owned loopback server. It is serialized as text/JSON in a newly created private evidence directory, using fixed filenames and exclusive writes. HTTP response data does not choose a pathname and is never executed or loaded as code. |
| <a id="alert-12"></a>[12](https://github.com/swiftraccoon/simplestChat/security/code-scanning/12) | medium | false-positive | [`load_tests/benchmark-podman.mjs:425`](../load_tests/benchmark-podman.mjs#L425) | The response is metrics/diagnostic data from the harness-owned loopback server. It is serialized as text/JSON in a newly created private evidence directory, using fixed filenames and exclusive writes. HTTP response data does not choose a pathname and is never executed or loaded as code. |
| <a id="alert-13"></a>[13](https://github.com/swiftraccoon/simplestChat/security/code-scanning/13) | medium | false-positive | [`load_tests/benchmark-podman.mjs:448`](../load_tests/benchmark-podman.mjs#L448) | The response is metrics/diagnostic data from the harness-owned loopback server. It is serialized as text/JSON in a newly created private evidence directory, using fixed filenames and exclusive writes. HTTP response data does not choose a pathname and is never executed or loaded as code. |
| <a id="alert-14"></a>[14](https://github.com/swiftraccoon/simplestChat/security/code-scanning/14) | medium | false-positive | [`load_tests/benchmark-podman.mjs:457`](../load_tests/benchmark-podman.mjs#L457) | The response is metrics/diagnostic data from the harness-owned loopback server. It is serialized as text/JSON in a newly created private evidence directory, using fixed filenames and exclusive writes. HTTP response data does not choose a pathname and is never executed or loaded as code. |
| <a id="alert-15"></a>[15](https://github.com/swiftraccoon/simplestChat/security/code-scanning/15) | medium | false-positive | [`load_tests/benchmark-local.mjs:528`](../load_tests/benchmark-local.mjs#L528) | The response is metrics/diagnostic data from the harness-owned loopback server. It is serialized as text/JSON in a newly created private evidence directory, using fixed filenames and exclusive writes. HTTP response data does not choose a pathname and is never executed or loaded as code. |
| <a id="alert-16"></a>[16](https://github.com/swiftraccoon/simplestChat/security/code-scanning/16) | medium | false-positive | [`load_tests/benchmark-local.mjs:587`](../load_tests/benchmark-local.mjs#L587) | The response is metrics/diagnostic data from the harness-owned loopback server. It is serialized as text/JSON in a newly created private evidence directory, using fixed filenames and exclusive writes. HTTP response data does not choose a pathname and is never executed or loaded as code. |
| <a id="alert-17"></a>[17](https://github.com/swiftraccoon/simplestChat/security/code-scanning/17) | medium | false-positive | [`load_tests/benchmark-local.mjs:599`](../load_tests/benchmark-local.mjs#L599) | The response is metrics/diagnostic data from the harness-owned loopback server. It is serialized as text/JSON in a newly created private evidence directory, using fixed filenames and exclusive writes. HTTP response data does not choose a pathname and is never executed or loaded as code. |
| <a id="alert-18"></a>[18](https://github.com/swiftraccoon/simplestChat/security/code-scanning/18) | medium | false-positive | [`load_tests/benchmark-local.mjs:635`](../load_tests/benchmark-local.mjs#L635) | The response is metrics/diagnostic data from the harness-owned loopback server. It is serialized as text/JSON in a newly created private evidence directory, using fixed filenames and exclusive writes. HTTP response data does not choose a pathname and is never executed or loaded as code. |
| <a id="alert-19"></a>[19](https://github.com/swiftraccoon/simplestChat/security/code-scanning/19) | high | false-positive | [`ops/ansible/tests/test_turn_public.py:112`](../ops/ansible/tests/test_turn_public.py#L112) | The test writes public repeated-a/repeated-c synthetic TURN values inside TemporaryDirectory to exercise configuration-drift rejection. No live credential, production account or remote relay is used; cleanup removes the fixture. |
| <a id="alert-20"></a>[20](https://github.com/swiftraccoon/simplestChat/security/code-scanning/20) | high | false-positive | [`ops/ansible/tests/test_turn_public.py:119`](../ops/ansible/tests/test_turn_public.py#L119) | The test writes public repeated-a/repeated-c synthetic TURN values inside TemporaryDirectory to exercise configuration-drift rejection. No live credential, production account or remote relay is used; cleanup removes the fixture. |
| <a id="alert-21"></a>[21](https://github.com/swiftraccoon/simplestChat/security/code-scanning/21) | high | false-positive | [`ops/ansible/tests/test_turn_public.py:123`](../ops/ansible/tests/test_turn_public.py#L123) | The test writes public repeated-a/repeated-c synthetic TURN values inside TemporaryDirectory to exercise configuration-drift rejection. No live credential, production account or remote relay is used; cleanup removes the fixture. |
| <a id="alert-22"></a>[22](https://github.com/swiftraccoon/simplestChat/security/code-scanning/22) | high | false-positive | [`ops/ansible/tests/test_turn_public.py:126`](../ops/ansible/tests/test_turn_public.py#L126) | The test writes public repeated-a/repeated-c synthetic TURN values inside TemporaryDirectory to exercise configuration-drift rejection. No live credential, production account or remote relay is used; cleanup removes the fixture. |
| <a id="alert-23"></a>[23](https://github.com/swiftraccoon/simplestChat/security/code-scanning/23) | high | false-positive | [`ops/ansible/tests/test_turn_public.py:140`](../ops/ansible/tests/test_turn_public.py#L140) | The test writes public repeated-a/repeated-c synthetic TURN values inside TemporaryDirectory to exercise configuration-drift rejection. No live credential, production account or remote relay is used; cleanup removes the fixture. |
| <a id="alert-24"></a>[24](https://github.com/swiftraccoon/simplestChat/security/code-scanning/24) | high | fix-pending | [`ops/ansible/files/monitoring_collect.py:156`](../ops/ansible/files/monitoring_collect.py#L156) | Assigned source remediation; this review authorizes no dismissal or exception. |
| <a id="alert-25"></a>[25](https://github.com/swiftraccoon/simplestChat/security/code-scanning/25) | high | fix-pending | [`build/release_http_policy.py:273`](../build/release_http_policy.py#L273) | Assigned source remediation; this review authorizes no dismissal or exception. |
| <a id="alert-26"></a>[26](https://github.com/swiftraccoon/simplestChat/security/code-scanning/26) | high | fix-pending | [`ops/ansible/files/turn_public.py:298`](../ops/ansible/files/turn_public.py#L298) | Assigned source remediation; this review authorizes no dismissal or exception. |
| <a id="alert-27"></a>[27](https://github.com/swiftraccoon/simplestChat/security/code-scanning/27) | high | false-positive | [`build/release_container_fixture.py:163`](../build/release_container_fixture.py#L163) | The Jinja Environment renders local shell/SQL/dotenv/Compose/YAML configuration or evaluates Ansible conditions with StrictUndefined and fixture values. It produces no HTML or browser response. HTML autoescaping would corrupt these formats; there is no XSS sink in this path. |
| <a id="alert-28"></a>[28](https://github.com/swiftraccoon/simplestChat/security/code-scanning/28) | high | false-positive | [`ops/ansible/tests/test_automation.py:39`](../ops/ansible/tests/test_automation.py#L39) | The Jinja Environment renders local shell/SQL/dotenv/Compose/YAML configuration or evaluates Ansible conditions with StrictUndefined and fixture values. It produces no HTML or browser response. HTML autoescaping would corrupt these formats; there is no XSS sink in this path. |
| <a id="alert-29"></a>[29](https://github.com/swiftraccoon/simplestChat/security/code-scanning/29) | high | false-positive | [`ops/ansible/tests/test_automation.py:395`](../ops/ansible/tests/test_automation.py#L395) | The Jinja Environment renders local shell/SQL/dotenv/Compose/YAML configuration or evaluates Ansible conditions with StrictUndefined and fixture values. It produces no HTML or browser response. HTML autoescaping would corrupt these formats; there is no XSS sink in this path. |
| <a id="alert-30"></a>[30](https://github.com/swiftraccoon/simplestChat/security/code-scanning/30) | high | false-positive | [`ops/ansible/tests/test_public_templates.py:82`](../ops/ansible/tests/test_public_templates.py#L82) | The Jinja Environment renders local shell/SQL/dotenv/Compose/YAML configuration or evaluates Ansible conditions with StrictUndefined and fixture values. It produces no HTML or browser response. HTML autoescaping would corrupt these formats; there is no XSS sink in this path. |
| <a id="alert-31"></a>[31](https://github.com/swiftraccoon/simplestChat/security/code-scanning/31) | high | false-positive | [`ops/ansible/tests/test_release_playbook.py:76`](../ops/ansible/tests/test_release_playbook.py#L76) | The Jinja Environment renders local shell/SQL/dotenv/Compose/YAML configuration or evaluates Ansible conditions with StrictUndefined and fixture values. It produces no HTML or browser response. HTML autoescaping would corrupt these formats; there is no XSS sink in this path. |
| <a id="alert-32"></a>[32](https://github.com/swiftraccoon/simplestChat/security/code-scanning/32) | high | false-positive | [`build/release_container_harness.py:136`](../build/release_container_harness.py#L136) | write_new defaults to mode0600 with O_CREAT\|O_EXCL. Its only mode0644 callsites publish a public one-day fixture CA certificate and sanitized public CI summary. Private keys, command output and configuration remain0600 beneath private evidence; the summary projects status and identities, not resolved secrets. |
| <a id="alert-33"></a>[33](https://github.com/swiftraccoon/simplestChat/security/code-scanning/33) | high | false-positive | [`src/metrics.rs:1153`](../src/metrics.rs#L1153) | The sink is a cfg(test) assertion diagnostic for a locally constructed metrics snapshot. The values are numeric counters/gauges; a password-named metric does not contain an account password. No credential or production log sink is involved. |
| <a id="alert-34"></a>[34](https://github.com/swiftraccoon/simplestChat/security/code-scanning/34) | high | false-positive | [`src/metrics.rs:1157`](../src/metrics.rs#L1157) | The sink is a cfg(test) assertion diagnostic for a locally constructed metrics snapshot. The values are numeric counters/gauges; a password-named metric does not contain an account password. No credential or production log sink is involved. |
| <a id="alert-35"></a>[35](https://github.com/swiftraccoon/simplestChat/security/code-scanning/35) | high | false-positive | [`src/signaling/mod.rs:1667`](../src/signaling/mod.rs#L1667) | The sink is a cfg(test) assertion diagnostic for a locally constructed metrics snapshot. The values are numeric counters/gauges; a password-named metric does not contain an account password. No credential or production log sink is involved. |
| <a id="alert-36"></a>[36](https://github.com/swiftraccoon/simplestChat/security/code-scanning/36) | high | false-positive | [`src/signaling/connection_authorization_tests.rs:250`](../src/signaling/connection_authorization_tests.rs#L250) | The sink is an assertion diagnostic in the cfg(test)-only authorization dispatcher fixture. It formats local synthetic operation results and roles, not a production logger or account credential. The reviewed test calls use fixed fixture participants and messages. |
| <a id="alert-37"></a>[37](https://github.com/swiftraccoon/simplestChat/security/code-scanning/37) | high | false-positive | [`src/signaling/connection_authorization_tests.rs:298`](../src/signaling/connection_authorization_tests.rs#L298) | The sink is an assertion diagnostic in the cfg(test)-only authorization dispatcher fixture. It formats local synthetic operation results and roles, not a production logger or account credential. The reviewed test calls use fixed fixture participants and messages. |
| <a id="alert-38"></a>[38](https://github.com/swiftraccoon/simplestChat/security/code-scanning/38) | critical | false-positive | [`src/auth/common_passwords.rs:266`](../src/auth/common_passwords.rs#L266) | All literals are inside cfg(test) blocklist tests: known weak strings must be rejected and unrelated phrases accepted. They are local policy inputs, never provisioned account credentials. |
| <a id="alert-39"></a>[39](https://github.com/swiftraccoon/simplestChat/security/code-scanning/39) | critical | false-positive | [`src/auth/common_passwords.rs:267`](../src/auth/common_passwords.rs#L267) | All literals are inside cfg(test) blocklist tests: known weak strings must be rejected and unrelated phrases accepted. They are local policy inputs, never provisioned account credentials. |
| <a id="alert-40"></a>[40](https://github.com/swiftraccoon/simplestChat/security/code-scanning/40) | critical | false-positive | [`src/auth/common_passwords.rs:268`](../src/auth/common_passwords.rs#L268) | All literals are inside cfg(test) blocklist tests: known weak strings must be rejected and unrelated phrases accepted. They are local policy inputs, never provisioned account credentials. |
| <a id="alert-41"></a>[41](https://github.com/swiftraccoon/simplestChat/security/code-scanning/41) | critical | false-positive | [`src/auth/common_passwords.rs:269`](../src/auth/common_passwords.rs#L269) | All literals are inside cfg(test) blocklist tests: known weak strings must be rejected and unrelated phrases accepted. They are local policy inputs, never provisioned account credentials. |
| <a id="alert-42"></a>[42](https://github.com/swiftraccoon/simplestChat/security/code-scanning/42) | critical | false-positive | [`src/auth/password.rs:50`](../src/auth/password.rs#L50) | All literals are inside cfg(test) password hashing/verification tests, including public RustCrypto known-answer values, NFC-equivalence fixtures, fresh-salt assertions and negative/empty inputs. Production password selection obtains values from requests; none of these constants is an operational credential. |
| <a id="alert-43"></a>[43](https://github.com/swiftraccoon/simplestChat/security/code-scanning/43) | critical | false-positive | [`src/auth/password.rs:51`](../src/auth/password.rs#L51) | All literals are inside cfg(test) password hashing/verification tests, including public RustCrypto known-answer values, NFC-equivalence fixtures, fresh-salt assertions and negative/empty inputs. Production password selection obtains values from requests; none of these constants is an operational credential. |
| <a id="alert-44"></a>[44](https://github.com/swiftraccoon/simplestChat/security/code-scanning/44) | critical | false-positive | [`src/auth/password.rs:56`](../src/auth/password.rs#L56) | All literals are inside cfg(test) password hashing/verification tests, including public RustCrypto known-answer values, NFC-equivalence fixtures, fresh-salt assertions and negative/empty inputs. Production password selection obtains values from requests; none of these constants is an operational credential. |
| <a id="alert-45"></a>[45](https://github.com/swiftraccoon/simplestChat/security/code-scanning/45) | critical | false-positive | [`src/auth/password.rs:57`](../src/auth/password.rs#L57) | All literals are inside cfg(test) password hashing/verification tests, including public RustCrypto known-answer values, NFC-equivalence fixtures, fresh-salt assertions and negative/empty inputs. Production password selection obtains values from requests; none of these constants is an operational credential. |
| <a id="alert-46"></a>[46](https://github.com/swiftraccoon/simplestChat/security/code-scanning/46) | critical | false-positive | [`src/auth/password.rs:58`](../src/auth/password.rs#L58) | All literals are inside cfg(test) password hashing/verification tests, including public RustCrypto known-answer values, NFC-equivalence fixtures, fresh-salt assertions and negative/empty inputs. Production password selection obtains values from requests; none of these constants is an operational credential. |
| <a id="alert-47"></a>[47](https://github.com/swiftraccoon/simplestChat/security/code-scanning/47) | critical | false-positive | [`src/auth/password.rs:68`](../src/auth/password.rs#L68) | All literals are inside cfg(test) password hashing/verification tests, including public RustCrypto known-answer values, NFC-equivalence fixtures, fresh-salt assertions and negative/empty inputs. Production password selection obtains values from requests; none of these constants is an operational credential. |
| <a id="alert-48"></a>[48](https://github.com/swiftraccoon/simplestChat/security/code-scanning/48) | critical | false-positive | [`src/auth/password.rs:69`](../src/auth/password.rs#L69) | All literals are inside cfg(test) password hashing/verification tests, including public RustCrypto known-answer values, NFC-equivalence fixtures, fresh-salt assertions and negative/empty inputs. Production password selection obtains values from requests; none of these constants is an operational credential. |
| <a id="alert-49"></a>[49](https://github.com/swiftraccoon/simplestChat/security/code-scanning/49) | critical | false-positive | [`src/auth/password.rs:74`](../src/auth/password.rs#L74) | All literals are inside cfg(test) password hashing/verification tests, including public RustCrypto known-answer values, NFC-equivalence fixtures, fresh-salt assertions and negative/empty inputs. Production password selection obtains values from requests; none of these constants is an operational credential. |
| <a id="alert-50"></a>[50](https://github.com/swiftraccoon/simplestChat/security/code-scanning/50) | critical | false-positive | [`src/auth/password.rs:77`](../src/auth/password.rs#L77) | All literals are inside cfg(test) password hashing/verification tests, including public RustCrypto known-answer values, NFC-equivalence fixtures, fresh-salt assertions and negative/empty inputs. Production password selection obtains values from requests; none of these constants is an operational credential. |
| <a id="alert-51"></a>[51](https://github.com/swiftraccoon/simplestChat/security/code-scanning/51) | critical | false-positive | [`src/auth/password.rs:82`](../src/auth/password.rs#L82) | All literals are inside cfg(test) password hashing/verification tests, including public RustCrypto known-answer values, NFC-equivalence fixtures, fresh-salt assertions and negative/empty inputs. Production password selection obtains values from requests; none of these constants is an operational credential. |
| <a id="alert-52"></a>[52](https://github.com/swiftraccoon/simplestChat/security/code-scanning/52) | critical | false-positive | [`src/auth/password.rs:83`](../src/auth/password.rs#L83) | All literals are inside cfg(test) password hashing/verification tests, including public RustCrypto known-answer values, NFC-equivalence fixtures, fresh-salt assertions and negative/empty inputs. Production password selection obtains values from requests; none of these constants is an operational credential. |
| <a id="alert-53"></a>[53](https://github.com/swiftraccoon/simplestChat/security/code-scanning/53) | critical | false-positive | [`src/auth/password.rs:98`](../src/auth/password.rs#L98) | All literals are inside cfg(test) password hashing/verification tests, including public RustCrypto known-answer values, NFC-equivalence fixtures, fresh-salt assertions and negative/empty inputs. Production password selection obtains values from requests; none of these constants is an operational credential. |
| <a id="alert-54"></a>[54](https://github.com/swiftraccoon/simplestChat/security/code-scanning/54) | critical | false-positive | [`src/auth/password.rs:99`](../src/auth/password.rs#L99) | All literals are inside cfg(test) password hashing/verification tests, including public RustCrypto known-answer values, NFC-equivalence fixtures, fresh-salt assertions and negative/empty inputs. Production password selection obtains values from requests; none of these constants is an operational credential. |
| <a id="alert-55"></a>[55](https://github.com/swiftraccoon/simplestChat/security/code-scanning/55) | critical | false-positive | [`src/auth/password.rs:104`](../src/auth/password.rs#L104) | All literals are inside cfg(test) password hashing/verification tests, including public RustCrypto known-answer values, NFC-equivalence fixtures, fresh-salt assertions and negative/empty inputs. Production password selection obtains values from requests; none of these constants is an operational credential. |
| <a id="alert-56"></a>[56](https://github.com/swiftraccoon/simplestChat/security/code-scanning/56) | critical | false-positive | [`src/auth/password.rs:105`](../src/auth/password.rs#L105) | All literals are inside cfg(test) password hashing/verification tests, including public RustCrypto known-answer values, NFC-equivalence fixtures, fresh-salt assertions and negative/empty inputs. Production password selection obtains values from requests; none of these constants is an operational credential. |
| <a id="alert-57"></a>[57](https://github.com/swiftraccoon/simplestChat/security/code-scanning/57) | critical | false-positive | [`src/auth/password.rs:106`](../src/auth/password.rs#L106) | All literals are inside cfg(test) password hashing/verification tests, including public RustCrypto known-answer values, NFC-equivalence fixtures, fresh-salt assertions and negative/empty inputs. Production password selection obtains values from requests; none of these constants is an operational credential. |
| <a id="alert-58"></a>[58](https://github.com/swiftraccoon/simplestChat/security/code-scanning/58) | critical | false-positive | [`src/auth/routes.rs:930`](../src/auth/routes.rs#L930) | This cfg(test) assertion verifies the timing-equalization dummy password hash does not match an arbitrary public input. It provisions no account and is outside runtime credential selection. |
| <a id="alert-59"></a>[59](https://github.com/swiftraccoon/simplestChat/security/code-scanning/59) | critical | false-positive | [`src/room/settings.rs:598`](../src/room/settings.rs#L598) | These literals are cfg(test) room-password validation boundary inputs for length, UTF-8 bytes and control rejection. They do not create or authenticate any operational room. |
| <a id="alert-60"></a>[60](https://github.com/swiftraccoon/simplestChat/security/code-scanning/60) | critical | false-positive | [`src/room/settings.rs:603`](../src/room/settings.rs#L603) | These literals are cfg(test) room-password validation boundary inputs for length, UTF-8 bytes and control rejection. They do not create or authenticate any operational room. |
| <a id="alert-61"></a>[61](https://github.com/swiftraccoon/simplestChat/security/code-scanning/61) | critical | false-positive | [`src/room/settings.rs:605`](../src/room/settings.rs#L605) | These literals are cfg(test) room-password validation boundary inputs for length, UTF-8 bytes and control rejection. They do not create or authenticate any operational room. |
| <a id="alert-62"></a>[62](https://github.com/swiftraccoon/simplestChat/security/code-scanning/62) | critical | false-positive | [`src/room/settings.rs:608`](../src/room/settings.rs#L608) | These literals are cfg(test) room-password validation boundary inputs for length, UTF-8 bytes and control rejection. They do not create or authenticate any operational room. |
| <a id="alert-63"></a>[63](https://github.com/swiftraccoon/simplestChat/security/code-scanning/63) | critical | false-positive | [`src/signaling/mod.rs:2032`](../src/signaling/mod.rs#L2032) | This fixed passphrase is inside the cfg(test) disposable-database registration enumeration-limiter test. It is used only with a unique fixture account and test-owned state, never a production identity. |
| <a id="alert-64"></a>[64](https://github.com/swiftraccoon/simplestChat/security/code-scanning/64) | critical | false-positive | [`src/turn.rs:155`](../src/turn.rs#L155) | This fixed TURN secret belongs only to cfg(test) credential-generation and non-disclosure fixtures, with a fictional turn.example endpoint. Runtime TURN credentials use configured secrets; this test value cannot be issued by the production path. |
| <a id="alert-65"></a>[65](https://github.com/swiftraccoon/simplestChat/security/code-scanning/65) | critical | false-positive | [`src/turn.rs:187`](../src/turn.rs#L187) | This fixed TURN secret belongs only to cfg(test) credential-generation and non-disclosure fixtures, with a fictional turn.example endpoint. Runtime TURN credentials use configured secrets; this test value cannot be issued by the production path. |
| <a id="alert-66"></a>[66](https://github.com/swiftraccoon/simplestChat/security/code-scanning/66) | critical | false-positive | [`src/turn.rs:138`](../src/turn.rs#L138) | This key is the public RFC2202 HMAC-SHA1 known-answer vector inside the cfg(test) module. The exact expected digest verifies protocol interoperability; it is not a deployed TURN key. |
| <a id="alert-67"></a>[67](https://github.com/swiftraccoon/simplestChat/security/code-scanning/67) | critical | false-positive | [`src/auth/common_passwords.rs:234`](../src/auth/common_passwords.rs#L234) | All literals are inside cfg(test) blocklist tests: known weak strings must be rejected and unrelated phrases accepted. They are local policy inputs, never provisioned account credentials. |
| <a id="alert-68"></a>[68](https://github.com/swiftraccoon/simplestChat/security/code-scanning/68) | critical | false-positive | [`src/auth/common_passwords.rs:243`](../src/auth/common_passwords.rs#L243) | All literals are inside cfg(test) blocklist tests: known weak strings must be rejected and unrelated phrases accepted. They are local policy inputs, never provisioned account credentials. |
| <a id="alert-69"></a>[69](https://github.com/swiftraccoon/simplestChat/security/code-scanning/69) | medium | false-positive | [`src/signaling/mod.rs:1658`](../src/signaling/mod.rs#L1658) | The sink is a cfg(test) assertion diagnostic for a response from the test-owned loopback HTTP server. The route returns fixed status/body bytes to test timeout and admission metrics; no external input or production logging path is used. |
