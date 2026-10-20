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

## October 9 attachment and account routes

At source revision `730710cde76dbd2dded001cdc02215926d32653f`, the bounded upload route, account endpoints and
strict download-CSP regression shift three existing findings. The complete flagged
lines and their enclosing test functions are byte-identical to the prior reviewed
`48ee485` source. Alert 35 moves from 1698 to 1742, alert 63 from 2063 to 2147,
and alert 69 from 1689 to 1733; columns and rendered-message hashes are unchanged.
All remain inside the existing `cfg(test)` module with the same fixed fixture
inputs and assertion sinks. Complete-file SHA-256 is `7e63f473cfb5d46c49937e0a4d6f92323ba01e1f10df22d6cf85f97fec03f97f`.
Canonical fingerprints and source provenance are rebound to that reviewed file;
original analysis provenance, rationale, owner and 2026-11-29 expiry remain.
This approves no new finding, remote dismissal or broader exclusion. The next
original CodeQL analysis must independently match these exact identities.

## October 12 security review follow-through

At source revision `60a2291356c8484c849925c0e933d794a240ba85`, the security-review follow-through (a test-only
metrics counter getter, the shared content-security-policy constant, the room
image routes and the dormant-room admission precheck) shifts five existing
findings and changes the rendered message of two more. The complete flagged
lines and their enclosing test functions are byte-identical to the prior reviewed
sources: alert 33 moves from `src/metrics.rs` 1153 to 1158 and alert 34 from
1157 to 1162 (complete-file SHA-256 `105505f83cb51ecf698ee1c4a17b253c75369043e4244dea38bc58f045f4276c`);
alert 35 moves from `src/signaling/mod.rs` 1742 to 1758, alert 63 from 2147 to
2187 and alert 69 from 1733 to 1749 (complete-file SHA-256
`02a06f401d0f964552da03548a1f7719bd2ba85fe623a7d43ef10d50bb793524`). Columns and rendered-message
hashes are unchanged for those five. Alerts 36 and 37 keep their file
(`2219caa33ed0862fffde8a9dccef20d3f37c7078e5cf870ca922e990a0027e28`), line and columns; only
their rendered message changed, because the room password now reaches the join
through `RoomManager::verify_room_password` and the data-flow labels name that
helper. Canonical fingerprints and source provenance are rebound to these
reviewed files; rationale, owner and the 2026-11-29 expiry remain.

One new Critical finding is reviewed: `rust/hard-coded-cryptographic-value` at
`src/room/dormant_join_tests.rs:99:99:57:79` (complete-file SHA-256
`345d6a1be199505c6bc4d2808ef59f8b6f575d3f8fd0107c247a933ffdd996f7`). The value is the
fixture passphrase of the ignored, worker-and-database dormant-room admission
test; it is hashed with the production Argon2 helper for one disposable room
row the test creates and deletes, and it never names a deployed credential. The
exception carries the same expiry as the other fixture passphrases. This
approves no remote dismissal or broader exclusion. The next original CodeQL
analysis must independently match these exact identities.

## October 12 media lane and header alignment

At source revision `7cbcaab7abbf37bef2cb49ef412154f21d0a2294`, the public media lane permit, its wait constant
and the aligned permissions-policy and cache-control text in
`src/signaling/mod.rs` shift three reviewed findings without touching their
lines: alert 35 moves from 1758 to 1779, alert 63 from 2187 to 2208 and
alert 69 from 1749 to 1770. The complete flagged lines and their enclosing
test functions are byte-identical to the `60a2291` review (complete-file
SHA-256 `864432e7029921a7ba33ba167921a738eb79e3843933389b943906fc457e093e`); columns and
rendered-message hashes are unchanged. Canonical fingerprints and source
provenance are rebound; rationale, owner and the 2026-11-29 expiry remain.
This approves no new finding, remote dismissal or broader exclusion.

## October 18 account and room controls

[Run 39096512724](https://github.com/swiftraccoon/simplestChat/actions/runs/39096512724)
analyzed revision `428576bda5f78167f169913940dcd9458539b5a4` with CodeQL
2.27.1. Its original Rust report contains three previously reviewed findings
whose exact identities changed: the metrics assertion (35), registration test
passphrase (63), and load-generator public-origin flow (953). The same report
also retains the reviewed loopback assertion (69) as an advisory because its
source identity moved. The report and invocation evidence remain unchanged.

The complete flagged lines and enclosing `cfg(test)` functions for 35, 63 and
69 are byte-identical to their previous review. Added routes move each by 40
lines. Their complete source hash is
`f3bcb5757eda885b8b0b8c9c369b9468ea9c996a76ca82bf6a3b0be6897bd3a6`.

Source revision `673c93fb2d09b5cdc932de2871c5493b26eb5186` then repairs the
earlier nickname fixture to create and clean up real disposable accounts. This
changes `connection_authorization_tests.rs` to SHA-256
`49ff37b88717b33c2b05568a082b75075651cf76e0643af94d7e6a4ee63c7d42`.
The flagged lines and complete enclosing dispatcher tests for 36 and 37 remain
byte-identical; only their positions move by 39 lines. Those two replacement
identities are a source-only review, retaining the rendered-message digests
observed in the original run. A new analysis must independently confirm them.

| Alert | Previous reviewed line | Current reviewed line |
| --- | --- | --- |
| 35 | 1779 | 1819 |
| 36 | 359 | 398 |
| 37 | 406 | 445 |
| 63 | 2208 | 2248 |
| 69 | 1770 | 1810 |

The separate [account-room public-origin review](reviews.md#account-room-public-origin-codeql-flow)
records 953's complete parent/include refresh. The six records replace their
superseded identities while keeping the original rationale, owner, exact
columns, query, analyzer version and 2026-11-29 expiry. No exception matches any
other changed Rust test file or its include dependencies. No query exclusion,
new finding or remote dismissal is approved, and static policy replay does not
claim a new CodeQL scan or a passing CI run.

## October 18 account-update source review

Source revision `4481e7f0e33969fcc822fe33919834d3ab962279` adds authenticated
account updates, a separate bounded account-data request lane, and atomic
notification-policy field updates. The complete source delta and the existing
findings' enclosing functions were reviewed.
The metrics assertion fixtures (35 and 69), disposable registration fixture
(63), and dummy-password-hash assertion (58) remain inside their existing
`cfg(test)` modules. Each flagged line and complete enclosing test function is
byte-identical to the successfully analyzed source
`82157a895bc710a3c382e356dac6ab3ad6e662a7`.

| Alert | Analyzed line | Reviewed line | Complete-file SHA-256 |
| --- | --- | --- | --- |
| 35 | 1819 | 1867 | `5fb0febf68f84262bd3cbf26cdd0ee9fb5238a663d16cadb9eb77c10542ea195` |
| 58 | 930 | 938 | `bde57a165d04acd28fd7b944dc96aeb4c08c1e0b754351f6603989693508637c` |
| 63 | 2248 | 2296 | `5fb0febf68f84262bd3cbf26cdd0ee9fb5238a663d16cadb9eb77c10542ea195` |
| 69 | 1810 | 1858 | `5fb0febf68f84262bd3cbf26cdd0ee9fb5238a663d16cadb9eb77c10542ea195` |

The account-data routing, guard, notification PATCH route and revocation hook
move the signaling fixtures by 48 lines. The account-request permit helper moves
the dummy-hash assertion by eight lines. Fixed fixture inputs, assertion sinks, query/tool versions,
primary columns and rendered-message hashes are unchanged. These findings
still do not expose an operational credential or a production logging path.
`connection_authorization_tests.rs` and its two reviewed diagnostics are
unchanged in this milestone.

The retained original SARIF from successful run
[39097391051](https://github.com/swiftraccoon/simplestChat/actions/runs/39097391051)
analyzed revision `82157a895bc710a3c382e356dac6ab3ad6e662a7`; its SHA-256 remains
`28b22429ffe4f73d4ed4d199b652202dc0222d1cce4b11f6e303163d0a1bb835`.
Only these four existing review identities are rebound to the current committed
source and line locations. Earlier `observedRevision` values, rationales,
owners, review count and the 2026-11-29 expiry are preserved;
`sourceProvenanceRevision` records this source-only review.

The local replay at
`results/three-items-20261018/codeql-source-review/verify.py` checks the unchanged
test functions, exact locations/messages/source bytes, original invocation
evidence and all retained reviews. It reports 0 blocked, 40 waived and 0 advisory
findings. The 47 offline policy, triage and source-identity regression tests also
pass. This is a static review and replay, not a new extraction or query run; the
next original analysis must independently confirm the exact identities. No
finding, test directory or query is newly excluded, and no remote alert changes.

## Source re-review: retained browser assets and release proof (2026-10-18)

Fresh local CodeQL 2.27.1 security analysis of
`27d3f03044fd041fa9f58a66d0a162942a0c48c1` executed all 50 Python queries.
Its original SARIF reproduced three existing reviewed findings with unchanged
queries and rendered messages, shifted primary ranges and new complete-file
identities. The flagged source bytes are identical to `e7660f9`; the changed
callers and outputs were independently reviewed. The original SARIF passes the
maintained triage helper after replacing only these three exact bindings.

- Alert 32, `build/release_container_harness.py:138`: exclusive `O_CREAT|O_EXCL`
  creation still defaults to mode 0600. New rollover HTML, scripts, metadata and
  configuration use that default in private fixture directories. Only the public
  one-day fixture CA and sanitized CI summary explicitly use mode 0644. The new
  browser summary has an exact field and value-type schema; keys, raw output and
  database dumps remain private.
- Alert 27, `build/release_container_fixture.py:171-175`: the unchanged
  `StrictUndefined` Jinja environment renders local dotenv, SQL, Compose and
  PostgreSQL configuration. The added proxy capability receipt is JSON outside
  Jinja. Browser fixture HTML is generated separately from bounded synthetic
  revisions and content-hash names. HTML escaping would corrupt configuration.
- Alert 31, `ops/ansible/tests/test_release_playbook.py:78`: the unchanged Jinja
  environment compiles tracked Ansible assertions to booleans with fixture
  values. Added receipt, ownership and upgrade-argument assertions have no HTML
  or browser output sink.

Exact replacement identities:

| Alert | Complete source SHA256 | Finding fingerprint |
| --- | --- | --- |
| 32 | `43423f31b39fc9f30a3114a4778d551c743bffa16820061011484a0f06f1952f` | `codeql:5f3f6e7ce7af4fcdaa61b2ded45e5e9be28422140985199603bfefd2ed097d78` |
| 27 | `072ef7e7851dc2d2232694d3f7f7224876391a4a8052977adce04309b0de24e8` | `codeql:5eb7c8f149fc0e6c44e5985d260562ed313c93a669d5a338f2d5a54020bc7f00` |
| 31 | `da212cc2a423f6b5c8026f76ce52dc5057434c1bc0af989fdb1a458166799272` | `codeql:18f0e41dbcd270ad728f67f86570f03da11e214c61639f1166fc1663e1341529` |

Original observation history, individual scope, owner and 2026-11-29 expiry are
retained. Source provenance and Git blobs identify the newly reviewed files.
No query, path or test exclusion was added, and no remote alert was dismissed.
This local analysis and SARIF replay do not claim hosted upload or ingestion;
current CI must independently satisfy those checks.

## Source re-review: bounded rollover failure diagnostics (2026-10-18)

Fresh original CodeQL 2.27.1 analysis of `8d2f3df3a86ff92ab3bdfa92d257e25e701754f5` executes all 50 Python security queries and changes only alert 32's exact identity. The complete `write_new` function is unchanged from `f475b09`; the flagged exclusive-create call moves from line 138 to 179. Its default remains 0600. The only explicit 0644 outputs remain the fixture's public one-day CA certificate and sanitized CI report.

The new failure report accepts only one at-most-1-KiB exact helper message and a fixed stage enum. Extra text, unknown stages, symlinks and oversized stderr yield `unavailable`. No child output, credentials or arbitrary error string enters this field. The changed callers, parser and public-report flow were reviewed; 49 Python mock and 40 Node tests pass.

Alert 32 now binds complete-source SHA256 `2528215712cfc3f97bfb4b91c6811fa26f201f028be3077481e9409fc08f0eb0`, range `179:179:18:75`, and fingerprint `codeql:3d3c9cd34cb0d0ba6c43f2d09da9871fd5074edf1be557cc935c5bc0caa6fbe8`. Original query, message, observation history, owner, scope and 2026-11-29 expiry remain unchanged. The original SARIF is replayed against this exact replacement; no query or path exclusion or remote dismissal is authorized. Hosted ingestion remains a separate required check.

## Source re-review: workspace layout and crate rename (2026-10-19)

Revision `61578db87ad7d0e1be8046008ac6a4582d78abaa` turns the repository into a Cargo workspace (the `simplestchat` library and server, `simplestchat-load-test` and `xtask`), moves the load generator's `include!`d sources to ordinary modules under `load_tests/src/`, wraps secrets in `SecretString`, narrows the crate's public surface and removes dead code. No flagged line changed: every rebinding below moves a region by whole-line shifts whose text is byte-identical at the reviewed base revision and at `61578db`. The recomputed identities were checked against four independent policy reports (hosted run 39198996777 at `a00c8df` for Rust and Python, local CodeQL 2.27.1 analyses of `61578db` for Rust and Python): 65 identities matched, none differed.

Alert 953 (`rust/cleartext-transmission`) now binds the module file `load_tests/src/account_session.rs` directly (range `274:274:19:62`, the `format!("{}/api/auth/refresh", self.origin)` request URL); the parent `load_tests/bin/load_test.rs` and its twelve-file include closure no longer exist, so the identity carries no `includedSources`. Alerts 64 and 65 (`src/turn.rs` test credentials) are no longer reported: those credentials now construct `SecretString`, which the hard-coded-value query does not trace; their records stay bound to the last reviewed bytes and expire with the others. Owner, rationale, scope semantics and the 2026-11-29 expiry are unchanged for every record. The local SARIFs replay against the updated exceptions with `passed` true; hosted ingestion remains a separate required check.

Complete-source SHA256 per rebound file:

- `build/release_container_harness.py`: `109b4d8ffce824671ca6992b942fe77d1034a22f479ef963d95869ae784f8d47`
- `load_tests/src/account_session.rs`: `d0eaf671e53beb81c763a8b0cfaa2c81b99e75d40603aecc93eeb15fcd92b05d`
- `src/auth/common_passwords.rs`: `d4324d430995b945ff02a258792f5baaed479d1c0bb3b2d69a5b11c1928237da`
- `src/auth/password.rs`: `7b0338401ce83eda21aa8e986baf2ee6e107578a1602daaddc36948755afd569`
- `src/auth/routes.rs`: `7ae411376021d9b46c2d476d31767809fb9e9eb67ac66c2533326aa8887dba59`
- `src/metrics.rs`: `773bd1884306327b8dedea8eee6f7e80de7207711384518a46cbb740e5477e85`
- `src/room/settings.rs`: `7b2bdfe34ce31a9246209ceea782644e97ba447ba5341b030a4333f39fc2d25d`
- `src/security_canary_tests.rs`: `4282b01b92cf22db047e87f37eb5cacba7617cac5fe2cdff8b6274f7c3aa03ae`
- `src/signaling/mod.rs`: `b045727d0f312367364a705004d41a6646074c3b2936d509439b43ce658f2809`
- `src/turn.rs`: `050dba94a642ac2d36aa3ee2d7aa5050805f485e60f584e6e74e7a8e47b22699`

| Alert | Range | Fingerprint |
|---|---|---|
| 32 | `build/release_container_harness.py:179:179:18:75` | `codeql:0422fbc40cb8ac4531e29355b005e2ac6f0be59fb67dbcbcc6d2cc24f2ce8987` |
| 33 | `src/metrics.rs:1159:1159:13:21` | `codeql:944df218d5a392de8291c4ff0ccc4306663cf88a6eab02d0879e34dfec8354ed` |
| 34 | `src/metrics.rs:1163:1163:13:21` | `codeql:27e6e2fed157505c33cf4a6a1da47add9faad4a122d6efa9f8619a3f2eee8e69` |
| 35 | `src/signaling/mod.rs:1864:1864:153:165` | `codeql:ecbca502bda84d5aff43830de1ec76d433bb20864ab3df68279e1ccbbfbeff2f` |
| 38 | `src/auth/common_passwords.rs:266:266:27:40` | `codeql:effa41367abe2f25c97d0b6c99d8752ffe62dba887159260a6fb746b6de848eb` |
| 39 | `src/auth/common_passwords.rs:267:267:27:42` | `codeql:c5ecc08a7a648cbc018bdafb3a24feab79557aaadcc4a8eddaac34054f5f77ca` |
| 40 | `src/auth/common_passwords.rs:268:268:28:58` | `codeql:ba2bd7dd5fee042f34d70e74ecfaa4a747d2219796ca90be5ccd1317af32f1db` |
| 41 | `src/auth/common_passwords.rs:269:269:28:51` | `codeql:3ddc2cd9248baaa0269c0dde9948ca37105f06598d40c8000e7198c608f226e8` |
| 42 | `src/auth/password.rs:55:55:26:51` | `codeql:96e95fc1f29e1130867f9d8dd58a8aec20e8b56007b5cc7e17ad2d7e76584c26` |
| 43 | `src/auth/password.rs:56:56:24:43` | `codeql:22648dccbde2c1533faa1fdc442e4c871cd310d13cd6da648f3196ac524468f7` |
| 44 | `src/auth/password.rs:61:61:42:62` | `codeql:03185972b84f9e305974f0ff5bd08d7099a7a2fbdaadbda3965c4c29359f3334` |
| 45 | `src/auth/password.rs:62:62:35:60` | `codeql:84375f4979e174b23d5a0292852ef5199117f35418ed7290b8f2f8c4f8e3b3ae` |
| 46 | `src/auth/password.rs:63:63:41:66` | `codeql:9e81f81ff0f10b16eef23658fc6c07d2d0b20bc77b2c796171a398472df15585` |
| 47 | `src/auth/password.rs:73:73:33:43` | `codeql:6942d2163da9a149e7aa10f1a3836602e61279bc60196e2e841f482da8cdd130` |
| 48 | `src/auth/password.rs:74:74:34:50` | `codeql:e748ea6c46346c60dd7eba209720aa4946b86387c12d7eea642eb9b4d104f60b` |
| 49 | `src/auth/password.rs:79:79:24:54` | `codeql:36df48f7808cba64590d0fe9d9d262ca7249bfd174e32c10eef3f64c0fa45844` |
| 50 | `src/auth/password.rs:82:82:34:50` | `codeql:3bdf0022cdc60cc596c9d303656145764bafa91a0dc902a07d4cd9441955922b` |
| 51 | `src/auth/password.rs:87:87:35:46` | `codeql:2672f757c091346eecb4d74fdbf7b298e2c3f8d9f1789f9a2a2c2cd0c1142c77` |
| 52 | `src/auth/password.rs:88:88:35:46` | `codeql:bfade2b93df3eced1ae4231b28d286e366e956293677b0923a2810fa4c107244` |
| 53 | `src/auth/password.rs:103:103:33:44` | `codeql:114b3a6a84fa11e39081441f6304a1a3109aa0ca9a255d4ceb26613fe67f80fc` |
| 54 | `src/auth/password.rs:104:104:33:44` | `codeql:cabe3addf56af59656ca6d877af9eb4e4bcdcd7eb12fa37efdc137c24149566c` |
| 55 | `src/auth/password.rs:109:109:34:36` | `codeql:1aa3a6c59dfb3bd7fed5f08389425991e0eb6056c2c1f983a3bc5f12ab7ec8ac` |
| 56 | `src/auth/password.rs:110:110:33:35` | `codeql:e030a63bd68d33e28c82a9c4dec9c0d5b74f7416afde596254208be002de4f4e` |
| 57 | `src/auth/password.rs:111:111:34:45` | `codeql:716cdd9ba8f50a57e460764992affd30e538119e5101d0f56053d16c131a17f8` |
| 58 | `src/auth/routes.rs:938:938:44:68` | `codeql:ded994331d3d73a944b7ac87f1b2a370bbaf7785dbb9289da683ea808484d8a9` |
| 59 | `src/room/settings.rs:605:605:33:42` | `codeql:1fcf2d03f22b246952847aaa629e79bbeb153d24c5bb38619d88d50e8578b418` |
| 60 | `src/room/settings.rs:610:610:33:43` | `codeql:337e8c69b8ba96c9b2229f589d7a59213f3fcf3bb083ba19e69cd66301a05a7b` |
| 61 | `src/room/settings.rs:612:612:33:43` | `codeql:6b14984c6e690f24c3bc31c609bf521bdac0b18fe2838baec76159c02013665e` |
| 62 | `src/room/settings.rs:615:615:33:45` | `codeql:0c00b0b31d8c6f7fc7cfff1f82c7a8b66882d96f7f06679658fec52f65adb652` |
| 63 | `src/signaling/mod.rs:2293:2293:31:56` | `codeql:abbafb7e47abceb8ede3ab2e0e01bfb4b98a631a84e6e538a23894f4a30b2144` |
| 66 | `src/turn.rs:139:139:49:59` | `codeql:56afdf505218139b48497f153e5cc3f6147f836a0247c16bfd89bb805ae81a5f` |
| 67 | `src/auth/common_passwords.rs:234:239:22:10` | `codeql:1df7a1f5a2f19d94de45b94d885ebeac0ec56a11fe9bfb43f073563b1a2e971f` |
| 68 | `src/auth/common_passwords.rs:243:248:22:10` | `codeql:45f234a486faa62c6aef4ca594c1a7dcc0995c6d35f8327e09ab6f3167300356` |
| 69 | `src/signaling/mod.rs:1855:1855:79:91` | `codeql:f8507fe451e15f5c5b9fb3b3225d110d0e0e88f066e1aae4bd3c2c53c86e9e79` |
| 102 | `src/security_canary_tests.rs:90:90:21:62` | `codeql:93594648d3260f9b5d31d6eda1bd77cd1c8802603ea35ecd4e844779585a19ae` |
| 953 | `load_tests/src/account_session.rs:274:274:19:62` | `codeql:0a072f64dd733d355d8e57a20da2d0f1aa4cd6513e9fe39544934a3b11373142` |

## Source re-review: typed configuration and named roles (2026-10-19)

The typed configuration (`4ca15a0`) removed the environment readers from `src/signaling/mod.rs` and `src/turn.rs`, and the named-role change moved two lines in `src/signaling/connection_authorization_tests.rs`, so the exact identities of six reviewed findings and the `src/room/dormant_join_tests.rs` exception moved by whole-line shifts; no flagged line changed. The recomputation was checked against the hosted Rust policy report of run 39206189199 (`a49f127`): every finding that report blocked or waived matched, and the dormant-join exception takes that report's identity directly. Owner, rationale, scope semantics and the 2026-11-29 expiry are unchanged; hosted ingestion remains a separate required check.

Complete-source SHA256 per rebound file:

- `src/room/dormant_join_tests.rs`: `e9d78ed118131605edc22ae4cd21ccc3ff246a96e2529f8f703897a2f30aae79`
- `src/signaling/connection_authorization_tests.rs`: `a9fa341e3da86e8a292a6676e1a865ed038d0f9395110614212a1a38cdc925eb`
- `src/signaling/mod.rs`: `538c77f84f239549fb6d4a2218c9b0a3d3ac9ed478c66b956e3468f6769172d8`
- `src/turn.rs`: `6a4b14b63a92c0dd55e5f72da0564efa901f7bad4f8b542a5e37c3835efe3cde`

| Alert | Range | Fingerprint |
|---|---|---|
| 35 | `src/signaling/mod.rs:1755:1755:153:165` | `codeql:54119f553c1725491241988aeab1dd1d5d33d70649e06648ef3744d49a7840d6` |
| 36 | `src/signaling/connection_authorization_tests.rs:398:398:21:62` | `codeql:d032ba09e7acf4ae83a5f48b218ee85b2083cce0c1b5a96331bb1dffe0039453` |
| 37 | `src/signaling/connection_authorization_tests.rs:445:445:17:51` | `codeql:7d6300a8eddb26ebd994cdd6abffc5fdecaec3288ad51f8ebed1c91728452a23` |
| 63 | `src/signaling/mod.rs:2186:2186:31:56` | `codeql:997564094e5e82df2718c53e513a715b39d049f9460bd593ccb97714a04f1c7c` |
| 66 | `src/turn.rs:137:137:49:59` | `codeql:07db376b287fd3e5002ee62da055cc5748c4dfb530a300caecf4b6b03fe70d36` |
| 69 | `src/signaling/mod.rs:1746:1746:79:91` | `codeql:1bcb648c98436f56e156a5170434de46202da566add251b549405331865cc3c5` |
| exception | `src/room/dormant_join_tests.rs:104:104:57:79` | `codeql:a653c7fd419f76d4f9924f7a28cb63264e71e5e21cd495ff67d4e0c6bffd671d` |

## Source re-review: typed social responses (2026-10-19)

The typed social response change (`b13c994`) made `RoomSettings` public in `src/room/settings.rs` so the protocol module can embed it in the public room snapshot. That edit changes one declaration line far from the four reviewed `cfg(test)` password-boundary literals, so the exact identities of alerts 59–62 rebind to the file's new complete-source SHA256 with unchanged ranges, messages, owner, rationale, scope semantics and the 2026-11-29 expiry. The flagged lines are byte-identical to the previously reviewed source; the recomputation follows the same line-mapping method as the earlier 2026-10-19 rebindings. Hosted ingestion remains a separate required check.

Complete-source SHA256 per rebound file:

- `src/room/settings.rs`: `30fd916a4e851ca6738f8b7264d36a922ceadff3b6242ee8e8813bf76b3934ff`

| Alert | Range | Fingerprint |
|---|---|---|
| 59 | `src/room/settings.rs:605:605:33:42` | `codeql:9d43688c3d854828f7221b4a142420abcf319849eb12fb0351100a708b03b302` |
| 60 | `src/room/settings.rs:610:610:33:43` | `codeql:1a3baf3792e223f94c968a944f32d0a4877495c2ea9724260fa9d36a1ddf3ec8` |
| 61 | `src/room/settings.rs:612:612:33:43` | `codeql:fe5628f3fe864517acedca3cb90c0066531f0a70b1b922605724977b4720ad94` |
| 62 | `src/room/settings.rs:615:615:33:45` | `codeql:6b9e12c415a4d0d4f57857c1608ee716240d937df8622c9e508457a151bf4752` |

## Source re-review: typed identifiers (2026-10-19)

The identifier refactor (`ff7dcbb`) retyped room, participant, message and account identifiers. In `src/room/settings.rs` the slug validator now delegates to `RoomId::new`, which moved the four reviewed `cfg(test)` password-boundary literals up by four lines; in `src/signaling/connection_authorization_tests.rs` typed imports moved the two reviewed test literals down by two lines; in `src/room/dormant_join_tests.rs` one fixture import moved the reviewed passphrase literal down by one line. Every flagged line is byte-identical to the previously reviewed source; the identities were recomputed with the same line-mapping method as the earlier 2026-10-19 rebindings, and the dormant-join exception's rule, tool version and message hash are those of the hosted report of run 39206189199. Owner, rationale, scope semantics and the 2026-11-29 expiry are unchanged; hosted ingestion remains a separate required check.

Complete-source SHA256 per rebound file:

- `src/room/dormant_join_tests.rs`: `e7dbb399e89850360a89d080b557ac81d1938a53971b89fbb22f49f192376e0f`
- `src/room/settings.rs`: `0de1cc388fb5c7741171298abdaf0d501fa16be633034a0eda61ab35b2b3bea4`
- `src/signaling/connection_authorization_tests.rs`: `b4d2e8203b1e4105d8612bf7d59b94f516530f0cbc75722a2443dfa8a7054b13`

| Alert | Range | Fingerprint |
|---|---|---|
| 36 | `src/signaling/connection_authorization_tests.rs:400:400:21:62` | `codeql:b32429a310d428dd1867181a654bea18670732cc0e3c2cc002fcfb4f115c62a8` |
| 37 | `src/signaling/connection_authorization_tests.rs:447:447:17:51` | `codeql:0964708386424b46303ca05b80e49e55a7961eb888fa7c2025dfcc40a3b2c1f4` |
| 59 | `src/room/settings.rs:601:601:33:42` | `codeql:1943573c70efa2d6b40cc938de81b1fcd3153e2f575c0846badc15273c7d0262` |
| 60 | `src/room/settings.rs:606:606:33:43` | `codeql:5dfa4efd47effbad417b40e0752a1068cc46f61c49d55d144eb8e3d20d23e1b8` |
| 61 | `src/room/settings.rs:608:608:33:43` | `codeql:7ff3c2baf5759e5c967ed8c6ba710b69e35bc48533894c14d6d4d9fe92def2ee` |
| 62 | `src/room/settings.rs:611:611:33:45` | `codeql:c982b68ddabd0f35629261cbd23c59abb19a12f7279a63ce31ccc71622640ab5` |
| exception | `src/room/dormant_join_tests.rs:106:106:57:79` | `codeql:ebc1cea4e4b9f17d3d3e0a91d189562bb62f9fe4bc71f6b465308253cb824c98` |

## Source re-review: room settings constructor and serde attributes (2026-10-19)

`216fde4` moved the default room settings into `RoomSettings::defaults` in `src/room/settings.rs` and `922ac30` added the omission attributes and the owner-id skip to the struct, which moved the four reviewed `cfg(test)` password-boundary literals down by 36 lines. Every flagged line is byte-identical to the previously reviewed source; the identities were recomputed with the same line-mapping method as the earlier 2026-10-19 rebindings (base `ff7dcbb`). Owner, rationale, scope semantics and the 2026-11-29 expiry are unchanged; hosted ingestion remains a separate required check.

Complete-source SHA256 of the rebound file:

- `src/room/settings.rs`: `bfdc90da1ca6ddd90920f17bf8a28af09241920ae76a2c2f2645924f5e95d60e`

| Alert | Range | Fingerprint |
|---|---|---|
| 59 | `src/room/settings.rs:637:637:33:42` | `codeql:3a713acbc2a4520c5536cdffec7d4e46b87f699fd03aae20598c20f9dcc35a34` |
| 60 | `src/room/settings.rs:642:642:33:43` | `codeql:63768c78dc001a9e9e96c112e9a2e65b1e2a75cf1219e9c6ddefa0e0f58247c7` |
| 61 | `src/room/settings.rs:644:644:33:43` | `codeql:05131f905d7b1cccceacde99c6d43f4c1b377426e2fab6cb58f6f6ca5f30a77c` |
| 62 | `src/room/settings.rs:647:647:33:45` | `codeql:663b1355a005d852f6f16df4e1b0af88ddc12bab18e45d995dafeebeba738cf4` |

## Source re-review: owner id reload fix (2026-10-19)

`8b56eed` changed the owner-id attribute of `RoomSettings` in `src/room/settings.rs` to skip serialization only (two more documentation lines) and appended a round-trip test after the reviewed `cfg(test)` module, which moved the four reviewed password-boundary literals down by two lines. Every flagged line is byte-identical to the previously reviewed source; the identities were recomputed with the same line-mapping method as the earlier 2026-10-19 rebindings (base `922ac30`). Owner, rationale, scope semantics and the 2026-11-29 expiry are unchanged; hosted ingestion remains a separate required check.

Complete-source SHA256 of the rebound file:

- `src/room/settings.rs`: `59b06c9750b48d681aa30bc1121ec96389d60ab797b3e4df10b8856c0f46d7bb`

| Alert | Range | Fingerprint |
|---|---|---|
| 59 | `src/room/settings.rs:639:639:33:42` | `codeql:96e9efbad37aa306558ce65f30852ff7a17040dfca76ca2c89a15f307d210942` |
| 60 | `src/room/settings.rs:644:644:33:43` | `codeql:8386448f1746eae63a1b312f6ba44d539f32cd37bbf866bbc25c21c4d3571aae` |
| 61 | `src/room/settings.rs:646:646:33:43` | `codeql:34eaa4b3879d063c8387728a6cc1535602fd16df218854f05cb852ab0a92ce17` |
| 62 | `src/room/settings.rs:649:649:33:45` | `codeql:e2c3eab59fcd8770ccad7bf18e8ca4f434f98466d492c7b0fd27bff55f0f66f5` |

## Source re-review: shared address cohort (2026-10-19)

`afab203` replaced the five identical IPv6 /64 address-identity functions with `net::cohort`; in `src/signaling/mod.rs` the removed `rate_limit_ip` and its identity test moved the reviewed lines (35, 63, 69) up. Every flagged line is byte-identical to the previously reviewed source; the identities were recomputed with the same line-mapping method as the earlier 2026-10-19 rebindings (base 4ca15a0). Owner, rationale, scope semantics and the 2026-11-29 expiry are unchanged; hosted ingestion remains a separate required check.

Complete-source SHA256 per rebound file:

- `src/signaling/mod.rs`: `46a0d2fb6703e33af6ee7f71529a4688f4b6c904dfe34fb866c4c2f1ec8eb8ed`

| Alert | Range | Fingerprint |
|---|---|---|
| 35 | `src/signaling/mod.rs:1733:1733:153:165` | `codeql:e4d3abdde1361dbabf349ca5117ed96922aa0ecef554a9aabdafba6a989b0727` |
| 63 | `src/signaling/mod.rs:2164:2164:31:56` | `codeql:81da0a0f50cb1c68618ac7d074369ebcfa46191b8877091105491481343645fd` |
| 69 | `src/signaling/mod.rs:1724:1724:79:91` | `codeql:32d000d21557ea7c9be5434762bfaf90b486304f3ec0530194b9fb7c9d7b41f4` |

## Source re-review: one settings patch type (2026-10-19)

`156dc9e` gave `settings::load_room` an executor parameter so the administration transaction reads the typed row instead of its own JSON loader, and `ff4ed3a` moved the `SettingsPatch` type and its `nullable` deserializer into `src/room/settings.rs` above the reviewed lines, so alerts 59–62 moved down 24 lines. Every flagged line is byte-identical to the previously reviewed source; the identities were recomputed with the same line-mapping method as the earlier 2026-10-19 rebindings (base 8b56eed). Owner, rationale, scope semantics and the 2026-11-29 expiry are unchanged; hosted ingestion remains a separate required check.

Complete-source SHA256 per rebound file:

- `src/room/settings.rs`: `9e4d1f3ecd98967ad923d4eefcf1f15ff69cace4445ff11c31e3cac3a3a384d5`

| Alert | Range | Fingerprint |
|---|---|---|
| 59 | `src/room/settings.rs:663:663:33:42` | `codeql:d99635d0f5df0867a67f158ed5a8a2f2c0de654423a6894fc2828439abaea9aa` |
| 60 | `src/room/settings.rs:668:668:33:43` | `codeql:e34a7b6718132fab97108415b14582bdf6a00a0863cad0c901f1dec5a2f2b4a9` |
| 61 | `src/room/settings.rs:670:670:33:43` | `codeql:c8b32581aa8089418b503c561051238061f22731f43990a05b2cdf5f6935322c` |
| 62 | `src/room/settings.rs:673:673:33:45` | `codeql:143e6b5562d1e857bf243cbad055166ca5939fa1a9bb75081c7260a4eb377dc7` |

## Source re-review: signaling security tests in a sibling file (2026-10-19)

`8b7a99f` moved the inline `security_tests` module of `src/signaling/mod.rs` to `src/signaling/security_tests.rs` (declared beside `authorization_tests`), so the three reviewed test-fixture findings now live in that file, two lines further down than their offset in the module body (the file's doc header) and four columns to the left (the module indentation). Every flagged line is byte-identical apart from that indentation; the identities were recomputed with the explicit relocation entries of the same tool that relocated alert 953 (base afab203). Owner, rationale, scope semantics and the 2026-11-29 expiry are unchanged; hosted ingestion remains a separate required check.

Complete-source SHA256 per rebound file:

- `src/signaling/security_tests.rs`: `d00dd474e013795d837835d45e6ab887ab540283f5abaddb0f43db066dd81099`

| Alert | Range | Fingerprint |
|---|---|---|
| 35 | `src/signaling/security_tests.rs:169:169:149:161` | `codeql:53a91bf4656726c03edcfdb9f95e1fb8f2ded2c774635428dcc93ea5b4fbc0df` |
| 63 | `src/signaling/security_tests.rs:600:600:27:52` | `codeql:8a2968846df11dbce42503727ccc8699873c791ff65018774f17244e495746be` |
| 69 | `src/signaling/security_tests.rs:160:160:75:87` | `codeql:e89d6f2ff22e7828b387aef0303755dac75f7d86b48257dd0735e7279f2b28d6` |

## Source re-review: capabilities test beside the signaling fixtures (2026-10-19)

`b7212bd` typed the public capabilities payload and rewrote the first test of `src/signaling/security_tests.rs` against the new struct, which moved the three reviewed fixture findings eight lines down; every flagged line is byte-identical. `20e28a4` recorded them at their old lines by mistake (a hard-coded relocation table); this section and the records now carry the lines found by text, the same method as the relocation, and the file's current SHA256. Owner, rationale, scope semantics and the 2026-11-29 expiry are unchanged; hosted ingestion remains a separate required check.

Complete-source SHA256 per rebound file:

- `src/signaling/security_tests.rs`: `149f091ff8e531a5c6eecff71594749cea1e65f87d8677e44a093047fd310212`

| Alert | Range | Fingerprint |
|---|---|---|
| 35 | `src/signaling/security_tests.rs:177:177:149:161` | `codeql:9b89bbc599bbdd31b2ee03bbd20cefc32cd023954961909a2d644ee24a4b023e` |
| 63 | `src/signaling/security_tests.rs:608:608:27:52` | `codeql:3d7ab5fb4d9946ccaebd4ca7a822c7fc4a703386384e3a815373dd7ae9740df9` |
| 69 | `src/signaling/security_tests.rs:168:168:75:87` | `codeql:e209a3ae5e761becfa49c632916399ce14e90fa6638d3d0506f3bdb9f130067e` |

## Source re-review: settings persistence on a query builder (2026-10-19)

`262484d` rebuilt `update_room_settings` on `sqlx::QueryBuilder` (47 lines shorter) and made `CreateRoomRequest` reject unknown fields, so the four reviewed test lines of `src/room/settings.rs` (alerts 59–62) moved 47 lines up; every flagged line is byte-identical to the previously reviewed source and the identities were recomputed with the same line-mapping method (base ff4ed3a). Owner, rationale, scope semantics and the 2026-11-29 expiry are unchanged; hosted ingestion remains a separate required check.

Complete-source SHA256 per rebound file:

- `src/room/settings.rs`: `d929d94c4bedd34efe01cc57441dc6e217e502f65d63334d31d68fe1a0436d1d`

| Alert | Range | Fingerprint |
|---|---|---|
| 59 | `src/room/settings.rs:616:616:33:42` | `codeql:ef74cc122b7eeec1599a499aa3ff9e44074d88da7c9fc8e39111c307337bc44e` |
| 60 | `src/room/settings.rs:621:621:33:43` | `codeql:8cf38fa9277784468a7eec886fa96a8a6a599d12a2b82d72e5691ebfa15ef748` |
| 61 | `src/room/settings.rs:623:623:33:43` | `codeql:366d6f928ab38d4001e68f14738c8fed814fb1e6884c25c144de81aa16489d06` |
| 62 | `src/room/settings.rs:626:626:33:45` | `codeql:4654d720ac739ee3c5d88ffda391d93ba9094a0788e890eefe3995d47b349861` |

## History rewrite: today's commits squashed by theme (2026-10-19)

The 77 commits of 2026-10-19 were rewritten into the 17 thematic commits below at the owner's request; every tree is a tree the original history also had, and the final tree is byte-identical to the original head (`0fbb7a58d6ea1296456e4091851a287b34855b67`). The sections above name the original commit ids; this table maps them. The review records' provenance revisions were rewritten to the new ids (fingerprints, scopes and file hashes are unchanged, so the exact reviews stay valid).

| Commit | Subject | Original commits |
|---|---|---|
| `2d82501` | build: form a Cargo workspace around the simplestchat crate and tidy the repository | `88e8ed0`, `a00c8df`, `ba010e1`, `e7172c2`, `73c0fa3`, `61578db`, `e050179`, `64596d2`, `dc64fbf`, `294206b` |
| `a3c3279` | refactor(config): read every setting through one typed Config | `4ca15a0`, `a49f127`, `546dc33`, `9e49249`, `ffb3052` |
| `006c968` | refactor(protocol)!: carry roles by name, give refusals error codes and type chat timestamps | `a2f2291`, `b1e6c89`, `3586755`, `98dcb4f`, `c3fb3cf`, `0016bad` |
| `d2791f6` | fix(deploy): compare the browser asset mount by its reviewed fields | `353c5e7`, `126386a`, `c3e7b6e` |
| `7906521` | chore(security): review the Fedora vim refresh and the rpm database regions it moved | `84ba4d8`, `62e37b1` |
| `76e1113` | refactor(protocol)!: type social responses and identifiers | `b13c994`, `cc94261`, `ff7dcbb`, `732867c` |
| `60d388e` | feat(protocol): generate the TypeScript contract from the Rust wire types | `5fe48e1`, `510c2aa`, `216fde4`, `922ac30`, `421ce3e`, `4d90826`, `8b56eed`, `50af78a`, `7f59901` |
| `42ce6c3` | refactor: one sliding window, address cohort and token bucket for the server's limits | `a744883`, `d265619`, `afab203`, `ddb844e`, `f36ccd7`, `d3d8317` |
| `041e9ec` | refactor(room): one typed settings loader and one settings patch | `156dc9e`, `ff4ed3a`, `0c266bf` |
| `07a7e41` | refactor(room): split the room manager, the social dispatcher and the route tree | `9bdb3cf`, `5b13433`, `612cc18`, `3df8ae5`, `8b7a99f`, `402e1e5`, `cf139bd`, `add9f78`, `34eefa4`, `4dca954` |
| `167a4e5` | refactor(protocol): declare field renaming once per enum and type the capabilities payload | `b7212bd`, `20e28a4`, `87b4981`, `44331df`, `56f85e5` |
| `aa797ef` | refactor(signaling): one violation window and one operation vocabulary | `5018454`, `629d765` |
| `19dd0a9` | refactor(room): name the persistence rows and state each rule once | `a1bda60`, `262484d`, `c324551`, `77d02a5`, `a8da2cb`, `fd713bb`, `14f9e88`, `ca9a1ee` |
| `77871e2` | fix(ops): confirm the seeded room's owner through the owned-rooms listing | `ac758ea` |
| `0dbe170` | fix(web): own a repeated Back while its history traversal is pending | `07049b2` |
| `0510e91` | refactor(protocol): refuse unknown chat treatments instead of showing them as accent | `b19f471` |
| `5ad95e3` | refactor: narrow the library's public surface and drop what nothing used | `0fbb7a5` |

## Source re-review: poison-tolerant lock wrappers (2026-10-20)

`f2c9572` replaced every per-site `unwrap_or_else(|e| e.into_inner())` recovery with
`crate::sync::{Mutex, RwLock}`. The sweep removed lines above reviewed findings in
`src/metrics.rs` (20 lines), `src/signaling/security_tests.rs` (3) and
`src/room/dormant_join_tests.rs` (4); no flagged line changed, and each flagged range was
verified by text at its new line. The dormant-join record lives only in the exceptions
ledger (its identity came from the run-39348649191 policy report) and was recomputed with
the same identity function; the others were rebound with the line-mapping tool.

Complete-source SHA256 per rebound file:

- `src/metrics.rs`: `0dd671b4e9e554ed2ca7db5d6de7739552b288d46684db9cbc5ddebb4f87e55b`
- `src/signaling/security_tests.rs`: `77e2e49ab31070d917d0c1cd93c8049ec8e42b9ef367fc22b1d7064974687466`
- `src/room/dormant_join_tests.rs`: `b4dc3ef6b41d56e62f5bd76c10356fc7fa99ebceeac4588abdb8abd5ae667bee`

| Alert | Range | Fingerprint |
|---|---|---|
| 33 | `src/metrics.rs:1139:1139:13:21` | `codeql:ce9a3344d757493849e7d92070e76e9bff583aced680eb46b1b81d072aff66e3` |
| 34 | `src/metrics.rs:1143:1143:13:21` | `codeql:14e51d4b7f28c26c00f31dfa87dd814221ce5a49ebeeb7f14dd47c2a82eb115c` |
| 35 | `src/signaling/security_tests.rs:174:174:149:161` | `codeql:84d47a452fdd2f905bde48ccb8d851c263dd012a25f6174f2b5c5a2f95f69a01` |
| 63 | `src/signaling/security_tests.rs:605:605:27:52` | `codeql:2b7a354c01f3045e6efc419e5a41e8a8e8a8b273ec28f7f5ab1269f9c449cda4` |
| 69 | `src/signaling/security_tests.rs:165:165:75:87` | `codeql:3cc46216401cbf199caa79d88880e1616d823da2357b6f7b687808c4272f0cf7` |
| dormant join | `src/room/dormant_join_tests.rs:102:102:57:79` | `codeql:783d7a6196a1d66de55a9cb7c08e3e3509c315dd71536ca7bfaf4e0be29ff03b` |

## Source re-review: settings refusals typed (2026-10-20)

`3320351` replaced the quota sentinel strings and their classifiers in `src/room/settings.rs`
with typed refusals, which moved the four reviewed test-fixture findings fifteen lines up; no
flagged line changed (verified by text at the new lines). Rebound with the line-mapping tool.

Complete-source SHA256 per rebound file:

- `src/room/settings.rs`: `ed06889dac33135b0fc1133bc2c3f62bafd02aef3ef473e450aee7b73b07bcd9`

| Alert | Range | Fingerprint |
|---|---|---|
| 59 | `src/room/settings.rs:601:601:33:42` | `codeql:c161ed379d7507af5f1845156eee4eba620ae8e9b8482015bc13bac500baa500` |
| 60 | `src/room/settings.rs:606:606:33:43` | `codeql:a788626023399a6f68538e55aef7a6cb75f6c0d2b126d8e5bf536236b1f11d18` |
| 61 | `src/room/settings.rs:608:608:33:43` | `codeql:5f26c9f7ef67b3049aaf4306d7dfeee13b0f3aaaddb91c1d0d8592ada5c1196d` |
| 62 | `src/room/settings.rs:611:611:33:45` | `codeql:e9f3e8158d018c450064f7671e35a74470d87cfa46f9049c7133c3eee56caa4b` |
