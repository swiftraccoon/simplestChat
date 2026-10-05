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

## Rust dependency health

The audit distinguishes these health warnings from vulnerability-class results:

| Package | Advisory | Review basis |
| --- | --- | --- |
| `atty 0.2.14` | RUSTSEC-2024-0375 | Unmaintained terminal detection in native build dependencies |
| `atty 0.2.14` | RUSTSEC-2021-0145 | Windows handle-alignment issue; supported production target is Linux |
| `instant 0.1.13` | RUSTSEC-2024-0384 | Unmaintained target-specific timing dependency |
| `paste 0.1.18` | RUSTSEC-2024-0436 | Unmaintained build-time procedural macro dependency |
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
