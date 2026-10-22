# Testing

Run commands from the repository root after [development setup](development.md).
Use disposable local services, never a shared or production database. Direct Cargo
commands require the development setup's static OpenSSL and AWS-LC environment,
including the matching AWS-LC Rust bindings. `cargo run --locked -q -p xtask -- check --rust` checks both
native prefixes before invoking Cargo.

## Fast checks

```sh
cargo run --locked -q -p xtask -- check
cargo test --locked --workspace -- --test-threads=1
```

`cargo run --locked -q -p xtask -- check` shares the CI quality gates: typed web lint, formatting,
source/helper tests, TypeScript/build, Rust Clippy, checked documentation, and
strict Python lint/format/types. Use `--web`, `--rust`, `--helpers`, or `--python`
for a focused group. It requires installed development dependencies and never
starts application services. Native tests
are deliberately separate from the source-quality gate.

The Rust workspace tests include the operations manifest policy checks and the
migrated quality-gate and local-launcher regressions. For tooling-only changes,
run the relevant crate without the server's native test graph:

```sh
cargo test --locked -p xtask -- --test-threads=1
cargo test --locked -p simplestchat-ops
cargo test --locked -p xtask ci::ops_artifact -- --test-threads=1
```

The [local Ops producer](../ops/README.md#controlled-local-unsigned-producer) has
inert selected-tool and owned-filesystem regressions. Those tests do not run
Cargo as a real builder, link or execute a candidate, acquire a toolchain,
sign or install anything. Its production command is separately authorized by
an original-request pin and complete caller-selected source/curated-kit pins;
those hashes are not authentication or proof of complete compiler I/O closure.

The same focused group includes the [selected local Git source exporter](../ops/README.md#selected-local-git-source-export).
It uses bounded fake commands and tiny temporary Git repositories with isolated
configuration/templates, including a corrupted nested-tree/OID fixture. An
installed regular Git supporting the fixed no-lazy-fetch options is
required for those local plumbing fixtures. No clone, fetch, build, candidate,
engine or host operation runs. Export observations do not authenticate acquisition
or replace eventual trusted CI and Debian runtime evidence.

The [offline registry hydrator](../ops/README.md#offline-lock-bound-registry-hydration)
uses tiny inert gzip/tar archives and owned filesystem faults. Focused tests
check original lock/archive SHA binding before parsing, mature TOML depth and
duplicate refusal, complete gzip/tar framing, safe paths and extension budgets,
actual stored-byte Cargo checksums, normalized modes and independent inventory.
They never invoke Cargo, a compiler or imported crate/build-script code. Tests
also fence known cancellation/deadline outcomes and descriptor-budget unwind;
an explicitly authorized real cache extraction is separate from those fixtures.

Rust tests include native media checks; database tests are opt-in. Serial
execution reduces UDP port-allocation races. Web tests use mocked browser APIs;
the web build runs TypeScript checking and produces the production UI.

Run the browser unit suite with `npm --prefix web test`. It uses Node 26's native
TypeScript support and `--experimental-test-module-mocks`; the package script
supplies that flag. A focused invocation from the repository root is:

```sh
node --experimental-test-module-mocks --test web/tests/chat-store.test.mjs
```

`web/tests/native-fixtures.mjs` imports actual source modules. Its small resolver
handles the browser's extensionless imports and unit-test stylesheet no-ops;
Node owns module linking, evaluation and dependency mocks. Each fixture captures
its injected browser globals in its own module instances, so concurrent sessions
remain independent. Tests may import unmocked dependencies normally. The compiler
separately checks the strict browser project and positive/negative type-contract
fixtures; native type stripping performs no type checking. Module mocking remains
an experimental Node API, so use the pinned Node release when reproducing CI.

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
cargo run -q -p xtask -- test-postgres \
  cargo test --locked --lib runtime_canary \
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

The automation job checks the actual branch-tip subject, including a pull
request's head commit, with `cargo run --locked -q -p xtask -- ci commit-subject`.
It accepts only a complete 40-hex `COMMIT_REVISION` and captures the fixed
`git log -1 --format=%B REVISION` command privately, with a 30-second deadline,
1 MiB output limit and 64 KiB first-line limit. The first LF ends the subject;
NUL removal and C-locale byte whitespace rules match the unchanged POSIX
`.githooks/commit-msg` hook, including non-UTF-8 input. Diagnostics never echo
the message. Known Git failures and interrupts retain their status; grammar or
bound failures return 1, and invalid invocation/revision returns 2. Owned
command cleanup covers descendants remaining in its group.

The same pinned compiler step now precedes this check, before automation cache
lookup. Rejected CI subjects therefore pay toolchain setup and any cold xtask
compilation first; hosted Linux timing remains unmeasured. Local Git commits
continue using the bootstrap-free POSIX hook. Byte-oracle and inert Git-process
fixtures check these boundaries without installing tools or contacting a host.

Use Python 3.12 or newer and the pinned controller/checking environment:

```sh
python3 -m venv ops/ansible/.venv
ops/ansible/.venv/bin/pip install -r build/python-requirements.txt
cargo run --locked -q -p xtask -- check --python
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
node build/diagnostics-smoke.mjs --binary "$PWD/target/debug/simplestChat" --generator "$PWD/target/debug/load_test"
```

This additionally requires complete recorder coverage, successful `join_room`
and `room_lock_wait` records, and clean server exit. It prints and retains a new
private `results/diagnostics-smoke.*` directory, including failure artifacts;
it does not use a browser, capture media or upload anything. See
[diagnostic interpretation](diagnostics.md#read-the-results).

## Disposable PostgreSQL and browser tests

The owned test environment is a Rust command: `cargo run -q -p xtask -- test-env`
wraps `test-postgres` (a disposable loopback cluster) around `test-server` (an owned
server with a clean configuration), and each runs alone too; `xtask/src/test_env/`
holds them. `cargo run -q -p xtask -- test-server` runs `target/debug/simplestChat` as it finds it, so
build first: a stale binary answers a new message type with "Invalid message
format". The disposable server also opens registration and raises two
per-address budgets for the five browsers a suite drives from one address
(`REGISTRATIONS_PER_IP_PER_HOUR=100`, `AUTH_REQUESTS_PER_MINUTE=300`); a
deployment keeps the defaults in [configuration](configuration.md).

CI's three controller database steps use a fixed typed selection:

```sh
cargo run --locked -q -p xtask -- ci database-check monitoring
RESTORE_CONTAINER_E2E=1 cargo run --locked -q -p xtask -- ci database-check restore
cargo run --locked -q -p xtask -- ci database-check migration
```

These retain the existing Python test patterns. The restore step first pulls the
same digest-pinned PostgreSQL 18.6 image and retains `RESTORE_CONTAINER_E2E=1` in
the workflow. Discovery runs literal `pg_config --bindir` with a 30-second outer
bound and 4096-byte stdout ceiling, admits one existing absolute directory, and
prepends it using `join_paths`. The optional pull has a 600-second outer bound;
the existing ten-minute automation-job deadline still applies. The environment
is captured once and threaded into preparation, cluster configuration and the
test child; no process-global environment is changed. Preparation transfers its
registered signals only after CLI group cleanup, then awaits the same cluster
owner directly without another Cargo build or nested harness. These checks do
not replace the Python suites' container ownership. Fake tools and inert children
verify the command/environment/cleanup boundary; hosted Linux PostgreSQL and
container runtime cost remain unmeasured until that integration runs.

With PostgreSQL tools on `PATH`, use the owned bootstrap:

```sh
cargo build --locked --bin simplestChat
cargo run -q -p xtask -- test-env \
  cargo test --locked --workspace -- --include-ignored --test-threads=1
```

It creates a fresh loopback-only PostgreSQL cluster on port 15434, starts and
migrates an owned application server, then stops both after the command. It
refuses an occupied port and retains its private temporary directory for
diagnostics. Set `TEST_POSTGRES_PORT` when another local service owns that port.
The trust-authenticated database is disposable, never a shared development DB.

Each harness test command owns a fresh process group. It retains the leader's
identity through final group cleanup, including when that leader exits normally
and leaves descendants, then reaps it. INT/TERM keeps the existing ten-second
test-command cleanup grace and returns 130/143; unproven group cleanup cannot
pass or hide an existing test failure. These signals use the shared numeric
process owner, without a `kill` command lookup. Groups do not cover children
that start a separate session or external resources owned by the selected test.

Startup signal handling is active before any setup or readiness helper starts.
The application, optional HTTPS proxy and TURN relay each retain a separate
owned group through bounded shutdown and reaping; `groupCleanupPassed` is
recorded separately from leader TERM/KILL attempts in server/relay receipts.
A zero leader status alone cannot pass shutdown. Readiness/version CLIs also
own groups and have outer deadlines; captured metadata is limited to 4096 bytes.
A normally exited helper's group is retired before waiting for final pipe EOF.
Cleanup continues through repeated interruptions; a pending INT/TERM still
prevents an otherwise successful harness from returning zero.

PostgreSQL daemon ownership uses `pg_ctl` and the newly created data directory,
separately from these CLI groups. Status 3 is a not-running observation; status
4 or a failed probe is uncertain, triggers an exact-data-directory stop attempt,
and cannot pass. A successful startup requires a final not-running observation
after shutdown. PostgreSQL detaches its startup child before writing its pidfile:
an unsuccessful, timed-out or interrupted dispatched start therefore stays
**unproven**, even if later probes all return 3. Cleanup makes bounded best-effort
status/stop retries over a five-second observation window, checked between
individually bounded helper calls; this is not a five-second total cleanup limit
or proof that a delayed detached child cannot remain. The original failure or
130/143 is preserved, the private diagnostics remain, and this path never claims
successful daemon closure. No executable-name or discovered-PID kill is used.


For browser tests, replace the Cargo test command with
`npm --prefix web/e2e test` after the [browser installation steps](../web/e2e/README.md).
Build the UI first; do not rebuild assets during a browser run.

### Browser runner

Maintained browser suites use the pinned `@playwright/test` runner. The Rust
`test-env` command owns PostgreSQL and the application; a runner fixture owns
each browser process and requires a clean observed exit. Existing `node
web/e2e/<suite>.cjs` commands remain compatible and select the same runner tests.
Importing a suite exposes its in-process `run` function without launching it.

```sh
E2E_BROWSER=firefox cargo run --locked -q -p xtask -- test-env \
  npm --prefix web/e2e test
E2E_BROWSER=chromium npm --prefix web/e2e run test:layout
E2E_BROWSER=firefox npm --prefix web/e2e run test:resilience
npm --prefix web/e2e run test:runner -- --list
AUTH_LIFECYCLE_E2E=1 cargo run --locked -q -p xtask -- test-env \
  npm --prefix web/e2e run test:runner -- --project=accounts-chromium --grep=auth-lifecycle
```

Projects group account suites under `accounts-<engine>`, media suites under
`media-<engine>`, and the bounded UI workload under `stress-chromium`.
Community, layout and resilience have their own engine projects. Each suite
keeps its explicit opt-in environment variable; selecting a group does not
enable its other scenarios. Passkey fixtures require Chromium. Layout and
resilience use intercepted local browser fixtures and own their frontend listener.
Real-backend Firefox media also requires an interface-owned `TEST_ANNOUNCE_IP`,
as described in the [browser guide](../web/e2e/README.md).

`cargo run --locked -q -p xtask -- media-ip` prints that address through a bounded
owned output process. `media-ip --github-env` instead appends only
`TEST_ANNOUNCE_IP=<IPv4>` to the captured existing `GITHUB_ENV` file. The file must
be absolute, regular, singly linked, owned by the current user, at most 1 MiB
including the new line, and not writable by group or other users. Existing
contents must be UTF-8 with LF termination and no NUL or CR. Symlinks, changed
identity and malformed files refuse publication. Retained descriptor checks
assume the caller exclusively owns that runner file; they are optimistic checks,
not a lock. A line can already be visible when a later close, signal or identity
check fails, so its presence alone does not certify successful completion.

Both modes capture the environment once and use one INT/TERM registration.
The unchanged Linux and Darwin parsers read only the default interface's route
and IPv4 listing. Each query has a three-second body timeout and a 64 KiB stdout
limit plus one overflow sentinel byte. Every acquired CLI group receives a
retirement attempt before its captured pipe's final EOF drain and explicit close,
including normal leader exit with inherited pipe writers. Successful output
requires positive group-retirement proof. The detector schedules two 25-second query slots
within 60 seconds; the standalone command schedules 90 seconds, including a
30-second output slot and final checks. These are admission/watchdog policies,
not hard OS, reap or filesystem timing guarantees. Known INT/TERM and timeout
outcomes retain 130/143/124. They do not prove closure of privileged or detached
processes outside the owned group. Cold compilation and hosted route/runtime
behavior have not been measured by the inert regression fixtures.

The five two-browser CI steps use
`cargo run --locked -q -p xtask -- ci browser-suite SUITE`, where `SUITE` is
`auth-lifecycle`, `auth-cross-tab`, `room-control-confirmation`, `media-continuity`
or `presentation`. This runs Chromium then Firefox through the existing owned
`test-server` harness, stopping at the first failure and preserving its status.
Use the same explicit suite opt-in, installed browser tooling, built application
and disposable database required by the individual suite. `RUNNER_TEMP` must name
an existing absolute directory; each browser retains the workflow's existing
artifact subdirectory there. The two media suites resolve one interface-owned
announcement before starting either browser. INT/TERM stops the active harness
and allows up to 45 seconds for its normal cleanup before terminating remaining
members of that owned process group; an interrupted plan never starts the next
browser or publishes successful coverage.

Projects have bounded timeouts: accounts and layout three minutes, resilience
two minutes, and community, media and stress ten minutes. `E2E_RETRIES=0` is the
default; values `1` or `2` retry the complete test with fresh fixture ownership.
The community membership journey remains serial, with its individual checks
reported as runner steps.

`E2E_ARTIFACTS` selects the private report directory; the Rust harness sets it
for owned-service runs, and standalone runner commands create a fresh temporary
directory. Reports include `runner-results.json`, `runner-results.html`, a safe
`step-trace.json` timeline, each suite's bounded reports and redacted screenshots,
and `browser-lifecycle.json` shutdown evidence beneath `test-results/`. Directories
are private and report files use mode `0600`. Raw Playwright traces, video,
automatic screenshots and action error logs are disabled because account and
media API parameters can contain credentials. The step timeline retains fixed
scenario names, timing and outcomes. Existing diagnostic, canary, soak and
performance measurement commands retain their standalone execution paths.

The WebKit follow-speaker reload issue remains open as of 2026-10-21 with
Playwright 1.64.0 / WebKit 27.2 (r2370). One full owned community run passed
44 checks with one documented measurement skip, but a bounded reload probe
reproduced two fetch access-control `pageerror` events, including with the push
fixture removed. A credential-free control loaded no application code: three
native fetches returned 200 before navigation; during a held document
replacement, three explicitly caught rejections produced three access-control
`pageerror` events and zero DOM error/unhandled-rejection events. Browser and
server exits were clean. This supports the engine mechanism described in
[Playwright issue 43120](https://github.com/microsoft/playwright/issues/43120);
the relationship to the application's intermittent reload failure remains an
inference. Zero-page-error assertions remain enforced, with no ignored category
or application workaround.

### Passkey verification

Run native WebAuthn ceremonies with Chromium's owned virtual authenticator:

```sh
PASSKEY_E2E=1 cargo run -q -p xtask -- test-env \
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
checks that the old server record and sessions are retired. Its session-management
checks create two additional sign-ins for that owned account, remove one through
Account and then sign out all others, and verify HTTP rejection and live WebSocket
closure while the current session survives. Finally it signs out the current
session and verifies that refresh no longer restores it. Passkey-only Account
controls must retain invitations, session management and forget-device actions:

```sh
PASSKEY_E2E=1 cargo run -q -p xtask -- test-env \
  node web/e2e/passkey-management.cjs
```

Run `cargo run --locked -q -p xtask -- ci native-dtls` after configuring the pinned OpenSSL installation
to exercise the worker's DTLS closure reasons, media warning context and native
bandwidth constraint handling. It builds
a separate temporary worker test target, leaving Cargo's production archive untouched. Paired native
transports cover orderly shutdown, fatal errors, fingerprint/SRTP failures and
handshake timeout; the orderly-close case also checks warning-level output.
Fake-clock media cases verify inactivity/activity timing, bounded private
identities, tuple-history expiry/reuse and warning coalescing, including timer
cleanup and a finite burst's delayed summary. They do not send network probes.
The bandwidth cases exercise returned target/pacer/probe work, learned estimate
preservation, bound changes, synchronous observer reentry and bounded discovery
for unmet video demand. They cover offline/idle admission, repeated allocation
changes, probe spacing, estimate preservation without feedback and bitrate caps.
Real impaired
presentation recovery remains a separate hosted browser check.
The controller uses a fresh 0700 temporary directory and copies only bounded,
regular, nofollow source entries: at most 4096 entries, 32 nested levels, 16 MiB per
file and 64 MiB total. It refuses links, special files, invalid UTF-8 names/text
and changing selected bytes or directory membership. Source files are stamped
when read and revalidated after copying. These are optimistic identity/stamp
checks, not an atomic source snapshot or an authenticated Cargo/runtime closure.
Retained namespace walks permit at most 24 absolute path components. The original
`build/check-native-dtls.sh` remains the unchanged reference for inert selection
and command-policy tests; ordinary checks do not compile the worker.

Python, CC, CXX and pkg-config selection uses captured PATH/caller-directory
semantics, including literal paths with spaces. Python executes through its
validated selected absolute path, preserving ordinary symlink/virtualenv lookup;
the owned worker executes through its validated canonical path. Literal PYTHON,
CC and CXX values still pass to Invoke. Executable metadata/path checks do not
authenticate pyvenv.cfg, Python modules, compiler binaries or the complete build
runtime, and Invoke's own working-directory changes can affect relative compiler
lookup. The clean environment preserves the reference's pinned wheel, pip,
Meson-output and test-tag settings without ambient Python/pip configuration.

Every phase has a fresh reserved process group. Combined build, DTLS and orderly
logs each stop at 64 MiB of actual file bytes; group retirement precedes bounded
EOF/explicit close. Failure retains the private logs and removes disposable
source/tool/build trees where their binding remains proven. Success removes
the prefix. Removal is limited to 100000 entries and 64 levels; failure is not
silently ignored. Existing 60-line failure and 3-line success tails, including long
lines, share a 64 MiB forwarding ceiling and use an owned bounded replay child.
Output, cleanup, late interruption and entry-relative deadline failures cannot
return 0. No process owner claims detached/session/privileged descendants.

CI keeps its lookup-only `native-dtls-v2` namespace and existing 12-minute check
and 25-minute job budgets. The expanded key includes vendored/reference/install
inputs plus xtask/shared providers, workspace manifests, Cargo.lock, compiler
configuration and the workflow itself. Unchanged UI bytes do not invalidate it.
A cache miss installs the existing pinned Rust 1.99.0 toolchain and runs the
controller with `--record-success`; ordinary local invocation never publishes a
marker. Cargo cold compilation inside that budget remains unmeasured. The old
roughly nine-minute native build timing is not evidence for this port.

The authorized CI option publishes the fixed unsigned `native-dtls-verified`
revision marker after successful assertions, output and owned-prefix cleanup.
The publisher explicitly closes, then checks pending cancellation/deadline.
Failures attempt rollback only through the original complete namespace witness
and exact staged inode/stamp, including uncertain rename/sync outcomes. If close
or re-admission fails, a marker may remain; its presence alone is not final
process/close proof or a release signature. Late failure logs are retained under
a fresh private `.failed` prefix only if that original parent can be re-admitted.
The existing artifact glob covers both prefixes. Inert fixtures and Python oracle
comparisons do not establish native compilation, wheel installation or hosted
runtime parity.

Exercise call outcomes with real browser decoding on owned loopback services:

```sh
CALL_OUTCOME_E2E=1 IMPAIR_SCRIPT=none IMPAIRED_PROFILES=baseline \
  cargo run -q -p xtask -- test-env \
  node web/e2e/impaired-network.cjs
```

This mode checks one terminal observation for an empty room, a video-only join,
an audio-only join and an audio reconnect using the diagnostic preview. It uses
synthetic devices and cannot establish physical output audibility.

### Chat removal and saved history

The community browser suite checks public removal confirmation/cancellation,
member/PM control visibility, quoted-text scrubbing on all peers and tombstones
after a real signaling reconnect and snapshot. It also exercises public-history
retention On/Off, guest reads, search and removal, plus reading and sending account
PMs through Messages with live delivery and recipient isolation. Run against the
freshly rebuilt server and production UI with an owned disposable database:

```sh
COMMUNITY_E2E=1 cargo run -q -p xtask -- test-env \
  node web/e2e/community.cjs
```

The focused Rust database case
`database_public_message_removal_scrubs_durable_history_without_runtime_copy`
persists messages, removes the runtime copy, and verifies old-history deletion,
quote scrubbing, PM/cross-room exclusion and one durable moderation event across
repeated requests. Run it with the migrated disposable database and
`--include-ignored`; ordinary Rust tests skip it. Source tests cover late replay,
retry acknowledgements, removed reactions and reply-draft cancellation.

`database_history_retention_pagination_private_isolation_and_monotonic_reads`
covers saved pagination, expiry, isolation and forward-only read positions;
`authorization_database_inbox_offline_delivery_read_sync_and_consent` covers the
real inbox HTTP routes, offline sends, retries and preference enforcement.
Browser source tests cover hidden/covered conversations staying unread, failed
searches, out-of-order loads, account changes and idempotent send retries.

Manually verify two physical devices and a server restart, newcomer access to
opted-in room history, retention shortening/extension and guest PM continuity.
These device/restart journeys and backup retention are separate from the
automated browser and database checks above.

### Authenticated chat continuity

Run the opt-in 20-minute session soak against a fresh local database and server:

```sh
SESSION_SOAK_E2E=1 cargo run -q -p xtask -- test-env \
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

For a silent UI review, set `E2E_ARTIFACTS` to a new directory and run
`node web/e2e/silent-ui.cjs` from the repository root with
`E2E_BROWSER=chromium` or `firefox`.
It checks auth-error visibility, keyboard activation, dialog focus return and
short-window reachability. Capture, peer creation, playback, Web Audio and speech
APIs are blocked before application code runs; any attempted use fails the review.
See the [browser guide](../web/e2e/README.md#silent-ui-review) for scope and artifacts.

### Automated accessibility smoke

After installing the browser tooling and building the UI:

```sh
ACCESSIBILITY_E2E=1 cargo run -q -p xtask -- test-env \
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
cargo run -q -p xtask -- test-server cargo test --locked --workspace -- --include-ignored --test-threads=1
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
cargo run --locked -q -p xtask -- container-smoke
```

The smoke creates its own disposable containers, applies every source migration
and checks health, HTML, all four public PWA files' MIME types and bytes, empty
rooms, one account write and restart of the same app container/image. HTTP is
published only on a random loopback port; PostgreSQL and the app share a fresh
network namespace and temporary data. Set `PRODUCTION_IMAGE` to another built
image. The local facade defaults to `simplestchat-ci:production`; CI uses
`cargo run --locked -q -p xtask -- ci container-smoke`, requiring an immutable
image ID and the explicit hosted/disposable Linux context.

The captured Docker selection must resolve to a local Unix endpoint. An explicit
`DOCKER_CONTEXT` retains Docker's precedence over `DOCKER_HOST`; unsupported
remote/TLS transports are refused. Every later call uses that frozen endpoint and
rechecks its bounded opaque engine ID. This is an equality observation, not
daemon authentication. Both images resolve to immutable IDs before the journal
and separate create/start phases. A settled inspection failure may trigger a
pull of the one fixed PostgreSQL digest; it is not evidence that an image is
absent. The pull shares the remaining active budget rather than a new 30-second
cold-pull limit.

Container ownership requires the generated name/nonce label, full ID and image
to match before removal. Successful scoped listing must confirm absence afterward;
an inspect failure alone cannot. Both retirements are attempted after failure or
cancellation. A lost create response without a positive witness remains unproven.
The five-minute watchdog reserves one minute for cleanup. Each readiness loop
allows at most 120 probes with 500ms delays, subject to that shared deadline;
their worst-case command times are not claimed to fit. Before every dispatch,
the body budget also reserves that command's grace plus 20 seconds for scheduled
terminal observation, group fallback and combined-log settlement, including
Darwin's zombie-only inventory proof. This is scheduling allowance, not a bound
on OS/filesystem stalls or reap scheduling. The outer CI step also includes
compilation. Cold hosted execution and real-image behavior remain unmeasured
for this port.

Artifacts stay private in a fresh 0700 directory, with exclusive 0600 fixed-name
files; `CONTAINER_TEST_ARTIFACTS` must select a new directory beneath an existing
parent. Temporary-parent aliases are canonicalized before anchoring. Sources
require no symlink ancestors/finals, four assets at most 16MiB each, and at most
4096 migration-directory entries; SQL is counted without reading its contents.
The bounded HTTP subset rejects folded/control-bearing headers and duplicate
Content-Type. Curl ignores inherited configuration, uses literal loopback URLs,
and never saves registration JWT/cookie output. Each container log retains both
channels privately, capped at 1MiB; raw guest logs are not echoed on failure.

The summary records individual assertion, engine, CLI-group and source-close
observations, without an overall success claim before its own publication/close.
CLI zero additionally requires final namespace/descriptor closes, pending-signal
and deadline checks. CLI groups cover only descendants remaining in their groups;
engine resource cleanup is a separate inspected-ID proof. Owned fake engine/HTTP/
clock tests and the unchanged `build/test-container.sh` oracle cover the policy.
The oracle records the env-shebang Bash choice/version. Bash3.2's `errexit` can
ignore the old failed `[[ ]]` port check; Rust always refuses non-loopback ports.

### Manual generator image ownership

The manually dispatched `load-generator-image.yml` keeps its Linux/amd64
`loadtest` target, fixed tag, restore-only layer cache, 45-minute job and two
uploaded filenames. After the pinned Rust 1.99 bootstrap it runs
`cargo run --locked -q -p xtask -- ci load-generator-image`. Bootstrap and cold
compile timing have not been measured; they consume the same external job
budget. This command requires an explicit hosted GitHub Linux runner or an
ACT runner marked `LOCAL_CI_DISPOSABLE=1`. It captures one environment and signal
registration, clears Git context overrides, and observes the selected checkout's
clean HEAD before and after the recipe. Selected Git, Docker and Zstd paths and
metadata remain bound through their final closes. These observations do not
authenticate executable bytes, Git configuration, source closure or the runtime.
Checkout and temporary paths must be UTF-8 and comma-free for the fixed cache
CSV argument; literal spaces and shell text remain ordinary path bytes.

The owner freezes one local Unix Docker endpoint and opaque daemon identity,
creates private Docker/Buildx configurations and records nonce builder/node,
container and labeled-volume intent before mutation. It validates the BuildKit
container's immutable ID, name, owner environment marker and exact state-volume
mount. Retirement inspects references, removes only the admitted exact container
and matching unreferenced volume, independently observes their absence, and
removes the private builder record with `--keep-daemon --keep-state`. An empty
lookup after an uncertain creation is unproved. Foreign replacements and changed
labels, mounts, references or daemon identity refuse success. These are
optimistic observations under exclusive caller-trusted engine authority, not
atomic ownership or whole-engine cleanup. Formatter shapes are source-checked
against Docker CLI 28.5.1 and Buildx 0.29.1/0.30.1; this is not a claim of
observed hosted-engine compatibility. The nonce builder deliberately replaces
the old shell's unconditional deletion of a fixed builder name.

The scheduling policy admits at most 2,700 seconds from command entry. The
40-minute build remains an upper cap; its actual body is clipped after reserving
120 seconds of grace, 20 seconds of settlement and a 1,500-second tail. Thus an
immediate build has at most 1,060 seconds, less preparation time. The tail accounts
for 27 actual cleanup CLIs (including daemon checks) at 26 seconds each, four
image observation CLIs at 104 seconds total, the save command's additional
26-second daemon check, three binary phases at 141 seconds each, two final Git
CLIs at 52 seconds, a shared 120-second hashing/graph budget, 25-second bounded
output replay and 30 seconds of closing/fences: 1,482 seconds with 18 seconds of
slack. Every dispatch rechecks remaining admission. Groups retire before final
EOF drain and explicit owned-file closes, including normal-exit descendants.
Hash workers retain the same async signal owner, cooperatively stop at chunk/
header/graph checkpoints and are always awaited before return. These are
watchdogs and scheduling allowances; OS, filesystem and reap stalls have no
hard wall-clock guarantee.

Export uses separately bounded binary stdout (8 GiB raw, 9 GiB compressed) and
private stderr (2 MiB), with metadata capped at 64 KiB and build logs at 64 MiB.
The closed Ops generator profile checks the original tar hash, runtime/revision,
fixed tag and every selected authoritative graph payload. Classic image IDs bind
the config hash and ordered raw layer DiffIDs; containerd IDs bind a verified
root descriptor and complete one-platform stored graph. Optional classic OCI
extensions must also be self-contained; a classic export retaining compressed
registration descriptors without their compressed payloads is refused. Unused
Docker `LayerSources` registration hints are outside that authority graph. No
layer is unpacked or loaded. General multi-platform/store/version compatibility
and hosted export acceptance remain unrun. The server archive verifier is
unchanged.

Compression and decompression use the selected Zstd executable, retained read-only
inputs and independent bounded outputs. The roundtrip raw hash/count must match
the verified export. Only the sealed compressed archive and exact observed
`image-id.txt` line enter the fresh final directory. Known child/guest failures,
130/143 cancellation and proven 124 timeouts remain primary over later cleanup
failures. Zero additionally requires group proof, joined hash workers, readback,
all owned closes and final signal/deadline checks. Final artifact visibility can
precede a late directory-close failure; presence alone is not CLI completion.
Private nonce directories/configurations/logs remain for failure evidence; no
raw tool or daemon output is publicly replayed. Engine daemon log storage is not
bounded by these CLI stream limits. Tests use synthetic tar bytes, owned fake
executables and the unchanged retained shell literal, without builds, downloads,
compression, engine or image workloads.

### Offline configuration validators

CI uses `cargo run --locked -q -p xtask -- ci validate-config promtool` and
`ci validate-config caddy` in the existing automation and deployment steps. Both
require the captured hosted or explicitly disposable Linux context and
`RUNNER_TEMP`. Caddy also requires the workflow's exact `CADDY_IMAGE` and literal
CI-only proxy secret. Image pins, guest argv, mounts, network isolation, capabilities,
tmpfs and Promtool resource limits match the retained shell command fixtures;
Caddy gains no new CPU, memory or PID limit. The commands do not build images.

Docker context selection resolves to one local Unix endpoint, then every
operation checks its opaque engine identity. This is an equality observation,
not daemon authentication. A successful bounded repository catalog permits a
pull of only the fixed selector when its digest is absent. Distinct tag aliases
may share one immutable ID; malformed rows, repeated tag/digest identities or
contradictory target IDs refuse. Failed commands, log completion or close errors
never authorize a pull. The selector is inspected again before immutable-ID
creation. Intent precedes create; exact name, nonce label, ID and image must
agree before removal. Empty lookup after an uncertain create is not absence
proof. Normal completion requires an independently inspected terminal guest
status, exact-resource removal and a successful subsequent absence observation.

The 570-second scheduling policy gives active work 120 seconds and reserves
450 seconds for settlement. The complete failure path admits at most 15 cleanup
CLIs with five-second bodies, one-second grace and 20-second settlement allowances,
plus bounded output replay and final fences. Every dispatch checks its remaining
allowance. This is a watchdog and refusal policy, not a hard bound on OS, reap or
filesystem stalls. Existing ten-minute automation and 95-minute deployment job
budgets are unchanged; earlier job setup and cold compilation/pulls can consume
them. Hosted cold timing and actual image execution remain unmeasured.

Selected inputs are the two current Promtool YAML files or `Caddyfile`, each at
most 1MiB, opened without symlink ancestors/finals and rechecked before explicit
close. Their framed original-byte hash is an optimistic selected-input observation,
not an authenticated revision or include/I/O closure. Private artifacts remain
under `RUNNER_TEMP/simplestchat-config-validation-KIND-TOKEN`, with a fresh 0700
leaf and exclusive 0600 fixed-name files. Metadata is capped at 64KiB (context
discovery at 4KiB), guest logs at 2MiB and admitted command logs at 96 files.
Both channels use one bounded combined pipe, and admitted CLI groups retire
before final EOF drain, explicit log close and anchored readback. This does not
bound the engine's own log storage or prove closure of detached/privileged groups.
Raw daemon/configuration output stays private; console output is a fixed bounded
guest-status projection. `config-validation.json` records prepublication facts;
its presence is not final CLI success. Zero additionally requires all input and
output closes, CLI/resource proof, bounded replay, pending-signal and deadline
fences. Owned fake processes/files/engines and the actual retained shell commands
provide inert coverage; no real engine or validator workload was run for this port.

### App-only releases and rollback

The workflow's unchanged 11-minute release step uses
`cargo run --locked -q -p xtask -- ci release-checks`. In the explicit hosted or
disposable Linux CI context, this resolves the first executable Node in its
captured `PATH` and runs the literal pinned Playwright Chromium-path expression.
Ordinary Node/Chromium symlinks resolve to canonical paths; both must be nonempty,
regular executables. The proof reads metadata, never executable contents, so it
imposes no executable-size ceiling. Chromium discovery is capped at 30 seconds
and 4096 stdout bytes; malformed, relative or control-character paths are refused.
Retained descriptors and file stamps are rechecked before the Python coordinator
and after it finishes, then explicitly closed. These are identity snapshots,
not descriptor execution or protection against a compromised same-UID writer.

The existing `python3 -B build/ci_release_checks.py` coordinator retains its exact
controller/image/output arguments, parallel scanner-download and release-fixture
policy, private output and status. Rust inherits coordinator output, preserves
failures, and owns only its CLI process groups. Cancellation gives the coordinator
15 seconds for its existing parallel 5-second TERM/5-second reap paths before
forced group cleanup; the 660-second command budget reserves that grace and
reaping time. The coordinator and privileged fixture retain ownership of their
separate-session children and containers. Successful Rust CLI-group cleanup
does not establish their cleanup or replace the fixture's report. Owned inert
Node/Python fixtures cover this wrapper; cold hosted runtime and an actual
release/rollback run remain unmeasured for the port.

CI also runs `build/test-release-container.py` against that existing production
image, using the real release helper and public deployment templates. It checks
a successful replacement, failed candidate startup and rollback while requiring
PostgreSQL and Caddy to stay running.

The same hosted fixture holds two Chromium documents across upgrade and rollback.
Each first imports its edition-specific deferred chunk only after the selected
backend no longer has it. The check requires direct backend 404s, successful
proxy delivery, uncached HTML and revision metadata after every image change,
unchanged document identities and zero capture calls. The fixture uses synthetic
public chunks with the production binary and pinned proxy; it does not run a
historical application or proxy binary.

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
remains Linux/AMD64; an ARM64 fixture host needs AMD64 emulation. Use the CI-pinned
Node/npm installation, install `web/e2e` with `npm ci --ignore-scripts`, and install
its pinned Chromium with `npm --prefix web/e2e exec -- playwright install --with-deps chromium`.
Pass both executable paths explicitly across `sudo`:

```sh
release_node="$(command -v node)"
release_chromium="$(node -e 'process.stdout.write(require("./web/e2e/node_modules/playwright").chromium.executablePath())')"
sudo ops/ansible/.venv/bin/python -B build/test-release-container.py \
  --disposable-host --image simplestchat-ci:production \
  --node "$release_node" --chromium "$release_chromium" \
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
all targets with `cargo build --locked --workspace --all-targets --profile test`,
including development dependencies and test harnesses; it then runs the complete
test command with ignored tests included and one test thread. A browser-only
binary cache cannot substitute for that test graph. The production image caches
completed application layers as well as warmed dependencies, with a new cache
key whenever its build inputs change. BuildKit validates fallback layers against
the current inputs. The native image policy commands are:

```sh
cargo run --locked -q -p xtask -- ci production-image build
cargo run --locked -q -p xtask -- ci production-image verify
```

They require Linux and either a GitHub-hosted runner or explicit
`ACT=true LOCAL_CI_DISPOSABLE=1`. Ordinary local invocation refuses engine work.
The pinned Rust bootstrap runs on both CI paths. Build checks the exact
`GITHUB_SHA` and a clean checkout before any engine operation, journals a fresh
unique builder before creation, and always attempts its cleanup. It preserves
the 50-minute build timeout and two-minute forced-stop grace; the ARM64 native
BuildKit bootstrap retains its three-minute timeout and ten-second grace.
The source guard runs again after cleanup. Only a canonical immutable image ID
with a matching revision label and `SOURCE_REVISION` image configuration reaches
`GITHUB_ENV`, after builder cleanup. Existing runner
output files are append-only and must be regular files. Cache hits import layers
without exporting; misses promote a completed export only after success.
Verification checks the same source revision and immutable image, user
`10001:10001`, root-owned application/assets/migrations, the complete migration
inventory and absence of the load generator. Its fixed guest shell runs with
network disabled, read-only root, all capabilities dropped and no new privileges.
The verification container has a journaled unique name and owner label; start
and cleanup use its inspected immutable ID. Cleanup refuses a changed identity.
SIGINT/SIGTERM return 130/143, timeouts return 124, invalid context returns 2,
and command failures preserve their exit status. Owned fake process tests check
these boundaries without contacting an engine; they do not establish real
container-build parity. The release fixture's controller preparation overlaps
the image build through two commands:

```sh
cargo run --locked -q -p xtask -- ci release-controller start
cargo run --locked -q -p xtask -- ci release-controller wait
```

The launcher creates an origin-bound private namespace and hands off to a
detached broker before publishing its commit. The worker dispatches no installer
before that commit. A ten-second precommit lease bounds an incomplete handoff;
publication uncertainty cannot authorize the launcher to signal a detached
worker by PID. The broker retains the original four apt/venv/pip commands,
inherited tracking/PIP environment and pinned requirements, whose descriptor
and namespace binding remain checked through installation. The venv and later
Python invocation keep their existing paths.

Private records are capped at 16 KiB, commit/cancel requests at 4 KiB, and actual
combined log bytes at 64 MiB. Descriptor walks admit at most 24 path components.
The worker has a 90-minute fallback watchdog, including a 20-second cleanup
reserve; this does not prove the remaining time in the 95-minute job. The
five-minute collector verifies closed log/status hashes before replaying the
original bytes through an owned bounded replay process. Failed installers still
fail collection with status 1; observed cancellation/deadline results remain
130/143/124. Malformed settlement cannot erase an already chosen interruption.

Prepared evidence covers settled installer CLI groups and the closed
requirements/log/status facts. It does not certify worker exit or the later
publisher-directory closes. The collector checks its own namespace/source
closes and final signal/deadline fences. No numeric PID is signaled across
steps. Sudo/root/sessioned descendants and engine resources remain outside the
CLI-group claim; a missing terminal record is unproven preparation. If later
steps skip collection, the worker watchdog and runner disposal remain separate
boundaries. Owned inert-command tests exercise the protocol without installing
packages or claiming hosted-runner cancellation parity.
The initial cold `xtask` compilation precedes the overlap; no hosted Linux
startup cost or installer timing has been measured for this port.

[CI](../.github/workflows/ci.yml) also runs formatting, locked Rust builds/tests,
web tests/build, readiness/shutdown and pinned Chromium integration tests, dependency audits, native
dependency guards and production-image non-root/loader checks.
The [browser compatibility workflow](../web/e2e/README.md#ci) adds weekly and
manual Firefox/Linux and WebKit/macOS runs.

The preceding Compose installation uses
`cargo run --locked -q -p xtask -- ci install-compose` in the same explicit
hosted/disposable Linux contexts. It authenticates the existing Compose 5.6.0
AMD64/ARM64 digests before either fixed `sudo install` command. Curl retains its
HTTPS/TLS policy, 10-second connection and 60-second transfer bounds, plus a
256 MiB artifact cap. Output goes directly to a retained descriptor in a fresh
0700 directory. Both the download and the private install copy receive bounded
SHA-256 checks with stable metadata/bindings; input files close before privilege.
The caller's version check retains its 10-second limit and two-second kill grace.
The root check retains its clean environment, fixed daemon socket and inner
GNU timeout (10 seconds plus two-second kill grace), with an outer 15-second
owned-command bound. Both must report exactly 5.6.0.

All exits attempt owned staging cleanup. A fixed private
`compose-install-<random>.json` records stage, status, verified/download state,
installation attempts/completion, version checks and separate process/staging
cleanup proofs. The process proof covers the owned CLI group; sudo can separate
root process groups. The retained inner timeout bounds the root version check.
Its parent
namespace stays descriptor-bound through the final exclusive 0600 report write;
received command text and paths never enter that evidence. An interrupted
privileged install can already have changed the plugin; cleanup retires staging
and does not undo that installation. The step keeps its three-minute budget,
including its first cold xtask compile; hosted cold-start timing remains
unmeasured. Owned fake commands verify policy and cleanup, without downloading,
installing or contacting an engine.

The npm setup steps keep `node build/install-npm.mjs` as the authenticated
installer, followed by `cargo run --locked -q -p xtask -- ci npm-path` to
publish its fixed 12.2.0 `bin` directory. Publication requires an existing
absolute `GITHUB_PATH` regular file and owned runner/prefix/bin directories
that are not writable by other users. It refuses final symlinks, special files,
relative paths and control characters in the published path; it preserves the
runner file and closes retained descriptors explicitly. The command starts no
child or download and does not reauthenticate the installation or prevent later
changes by the same filesystem owner. The web job installs its pinned Rust
toolchain before this step. Filesystem fixtures cover the publication boundary;
the Node installer's existing tests remain the reference for installation.

The automation controller's apt, venv and pip commands also remain direct. After
successful installation, `cargo run --locked -q -p xtask -- ci controller-path`
publishes only `RUNNER_TEMP/ansible-controller/bin` through the same protected
PATH-file writer. It accepts no alternate prefix, starts no installer and cannot
substitute npm's directory. The existing compiler setup precedes controller
installation; no new toolchain bootstrap or token-bearing step is introduced.
Both roles attempt every retained descriptor close, and a write or close failure
cannot report success. This verifies publication policy, not controller-package
authenticity or immunity to later changes by the same filesystem owner.

The unconditional Rust dependency audit uses
`cargo run --locked -q -p xtask -- ci audit-rust` in the same explicit
hosted/disposable Linux contexts. It retains the reference
`python3 build/security_tools.py install --tools cargo-audit --directory PATH`
and `path cargo-audit --directory PATH` commands. That installer authenticates
the scanner and its receipt. The native command accepts only the selected
`tools/bin/cargo-audit` beneath its fresh 0700 prefix, retains its descriptors,
and rechecks file and directory identities before execution and after audit.
Files must be executable, regular, single-link and at most 256 MiB; directories
and the scanner cannot permit group/world writes. This is a namespace/identity
snapshot, not descriptor execution or a lock against a compromised same-UID
runner.

The scanner retains inherited audit output and exactly `audit --deny unsound`,
with only `RUSTSEC-2021-0145` and `RUSTSEC-2026-0097` excluded for mediasoup-sys
build tools. The removed optional SQLx/rsa advisory remains unsuppressed.
Installer, path resolution and audit share one 30-minute command deadline; the
job remains unconditional with its existing 30-minute budget, including any
cold compilation. Every exit attempts descriptor closes and bounded,
descriptor-relative prefix cleanup without following links. A fixed private
`rust-audit-<random>.json` records stages, status and separate executable,
CLI-process-group and namespace cleanup evidence through the retained parent
descriptor. Received output and paths never enter that report. SIGINT/SIGTERM
return 130/143, timeout returns 124, invalid context returns 2, and subprocess
failures preserve their status. Owned fake installers/scanners verify these
boundaries without an install, advisory database download or live audit;
authenticated Linux installation and cold-start runtime remain unmeasured.

### Run CI locally

Commit the candidate locally, then run the complete required workflow without a
push or pull request:

```sh
cargo run --locked -q -p xtask -- ci-local all
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
  cargo run --locked -q -p xtask -- ci-local all --disposable-engine
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
so build commands use the kernel's Rosetta registration. The container driver and
exported layer cache remain in use, with a fresh journaled builder name for each
invocation; no pre-existing fixed-name builder is deleted. Authenticated OpenSSL
extraction uses Python's filtered tar reader, which works with Rosetta. These ARM64 test runs
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
both the executable and running server version. Authenticated containerd 2.4.1
and Buildx 0.37.2 releases override the older bundled components; the running
containerd and runc versions must match the maintained host selection. The outer engine socket is
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
matrices cannot report a complete local gate. Workflow jobs publish those records
with `cargo run --locked -q -p xtask -- ci-evidence record --id CHECK`; the final
local aggregate runs `ci-evidence complete`. The Rust producer preserves the
Python reference's JSON schema and bytes, reads the real workflow matrices, and
needs no Python YAML environment. YAML parsing accepts aliases and merge keys
with explicit document, depth and replay limits; includes and custom tags are
rejected. The local runner uses the same receipt identity and matrix validation.

`--base REF` selects the ancestor
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

The shared `cargo run --locked -q -p xtask -- ci required` policy reads
`GATE_RESULTS` once and requires successful results from exactly the ten canonical
workflow jobs. It rejects missing, skipped, cancelled or failed jobs, duplicate
job/result fields, malformed JSON and input above 64 KiB. Other job metadata and
outputs cannot establish success and are never printed. Both hosted and local
aggregate jobs install pinned Rust before this policy; hosted CodeQL health and
local execution evidence still run afterward. The added hosted compilation cost
has not been measured on GitHub.

Local diagnostic preservation uses `ci copy-evidence image-database`,
`ci copy-evidence release-container` and `ci copy-evidence release-checks` through
the same xtask entry point. These commands read the owned `RUNNER_TEMP` and
`LOCAL_CI_EVIDENCE` roots and copy only the workflow's fixed report/log allowlists.
An absent, non-directory or symlink source succeeds without requiring an evidence
root; a valid source creates a new directory even when optional files are absent.
Destination directories use `0700`, files use `0600`, and existing destinations
fail rather than overwrite earlier evidence. Symlink and special-file entries
are skipped; descriptor-relative access prevents replacement paths from escaping
the admitted directories. Each command bounds inventories to 4,096 entries per
directory, 1,024 copied files, 16 MiB per file and 256 MiB total. Exceeded limits,
changed sources and I/O failures fail quietly with a fixed message; private
paths and file contents never enter the console. The affected jobs bootstrap
the same pinned Rust toolchain only in the isolated local runner.

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
workflow installs the pinned Rust toolchain before the compiled Rust database-key
lookup. A database hit still skips fresh extraction and dependency-output reuse;
it now requires the small tooling build as well as the compiler bootstrap.

The key command is
`cargo run --locked -q -p xtask -- ci codeql-cache key --language rust --source-root /absolute/source`.
It requires the same declared hosted image or pinned local runner identity as
`ci key`; run it inside that runner environment.
The same entrypoint offers `validate-bundle`, `validate-source`, `save` and
`restore`; use `ci codeql-cache --help` for their explicit paths and arguments.
Keys reuse the CI source inventory and bind the analyzer/query pin, source path,
suite, trust/platform namespace, and native package/compiler/OpenSSL identities
where applicable. The Python module remains the scanner driver's import API and
parity reference. Reusing a database still regenerates reports and applies
current finding policy.

Native source readback supports the Stored and Deflate methods used by genuine
CodeQL archives. Other compression methods can be inspected as bounded bundle
metadata but are rejected when source bytes must be hashed. The validator never
extracts members: it rejects unsafe paths, decoded name aliases, links,
encryption, cached SARIF and missing evaluated queries. Matching UnicodePath
metadata is validated along with UTF-8/CP437 names; ambiguous NUL names are
rejected even where an older Python ZIP reader truncated them. Archives are
limited to 100,000 members and four GiB of expanded data, with sixteen MiB per
keyed source file. Save uses private staging and the existing exact receipt
format; restore verifies the bytes before the installed CodeQL CLI can unpack
them. Tests use owned files and fake analyzer commands, including cross-language
receipt interchange with the Python reference.
On 2026-10-21, a fresh debug tooling target built in 11.33 seconds on the local
macOS ARM64 workstation with Rust 1.99.0 and registry sources already downloaded.
That measurement excludes toolchain installation and downloads; hosted cold
timing has not been measured.

After successful push CI on main, `cache-retention.yml` removes obsolete caches
from known families. It verifies the triggering run and current main revision,
protects the newest cache in each trust/platform namespace and every cache
created or accessed within two hours, and targets eight GiB of total storage.
Unknown cache families and other refs remain untouched. Run
`cargo run --locked -q -p xtask -- ci-retention` for a read-only plan; applying it requires
`--apply --run-id <successful-CI-run> --revision <current-main-SHA>`. Deletions
are limited to cache IDs, with refreshed inventory before each deletion and
limits of 100 deletions and 240 seconds.
The hosted maintenance workflow builds the tool before exposing its scoped
token. The Python implementation remains a reference for planner parity; unit
tests exercise the Rust client with a fake `gh`, including incomplete inventory,
changed cache identity, stale main revisions, output limits and cancellation.
Canonical UTC GitHub timestamps are checked across supported Python versions;
the separate oracle for noncanonical ISO extensions follows the maintained
Python 3.14 reference and runs only on compatible interpreters.

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

CI fingerprints and success receipts run through `xtask ci key|save|verify`.
The pinned toolchain is available before a cache lookup. Keys retain the existing
SHA-256 framing, scope/trust/runner identities and receipt schema; the tracked
compiled tool and its Cargo/toolchain inputs are included in every source closure.
`--root <checkout>` selects an owned canonical checkout for local verification;
it defaults to this repository. `save` and `verify` require an absolute receipt
directory. Python remains the parity reference and supplies the current security
helpers' source-closure API; scanner drivers retain their existing implementation.

The four standard workflow fingerprint steps use
`cargo run --locked -q -p xtask -- ci key --scope "$CI_CACHE_SCOPE" --github-output`.
This computes the same scoped key and appends exactly one `key=...` line to the
runner-provided `GITHUB_OUTPUT`, without printing the key on stdout. The file must
already exist at an absolute path and be regular; the command appends without
creating, truncating or following its final symlink. Missing output configuration,
unsafe/open failures, failed fingerprints and write/close errors fail the step
with fixed labels. The flag applies only to `key`; ordinary `ci key` still prints
its key for local use. Cache namespaces, restore/save guards and success receipts
retain their existing policy. Database-byte selectors use the separate image-cache
publisher described below.

`cargo run --locked -q -p xtask -- ci automation` runs the installed Python
quality gate first, then offline Ansible lint, every maintained controller
playbook's syntax check, ShellCheck and operations unit tests. Each command's
arguments are explicit and the first failure stops the gate. It installs no
tools and applies no playbook to a host.

`cargo run --locked -q -p xtask -- ci native-security` verifies the native
dependency versions, source pins and overlay hashes used by the Rust test job,
then scans the built server for the required and forbidden OpenSSL versions and
checks Linux linkage with `ldd`. It reads local inputs and runs the fixed Cargo
inventory/linkage commands; it performs no installation or native compilation.
The Linux CI step runs it after building the server.

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
archive-generation key with separate main and untrusted namespaces. Every
invocation still runs the online updater, verifies the database hash and allowed age, and performs
the full vulnerability scan and current policy checks. Restored bytes have a
bounded file inventory and checked hashes; a malformed cache fails validation.
No image scan verdict is cached. Local image checks can use the same path with
`--image-database-cache <absolute-directory>`.

The two CI jobs use `ci image-cache hint` to publish the same Python reference's
prefix and optional metadata hint. Prefix failure remains fatal. A failed hint
falls back to that exact prefix with the existing warning only after its owned
CLI group is proven stopped/reaped; cancellation or uncertain cleanup fails.
Malformed successful replies fail before publication. Trust must match the
current main/pull-request boundary, and every scanner/generation digest must be
64 lowercase hexadecimal characters. These keys select download bytes; the
Python online updater and scanner remain unchanged.

After successful preparation or production scanning, respectively,
`ci image-cache status preparation` and `ci image-cache status production`
resolve only the fixed status paths beneath `RUNNER_TEMP`, using the unchanged
Python `--status` reference. Both key and prefix are validated before one fixed
record is appended to the runner's existing absolute regular `GITHUB_OUTPUT`
file. It never creates/truncates that file or follows a final symlink; its
retained descriptor closes on success or failure. Received command output and
paths are not printed. Every Python selector has a 30-second bound and
five-second forced-stop grace, and only owned CLI-process-group cleanup is
proven. A timed-out hint may fall back after successful cleanup; status failures
remain fatal. SIGINT/SIGTERM return 130/143 and cannot become a cache miss.

The image-database compiler bootstrap now also runs on hosted CI, at the same
reviewed action location and immutable Rust pin. Its 25-minute job and the
20-minute preparation / 25-minute production scan steps retain their guards and
budgets, including cold compilation. Hosted cold-start cost is unmeasured.
Owned fake selectors and pure Python-oracle fixtures verify output/fallback
policy without registry, updater, scanner or engine work; they do not establish
online freshness, cache integrity or Linux runtime parity.

Action and build caches persist under `target/act`. Each invocation writes a
private directory under `results/` containing its event, workflow log, source
revision/base, actual runner platform, excluded vendor-analysis scope, AMD64 production target,
exit status and retained compact check summaries. `--output`
accepts a new directory. A single-job run is available for focused fixes and
always reports that it is partial:

```sh
cargo run --locked -q -p xtask -- ci-local web
cargo run --locked -q -p xtask -- ci-local rust-lint
cargo run --locked -q -p xtask -- ci-local browser --matrix group:accounts
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
to repair this condition. Rerun `cargo run --locked -q -p xtask -- ci-local all` after reboot; its preflight
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

The hosted Linux comparison uses
`cargo run --locked -q -p xtask -- ci performance-compare`. It captures the
environment once, resolves the baseline before recording worktree creation
intent, and preserves the baseline Cargo working directory, inherited native
toolchain settings and shared candidate `target`. The saved baseline binary is
copied, read back and closed before the exact shared-target source is unlinked.
The candidate server and generator then build in their original order. The
10/30/100-client profiles, one/two-worker choices, three alternating repetitions,
ramp/warmup/measurement intervals and ring graph stay fixed. The selected Cargo
path is invoked as selected, including a rustup shim's basename; this is not a
compiler, build-script or source-origin authentication boundary.

Each CLI has a reserved process group and a bounded private combined log. Git
metadata is read back through its original anchored file binding (4 MiB); build
and comparison logs each allow 64 MiB. After any dispatched worktree add, cleanup
reconciles a complete successful Git inventory and only removes this invocation's
exact detached revision and retained directory. Missing, foreign or ambiguous
ownership cannot authorize deletion. Positive Linux removal also requires the
retained directory's link count to reach zero. Failed cleanup cannot return zero.

The comparison receives a generated `BENCH_CI_RUN_ID`; ordinary `bench local`
commands omit it. Its fixed `comparison-owner.json` is an exclusive private
completion observation bound to that ID, all six finished runs and actual owned
native group/stream retirement, including auxiliary commands. The outer owner
checks it after every admitted comparison settlement, independently of CLI status.
A missing or invalid receipt leaves inner closure unknown; it supplies no PID
authority or proof about escaped sessions, privileged resources or the host.
The outer CLI group alone cannot close the benchmark's separate service groups.

The owner admits commands within 90 minutes with a five-minute finalization
reserve, including three bounded Git cleanup slots. It reserves 90 seconds for
cooperative benchmark cleanup and 20 seconds of CLI settlement scheduling space;
before launch the six measurement intervals must still fit (450/810/1632 seconds
for 10/30/100 clients). These are admission policies, not bounds on OS scheduling,
filesystem operations, reap or cold compilation. The unchanged 90-minute job also
includes earlier setup and unmeasured compilation. Fixed profile, inert process
and retained Bash-reference fixtures do not establish hosted build or workload
timing. Threshold evaluation and artifact retention retain their existing guards.

For the separate fixed thirty-account, twenty-minute media/chat measurement,
dispatch `gh workflow run performance.yml --ref main -f only=account-room`.
Its measured command is `./target/release/load_test bench account-room`; the
Rust owner starts and retires both selected CLI groups without nested Cargo.
Unit tests use fake HTTP/database/process observations and actual JavaScript
policy oracles; they do not establish hosted performance or a live database run.
Run the focused, serialized fixtures with
`cargo test --locked -p simplestchat-load-test --lib account_room -- --test-threads=1`.
This job runs only on demand on an isolated GitHub-hosted Linux runner, with a
fresh PostgreSQL service, and exercises real refresh on every media connection.
It retains safe resource, delivery and database-inventory evidence, including
completed failures. See [the measurement contract](performance.md#continuous-comparison)
for its receive policy, validity gates and scope. It is outside the weekly jobs
and the required CI suite.

## Media correctness and performance

See [performance](performance.md) for the optional RTP generator and comparison
workflow, or [browser/API measurements](../web/e2e/README.md#informational-browserapi-performance)
for startup, chat delivery and decoded media. The short CI RTP smoke checks
delivery correctness; it does not set a performance budget.

### Repeated-session lifecycle check

With the server/UI built and Chromium installed:

```sh
LIFECYCLE_E2E=1 cargo run -q -p xtask -- test-env \
  node web/e2e/lifecycle.cjs
```

This opt-in check reuses two pages across six media sessions, including reconnect
and real grace-period expiry. It checks sampled media progress and cleanup while
retaining bounded diagnostics and informational memory trends. It is not part of
default CI or a substitute for physical-device checks. See
[configuration and evidence limits](../web/e2e/README.md#repeated-session-lifecycle-check).

### Session soak with a database interruption

The scheduled performance job uses `cargo run --locked -q -p xtask -- ci
session-soak` for the fixed full twenty-minute profile. The workflow passes its
PostgreSQL service ID, network and published port through the documented
[Actions job context](https://docs.github.com/en/actions/reference/workflows-and-actions/contexts#job-context).
The supervisor binds the fixed disposable database URLs to port 5432, pins the
local Unix Docker transport and rechecks the observed opaque daemon identity,
immutable image, full container ID, service network and published port before
mutation. Daemon ID equality is an observation, not authentication or a global
uniqueness proof. Ordinary host and mismatched-database contexts are refused.

One captured environment and registered signal owner pass directly through
the existing server harness, workload and Docker helpers. No nested Cargo,
background restarter or detached task owns cancellation. The 600-second delay
begins at supervisor entry; eight seconds begin only after a positively stopped
inspection. The same service must subsequently be observed running and healthy.
Failed or interrupted stop attempts receive bounded restoration attempts,
including polling transitional/starting states, before server cleanup. An
observed healthy state after an uncertain engine request does not prove that
no delayed daemon action can occur. The runner still owns service disposal.

The active fallback watchdog is 37 minutes beneath the unchanged 40-minute step;
the first workload-terminal observation must precede it. Cleanup may use the
reserve: restoration is scheduled against a 90-second deadline ending 40 seconds
before the entry-relative step boundary. Each 5-second inspection or 20-second
stop/start body is capped by remaining time minus 14 seconds for CLI settlement.
Workload/server retirement receive separate scheduling allowances. Calls that
cannot fit are refused, and uncertain restoration or cleanup cannot pass. The
last step-relative deadline and pending signals are checked after harness
cleanup. These allowances do not bound OS/filesystem stalls, cold compilation
or external job cancellation; hosted runtime remains unmeasured.

The fixed JSON supervision projection goes to command output after harness
cleanup. It contains only observed booleans and the harness status, and is not
a durable receipt or runner-disposal proof. `dockerCliGroupsSettled` covers
Docker helper groups; the overall status separately gates harness cleanup. It contains no container
ID, daemon identity, database URL or child output. Existing browser and server
artifacts retain their original locations. Known nonzero outcomes survive
restoration failures; success requires the scheduled outage, positive observed
restoration and successful CLI/harness settlement. Inert child and fake
clock/engine tests do not establish real soak or container behavior.

## Manual release checklist

### Real-browser targets

Record the OS and browser version, result and any reproduction steps for each:

| Device | Browser                                            |
| ------ | -------------------------------------------------- |
| macOS  | Chrome and Firefox, tested separately and together |
| iPhone | Kagi                                               |
| iPad   | Safari                                             |

On the Mac, run `cargo run --locked -q -p xtask -- run`, open `http://localhost:3000` in Chrome
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
- On physical iOS Safari and Android Chrome, repeat those lifecycle checks with
  the camera on and microphone muted, then with push-to-talk. Confirm a resume
  does not turn on a device the user switched off or reopen a private preview.
  Switch Wi-Fi/cellular and reconnect Bluetooth headphones; retain per-person
  mute/hide/volume and the selected speaker, or show a clear speaker-selection
  action if that output disappeared. Switch front/back cameras and unplug/replug
  an active input. After leaving or fully rejoining, capture must stay off until
  the user explicitly enables it. These hardware checks remain manual; the
  media-continuity suite uses synthetic lifecycle hints and fake device lists.
- Install on a physical iPhone/iPad Home Screen and an Android browser that
  supports Web Push. Enable notifications from Account, send an unread account
  PM with the app closed, and verify the generic notification opens Messages.
  Check denial, disabling, logout, another account on the same browser and revoked
  sessions. Browser fixtures validate the UI and service worker logic without
  contacting a real push provider or proving OS lock-screen delivery.

Headless tests use fake devices and resized desktop viewports. Complete the
manual checklist on real browsers and devices before a release.
