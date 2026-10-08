# Exact CodeQL review and enforcement

The dated findings and source re-reviews below are historical evidence, not the
current GitHub alert inventory. Preserve their observed revisions and decisions;
current runs must independently satisfy the maintained policy and exact active
records in `exceptions.json`. The command contract below describes the maintained
triage helper.

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

## Source re-review: assertion fixture

Alert 31 was re-reviewed against local source commit
`f2bb7be1d2f1b848ae1e5d8b92635b6af52011c4` before its next analyzer run.
The only change to `ops/ansible/tests/test_release_playbook.py` adds a
`RELEASE.ROOT` binding to an isolated Compose fixture near line 488. A byte
comparison against the parent commit confirms that the entire flagged line 76
and its columns 23–79 remain identical:
`Environment(undefined=StrictUndefined, autoescape=False)`. The surrounding
method still evaluates Ansible assertion expressions with bounded fixture
values; it produces no HTML or browser response. The original false-positive
rationale therefore remains valid for these specifically reviewed bytes.

The complete-file SHA-256 changes from
`11fcd936687e12bbfb93ca10d6118914692918938a867692aa28732641399f46` to
`0e5ae936e7ed44248ce99e85c824c213427dfa8a5bab0371c79b2ddb4f499cdc`.
The canonical finding helper derives the replacement exact fingerprint using
the unchanged query, CodeQL version, range and rendered-message digest. The
superseded exception is replaced and its 2026-11-29 expiry is unchanged.
`observedRevision` retains the actual earlier analysis; `sourceProvenanceRevision`
records the new source review. This is not a claim that the new revision has
already passed CodeQL: the next original SARIF must independently match the
complete identity or the gate rejects it. No remote alert state was changed.

## Source re-review: disposable release controller

The 2026-10-05 Caddy 2.11.7 selection changes only lines 59-61 relative to
`2a6ae6b3714b5378343debf4d023b5db04c2c369`. The complete flagged line 136 and all callers remain
byte-identical. The reviewed Git blob is `9727472d0cae22313bf1df3a5c9cd6e5cabd53b6`; its
whole-file SHA-256 is `87c30a0fea7efbde1498dd873a2d539bffe3f443c94bcfb6ad24d2ad2038fa92`. The exact
replacement fingerprint is `codeql:88c044006c3fd143909245b73cc26f156bc886913a2346d53264bf034cbdeb53`. The original
query, tool version, range, message, disposition and 2026-11-29 expiry are retained.
This companion source review does not claim a new CodeQL run: the next original
analysis must independently match the new identity or fail. The JSON retains the
original observed analysis and records this source review's base revision and blob.

The preceding review history follows.

Alert 32 was re-reviewed against source commit
`661fbc763030f6b5423232a7be817e1c09872898`. The only change to
`build/release_container_harness.py` permits the disposable Linux controller to
run on ARM64 and updates its refusal message. The flagged operation at line 136
and all eight `write_new` calls are unchanged. Exclusive creation, default mode
0600, process umask 0077 and private evidence directories remain in place. The
only public mode 0644 outputs remain the one-day fixture CA certificate and the
sanitized CI summary; private keys, command output and configuration stay private.

The complete-file SHA-256 changes from
`05deaeb4565baa51e86758ed9a16a3b46394668656f960e549cfc946d3eee9e0` to
`3ad8d0f5d1a33a20b78e168117b54cdd770cd016bddb96eb998c9ce6ee063ee9`.
A fresh CodeQL 2.27.1 Python security-extended analysis of that exact commit ran
all 50 security rules and independently reproduced the unchanged query, primary
range and rendered message with the new whole-file identity. The original SARIF
passed extraction and analysis-health checks; its initial policy failure exposed
the stale review. The maintained triage helper derives the replacement fingerprint
`codeql:4276b44b448f6e8c90e2203acf7575dbd7633debbd8a0045333f4ee91307867e`.
The exact exception is replaced with the original 2026-11-29 expiry. This review
authorizes no other finding, path or query exclusion, and changes no remote alert
state. It does not claim a complete passing CI run.

## Source re-review: passkey RP configuration fixtures

Alerts 27 and 30 were re-reviewed against source commit
`cf941371acc877ed568f84f4e545dd622fe7badd`. The only change to
`build/release_container_fixture.py` adds an explicit `localhost` RP ID. Its
flagged `Environment` moves from lines 163-167 to 164-168 unchanged. Tracked
local dotenv, SQL, Compose and PostgreSQL configuration remains its only output;
`StrictUndefined`, validated fixture identities, callers and exclusive mode 0600
file creation are unchanged.

In `ops/ansible/tests/test_public_templates.py`, the flagged configuration
renderer moves from line 82 to 83 unchanged. The added RP fixture value, hostname
derivation and parent-RP tests render existing local dotenv templates and evaluate
deployment values. Neither path produces HTML or a browser response, and HTML
autoescaping would corrupt these configuration formats.

The complete-file SHA-256 changes are:

| Alert | Previous SHA-256 | Reviewed SHA-256 |
| --- | --- | --- |
| 27 | `872273a5c7940e95987d967cfe591c39270f7c776e3c854b16527d0e5621d80d` | `f4c113823c89c511b8b2f73e82ae4a1ffbf4c0c4b367984bd1139757353f51e9` |
| 30 | `c9812f1d39fbf073c580220868ba692b93acb440a5ca2428d41c3d0887465619` | `e21db684ef354217e48922628ab19995335e0ab3a62c8d04bc5ba4efd45bab17` |

A fresh CodeQL 2.27.1 Python security-extended analysis of that exact commit ran
all 50 security rules. Its original SARIF confirms successful extraction and
analysis and independently reproduces both complete identities, including the
shifted ranges. Initial policy evaluation blocked only these two stale reviews;
nine unchanged findings retained their existing exact reviews. Both
`observedRevision` and `sourceProvenanceRevision` now record this actual local
analysis and source review. The exact exceptions replace their superseded
identities and retain the original 2026-11-29 expiry. No other finding, path or
query is excluded; no remote alert state changed. This does not claim a complete
passing CI run.

## Source re-review: immutable Caddy image fixture

Alert 27 was re-reviewed on 2026-10-05 against source commit
`b5b07c208b58828a7c2a0da6c85b7162269006c1`. The only change since its previous
review permits the exact `ghcr.io/swiftraccoon/simplestchat-caddy` image name
alongside the official Caddy image. Both selectors still require a tag and a
complete SHA-256 digest. The unchanged `StrictUndefined` renderer moves from
lines 164-168 to 170-174 and continues to produce tracked local dotenv, SQL,
Compose and PostgreSQL configuration. Its callers and exclusive mode 0600
writes are unchanged; there is no HTML or browser-response sink.

The complete-file SHA-256 changes from
`f4c113823c89c511b8b2f73e82ae4a1ffbf4c0c4b367984bd1139757353f51e9` to
`360ca5eeb6210e56983155e1391b3895694e86aa3a4aee78e997cc2b787d7d6a`.
The successful hosted Python analysis in
[run 37276379828](https://github.com/swiftraccoon/simplestChat/actions/runs/37276379828/job/111654198353)
produced the original policy report identifying this sole stale review. Its
query, analyzer version, complete-file hash, range and message bind the replacement
fingerprint `codeql:2dbbdc8162f2d76a38d73a1f5fae9b07f4b5a48074b312989f156d8a98a81a5d`.
The superseded exception is replaced with the original 2026-11-29 expiry.
Replaying that hosted report validates the updated policy; it does not constitute
a new CodeQL analysis or a complete passing CI run. No other finding, query or
path is excluded, and no remote alert state changed.

## Source re-review: TURN migration fixtures

Alerts 19–23 were re-reviewed on 2026-10-04 against source commit
`9eaabb7b5ab586ca694283afc1550055ef2c3136`. The five flagged writes in
`ops/ansible/tests/test_turn_public.py` still contain only the public repeated-a
and repeated-c fixture values and configuration text derived from them. They
remain inside disposable `TemporaryDirectory` instances; the activation test
uses `FixtureRunner`, and both tests replace the production configuration root.
Neither test reads deployment credentials or contacts a remote relay.

Changes since the previous review add TLS and target-address regression coverage.
Two imports and the synthetic TLS peer fixture move the flagged operations from
lines 112, 119, 123, 126 and 140 to 127, 134, 138, 141 and 155. Their expressions
and fixed inputs are unchanged. The remaining changes exercise a fixed
documentation address with mocked network calls and add no sensitive input to
the reviewed writes.

The complete-file SHA-256 changes from
`ccd57287b97b59e5a7fb089201af29597eae8e7749d1b10c6047e0aaea941c56` to
`3a87423b1955d1f11e87b9ff048411e5186614fad914092cd86da0b20bb8f5de`.
The original hosted policy artifacts from runs
[37222426376](https://github.com/swiftraccoon/simplestChat/actions/runs/37222426376)
and [37237511980](https://github.com/swiftraccoon/simplestChat/actions/runs/37237511980)
report the same five exact identities. Their Python analyses succeeded; the
policy gate correctly rejected the obsolete source reviews. Current GitHub
alert locations and messages independently match those artifacts and the
reviewed checkout.

The five replacement exceptions retain the original 2026-11-29 expiry and exact
query, analyzer version, complete-file hash, primary range and message binding.
No gate, query or path is excluded, and no remote alert state is changed. Replaying
the two hosted finding reports against the updated policy is policy validation,
not a claim that a new full CodeQL or CI run has completed.

## Follow-up: canary assertion diagnostic

<a id="alert-102"></a>[Alert 102](https://github.com/swiftraccoon/simplestChat/security/code-scanning/102)
is a High `rust/cleartext-logging` result from CodeQL `2.27.1`, run
`36716564147`, revision `def5a4a0ed7949d34be3643744ae8b110473835a`.
Its primary range is `src/security_canary_tests.rs:90:90:21:62`. This individual
result is a false positive; the original 69-alert disposition above is unchanged.

`SensitiveValues` stores each fixed descriptive label separately from its
sensitive value. `assert_absent` checks the value and three encodings against
captured output, then passes only the resulting boolean to `assert!`. The
failure format interpolates **only `label` and `surface`**. Neither `value`,
`encoded`, `output` nor the collection receiver is a formatting argument.
For example, a failure can say `sensitive password appeared in application
tracing`; it cannot include the password through these arguments.

The 13 paths reported by CodeQL cover every current callsite:

| Reviewed caller | Literal surface arguments |
| --- | --- |
| [`src/security_canary_tests.rs:118`](../src/security_canary_tests.rs#L118), line 121 | `guard fixture`, `safe fixture` |
| [`src/signaling/auth_secret_canary_tests.rs:149`](../src/signaling/auth_secret_canary_tests.rs#L149), lines 215, 239, 261, 306–309 | `denied login response`, `denied invitation response`, `denied credential response`, `revoked-session response`, `application tracing`, `HTTP metrics`, `media diagnostic response`, `request URIs and URL response headers` |
| [`src/signaling/connection_secret_canary_tests.rs:146`](../src/signaling/connection_secret_canary_tests.rs#L146), lines 147–148 | `dispatcher tracing`, `diagnostic JSONL`, `dispatcher metrics` |

Every `add` label is also a literal, either passed directly or obtained from a
fixed tuple array in those three files. The helper is declared under
`#[cfg(test)]` in `src/lib.rs:29–30`; the HTTP and dispatcher callers are nested
under the test-only authorization modules in `src/signaling/mod.rs:5–6` and
`src/signaling/connection.rs:2093–2095`. The decision rests on the actual format
arguments as well as this test boundary, not on a general exemption for tests.

The reviewed complete-file SHA256 values are:

| Source | SHA256 |
| --- | --- |
| `src/security_canary_tests.rs` | `465a36f3c06f71753da0d1f21cef3f451ec2ef2fb23d5d22a6898d943858d26e` |
| `src/signaling/auth_secret_canary_tests.rs` | `1267148612660ecc2387546f3f139bf9fa0fc2ea3d7c6d1395350eee97df372b` |
| `src/signaling/connection_secret_canary_tests.rs` | `fc65f814dbfec87de61253c68d16a30290efbdb540535cf00604ab01a1aeb812` |

All bytes matched the analyzed revision when reviewed. The exception binds the
exact query, analyzer, primary range, complete rendered message and helper-file
digest and expires on 2026-11-29. Changed callers still require renewed review;
their behavior is not covered by the primary-file fingerprint. No Rust source
was changed to silence this query, and this review performs no remote dismissal.

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

CI submits validated original or regenerated SARIF to GitHub code scanning.
Downloadable artifacts contain only the explicit policy `report.json`, fixed
`failure.json` or compact analysis summary; raw reports and working directories
are not uploaded as artifacts. The policy reports contain no source text or raw
diagnostic messages. Fixed validation codes identify rejected report shapes
without exposing report content. Native/Rust cache hits retain their original
evaluation revision and declare query reuse; current policy is always reapplied.

For CodeQL line/column locations, the helper expands absent `endLine` to
`startLine` and absent `startColumn` to 1, as required by
[SARIF 2.1.0 sections 3.30.6–7](https://docs.oasis-open.org/sarif/sarif/v2.1.0/os/sarif-v2.1.0-os.html).
These are the omissions documented in
[CodeQL's current output contract](https://docs.github.com/en/code-security/reference/code-scanning/codeql/codeql-cli/sarif-output).
The expanded range must retain the same fingerprint as GitHub's explicit API
location. Explicit malformed values still fail. This gate requires CodeQL's
explicit `startLine` and `endColumn`; offset-only ranges and source-dependent
end-column inference are unsupported and fail with fixed validation codes.
GitHub's converted analysis SARIF is useful for comparing location identities,
but lacks the original invocation evidence and cannot establish a passing local
analysis. No invocation, suppression or result-completeness check is bypassed.

## Source re-review: Rust authorization and room validation fixtures

Alerts 36–37 and 59–62 were re-reviewed against revision
`9eaabb7b5ab586ca694283afc1550055ef2c3136`. The original hosted policy evidence
from runs `37222426376` and `37237511980` confirms that Rust extraction and
query evaluation completed successfully. The first run rejected two stale
exact reviews; the second also rejected four reviews whose source changed with
room appearance support.

The authorization module remains included only through `#[cfg(test)]` in
`src/signaling/connection.rs`. New nickname tests and removal of the former
account-name argument move the unchanged assertion diagnostics from lines
250/298 to 359/406. The reviewed function still creates synthetic participants,
uses a test-owned room manager with no database, and prints only fixture role
and operation results when assertions fail. It has no production logging sink
or real account credential.

The room settings changes add name/topic appearance fields and serialization
checks. They move the unchanged password-validation boundary literals from
598/603/605/608 to 613/618/620/623. All four remain inside `#[cfg(test)]`, exercise
only `validate_create_request`, and neither create rooms nor authenticate users.
The full source changes and the two test boundaries were inspected; identical
flagged-line bytes were checked against the previously reviewed revisions.

| Alerts | Previous whole-file SHA-256 | Reviewed whole-file SHA-256 |
| --- | --- | --- |
| 36–37 | `9f56f0c82a12603884ef7839f1d1560e10313a5a36a141ff8f77516b8c90df7b` | `2219caa33ed0862fffde8a9dccef20d3f37c7078e5cf870ca922e990a0027e28` |
| 59–62 | `0d2fd66e20b10a82ec16d43aef5a18050c4dd0fa75b131d7925d48cf128cfa26` | `baa741a3f3f5d880569fe94b1789ad9d4964b502fb86720d99350a4876be57aa` |

Only these six identities are replaced with the fingerprints from the current
hosted policy report. Queries, severity thresholds, exact matching and the
original 2026-11-29 expiry remain unchanged. This source re-review does not
claim that the complete CI run passed and does not change remote alert states.

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

Apply allows at most 128 writes within one 900-second operation deadline; read-only
API operations retain their 300-second deadline. Every subprocess is also bounded
by the remaining deadline and its 20-second request limit. At least one second
elapses after each mutation completes before the next mutation can start. The
helper waits before refreshing the next alert, then rechecks its complete source
identity, the remote branch head, and the exact review and exception bytes before
writing. It verifies the returned finding, dismissal comment and branch head
before confirming the action. No failed request is retried automatically.

This pacing follows [GitHub's REST guidance](https://docs.github.com/en/rest/using-the-rest-api/best-practices-for-using-the-rest-api#pause-between-mutative-requests)
and keeps one invocation below the general 80-content-writes-per-minute limit.
Other account activity and endpoint-specific limits still share GitHub's budget;
a rate-limit error stops the operation and retains its journal. The update endpoint
does not provide an atomic alert-version or branch-head precondition, and GitHub
does not generally support conditional PATCH requests. A change during an in-flight
write can therefore be detected after that write, but cannot be prevented by these
checks. Such a response remains unconfirmed and stops the batch; no automatic
reopen or rollback risks overwriting another operator's decision.

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
| <a id="alert-19"></a>[19](https://github.com/swiftraccoon/simplestChat/security/code-scanning/19) | high | false-positive | [`ops/ansible/tests/test_turn_public.py:127`](../ops/ansible/tests/test_turn_public.py#L127) | The test writes public repeated-a/repeated-c synthetic TURN values inside TemporaryDirectory to exercise configuration-drift rejection. No live credential, production account or remote relay is used; cleanup removes the fixture. |
| <a id="alert-20"></a>[20](https://github.com/swiftraccoon/simplestChat/security/code-scanning/20) | high | false-positive | [`ops/ansible/tests/test_turn_public.py:134`](../ops/ansible/tests/test_turn_public.py#L134) | The test writes public repeated-a/repeated-c synthetic TURN values inside TemporaryDirectory to exercise configuration-drift rejection. No live credential, production account or remote relay is used; cleanup removes the fixture. |
| <a id="alert-21"></a>[21](https://github.com/swiftraccoon/simplestChat/security/code-scanning/21) | high | false-positive | [`ops/ansible/tests/test_turn_public.py:138`](../ops/ansible/tests/test_turn_public.py#L138) | The test writes public repeated-a/repeated-c synthetic TURN values inside TemporaryDirectory to exercise configuration-drift rejection. No live credential, production account or remote relay is used; cleanup removes the fixture. |
| <a id="alert-22"></a>[22](https://github.com/swiftraccoon/simplestChat/security/code-scanning/22) | high | false-positive | [`ops/ansible/tests/test_turn_public.py:141`](../ops/ansible/tests/test_turn_public.py#L141) | The test writes public repeated-a/repeated-c synthetic TURN values inside TemporaryDirectory to exercise configuration-drift rejection. No live credential, production account or remote relay is used; cleanup removes the fixture. |
| <a id="alert-23"></a>[23](https://github.com/swiftraccoon/simplestChat/security/code-scanning/23) | high | false-positive | [`ops/ansible/tests/test_turn_public.py:155`](../ops/ansible/tests/test_turn_public.py#L155) | The test writes public repeated-a/repeated-c synthetic TURN values inside TemporaryDirectory to exercise configuration-drift rejection. No live credential, production account or remote relay is used; cleanup removes the fixture. |
| <a id="alert-24"></a>[24](https://github.com/swiftraccoon/simplestChat/security/code-scanning/24) | high | fix-pending | [`ops/ansible/files/monitoring_collect.py:156`](../ops/ansible/files/monitoring_collect.py#L156) | Assigned source remediation; this review authorizes no dismissal or exception. |
| <a id="alert-25"></a>[25](https://github.com/swiftraccoon/simplestChat/security/code-scanning/25) | high | fix-pending | [`build/release_http_policy.py:273`](../build/release_http_policy.py#L273) | Assigned source remediation; this review authorizes no dismissal or exception. |
| <a id="alert-26"></a>[26](https://github.com/swiftraccoon/simplestChat/security/code-scanning/26) | high | fix-pending | [`ops/ansible/files/turn_public.py:298`](../ops/ansible/files/turn_public.py#L298) | Assigned source remediation; this review authorizes no dismissal or exception. |
| <a id="alert-27"></a>[27](https://github.com/swiftraccoon/simplestChat/security/code-scanning/27) | high | false-positive | [`build/release_container_fixture.py:170`](../build/release_container_fixture.py#L170) | The unchanged StrictUndefined renderer produces local configuration only. The exact project GHCR selector shifts the flagged Environment by six lines; callers and exclusive mode0600 writes are unchanged. Source and the hosted policy report were re-reviewed at `b5b07c208b58828a7c2a0da6c85b7162269006c1`; HTML escaping would corrupt these formats. |
| <a id="alert-28"></a>[28](https://github.com/swiftraccoon/simplestChat/security/code-scanning/28) | high | false-positive | [`ops/ansible/tests/test_automation.py:39`](../ops/ansible/tests/test_automation.py#L39) | The Jinja Environment renders local shell/SQL/dotenv/Compose/YAML configuration or evaluates Ansible conditions with StrictUndefined and fixture values. It produces no HTML or browser response. HTML autoescaping would corrupt these formats; there is no XSS sink in this path. |
| <a id="alert-29"></a>[29](https://github.com/swiftraccoon/simplestChat/security/code-scanning/29) | high | false-positive | [`ops/ansible/tests/test_automation.py:395`](../ops/ansible/tests/test_automation.py#L395) | The Jinja Environment renders local shell/SQL/dotenv/Compose/YAML configuration or evaluates Ansible conditions with StrictUndefined and fixture values. It produces no HTML or browser response. HTML autoescaping would corrupt these formats; there is no XSS sink in this path. |
| <a id="alert-30"></a>[30](https://github.com/swiftraccoon/simplestChat/security/code-scanning/30) | high | false-positive | [`ops/ansible/tests/test_public_templates.py:83`](../ops/ansible/tests/test_public_templates.py#L83) | The unchanged StrictUndefined renderer produces local configuration only. Added RP fixture and migration tests introduce no HTML sink. Source and fresh original SARIF were re-reviewed at `cf941371acc877ed568f84f4e545dd622fe7badd`; HTML escaping is inappropriate for these formats. |
| <a id="alert-31"></a>[31](https://github.com/swiftraccoon/simplestChat/security/code-scanning/31) | high | false-positive | [`ops/ansible/tests/test_release_playbook.py:76`](../ops/ansible/tests/test_release_playbook.py#L76) | The Jinja Environment renders local shell/SQL/dotenv/Compose/YAML configuration or evaluates Ansible conditions with StrictUndefined and fixture values. It produces no HTML or browser response. HTML autoescaping would corrupt these formats; there is no XSS sink in this path. |
| <a id="alert-32"></a>[32](https://github.com/swiftraccoon/simplestChat/security/code-scanning/32) | high | false-positive | [`build/release_container_harness.py:136`](../build/release_container_harness.py#L136) | Exclusive creation and default mode 0600 are unchanged; only the public fixture certificate and sanitized summary use mode 0644. The 2026-10-05 source re-review covers only the Caddy image selector change at lines 59-61. Fresh analysis must independently match the new whole-file identity. |
| <a id="alert-33"></a>[33](https://github.com/swiftraccoon/simplestChat/security/code-scanning/33) | high | false-positive | [`src/metrics.rs:1153`](../src/metrics.rs#L1153) | The sink is a cfg(test) assertion diagnostic for a locally constructed metrics snapshot. The values are numeric counters/gauges; a password-named metric does not contain an account password. No credential or production log sink is involved. |
| <a id="alert-34"></a>[34](https://github.com/swiftraccoon/simplestChat/security/code-scanning/34) | high | false-positive | [`src/metrics.rs:1157`](../src/metrics.rs#L1157) | The sink is a cfg(test) assertion diagnostic for a locally constructed metrics snapshot. The values are numeric counters/gauges; a password-named metric does not contain an account password. No credential or production log sink is involved. |
| <a id="alert-35"></a>[35](https://github.com/swiftraccoon/simplestChat/security/code-scanning/35) | high | false-positive | [`src/signaling/mod.rs:1667`](../src/signaling/mod.rs#L1667) | The sink is a cfg(test) assertion diagnostic for a locally constructed metrics snapshot. The values are numeric counters/gauges; a password-named metric does not contain an account password. No credential or production log sink is involved. |
| <a id="alert-36"></a>[36](https://github.com/swiftraccoon/simplestChat/security/code-scanning/36) | high | false-positive | [`src/signaling/connection_authorization_tests.rs:359`](../src/signaling/connection_authorization_tests.rs#L359) | The sink is an assertion diagnostic in the cfg(test)-only authorization dispatcher fixture. It formats local synthetic operation results and roles, not a production logger or account credential. The reviewed test calls use fixed fixture participants and messages. |
| <a id="alert-37"></a>[37](https://github.com/swiftraccoon/simplestChat/security/code-scanning/37) | high | false-positive | [`src/signaling/connection_authorization_tests.rs:406`](../src/signaling/connection_authorization_tests.rs#L406) | The sink is an assertion diagnostic in the cfg(test)-only authorization dispatcher fixture. It formats local synthetic operation results and roles, not a production logger or account credential. The reviewed test calls use fixed fixture participants and messages. |
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
| <a id="alert-59"></a>[59](https://github.com/swiftraccoon/simplestChat/security/code-scanning/59) | critical | false-positive | [`src/room/settings.rs:613`](../src/room/settings.rs#L613) | These literals are cfg(test) room-password validation boundary inputs for length, UTF-8 bytes and control rejection. They do not create or authenticate any operational room. |
| <a id="alert-60"></a>[60](https://github.com/swiftraccoon/simplestChat/security/code-scanning/60) | critical | false-positive | [`src/room/settings.rs:618`](../src/room/settings.rs#L618) | These literals are cfg(test) room-password validation boundary inputs for length, UTF-8 bytes and control rejection. They do not create or authenticate any operational room. |
| <a id="alert-61"></a>[61](https://github.com/swiftraccoon/simplestChat/security/code-scanning/61) | critical | false-positive | [`src/room/settings.rs:620`](../src/room/settings.rs#L620) | These literals are cfg(test) room-password validation boundary inputs for length, UTF-8 bytes and control rejection. They do not create or authenticate any operational room. |
| <a id="alert-62"></a>[62](https://github.com/swiftraccoon/simplestChat/security/code-scanning/62) | critical | false-positive | [`src/room/settings.rs:623`](../src/room/settings.rs#L623) | These literals are cfg(test) room-password validation boundary inputs for length, UTF-8 bytes and control rejection. They do not create or authenticate any operational room. |
| <a id="alert-63"></a>[63](https://github.com/swiftraccoon/simplestChat/security/code-scanning/63) | critical | false-positive | [`src/signaling/mod.rs:2032`](../src/signaling/mod.rs#L2032) | This fixed passphrase is inside the cfg(test) disposable-database registration enumeration-limiter test. It is used only with a unique fixture account and test-owned state, never a production identity. |
| <a id="alert-64"></a>[64](https://github.com/swiftraccoon/simplestChat/security/code-scanning/64) | critical | false-positive | [`src/turn.rs:155`](../src/turn.rs#L155) | This fixed TURN secret belongs only to cfg(test) credential-generation and non-disclosure fixtures, with a fictional turn.example endpoint. Runtime TURN credentials use configured secrets; this test value cannot be issued by the production path. |
| <a id="alert-65"></a>[65](https://github.com/swiftraccoon/simplestChat/security/code-scanning/65) | critical | false-positive | [`src/turn.rs:187`](../src/turn.rs#L187) | This fixed TURN secret belongs only to cfg(test) credential-generation and non-disclosure fixtures, with a fictional turn.example endpoint. Runtime TURN credentials use configured secrets; this test value cannot be issued by the production path. |
| <a id="alert-66"></a>[66](https://github.com/swiftraccoon/simplestChat/security/code-scanning/66) | critical | false-positive | [`src/turn.rs:138`](../src/turn.rs#L138) | This key is the public RFC2202 HMAC-SHA1 known-answer vector inside the cfg(test) module. The exact expected digest verifies protocol interoperability; it is not a deployed TURN key. |
| <a id="alert-67"></a>[67](https://github.com/swiftraccoon/simplestChat/security/code-scanning/67) | critical | false-positive | [`src/auth/common_passwords.rs:234`](../src/auth/common_passwords.rs#L234) | All literals are inside cfg(test) blocklist tests: known weak strings must be rejected and unrelated phrases accepted. They are local policy inputs, never provisioned account credentials. |
| <a id="alert-68"></a>[68](https://github.com/swiftraccoon/simplestChat/security/code-scanning/68) | critical | false-positive | [`src/auth/common_passwords.rs:243`](../src/auth/common_passwords.rs#L243) | All literals are inside cfg(test) blocklist tests: known weak strings must be rejected and unrelated phrases accepted. They are local policy inputs, never provisioned account credentials. |
| <a id="alert-69"></a>[69](https://github.com/swiftraccoon/simplestChat/security/code-scanning/69) | medium | false-positive | [`src/signaling/mod.rs:1658`](../src/signaling/mod.rs#L1658) | The sink is a cfg(test) assertion diagnostic for a response from the test-owned loopback HTTP server. The route returns fixed status/body bytes to test timeout and admission metrics; no external input or production logging path is used. |

## Source re-review: chat and push routing fixtures

On 2026-10-08, alerts 35, 63 and 69 were re-reviewed against source commit
`48ee485c740fcac30536c55f78c6f011a60dafff`. Added chat/push routes change the complete
`src/signaling/mod.rs` digest and shift these existing `cfg(test)` findings.
Each complete flagged line is byte-identical to its prior reviewed source;
primary columns, rule/tool version and rendered-message digest are unchanged.
The numeric metrics assertion, isolated registration-fixture passphrase and
test-owned loopback HTTP assertion remain within `mod security_tests` behind
`#[cfg(test)]`. No new production sink or test-wide exclusion is approved.

| Alert | Previous line | Reviewed line |
| --- | --- | --- |
| 35 | 1679 | 1698 |
| 63 | 2044 | 2063 |
| 69 | 1658 | 1689 |

The reviewed complete-file SHA-256 is
`c87670b468bcd77955e1f9b6775adb0354d9662ceb185e9578a65013c19ddec9`. The canonical identity helper
derives replacement fingerprints from these exact source identities. Their
original rationale, owner and 2026-11-29 expiry remain unchanged.
`observedRevision` retains the actual prior analysis; `sourceProvenanceRevision`
records this source-only review. The next original CodeQL analysis must match
these complete identities independently. No remote alert state changed.
