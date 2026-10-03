# Security checks and release enforcement

Security evidence is layered. Source scanners inspect declared dependencies and
specific code patterns; authorization tests exercise application decisions;
native sanitizers inspect runtime behavior; image checks inspect the bytes that
will be shipped. A passing layer does not substitute for another. The repository
uses the same maintained entry points locally and in GitHub Actions.

## Run the shared checks

Start from the repository root with Python 3.12 or newer, Git, ShellCheck, Node/npm
and the Rust toolchain declared in `rust-toolchain.toml`. The shell entry point
selects that Rust toolchain explicitly. It installs authenticated scanner binaries
and a separate hashed Python environment under ignored `target/` directories.
The first run needs network access to the pinned distributions and current
advisory databases. Installation and database failures fail the check.

```sh
build/check-security.sh fast
build/check-security.sh fast --base FULL_ANCESTOR_COMMIT_SHA
build/check-security.sh deep
build/check-security.sh image sha256:FULL_IMAGE_ID
```

`--base` adds the explicit committed Git range to the source and uncommitted-diff
secret checks. It must be a full ancestor commit SHA. CI supplies the PR base or
previous push revision. It is not a replacement for scanning the current tree.

Each invocation creates a new private `results/security.<tier>.*` directory.
Use `--output /private/new-directory` to select a different, nonexistent output
directory. The final `summary.json` records the tier, each completed command,
status, elapsed time and any fixed failure identifier. A failed process, omitted
report or incomplete check cannot become a passing summary. Raw reports remain
private and are not printed to the terminal.

The source gate records the initial and final file inventories and revision.
Concurrent source changes fail that run; rerun after edits settle. Ignored build
outputs, local credentials and operator files are outside the source inventory.
Do not store maintained source or scanner policy under an ignored path.

The deep tier runs the source gate, verifies vendored source, then runs both
native and policy mutation checks. Native checks need an explicitly owned Docker
engine; `--engine podman` selects a local Podman engine. They build the pinned
checker, run ASan and UBSan, then replay the finite reviewed parser corpus. See
[native security](native-security.md) for resource limits and input provenance.
They do not initiate a continuous fuzzing campaign or contact a deployment.

Mutation checks require the documented native compiler, locked Cargo cache and
checksum-pinned static OpenSSL prefix. `--openssl-prefix` defaults to `OPENSSL_DIR`
or the checkout's `target/openssl-3.5.9`. The runner builds a private source copy
and measures assertions over selected pure role, label and password policies.
See [mutation checks](../security/mutation/README.md) for platform support, limits
and failure interpretation. Unsupported tool platforms fail explicitly.

Use `--deep-check native` or `--deep-check mutation` to run one component while
retaining source and vendor verification. The default is `--deep-check all`.
Scheduled CI runs the two components in separate bounded jobs. A component-only
summary identifies its scope and does not establish a complete deep-tier pass.

The image tier needs the selected immutable production image on a Linux amd64
Docker host and a clean source checkout. It exports that exact image once. When
the canonical exporter already ran, supply
`--artifact-dir /private/canonical-release` to scan those same archive bytes.
The tool never rebuilds an image to stand in for the requested ID. Case-insensitive
filesystems and noncanonical exports fail. See [image security](image-security.md)
for detailed archive, scanner, database, license and ELF contracts.

## Blocking source checks

| Check | Enforced policy | Important boundary |
| --- | --- | --- |
| Actionlint | Workflow syntax, expression and embedded shell checks | Does not establish application authorization. |
| Zizmor | Offline, pedantic analysis with inline/config ignores disabled | Exact reviewed portability findings remain visible. |
| Gitleaks | Redacted current-tree and change-range scans with exact public-fixture exceptions | Pattern detection cannot recognize every secret or encoding. |
| Semgrep | Local tested rules; complete explicit target and rule inventories | Scope and limits are documented in the rule pack. |
| Cargo audit | Current RustSec database and all reported advisories | Reviewed exact dependency-health warnings are counted. |
| OpenSSL freshness | The pinned major/minor series must match the vendor's current stable patch | Complements advisory databases; does not establish coverage of other native libraries. |
| Cargo deny | Approved licenses, crates.io origins, banned crates and dependency constraints | A separate exact duplicate budget covers the all-feature graph. |
| Pip audit | All five hashed build, deployment, scanner and native Python requirement sets | Uses current package advisory service responses. |
| Npm audit | Both web and browser-harness lockfiles | Lockfile audit remains necessary for bundled JavaScript. |
| Squawk | New PostgreSQL migrations, transaction-aware and pinned to the deployed major | The fixed existing-migration baseline forbids edits to old SQL. |
| Runtime configuration | Maintained rendered container restrictions and configuration invariants | Source tests cannot prove live kernel or provider state. |

The current-tree secret gate projects every tracked or nonignored file, including
binaries and lockfiles, into bounded printable ASCII under neutral filenames.
An actual-detector canary must pass first; original/projection hashes and byte
counts identify what was scanned. Prefix overhead and free disk are checked
before writing. Fixture reviews retain their exact original path and value.
Coverage includes contiguous ASCII credentials and supported encodings; UTF-16,
encrypted/compressed content and strings split by nonprintable bytes are outside
that scope. Git history checks cover textual diffs. The
[source coverage guide](../security/README.md#source-secret-coverage) documents
the limits and real-engine tests. Broad community Semgrep packs are not a
blocking substitute for the curated rules and their positive/negative fixtures.

Migration lint uses a generated private configuration with no rule or path
exclusions. New SQL cannot contain inline Squawk suppression directives, and
both the process status and the structured findings must report success.

The OpenSSL freshness check fetches the official current-release page over HTTPS
without redirects or inherited credentials, under a 35-second process deadline,
1 MiB response ceiling and 16 KiB error ceiling. It requires exactly one stable
archive for the pinned major/minor series and exact equality with the native
manifest's version. A newer patch, missing series, ambiguous page, network error
or changed response layout fails the gate. Prereleases and other series cannot
cause an automatic upgrade. The private receipt records the observed versions,
timestamp and source-page/native-manifest hashes. Every fast run, including the
daily scheduled source gate, refreshes this observation. Vendor releases can
precede vulnerability-database affected-package mappings; this check closes that
specific delay for OpenSSL without treating a current version as vulnerability
clearance or changing any pinned source automatically.

Scanner subprocesses have deadlines, byte ceilings and owned process-group
cleanup. Their environment drops inherited credentials and scanner overrides.
The host account and checkout remain trusted; these checks do not defend against
an administrator changing the interpreter or checked source during execution.
The Python environment's hash-named receipt records a completed installation,
not a fresh hash of every installed package on each run. Native scanner resolution
does verify each installed executable and companion library hash.

See [tool inputs](../security/README.md) for exact installation/update commands,
[review records](../security/reviews.md) for initial exceptions, and the
[Semgrep pack](../security/semgrep/README.md) for its precise production scope.

## CI topology and scheduling

`ci.yml` invokes the security and CodeQL workflows at the caller's exact source
revision. No secrets are inherited. Ordinary pull requests run source checks,
dependency review, native sanitizers/replay and the existing Rust, database,
browser and production-image suites. A trusted release is eligible only after
all required jobs succeed; a skipped or failed dependency is not release approval.

Daily security runs refresh advisory results even when source has not changed.
The daily deep jobs run finite native checks and selected pure-policy mutations. CodeQL runs
`security-extended` for ordinary CI. Standalone pull-request, scheduled and manual
CodeQL runs use `security-and-quality` in separate `quality-advisory` categories.
Running those same configurations on pull requests lets GitHub compare every
CodeQL configuration already present on the base branch before permitting a merge.
The broader categories remain advisory; findings still need triage. The reusable
workflow's `security_gate` input defaults to true, preserving CI's security
categories and exact High/Critical review enforcement.

CodeQL covers Actions, JavaScript/TypeScript, Python and Rust without a build.
C/C++ is a separate manual-build job: it compiles the real vendored worker under
the analyzer, with the actual generated inputs, include paths and definitions.
A query then requires observed compilation of the DTLS, STUN, SCTP and RTP
implementations. An empty database or a source-only native scan cannot pass that
coverage check. This job starts a fresh native build and does not restore a
previous worker compilation.

After ordinary analysis, every language job validates its original SARIF through
`build/security_codeql_triage.py`. Unreviewed High and Critical findings fail the
job even when the scanner process itself succeeds. Each exception binds the
query, analyzer version, primary range, rendered-message digest and complete
source identity. Generated native files require authenticated upstream archive
and maintained-overlay evidence. Missing or incomplete analysis fails the gate.

The required aggregate also runs `build/security_codeql_triage.py health` against
GitHub's stored analysis records. For each of the five security categories, the
newest record must match the caller's exact ref and commit, contain executed
queries, and have no error or warning. A successful upload or green analysis job
does not establish successful ingestion. The check reads the current ref before
and after bounded, uncached API requests; missing, stale or inaccessible evidence
blocks signing. It reads no exception ledger or source archives. Only compact
analysis identities/counts or a fixed failure code are retained.

This aggregate uses `contents: read` and `security-events: read` with the workflow's
`GITHUB_TOKEN`, including fork PR merge refs; it has no mutation or signing
permission. GitHub permits [read permissions for fork workflows](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax#permissions),
and the [analysis-list API](https://docs.github.com/en/rest/code-scanning/code-scanning#list-code-scanning-analyses-for-a-repository)
requires read access. API authorization failures fail the same gate without a
credential fallback or a fork-specific skip.

The [exact review procedure](../security/codeql-review-2026-09-30.md) separates
local enforcement, read-only GitHub plans and authorized false-positive dismissal.
Remote dismissal alone never exempts a finding from the repository gate. CI
retains only the compact policy report or fixed failure code; complete SARIF and
scanner working data remain private. Scheduled quality analysis stays advisory.

Only SARIF jobs receive `security-events: write`. PR jobs receive no deployment
credentials or signing permission. Native, Rust and image caches include explicit
trust and architecture boundaries, with no PR cache fallback into main builds.
The pinned local `act` runner remains a privileged execution environment: run
only trusted changes inside the documented disposable engine.

After the aggregate gate succeeds, the main-push signer attests four subjects:
the original image archive, SPDX SBOM, runtime proof and OpenVEX disposition.
The signed predicate binds the remaining release metadata. Deployment verifies
the signatures against the exact repository, workflow, source revision, run and
attempt before remote operations; required runtime source reviews must also
remain unexpired. Unsigned development builds are not eligible. See the
[release procedure](../ops/ansible/RELEASES.md) for acquisition and verification.

The daily [disposable Debian VM check](../security/vm/README.md) acquires that
same signed release, applies the real host and application configuration twice,
then checks host permissions, service policy and an isolated backup restore.
It runs on an owned local KVM guest and accepts no external target. This adds
host evidence beyond the container fixture; public DNS/TLS, provider firewalls,
TURN and encrypted off-host backup delivery remain separate operational checks.

## Main-branch rules

`security/rulesets.json` is the desired repository policy; the GitHub API is the
source of truth for whether it is currently enforced. Reconcile it with:

```sh
python3 build/security_rulesets.py plan
python3 build/security_rulesets.py check
python3 build/security_rulesets.py apply --revision FULL_GREEN_MAIN_COMMIT_SHA
```

`plan` reads the current rules and reports differences. `check` also exits
nonzero on drift. `apply` creates or updates only the two named rulesets and
reads them back. It requires a clean checkout at the exact remote main revision,
a completed successful main-push CI run, the aggregate GitHub Actions gate,
the newest security analysis for each of the five CodeQL categories at that same
revision, and no open High/Critical security alerts. Analysis warnings, missing
query counts and empty query coverage fail the preflight. It rechecks the remote
main ref before each write and before and after readback. A changed head stops
remaining operations and reports failure; protection already installed remains
in place and a fresh `plan` shows any remaining differences.
Readback compares the complete rule parameters. The review policy explicitly
records GitHub's current `required_reviewers: []` and
`require_extra_approval_for_unattributed_changes: true` values; changed or added
parameters remain drift rather than being discarded as defaults. Fixed failure
codes distinguish failed API commands from mismatched readback without exposing
API response bodies or exception details.
It has no delete or disable mode. The operator's authenticated `gh` account must
have repository administration permission; no administration token enters PR CI.

The security ruleset has no bypass actors. It forbids deletion/force pushes,
requires the current aggregate gate and blocks new High/Critical CodeQL findings.
The aggregate itself requires every declared correctness, security and image
job to succeed; failed, cancelled or skipped dependencies fail it.

The separate review ruleset requires a pull request, one current approval after
the last reviewable push, stale-review dismissal and resolved conversations.
This repository currently has one maintainer, who cannot approve their own PR.
The named owner's numeric GitHub identity therefore has a **pull-request-only**
bypass of the review ruleset. That exception does not bypass security checks,
CodeQL, force-push restrictions or deletion protection, and does not authorize
direct pushes. Remove it when another reviewer is available. It is an explicit
availability tradeoff, not a claim of independent human review.

Activate the rules only after the new checks have completed successfully on
main. After activation, publish changes through pull requests; temporary local
review branches are not themselves release artifacts. Administrators can still
change repository policy, so ownership of administrative credentials remains a
separate trust boundary.

## Security behavior tests

The [authorization policy inventory](authorization-testing.md) enumerates current
HTTP routes and every WebSocket operation. Tests require policy fixtures when
those inventories change, exercise real handlers, and check rejected sessions,
roles and cross-room state. Database cases use disposable PostgreSQL transactions.
An inventory entry is not itself proof that all races or permissions are correct.

The `runtime_canary` regressions send inert credentials and messages through real
in-process authentication routes and signaling dispatch. They require actual
success/denial events and complete bounded diagnostic capture, then reject
submitted or issued secrets in tracing, metrics, diagnostic exports and URL
surfaces. See [runtime exposure coverage and limits](testing.md#runtime-secret-exposure-regressions).
These tests complement the secret scanner's detector self-test.

The Rust `boundary_properties` filter runs finite normalization, serialization,
limiter and ticket-clock checks. The disposable release fixture adds five fixed
anonymous HTTPS response-policy checks through real Caddy; see
[response policy](response-policy.md). It has no configurable external target,
account mutations, endpoint discovery or active scanner. Existing browser and
database suites remain necessary for authenticated behavior and media lifecycle.

## Evidence, findings and changes

Keep full scanner directories private. They can contain source excerpts, image
configuration, command output, package metadata and candidate secret values.
CI uploads the explicit summaries or the image guide's allowlisted evidence,
never a wildcard over scanner working directories. A redacted report does not
make an unreviewed adjacent file safe to publish.

An exception must name one scanner/fingerprint/scope, its owner, rationale,
reachability assessment, review link and expiry in `security/exceptions.json`.
Expired, malformed or wildcarded records fail the gate. Changing a rule class or
severity filter is not an acceptable way to suppress an individual finding.
Reproduce scanner behavior with bounded synthetic fixtures and inspect the
affected source before deciding whether a finding is applicable.

Tool updates are focused changes: authenticate upstream bytes, update the lock,
run actual engine fixtures and relevant image/native integration, and review
changed report formats before accepting them. Advisory database updates remain
dynamic and have explicit freshness checks where the scanner exposes that
evidence. Pinned tooling does not imply permanently pinned vulnerability data.

No layer proves absence of vulnerabilities. Current limitations include native
advisory matching for adapted source subsets, minified JavaScript reconstruction,
real-device behavior, provider firewall policy, and operational backup recovery.
Automated checks cannot invent a human dependency review, license grant or
production capacity guarantee. `cargo-vet` adoption requires actual reviewed
audits; a generated audit file would not establish that evidence.
