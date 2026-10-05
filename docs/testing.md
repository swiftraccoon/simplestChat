# Testing

Run commands from the repository root after [development setup](development.md).
Use disposable local services, never a shared or production database.

## Fast checks

```sh
build/check.sh
cargo test --locked --all-features -- --test-threads=1
```

`build/check.sh` shares the CI quality gates: typed web lint, formatting,
source/helper tests, TypeScript/build, Rust Clippy, checked documentation, and
strict Python lint/format/types. Use `--web`, `--rust`, `--helpers`, or `--python`
for a focused group. It requires installed development dependencies and never
starts application services. Native tests
are deliberately separate from the source-quality gate.

Rust tests include native media checks; database tests are opt-in. Serial
execution reduces UDP port-allocation races. Web tests use mocked browser APIs;
the web build runs TypeScript checking and produces the production UI.

Credential-continuity tests use bounded validation futures to distinguish
database unavailability from revocation and exercise expiry, drain, notification
loss and grace-registration races. Browser unit tests cover scheduled HTTP refresh
and same-socket renewal retries, deadlines and retirement by newer authentication
actions. These do not constitute a database-outage load test; the ignored
database-backed renewal test exercises successful real validation.

Room-control tests cover write ordering, sender/permission changes and caller
cancellation. The ignored persistence test uses a row lock in the disposable
database to check chat/state access during blocked SQL and publication after
the requesting task is cancelled. It is a concurrency regression test, not a
database-outage or throughput benchmark.

### Runtime secret exposure regressions

The `runtime_canary` Rust tests submit unique inert values through the real
in-process Axum router and signaling dispatcher, then inspect application
observations. They complement the Gitleaks detector self-test: a working secret
scanner does not prove the running application keeps credentials out of logs.

```sh
build/with-test-postgres.sh \
  cargo test --locked --all-features --lib runtime_canary \
  -- --include-ignored --test-threads=1
```

The database case registers an owned account, checks accepted and rejected
password login, reads its profile, issues a WebSocket ticket, rotates its refresh
cookie, rejects malformed credentials and an absent invitation, then checks
logout revocation. Submitted canaries and issued access, refresh and WebSocket
credentials must remain absent from real tracing JSON, the authenticated metrics
response, the enabled media snapshot, request URIs and URL-bearing response
headers. Intended authentication bodies, refresh cookies and profile data remain
observable only to their test client; they are not mistaken for logging leaks.
Cleanup names only the account created by that fixture.

The dispatcher case admits an owned participant, delivers a chat message and
denies a subsequent message after membership removal. It checks the actual reply
channel, membership event and successful/failed diagnostic operation records,
then verifies that names, messages, supplied passwords and reconnect credentials
are absent from tracing, metrics and the real diagnostic JSONL recorder. Recorder
shutdown must flush successfully. Both capture buffers are capped at one MiB;
overflow remains a test failure even if a logging layer swallows its write error.
HTTP response collection is capped at 512 KiB. Positive event assertions prevent
an empty capture from passing.

The subscriber follows request-future polling across awaits without changing the
process-global subscriber. Capture uses the default production info-level JSON
filter, and the guards check complete values plus standalone URL-form and Base64
encodings. These tests cover the selected request futures and exports; they do
not cover uninstrumented detached tasks, arbitrary partial/nested encodings,
browser storage, PostgreSQL/proxy logs or external artifact upload pipelines.
No network client, deployed target or existing database is used. Ordinary Rust
tests run the dispatcher and capture checks; CI's disposable database suite also
runs the ignored authentication case.

### Python automation

Use Python 3.12 or newer and the pinned controller/checking environment:

```sh
python3 -m venv ops/ansible/.venv
ops/ansible/.venv/bin/pip install -r build/python-requirements.txt
build/check-python.sh
PATH="$PWD/ops/ansible/.venv/bin:$PATH" \
  ops/ansible/.venv/bin/python -m unittest discover -s ops/ansible/tests -v
```

The source gate runs Ruff's complete stable rule set, its formatter, and
basedpyright's `all` mode over every maintained Python file and type stub,
including tests. It rejects a new source directory until explicitly included.
Third-party `vendor/` sources and ignored private operator scripts are outside
this policy. ShellCheck, `jq`, and the Ansible dependencies are required for the
offline tests; the optional Compose renderer needs the Docker CLI, not a daemon.

`pyproject.toml` defines the policy. Mutually exclusive formatting/docstring
rules and standalone-module/unittest conventions have documented exceptions;
reviewed subprocess, CLI-output and transaction-complexity exceptions stay next
to the affected code. `Any`, unknown types, untyped arguments, unchecked casts
from decoded data, and test-wide typing exemptions are not used to pass the gate.
JSON boundaries validate decoded values before constructing typed records;
options and fixture state use dataclasses where appropriate. The narrow Ansible
stubs in `typings/` describe only the pinned APIs used here and retain `object`
for values that callers must validate.

CI uses this same gate. Set `PYTHON_CHECK_ENV` to another virtual environment
directory to select both its pinned tools and its Python import environment.
The checks do not install packages, contact deployment hosts, or start services.
Updating a host helper requires `build/deploy.py --install-helpers` once so that
the complete wrapper/module set is reconciled; subsequent releases retain the
prepared-helper hash checks and app-only replacement flow.

### Readiness and shutdown

After building the server, run the same process-lifecycle smoke used by CI:

```sh
cargo build --locked --bin simplestChat
node build/shutdown-smoke.mjs --binary "$PWD/target/debug/simplestChat"
```

The smoke starts its own guest-only server on unused ports, checks `/ready`,
and verifies that a client-initiated close finishes cleanly with code 1000.
It then opens a separate connection, joins a room, and sends `SIGTERM` only to
that child. It requires the temporary `serverRestarting` notice (and rejects
terminal `roomClosed`), clean WebSocket close code
1001 and exit status zero within its 20-second deadline. It never connects to
an existing server or database.
Native tests separately cover drain races, stalled cleanup and readiness probe
failures. See [shutdown guarantees and limits](configuration.md#shutdown).

The Rust filter `boundary_properties` runs finite property checks without services:
2,048 seeded Unicode labels, 1,024 decorated ASCII identities, every signaling
operation's canonical round trip, 512 escaped-text examples, integer/envelope
boundaries, all 27 nullable-settings combinations, bounded limiter counters and
448 combinations of ticket lifetime and independent clocks. Inputs and case
budgets are fixed in the tests; ticket secrets still come from the real secure
issuer and are never printed. These tests check wire types separately from
handler authorization and message-size admission. They do not run a continuous
fuzzer, publish media, or replace the database and real-browser tests.

Mocked client checks cover restart versus permanent deletion, retained room/lobby
intent, public-draft isolation, bounded jittered retries, explicit retry and leave,
and stale or interrupted rejoins. With the UI built and
[browser tooling installed](../web/e2e/README.md), run the real restart check:

```sh
node build/restart-browser-smoke.mjs --binary "$PWD/target/debug/simplestChat" \
  --browser chromium --output "$PWD/results/restart-browser"
```

Use a fresh output directory each time. This check owns its local servers and
browser, verifies two guests rejoin without reloading, preserves a public draft,
and delivers messages before and after restart. Capture calls are intercepted
and must remain zero. CI runs Chromium; `--browser firefox` and `--browser webkit`
use those installed Playwright engines. Retained results include recovery timing
and browser/server exit status. They do not prove physical-device media recovery.

Manually check leaving during recovery and a prolonged outage: after two minutes,
automatic retries stop and explicit retry remains available. Do not interrupt a
shared server without authorization.

To validate the server's actual diagnostic JSONL schema with the same owned
guest-only workflow, also run by CI:

```sh
node build/diagnostics-smoke.mjs --binary "$PWD/target/debug/simplestChat"
```

This additionally requires complete recorder coverage, successful `join_room`
and `room_lock_wait` records, and clean server exit. It prints and retains a new
private `results/diagnostics-smoke.*` directory, including failure artifacts;
it does not use a browser, capture media or upload anything. See
[diagnostic interpretation](diagnostics.md#read-the-results).

## Disposable PostgreSQL and browser tests

`build/with-test-server.sh` runs `target/debug/simplestChat` as it finds it, so
build first: a stale binary answers a new message type with "Invalid message
format". The disposable server also opens registration and raises two
per-address budgets for the five browsers a suite drives from one address
(`REGISTRATIONS_PER_IP_PER_HOUR=100`, `AUTH_REQUESTS_PER_MINUTE=300`); a
deployment keeps the defaults in [configuration](configuration.md).

With PostgreSQL tools on `PATH`, use the owned bootstrap:

```sh
cargo build --locked --bin simplestChat
build/with-test-postgres.sh build/with-test-server.sh \
  cargo test --locked --all-features -- --include-ignored --test-threads=1
```

It creates a fresh loopback-only PostgreSQL cluster on port 15434, starts and
migrates an owned application server, then stops both after the command. It
refuses an occupied port and retains its private temporary directory for
diagnostics. Set `TEST_POSTGRES_PORT` when another local service owns that port.
The trust-authenticated database is disposable, never a shared development DB.

For browser tests, replace the Cargo test command with
`npm --prefix web/e2e test` after the [browser installation steps](../web/e2e/README.md).
Build the UI first; do not rebuild assets during a browser run.

### Passkey verification

Run native WebAuthn ceremonies with Chromium's owned virtual authenticator:

```sh
PASSKEY_E2E=1 build/with-test-postgres.sh build/with-test-server.sh \
  node web/e2e/passkey.cjs
```

The opt-in helper uses `http://localhost` as the browser origin and fixes the RP
to `localhost`; the HTTP bind and database remain on loopback. The suite exercises
resident-key registration and usernameless login against the actual verifier,
including rejected email selectors, wrong handles, changed signatures, replay,
stale/equal counters and a natively signed assertion from a different origin.
It also verifies that an otherwise functional nonresident credential cannot enter
the discoverable flow. A separate finite scenario registers three accounts through
the actual UI in one persistent virtual authenticator. It checks distinct UUID
handles, matching backend identities and three coexisting resident credentials,
then signs in through the UI with all three available. To verify each account
deterministically, the fixture temporarily removes the other two credentials from
that owned authenticator and restores them from memory, preserving current
signature counters. Browser request options and assertions are not rewritten;
the test checks modal mediation and an empty credential allowlist at the native
API boundary, plus the resulting backend and displayed account identities.

Credentials remain in memory and the disposable database; reports contain only
outcomes. The virtual authenticator selects credentials automatically, so this
proves coexistence and account routing, not a visible multi-account chooser.
Native Bitwarden/Keychain selection and physical authenticator behavior still
require device testing.

The companion management suite exercises fresh proof, backup-key enrollment,
last-sign-in-method protection, session revocation and recovery using native
credentials and owned disposable accounts. Its removal journey verifies that
password proof stays available in the confirmation and deletes the selected key
without opening a passkey prompt. A passkey-only replacement journey keeps one
authenticator throughout, requires fresh proof and a saved-recovery-key
acknowledgment before a separate registration gesture, then verifies the new
credential signs into the same account without enabling a password. It also
checks that the old server record and sessions are retired:

```sh
PASSKEY_E2E=1 build/with-test-postgres.sh build/with-test-server.sh \
  node web/e2e/passkey-management.cjs
```

Run `build/check-native-dtls.sh` after configuring the pinned OpenSSL installation
to exercise the worker's DTLS closure reasons and media warning context. It builds
a separate temporary worker test target, leaving Cargo's production archive untouched. Paired native
transports cover orderly shutdown, fatal errors, fingerprint/SRTP failures and
handshake timeout; the orderly-close case also checks warning-level output.
Fake-clock media cases verify inactivity/activity timing, bounded private
identities, tuple-history expiry/reuse and warning coalescing, including timer
cleanup and a finite burst's delayed summary. They do not send network probes.
CI runs the helper in its own job and records a passing verification under a cache key
made from the vendored sources, the helper, the pinned OpenSSL installer and the pip
constraints; a push that leaves those unchanged skips the nine-minute rebuild, and any
change to them verifies again.

Exercise call outcomes with real browser decoding on owned loopback services:

```sh
CALL_OUTCOME_E2E=1 IMPAIR_SCRIPT=none IMPAIRED_PROFILES=baseline \
  build/with-test-postgres.sh build/with-test-server.sh \
  node web/e2e/impaired-network.cjs
```

This mode checks one terminal observation for an empty room, a video-only join,
an audio-only join and an audio reconnect using the diagnostic preview. It uses
synthetic devices and cannot establish physical output audibility.

### Authenticated chat continuity

Run the opt-in 20-minute session soak against a fresh local database and server:

```sh
SESSION_SOAK_E2E=1 build/with-test-postgres.sh build/with-test-server.sh \
  npm --prefix web/e2e run test:session-soak
```

Two separately registered accounts send through the real chat UI across scheduled
token refresh and original token expiry. The test checks delivery, drafts, room
membership and cleanup; an automatic full rejoin does not count as uninterrupted
service. Production token lifetimes and rate limits stay unchanged. Reports retain
timings and outcomes, never tokens, passwords or cookies.

Add `SESSION_SOAK_PROFILE=smoke` for a 30-second harness check; it does **not** test
refresh or expiry. Both profiles use desktop and 375px Chromium viewports, with
capture denied. They do not establish physical mobile or media continuity.

### Frontend lifecycle and degraded-browser regression checks

After building the production UI, run `npm --prefix web/e2e run test:resilience`.
This isolated Chromium suite routes HTTP and signaling fixtures into the actual
production bundle. It checks native authentication/creation dialog focus and
background isolation, blocked or full preference storage, stale room-creation
responses, capability-aware onboarding, roster focus preservation and composer
containment at short mobile viewport sizes. It uses no database or live backend
and does not capture media. A separate temporary artifact directory records the
checks and browser cleanup; `E2E_ARTIFACTS` selects an explicit output directory.

Run `npm --prefix web/e2e run test:layout` for the broader responsive-layout matrix
and the real-backend community/accessibility suites for integration behavior.
Mocked UI success does not establish native-device, mobile-browser or backend
correctness. CI runs both the layout and lifecycle suites against built assets.

### Automated accessibility smoke

After installing the browser tooling and building the UI:

```sh
ACCESSIBILITY_E2E=1 build/with-test-postgres.sh build/with-test-server.sh \
  node web/e2e/accessibility.cjs
```

This separate Chromium gate scans join/authentication, room creation, chat,
account and every settings tab, plus 320px layouts and light/dark Help. It also
checks keyboard activation of the newest-message button after real chat overflow,
native dialog Escape/focus restoration and Help opening without leaving the room.
See [browser accessibility coverage](../web/e2e/README.md#accessibility) for report
details. Automated rules and selected keyboard paths do not establish full WCAG
conformance; screen-reader, zoom, device and manual checks remain necessary.

### Advanced: manage your own disposable cluster

Create a fresh database ending in `_test`. The server helper requires
`DISPOSABLE_TEST_DATABASE=1` and a loopback `DATABASE_URL`; it rejects remote hosts
and connection-query overrides other than `sslmode=disable`. It does not create
or delete the database.

With PostgreSQL tools on `PATH`, first check that port 15434 is unused, then:

```sh
test_cluster="$(mktemp -d "${TMPDIR:-/tmp}/simplestchat-pg.XXXXXX")"
initdb -D "$test_cluster/db" -U test_owner --auth=trust --encoding=UTF8 --no-locale
pg_ctl -D "$test_cluster/db" -l "$test_cluster/postgres.log" \
  -o "-h 127.0.0.1 -p 15434 -k $test_cluster" start
createdb -h 127.0.0.1 -p 15434 -U test_owner simplestchat_test
export DATABASE_URL='postgres://test_owner@127.0.0.1:15434/simplestchat_test?sslmode=disable'
export DISPOSABLE_TEST_DATABASE=1

cargo build --locked --bin simplestChat
build/with-test-server.sh cargo test --locked --all-features -- --include-ignored --test-threads=1
```

Trust authentication is only for this disposable loopback cluster. The helper
starts its own server, applies SQLx migrations, waits for database-backed API
readiness and exports `TEST_DATABASE_URL`. It stops the server when finished.
The additional tests
cover recovery/revocation, refresh-token rotation and persisted social/moderation
behavior. A previously migrated disposable `TEST_DATABASE_URL` can also be used
directly with `cargo test`.

Keep the cluster running for [browser tests](../web/e2e/README.md), which document
installation, port overrides and Firefox/macOS WebKit setup. Do not rebuild UI
assets during a browser run.

When finished, stop only the cluster created above, in the same shell:

```sh
pg_ctl -D "$test_cluster/db" stop -m fast
```

Keep its logs if needed, then remove the temporary directory after confirming
its path.

## Container and CI checks

With Docker available:

```sh
docker build --target production -t simplestchat-ci:production .
build/test-container.sh
```

The smoke creates its own disposable containers, applies migrations and checks
the UI, database-backed API, account registration and restart. HTTP is published
only on a random loopback port. Its containers and temporary database data are
removed afterward. Set `PRODUCTION_IMAGE` to test another built image.

### App-only releases and rollback

CI also runs `build/test-release-container.py` against that existing production
image, using the real release helper and public deployment templates. It checks
a successful replacement, failed candidate startup and rollback while requiring
PostgreSQL and Caddy to stay running.

After fixture readiness, the harness also checks five fixed anonymous HTTPS
responses for proxy headers/CSP, public JSON cache policy, absent cookies,
missing-authorization rejection, private endpoint omission and bounded content.
It verifies fixture ownership and the local CA and sends no account or chat
mutations. See the [passive response-policy check](response-policy.md) for scope,
limits and existing complementary coverage.

Push CI retains that same immutable production image for deployment. Offline
operations tests cover export without rebuilding, commit/run/artifact identity,
bounded CI waiting, single-host selection and stopping after a failed deployment
or verification. Deploying a retained image does not rerun the full suite; it
checks artifact integrity, runtime compatibility and readiness, then runs the
bounded anonymous HTTP checks of `/health`, `/ready` and `/`. The separate
`--public-chat-smoke` option explicitly opts into creating guests and sending a
public chat message; it is disabled by default. The supported `--force` path
accepts an exact-revision unsigned build without waiting for CI, while retaining
the runtime safeguards and recording that override. See the
[release workflow](../ops/ansible/RELEASES.md).

Run it only as root on a fresh, disposable Linux AMD64 or ARM64 host with a local
Docker Engine (API 1.48+), Compose, OpenSSL, curl, `nsenter`,
`update-ca-certificates`, and the
[controller Python environment](../ops/ansible/README.md). The production image
remains Linux/AMD64; an ARM64 fixture host needs AMD64 emulation:

```sh
sudo ops/ansible/.venv/bin/python -B build/test-release-container.py \
  --disposable-host --image simplestchat-ci:production \
  --output /tmp/simplestchat-release-check
```

The output directory must be new. The helper refuses existing public deployment
paths/containers and occupied fixture ports. It creates private deployment
fixtures and installs a local test certificate authority; do not use a workstation
or deployed VPS. Verified owned containers are removed on normal completion;
an interrupted command leaves uncertain resources untouched and reports failed
cleanup. Private fixture files and derived test images remain until the disposable
host is destroyed. CI uploads only sanitized `report.json`,
never private logs, credentials or database dumps. This does not reboot a host,
exercise real users/media or establish production outage duration.

Rust linting, native/PostgreSQL tests, the memoized native DTLS check and three
browser suite groups (accounts, media, stress) run as parallel CI jobs. Their
native toolchain caches bind the vendored sources, whose files receive a fixed
mtime so reusable worker artifacts stay valid across checkouts. Rust lint,
native/PostgreSQL tests and browser backends use separate `rust-check`,
`rust-test` and `rust-build` dependency-cache namespaces. The test job primes
all targets with `cargo build --locked --all-features --all-targets --profile test`,
including development dependencies and test harnesses; it then runs the complete
test command with ignored tests included and one test thread. A browser-only
binary cache cannot substitute for that test graph. The production image caches
completed application layers as well as warmed dependencies, with a new cache
key whenever its build inputs change. BuildKit validates fallback layers against
the current inputs. The release fixture's controller installs in the
background while the image builds.
[CI](../.github/workflows/ci.yml) also runs formatting, locked Rust builds/tests,
web tests/build, readiness/shutdown and pinned Chromium integration tests, dependency audits, native
dependency guards and production-image non-root/loader checks.
The [browser compatibility workflow](../web/e2e/README.md#ci) adds weekly and
manual Firefox/Linux and WebKit/macOS runs.

### Run CI locally

Commit the candidate locally, then run the complete required workflow without a
push or pull request:

```sh
build/ci-local.sh all
```

The launcher uses [act](https://github.com/nektos/act) to execute
[CI](../.github/workflows/ci.yml)'s `required` job and its actual dependency graph,
including the reusable security and CodeQL workflows. There is no separate list
of local checks to drift from the workflow. Install Go 1.25.0 or newer and Podman on
macOS (`brew install go podman`). Git, Python 3, curl and `patch` are also
prerequisites. The launcher builds its checksum-pinned act 0.2.89 with the
repository's shared concurrency limit, then verifies and reuses that executable
under `.cache/ci-act`. It does not use an ambient act installation.
The launcher enables act's shallow action cache so pinned actions fetch their
exact revision without downloading full repository history.

On macOS, the launcher creates or reuses only its rootful `simplestchat-ci`
Podman VM (12 CPUs, 80 GiB RAM, 80 GiB disk). It refuses to stop another running
VM or select Docker Desktop implicitly. A VM started by the launcher is stopped
when it exits; an already running VM stays running. Existing VMs keep their
configured resources. To resize an older owned VM, first end its CI invocation
and verify that `podman --connection simplestchat-ci-root ps --all` lists no
containers, then run:

```sh
podman machine stop simplestchat-ci
podman machine set --cpus 12 --memory 81920 simplestchat-ci
podman machine start simplestchat-ci
```

On Linux, use an empty engine
inside a dedicated disposable VM and identify its local socket explicitly:

```sh
DOCKER_HOST=unix:///path/to/disposable/docker.sock \
  build/ci-local.sh all --disposable-engine
```

AMD64 Linux uses the pinned Ubuntu 24.04 AMD64 runner and provides the same ISA
coverage as GitHub. Apple silicon uses its pinned ARM64 counterpart for the same
suites, including first-party CodeQL and functional native tests. Optional vendor
source analysis and native sanitizer/replay checks are outside this CI graph.
Production images remain
explicitly AMD64 on both paths. After checking that the owned VM has no containers,
the Mac launcher enables its registered Rosetta interpreter for AMD64 production
commands and disables the competing QEMU handler. The packaged QEMU crashes when
querying the pinned AMD64 Rust compiler. Local ARM64 production builds derive a
digest-pinned native BuildKit image with only its bundled AMD64 QEMU removed,
so build commands use the kernel's Rosetta registration. The existing container
builder and exported layer cache remain in use. Authenticated OpenSSL extraction uses
Python's filtered tar reader, which works with Rosetta. These ARM64 test runs
exercise the same checks but do not claim identical ISA coverage to GitHub.
The default signed deployment requires the hosted AMD64 workflow. An explicit
owner-requested [`--force` deployment](../ops/ansible/RELEASES.md) is a separate,
recorded override and does not report skipped CI as passed.
At most four actual jobs run concurrently across the entire graph, including
matrix entries and reusable workflows. Each is limited to 3 CPUs and 16 GiB RAM,
leaving 16 GiB of the owned VM's memory for services and the engine. The maintained
act patch shares one limit across nested workflows; changing the unpatched
`--concurrent-jobs` alone would multiply independent matrix pools. The canonical
workflow matrices and their coverage remain unchanged.
The privileged job containers are confined to the disposable engine. Each
Docker-dependent job authenticates the official Docker 29.8.2 static archive for
its architecture and starts that pinned daemon with a fresh storage volume, private
cgroup namespace and private socket. The installer replaces the runner image's
older embedded daemon only inside the guarded disposable job container; it checks
both the executable and running server version. The outer engine socket is
never mounted, and the production release fixture keeps its empty-engine and
disposable-host checks.

PostgreSQL services use distinct declared ports for the Rust and three browser
groups. A local-only relay connects each job's loopback address to its published
service; the existing loopback-only database checks remain active. The launcher
refuses pre-existing containers. It requests act cleanup and checks for leftover
containers after execution; an interrupted setup can require inspection of the
owned VM before another run.

The full gate requires a clean committed checkout and checks its identity again
after execution. Completion receipts bind every required matrix entry and the
successful aggregate to the current run and revision; dry runs or filtered
matrices cannot report a complete local gate. `--base REF` selects the ancestor
used for changed-source checks; by default it uses the merge base with local
`origin/main`, or `HEAD^`
when already at that base. Update the tracking ref before validation when needed.
Local `.env`, secret, input and variable files are not passed to the workflow.
CodeQL installs the checksum-pinned bundle for each job's actual architecture.
If `.cache/codeql-tools` already contains the pinned bundle and receipt for the
actual runner architecture, the launcher mounts that cache read-only; every scan
still verifies its CLI and query pins. An incomplete cache is left unused.
The analyzer cache stays outside Cargo's `target/`, which Rust caching restores
and cleans recursively. Existing authenticated bundles can be moved from
`target/codeql-tools` to `.cache/codeql-tools` while no local CI run is active;
the same receipts and analyzer/query checks apply at the new location.

The four local CodeQL scans enforce the same first-party source scope, security
query suites, exact finding reviews and completed-query health. `vendor/` is
excluded from automated source analysis. Native C/C++ source analysis and
ASan/UBSan/replay suites are optional local checks, absent from push, scheduled,
manually dispatched and complete local CI. Their `--include-vendor` opt-in is
rejected in CI environments. Dependency and production-image audits, source
provenance, build integrity, application native tests and the functional DTLS
regression gate remain active. GitHub's
stored-analysis ingestion check and OIDC release signing/publication remain
hosted operations; a successful local gate does not claim a GitHub signature.
Rust CodeQL uses the same pinned CLI runner locally and on GitHub.
Query RAM follows the pinned official action's allocation: cgroup-limited memory
minus one GiB and five percent of memory above eight GiB. It can reuse a complete
evaluated database only for identical source, analysis scope, suite,
analyzer and query pins, runner generation/architecture/trust, and absolute
source paths. Rust inputs include every Rust source and Cargo manifest, the
`src/`, `tests/`, `migrations/`, `vendor/` and `.cargo/` trees, authorization data,
shared `web/tests/` JSON fixtures and the analysis helpers and pins. Vendor bytes
remain compiler/dependency inputs even when vendor source diagnostics are outside
the selected analysis scope. Documentation,
deployment files and unrelated frontend packages do not invalidate Rust analysis;
the source archive guard rejects any omitted tracked file.
The restored bundle hash, extraction metadata and archived source bytes must
match. A miss performs real extraction and query evaluation; a corrupt entry
fails. Complete healthy query results are saved before current policy runs, so
a finding that needs review does not discard successful analysis. Policy failure
still fails the job and blocks signing. The five exact review data/document files
are outside the analysis-only cache key; archived source must not reference them.
Successful-check caches keep their separate input rules. On a hit, CodeQL reuses
its own BQRS results and regenerates original SARIF; raw SARIF is never cached
or relabeled. Rust bundles discard intermediate query caches while
retaining all final results and diagnostics with `--cache-cleanup=clear
--include-results --include-diagnostics`. Rust policy artifacts retain the
original SARIF and query execution log for complete cold/warm comparisons.
Evidence records the original evaluation
revision and explicitly enables `queryReuse`. The CLI also writes
`queryReuseEnabled` and `originalEvaluationRevision` into each generated SARIF,
including reports whose current policy fails. Every automated run applies current
first-party finding reviews. Valid regenerated SARIF still
uploads when findings fail policy; that failure continues to block signing.

Optional vendor CodeQL runs use the same CLI with an explicit local flag:

```sh
python3 build/security_codeql_local.py --revision "$(git rev-parse HEAD)" \
  --include-vendor --language c-cpp --suite all \
  --openssl-prefix "$PWD/target/openssl-4.0.3" \
  --output "$PWD/results/codeql-native-local"
```

The output directory must be new. Native analysis needs Linux and the pinned
OpenSSL installation. Omitting `--language` with `--include-vendor` selects all
five supported languages; without the flag, the default is the four first-party
languages. Optional native creation records initialization, traced compilation
and database import separately and requires actual DTLS, STUN, SCTP and RTP
coverage. Its cache additionally binds compiler and installed package identity,
OpenSSL headers/libraries/settings, the complete vendor tree and native analysis
helpers. Compact bundles preserve final query results and diagnostics, and
current policy still runs. This command is never a CI or deployment prerequisite.

Rust can separately reuse Cargo build outputs when fresh extraction is needed.
This cache binds the actual pinned compiler, runner, trust, instruction set,
dependency bytes and fixed source path. Only keyed vendor and pip constraint
modification times are stabilized; changed Rust source still requires fresh
extraction, full queries and current policy. Cache publication is capped at one
GiB of distinct file data, counting hardlinks once. Direct local runs accept
`--rust-cargo-cache`; the official
workflow installs the pinned Rust toolchain only on an evaluated-database miss.

After successful push CI on main, `cache-retention.yml` removes obsolete caches
from known families. It verifies the triggering run and current main revision,
protects the newest cache in each trust/platform namespace and every cache
created or accessed within two hours, and targets eight GiB of total storage.
Unknown cache families and other refs remain untouched. Run
`python3 build/ci_cache_retention.py` for a read-only plan; applying it requires
`--apply --run-id <successful-CI-run> --revision <current-main-SHA>`. Deletions
are limited to cache IDs, with refreshed inventory before each deletion and
limits of 100 deletions and 240 seconds.

Scheduled mutation, performance/soak and macOS WebKit compatibility workflows
are additional tiers, not part of the required push gate.

Verified successes for Rust checks are reused
only when their exact input key and private receipt match. Keys bind file modes,
workflow/tool/security policy, runner image identity, architecture and the
main-versus-untrusted cache namespace. Source, executable mode, runner and trust
changes still invalidate them. Rust
keys include every tracked non-web input and web JSON configuration and shared fixtures such as
`web/tests/layer-cap-cases.json`. Operations checks conservatively include every
tracked file because they inspect frontend configuration and test helpers.
A missing cache runs the original checks; an invalid restored receipt fails.
Logs identify the original checked revision rather than claiming reexecution.
Dependency/advisory audits and JavaScript helper regressions still run each time.

The optional local [native sanitizer/replay runner](native-security.md) can
reuse prepared images and verified compiled executables. Those artifacts are
not CI success receipts and do not add a native-analysis prerequisite to the
required graph. Every selected local runtime suite still executes against its
bound executable; compilation and runtime evidence remain separate.

Browser jobs reuse backend executables only for the same bound inputs and after
checking both executable and compiler dependency-file hashes. Backend keys bind
all Rust targets (including `load_tests`), embedded source/fixture trees, Cargo
manifests/configuration, the complete native-toolchain action and its helpers,
compiler environment overrides, and runner/trust identities. The CI workflow's
global configuration and browser job through backend publication are bound;
independent image-job, operations-test and documentation edits do not rebuild
unchanged binaries. Unsupported workflow layouts fail closed.
Before publication, both Cargo `.d` files must name the expected binary and only
tracked, keyed sources or the known generated FlatBuffers/current pinned OpenSSL
inputs. External files, untracked embedded files and unknown generated inputs
are rejected. Restores recheck this dependency boundary before installing either
binary. Every browser suite still runs against the current frontend. Entries are
saved only after the corresponding build or check succeeds, without prefix
fallback. Cold caches and changed compiler inputs still require complete builds.

Production CI downloads and authenticates its pinned image-scanner tools while
the disposable release/rollback fixture runs. Both bounded results must pass;
their private logs stay separate. The later image scan rechecks tool hashes and
runs every image policy against the same exported image. Scanner containers run
only after the fixture has cleaned its engine. Within the image gate, fresh
database preparation, offline SBOM generation/conversion, and secret scanning
run in three independent branches. Two shared scanner slots retain the existing
maximum of four container CPUs and four GiB of container memory; a waiting
branch observes cancellation before starting a scanner. Each branch has separate
logs, timing receipts, and invocation limits within the unchanged eight-scanner
ceiling. Vulnerability matching waits for all three branches. Neither overlap
establishes a measured five-minute job budget.

The image gate also caches Grype database download bytes under a scanner-specific,
daily key with separate main and untrusted namespaces. Every invocation still
runs the online updater, verifies the database hash and allowed age, and performs
the full vulnerability scan and current policy checks. Restored bytes have a
bounded file inventory and checked hashes; a malformed cache fails validation.
No image scan verdict is cached. Local image checks can use the same path with
`--image-database-cache <absolute-directory>`.

Action and build caches persist under `target/act`. Each invocation writes a
private directory under `results/` containing its event, workflow log, source
revision/base, actual runner platform, excluded vendor-analysis scope, AMD64 production target,
exit status and retained compact check summaries. `--output`
accepts a new directory. A single-job run is available for focused fixes and
always reports that it is partial:

```sh
build/ci-local.sh web
build/ci-local.sh rust-lint
build/ci-local.sh browser --matrix group:accounts
```

#### Recover inconsistent ARM CPU features

Some Apple Silicon guests advertise SVE2 without the SVE instructions it requires.
The launcher reads the owned VM's actual Linux auxiliary-vector flags before
configuring its emulator or starting CI, and refuses that inconsistent combination.
This caused the pinned `cryptography` wheel to crash during OpenSSL initialization,
before `ansible-lint --version` could run: the failing instruction was `cntb` in
`_armv8_sve_get_vl_bytes`. The [upstream cryptography report](https://github.com/pyca/cryptography/issues/14733)
and [Parallels diagnosis](https://kb.parallels.com/en/131179) describe this guest
CPU-feature problem. The kernel documents
[`arm64.nosve`](https://www.kernel.org/doc/html/latest/admin-guide/kernel-parameters.html)
to disable the unsupported feature exposure.

For the owned Fedora CoreOS VM only, end its CI invocation and verify that
`podman --connection simplestchat-ci-root ps --all` lists no containers. Then
apply the kernel argument and restart that VM:

```sh
podman machine ssh simplestchat-ci 'sudo rpm-ostree kargs --append-if-missing=arm64.nosve'
podman machine stop simplestchat-ci
podman machine start simplestchat-ci
```

[Fedora CoreOS documents kernel-argument changes through rpm-ostree](https://docs.fedoraproject.org/en-US/fedora-coreos/kernel-args/#_modifying_kernel_arguments_on_existing_systems).
The launcher never changes kernel arguments or restarts an already running VM
to repair this condition. Rerun `build/ci-local.sh all` after reboot; its preflight
checks the resulting feature flags, and the unchanged dependencies and lint rules
must pass normally. These commands apply only to `simplestchat-ci`, not another
project's VM or a shared engine.

### Weekly performance and soak

The [Performance workflow](../.github/workflows/performance.yml) (weekly and
manual) compares a baseline build with the candidate on one hosted runner under
coarse regression budgets, runs the session soak with an eight-second
PostgreSQL outage at its midpoint, and runs the impaired-downlink browser and
generator checks under netem. See [continuous comparison](performance.md#continuous-comparison)
for the budgets and their limits, and
[production shape and impaired networks](performance.md#production-shape-and-impaired-networks)
for the Podman capacity runs and the local impairment helpers.

## Media correctness and performance

See [performance](performance.md) for the optional RTP generator and comparison
workflow, or [browser/API measurements](../web/e2e/README.md#informational-browserapi-performance)
for startup, chat delivery and decoded media. The short CI RTP smoke checks
delivery correctness; it does not set a performance budget.

### Repeated-session lifecycle check

With the server/UI built and Chromium installed:

```sh
LIFECYCLE_E2E=1 build/with-test-postgres.sh build/with-test-server.sh \
  node web/e2e/lifecycle.cjs
```

This opt-in check reuses two pages across six media sessions, including reconnect
and real grace-period expiry. It checks sampled media progress and cleanup while
retaining bounded diagnostics and informational memory trends. It is not part of
default CI or a substitute for physical-device checks. See
[configuration and evidence limits](../web/e2e/README.md#repeated-session-lifecycle-check).

## Manual release checklist

### Real-browser targets

Record the OS and browser version, result and any reproduction steps for each:

| Device | Browser                                            |
| ------ | -------------------------------------------------- |
| macOS  | Chrome and Firefox, tested separately and together |
| iPhone | Kagi                                               |
| iPad   | Safari                                             |

On the Mac, run `build/run-local.sh`, open `http://localhost:3000` in Chrome
and Firefox, and join the same room with different names. Guest-only local mode
is enough for media checks; account and room-management checks need the
[disposable database setup](#disposable-postgresql-and-browser-tests).

Phone/tablet camera tests need an HTTPS test address reachable from those
devices. `localhost` refers to the device opening the page, not the Mac, and
the local launcher intentionally keeps HTTP on the Mac. Until a mobile test
address is available, record mobile checks as **not run**, not passed.

### Hands-on checks

Use an owner and a guest in separate browser contexts:

- Preview and cancel without publishing; leave during a permission prompt. Test
  denied permissions, device changes, unplugging a live mic/camera and explicit
  restart. Confirm the other capture kind stays active.
- Exercise screen sharing, fullscreen and picture-in-picture where supported.
  Verify viewer mute, volume and hide controls do not affect another viewer.
- Reconnect during public/private conversations; check drafts, unread counts and
  account isolation after logout. Verify a redeemed recovery key cannot be reused.
- Check keyboard-only dialogs, screen-reader labels, push-to-talk, participant
  menus, desktop resizing and real mobile navigation/capture.
- On touch devices, open the keyboard, rotate the device, switch apps or lock
  the screen, then return. Check that controls remain reachable and media either
  works or gives clear recovery guidance. Record what happened; background media
  behavior is not assumed to match desktop browsers.

Headless tests use fake devices and resized desktop viewports. Complete the
manual checklist on real browsers and devices before a release.
