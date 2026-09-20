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

Push CI retains that same immutable production image for deployment. Offline
operations tests cover export without rebuilding, commit/run/artifact identity,
bounded CI waiting, single-host selection and stopping after a failed deployment
or public smoke. Deploying a retained image does not rerun the full suite; it
checks artifact integrity, runtime compatibility and readiness, then runs the
bounded public chat smoke. See the [release workflow](../ops/ansible/RELEASES.md).

Run it only as root on a fresh, disposable Linux/amd64 host with a local Docker
Engine (API 1.48+), Compose, OpenSSL, curl, `nsenter`, `update-ca-certificates`, and the
[controller Python environment](../ops/ansible/README.md):

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

Rust linting, native/PostgreSQL tests and the browser suites run as parallel
CI jobs. The build jobs share one dependency cache keyed by the vendored
sources, whose files are given a fixed mtime so cached native-worker artifacts
stay valid across checkouts, and every Rust target is built with the same
feature set so the worker compiles once. The production image reuses cached
layers up to a warmed dependency build.
[CI](../.github/workflows/ci.yml) also runs formatting, locked Rust builds/tests,
web tests/build, readiness/shutdown and pinned Chromium integration tests, dependency audits, native
dependency guards and production-image non-root/loader checks.
The [browser compatibility workflow](../web/e2e/README.md#ci) adds weekly and
manual Firefox/Linux and WebKit/macOS runs.

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

| Device | Browser |
| --- | --- |
| macOS | Chrome and Firefox, tested separately and together |
| iPhone | Kagi |
| iPad | Safari |

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
