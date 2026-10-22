# Initial security review records

These notes support the exact records in `exceptions.json`. They are not a
blanket acceptance of a scanner, package, directory or severity. The initial
review uses the supplied security assessment and the checked-out sources at
`2bd6d38215ddc4217b3f6cde0bd8fa30d93ce1cb`, with new scanner results reviewed on
2026-09-30. Records expire on 2026-11-29 and remain visible in gate summaries.
The repository owner is responsible for replacing the dependency or renewing a
specific assessment with current evidence.

## Public source fixtures

The initial Gitleaks findings were exact public values in isolated JWT/connection
tests, the RFC 6455 example handshake nonce, upstream mediasoup documentation and
tests, and two scalability-mode enum strings. They are not credentials enrolled
in any service. Each exception binds both the complete value and its original
file path. A different value in the same test, or that value in another file,
does not inherit the exemption.

The primary source scan uses neutral filenames so upstream default filename
allowlists cannot omit lockfiles, minified sources or binary extensions. The
private evidence map retains the original path and source digest. The reviewed
fixture path is translated to one exact neutral filename; a repository-wide
value exemption is never generated. The supplementary Git range scan retains
the original paths and includes redaction as well.

## GitHub Actions portability findings

Ten initial Zizmor findings have narrow records:

- Two informational `superfluous-actions` findings refer to the pinned Rust
  toolchain installer. The maintained local `act` image must bootstrap the
  repository toolchain; it does not have the same preinstalled tools as a hosted
  GitHub runner.
- Eight low-severity `self-repository` findings refer to six specific invocations
  of the maintained native-toolchain composite action and two same-revision
  reusable workflow calls. GitHub supports the newer
  `$/.github/actions/...` syntax, but the supported `act` 0.2.89 parser recognizes
  only `./` as a local action. The exact action source is obtained by checkout
  without persisted credentials. No step replaces the checkout with another
  repository before invoking the composite.

Each record binds the audit ID, hash of the actual action feature and complete
symbolic workflow location. An added use, changed action reference or moved
step requires review. The two high-severity `GITHUB_ENV` findings were corrected
by declaring the OpenSSL paths in workflow environment mappings. Missing
concurrency controls were also corrected; neither category is exempted.

The scheduled mutation job adds one exact `self-repository` review for its
native-toolchain invocation. It uses the same checked-out action and has no
intervening source replacement. Its separate mutation cache namespace and
private compiler outputs do not supply production release artifacts. The
existing local-action syntax rationale applies to this additional call site.

On 2026-10-03, two additional informational `superfluous-actions` findings were
reviewed for step 2 of `security-fast` and `security-deep` in `security.yml`.
The actual local fast job at `984c4d8` failed before its shared checks because
`rustup` was absent; both jobs previously invoked it directly. The existing
`dtolnay/rust-toolchain` action at
`6bed0761d98439e5a578e2877258200ad565ba87` bootstraps missing rustup, exports its
binary path and installs the explicitly selected Rust 1.98.1 toolchain. Its
bootstrap uses the official HTTPS rustup installer; this review does not claim
that the installer bytes are independently checksum-pinned. The two records
bind the action reference and each exact job location, retain the 2026-11-29
expiry and leave the shared checks blocking.

The later change to optional local vendor analysis removes the `security-deep`
hosted job and its obsolete review. Of those two historical records, only the
`security-fast` installer review remains active.

On 2026-10-05, the same informational installer finding was reviewed for
`codeql.yml#jobs/source-analysis/steps/5/uses`. The Rust matrix entry installs
1.98.1 only when its exact evaluated database is absent. The maintained local
`act` runner needs this bootstrap before Cargo-backed extraction. The action
remains pinned to `6bed0761d98439e5a578e2877258200ad565ba87`; the version matches
`rust-toolchain.toml`. The official HTTPS rustup bootstrap has the same supply
boundary described above. This additional exact record expires on 2026-11-29;
it does not exempt extraction, query completion, cache validation or findings.

On 2026-10-21, compiled CI fingerprints and local execution receipts require
the pinned Rust toolchain before cache lookup and native-library setup. The
maintained local runner has no rustup; calling `rustup` directly cannot supply
that bootstrap. Six exact installer-action locations were reviewed: the
`automation`, `rust-lint`, `rust-test`, `browser` and local-only `required` jobs
in `ci.yml`, and local-only `source-analysis` in `codeql.yml`. Each uses the
already-reviewed immutable `89b12181fb390509a0842a86cc55eeb8eb928c1d` action with
explicit Rust 1.99.0. Checkout still disables persisted credentials; repository
source fixes all installer arguments. The official HTTPS bootstrap boundary
and 2026-11-29 expiry remain unchanged. Three existing local-native-action
review locations and the existing CodeQL Rust installer location move by one
step; their action bytes, fingerprints and scope are otherwise unchanged.
Receipt verification and all source/security gates remain blocking.

The compiled required-job policy moves that job's installer to
`ci.yml#jobs/required/steps/1/uses` and also uses it on hosted runners. Checkout
still disables persisted credentials, followed by the same immutable action and
explicit Rust 1.99.0. No token is supplied to compilation. The policy requires
success from exactly the ten declared correctness/security jobs; hosted stored
CodeQL health and local execution evidence remain separate blocking checks.
The official HTTPS bootstrap boundary, action fingerprint and 2026-11-29 expiry
are unchanged. Added hosted compilation cost has not been measured on GitHub.

The cache-retention port adds one exact installer-action review at
`cache-retention.yml#jobs/retain/steps/1/uses`. It uses that same immutable action
and explicit Rust 1.99.0 to build the maintenance binary before the scoped
GitHub token is supplied. The successful same-repository main-push trigger,
credential-free checkout, API allowlist, complete-inventory checks and exact-ID
deletion limits remain enforced. The official HTTPS bootstrap boundary and
2026-11-29 expiry are unchanged; this record does not authorize a wider action
scope or weaken maintenance authorization.

The analyzed-database cache key now uses compiled tooling on hosted Rust jobs.
The existing exact `codeql.yml#jobs/source-analysis/steps/1/uses` installer review
also covers that Rust matrix entry before cache lookup, while retaining local
receipt bootstrap for the other matrix entries. The redundant installer on a
database miss and its old step-6 review are removed. The immutable action,
explicit Rust 1.99.0, credential-free checkout, official HTTPS bootstrap boundary
and 2026-11-29 expiry are unchanged. A database hit still avoids fresh extraction
but now incurs the tool build; this review makes no hosted cold-time claim.
Restored databases still require source/receipt validation, regenerated reports
and current policy.

## Account-room action reference

On 2026-10-18, the new manual-only account-room experiment produced one
low-severity `self-repository` finding at
`performance.yml#jobs/account-room/steps/1/uses`. Its exact feature is
`./.github/actions/native-toolchain`, with SHA-256
`7d65041123978cf420877842e3f127995a022f851b87c4b322a0a4ea398c9df6`.

[GitHub's July 30 announcement](https://github.blog/changelog/2026-07-30-reference-same-repository-actions-with-self-repository-syntax/)
confirms that `$/` resolves an action from the running commit. Replacing this
single call with `$/.github/actions/native-toolchain` removed the new finding
under checksum-verified Zizmor 1.30.1, but checksum-verified actionlint 1.7.12
rejected that reference as an action missing its ref. The supported syntax is
therefore blocked by the maintained syntax checker. This review retains the
existing `./` form at this one call site until that checker supports `$/`.

The job requires both `workflow_dispatch` and `only=account-room`. Its pinned
checkout disables persisted credentials and supplies the exact running source;
no step replaces that checkout before the composite runs. The action receives
only the fixed cache namespace. Job permissions inherit `contents: read`, and
the workload creates isolated loopback accounts, PostgreSQL and media resources
without production or signing credentials. The exact finding, feature hash and
symbolic location expire on 2026-11-29. Other locations, changed action source
references and all remaining workflow checks require their own assessment.

## Account-room public-origin CodeQL flow

On 2026-10-18, [alert 953](https://github.com/swiftraccoon/simplestChat/security/code-scanning/953)
reported `rust/cleartext-transmission` under CodeQL 2.27.1. The recorded path
starts at the whole local `account` container in `load_test.rs:3641`, enters
`AccountSession::refresh`, and reaches `format!(self.origin)` as a request URL.
The pinned query's account-name heuristic taints the container and receiver; it
does not show a password, token or cookie flowing into that URL. This query
has no loopback exemption.

The request URL contains a validated public origin and the fixed
`/api/auth/refresh` path. Account workloads require a plain literal loopback
IPv4 WebSocket endpoint. The included account helper also rejects userinfo,
query, fragment and paths other than `/ws`, and its HTTP client disables
redirects and proxies. The manual hosted experiment creates its own local
accounts and disposable server/database. Fixture passwords and cookies are
intentionally transmitted in separate request bodies or headers over this
loopback transport; this review approves the reported public-URL taint in that
owned fixture and does not approve remote cleartext authentication.

Rust extraction maps the sink to the parent `include!` at
`load_tests/bin/load_test.rs:57`. A parent-file hash alone would fail to bind
the actual helper's guards. CodeQL identities now also hash the complete,
bounded closure of committed literal Rust includes. Both SARIF and stored
alert checks use that identity. Unsupported dynamic includes, cycles, escaped
paths, edited files and symlinks fail closed. A module-only committed edit
changes the fingerprint and loses approval; a focused regression proves this
for both gate paths. Existing reviewed Rust files have no include closure and
retain their previous identities.

The exact record in `codeql-review-2026-09-30.json` includes the parent hash
and all 12 included-file hashes, including `account_session.rs`. Its new
fingerprint is
`codeql:c5ab569d37088053fc385a7863c99df0b53b6050339199be1f020146baea10c9`;
the exact scope is `load_tests/bin/load_test.rs:57:57:5:47`. The review expires
on 2026-11-29. It adds no query exclusion and authorizes no remote dismissal.

On 2026-10-19 the workspace change (`61578db87ad7d0e1be8046008ac6a4582d78abaa`)
made the helper an ordinary module, `load_tests/src/account_session.rs`; the
parent `load_tests/bin/load_test.rs` and its `include!` closure no longer exist.
The record now binds that module file's complete hash and the sink's own range
`load_tests/src/account_session.rs:274:274:19:62` (the request-URL `format!`), with fingerprint
`codeql:0a072f64dd733d355d8e57a20da2d0f1aa4cd6513e9fe39544934a3b11373142`.
Rationale, owner, scope semantics and the 2026-11-29 expiry are unchanged, and the
include-closure identity stays in force for any future literal include.

### October 18 account-conversation source refresh

CodeQL 2.27.1 run
[39096512724](https://github.com/swiftraccoon/simplestChat/actions/runs/39096512724)
analyzed `428576bda5f78167f169913940dcd9458539b5a4` and reported the same
public-origin flow, rule, message digest and `57:57:5:47` primary range. The
complete delta from the prior reviewed source adds only
`account_conversation: None` to one `cfg(test)` `ChatEntry` constructor in
`load_test.rs` and one in included `chat_load.rs`. The parent SHA-256 is now
`ffd79270fa38b8e5a845b2cf3131ea2253af99d52eac8553e9dfba85d0fedeac`;
`chat_load.rs` is
`6c62b897f5d14bce4925118ca4edfa8ca073621ee7010c1fed596f436cb75442`.
`account_session.rs`, its endpoint validation, credential handling and refresh
request are byte-identical, as are the other ten included files. The actual
reported URL still contains only the validated public loopback origin and
fixed refresh path.

The replacement fingerprint is
`codeql:74802ebd681ace190d6a54d6fa04bd7803b18c1f0580b0e5ce2ebb0530c73275`.
The review binds all 12 included-file hashes and source revision
`673c93fb2d09b5cdc932de2871c5493b26eb5186`, preserves the original rationale,
owner, scope and 2026-11-29 expiry, and removes the superseded fingerprint.
This is an exact identity refresh of the previously reviewed owned fixture;
it does not approve remote cleartext credentials or add a general test/query
exclusion. Static policy replay is distinct from a new analyzer run.

## Cache retention trigger

On 2026-10-05, the high-severity `dangerous-triggers` finding for
`cache-retention.yml#on` was reviewed against the complete workflow and
`build/ci_cache_retention.py`. Its feature SHA-256 is
`60b335d5888cbd8f0bfc149534de1747011b3fb170702cb69d34688fb75731ec`.

The `workflow_run` event matches completed CI on main. Before checkout, the job
also requires this repository, a successful push event, main as the source
branch and this repository as the source repository. It therefore does not
execute fork or pull-request code with the maintenance token. Checkout is
pinned and selects the triggering SHA without persisted credentials. No
artifacts or cached code are downloaded or executed by this workflow.

The helper independently fetches the run from GitHub and requires its exact ID,
CI workflow name and path, successful completed push status, both repository
identities, main branch and requested SHA. It rejects a SHA that is no longer
current main, including before each proposed deletion. It then re-fetches the
inventory, re-plans retention and checks the cache ID, key, ref, version, size,
creation time and recent access. Unknown families, other refs, the latest entry
in each namespace and recently used or created entries remain protected.
The fixed API allowlist can delete only cache IDs in this repository, with a
100-deletion ceiling and a 240-second deadline. No source, run or artifact
endpoint can be deleted. A concurrent GitHub eviction cannot be locked by this
helper; refreshed retention protects the remaining current copies before each
request, and the objects involved remain disposable build caches.

The job grants `contents: read` and `actions: write` for this narrow operation;
its helper defaults to dry-run outside the explicitly applying workflow.
`test_ci_cache_retention.py` exercises the trigger guards, rejected run states,
stale main revisions, complete inventories, recent-use protection, re-planning
and forbidden deletion endpoints without issuing API mutations. The review
expires on 2026-11-29 and covers only this exact trigger feature and location.
Changing the guards, checkout provenance, token scope or helper deletion
boundary requires renewed assessment; no other trigger finding is suppressed.

## Scheduled check notification trigger

On 2026-10-15, the `dangerous-triggers` finding for
`scheduled-check-alerts.yml#on` was reviewed against the complete workflow and
`build/scheduled-check-alerts.mjs`. The completion trigger names seven maintained
scheduled workflows and filters main. The job additionally requires this exact
repository, its main branch and a scheduled or manually dispatched source run.
Checkout selects trusted main with persisted credentials disabled. No triggering
source, artifacts, logs or caches are downloaded or executed.

The helper independently verifies the run's repository, source repository,
workflow name/path, ID, attempt and commit through GitHub before using its outcome.
It selects the latest completed result and requires every full-workflow job
before closing an issue, preserving failures through diagnostic-only successes. The
token can read contents and run metadata and write issues. Its fixed API routes
can create or update only this repository's issues; an update requires the exact
workflow marker on an issue authored by `github-actions[bot]`. Requests refuse
redirects and have time and response-size bounds. No shell command contains
trigger-provided text. Notification tests cover trust boundaries, out-of-order
events, reruns, repeated failures, recovery, partial success and transport limits.

## Rust dependency health

The audit distinguishes these health warnings from vulnerability-class results:

| Package | Advisory | Review basis |
| --- | --- | --- |
| `atty 0.2.14` | RUSTSEC-2024-0375 | Unmaintained terminal detection in native build dependencies |
| `atty 0.2.14` | RUSTSEC-2021-0145 | Windows handle-alignment issue; supported production target is Linux |
| `instant 0.1.13` | RUSTSEC-2024-0384 | Unmaintained target-specific timing dependency |
| `paste 0.1.18` | RUSTSEC-2024-0436 | Unmaintained build-time procedural macro dependency |
| `basic-toml 0.1.10` | RUSTSEC-2026-0374 | Unmaintained template configuration parser, reached only through the vendored worker's flatbuffers code generation at build time |
| `rand 0.7.3` | RUSTSEC-2026-0097 | Narrow custom-logger/reseed interaction; no affected production path was identified in the supplied assessment |

The gate runs against the current advisory database and permits only the exact
advisory/package/version triples. Another advisory for the same crate or another
version fails. Warning-class findings remain counted, rather than being hidden
with a global command-line ignore list. These reviews do not establish that an
unmaintained dependency is safe indefinitely.

The duplicate budget is an explicit inventory of the reviewed Cargo graph.
`cargo deny` also checks licenses, allowed source origins, wildcard dependencies
and the forbidden bundled-OpenSSL crate. Duplicate reductions pass; new groups
or versions require a focused budget review. No dependency subtree is ignored.

### Local-CI workflow parser (2026-10-21)

The `xtask ci-evidence` migration pins `serde-saphyr 2.0.0` with only its
deserializer feature. The [upstream release](https://docs.rs/crate/serde-saphyr/2.0.0)
and packaged source were reviewed: this maintained parser uses `granit-parser`
and forbids unsafe code in its own library. That statement does not cover its
transitive dependencies. The dependency is confined to repository automation;
it adds no production server or browser dependency and needs no advisory waiver.

The caller first bounds each regular, nonsymlink workflow document to 1 MiB.
Its [parser options](https://docs.rs/serde-saphyr/2.0.0/serde_saphyr/) explicitly
limit structural depth to 64, aliases and anchors to 1,024 each, retained and
replayed events to 100,000, and alias replay depth to 32. Only one YAML document
is accepted. Include features are disabled and custom tags are rejected.
Last-wins duplicate keys, YAML 1.1 booleans and merge keys retain the Python
safe-loader policy; duplicate receipt publication and unknown receipt fields
remain rejected. Tests compare real workflow receipts byte for byte with the
Python reference and exercise matrix drift, stale identities, aliases, merges,
duplicate keys, recursive aliases, depth limits, excess aliases, custom tags
and multiple-document refusals. No dependency warning is suppressed.


### Dependency refresh (2026-10-05)

The update from `e0bff7aef860a407bf9718a891e0b34a9960ceeb` keeps the
source/license policies and exact duplicate-version budget. Latest rtc and
rtc-dtls 0.21 require pem 3.0.6 while their latest rcgen 0.14.10 requires pem
4.0.0; this split is confined to the optional load-test client. Current ICU
derives require synstructure 0.14.0 while asn1-rs-derive 0.5.1 (used by the
current WebAuthn certificate parser) still requires 0.13.2. Those two exact
splits are reviewed; disappeared groups and obsolete skip entries are removed.
The remaining groups retain their upstream-required version lines, with the
reviewed compatible patch releases recorded in the exact budget. No wildcard
allowance or dependency-advisory suppression is added.

The immutable Rust installer action was reviewed at
`89b12181fb390509a0842a86cc55eeb8eb928c1d`. It uses repository-controlled
version/component inputs and the existing official HTTPS rustup bootstrap;
its new retry loop is bounded to five attempts for official-server checksum
propagation errors. The four existing portability exceptions retain their
original scopes and expiry.

Historical native CodeQL findings and public-fixture exceptions remain bound
to their original 0.17.0/0.27.0 sources. They do not transfer to the upgraded
vendor tree. Source scanning of that tree remains optional local work.

### Exact legacy Python license metadata (2026-10-05)

basedpyright 1.40.2 and Meson 1.12.1 omit `License-Expression`. Their
checksum-authenticated PyPI wheels were downloaded and the packaged license
texts reviewed. The narrow mapping in `build/security_dependency_licenses.py`
binds package name, version, complete PEP 658 metadata SHA-256 and locked wheel
SHA-256. Unknown declarations still fail; no general classifier or free-text
license inference is introduced. The normal license allowlist still applies.

| Release | Packaged license | License-text SHA-256 | Interpretation |
| --- | --- | --- | --- |
| [basedpyright 1.40.2](https://pypi.org/project/basedpyright/1.40.2/) | `basedpyright-1.40.2.dist-info/licenses/LICENSE.txt` | `f7c936bc43f132b08497ac952e9376cbc102e5eedb4bf6ec902ea8442bd9c68d` | MIT |
| [Meson 1.12.1](https://pypi.org/project/meson/1.12.1/) | `meson-1.12.1.dist-info/licenses/COPYING` | `cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30` | Apache-2.0 |

The existing standalone Ansible linter is GPL-3.0-or-later. Its 26.9.0
controller-only update has a separate exact identity review, confined to
`ops/ansible/requirements.txt`; it is not linked into or shipped with the server.
The authenticated wheel's `ansible_lint-26.9.0.dist-info/licenses/COPYING`
SHA-256 is `3972dc9744f6499f0f9b2dbf76696f2ae7ad8af9b23dde66d6af86c9dfb36986`.
The check binds its wheel and metadata hashes, manifest, version and expression.
It does not add GPL to the application/image license allowlist or authorize
other packages, versions or artifacts.

### Python CodeQL identity review after dependency pins (2026-10-05)

Run [37268650754](https://github.com/swiftraccoon/simplestChat/actions/runs/37268650754)
reported three prior Python findings with changed whole-file identities.
The complete `e0bff7a..3db0a47` delta in `test_automation.py` changes only four
expected runtime package versions; both non-HTML Jinja uses and their fixed
fixture inputs are unchanged. The delta in `release_container_harness.py`
changes only the PostgreSQL and Caddy digests; `write_new` and all eight calls
are unchanged. Only the public fixture CA and sanitized report use mode 0644;
private evidence retains mode 0600 and exclusive creation. Those three exact
reviews are rebound to the current CodeQL report and independently checked file
hashes, retaining the original scope and expiry. No source finding is hidden by
a test-directory exclusion.


The 2026-10-05 Planus 1.3 and current async dependency upgrade removes 16 obsolete
Cargo declaration records and replaces seven with authenticated current releases:
Planus/codegen/lexer/translation/types 1.3.0, ascii-canvas 4.0.0 and string-interner
0.19.0. Their locked archive checksums, packaged manifests and complete MIT/Apache
notices match the retained evidence. The review now contains 25 current records;
no older package identity remains allowed by that evidence. The duplicate budget
shrinks from 41 to 31 groups: exact bit-vec 0.8.0, foldhash 0.1.5 and hashbrown
0.15.5 branches follow bit-set 0.8 and string-interner 0.19's published constraints.
Nineteen obsolete version exemptions are removed from `deny.toml`; no wildcard
version or source exemption is added.
