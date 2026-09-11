# Browser integration tests

The Playwright suite creates accounts and rooms. Use a disposable local
database and server.

## Install and run

Complete [development setup](../../docs/development.md), then start the
[disposable PostgreSQL cluster](../../docs/testing.md#disposable-postgresql-and-browser-tests).
Keep its `DATABASE_URL` and `DISPOSABLE_TEST_DATABASE=1` exports in the same shell.
From the repository root:

```sh
cargo build --locked --bin simplestChat
npm --prefix web ci --ignore-scripts
npm --prefix web run build
npm --prefix web/e2e ci --ignore-scripts
npm --prefix web/e2e run install:browser
build/with-test-server.sh npm --prefix web/e2e test
```

On Linux, install system dependencies with
`npm --prefix web/e2e exec -- playwright install --with-deps chromium`.
Playwright is test-only, excluded from the application bundle and production image.

The helper migrates the database, enables registration, waits for API readiness,
and stops the server afterward. It refuses occupied ports and non-test databases.
Do not rebuild `web/dist` during tests.

Defaults and overrides:

| Setting | Default |
| --- | --- |
| `TEST_SERVER_PORT` | HTTP `3119`, loopback only |
| `TEST_MEDIA_PORT` | UDP `41010`, one media worker |
| `TEST_SERVER_BINARY` | `target/debug/simplestChat`; use an absolute override |
| `TEST_ANNOUNCE_IP` | `127.0.0.1`; optional interface-owned IPv4 |

The helper exports `BASE_URL`, `COMMUNITY_E2E=1` and `TEST_DATABASE_URL`.
`build/with-test-server.sh true` checks migration/startup only. To use an
already-running disposable loopback server:

```sh
COMMUNITY_E2E=1 BASE_URL=http://127.0.0.1:3119 npm --prefix web/e2e test
```

`PLAYWRIGHT_MODULE` and `PLAYWRIGHT_BROWSERS_PATH` support isolated tool/browser
installations.

## Firefox and WebKit

Chromium is the default. Install the other pinned engines and run each with a
fresh server:

```sh
npm --prefix web/e2e run install:browsers
test_media_ip="$(node build/test-media-ip.mjs)"
TEST_ANNOUNCE_IP="$test_media_ip" E2E_BROWSER=firefox build/with-test-server.sh npm --prefix web/e2e test
TEST_ANNOUNCE_IP="$test_media_ip" E2E_BROWSER=webkit build/with-test-server.sh npm --prefix web/e2e test
```

These engines may need LAN rather than loopback media candidates. The helper
validates address ownership; HTTP and database access remain loopback-only.
Media UDP already binds all IPv4 interfaces. WebKit fake capture is supported by
this runner on macOS only. Unknown engine names fail rather than fall back.
Tests use isolated profiles and fake capture without disabling autoplay policy.

## CI

[Browser compatibility](../../.github/workflows/browser-compatibility.yml) runs
Firefox on Linux and WebKit on macOS every Tuesday at 07:23 UTC. To run it on
demand, select **Actions → Browser compatibility → Run workflow**. Chromium
remains in the push/pull-request checks.

Each job creates and stops its own loopback PostgreSQL cluster and server.
Reports, screenshots and logs are retained for seven days; database files are
not uploaded. Firefox's unsupported capture-termination check remains an explicit
skip in the report.

## Coverage and diagnostics

`community.cjs` covers accounts/owned rooms, private-message
isolation and preferences, mentions, profiles/images, moderation, recovery,
preview, decoded video/audio, viewer controls, capture restart and mobile layout.
Audio must be unmuted at positive volume with advancing playback. A separately
simulated autoplay rejection checks the visible retry button.

Capture termination is simulated on owned fake tracks. The check verifies remote
removal, controls, restart guidance, preservation of the other capture kind and
no automatic recapture. Firefox suppresses the synthetic track event; the runner
probes support and reports this scenario as **skipped**, not passed. Native Firefox
device termination still needs manual testing.

The helper prints the artifact directory; set `E2E_ARTIFACTS` to choose one per
run. It contains `server.log`, screenshots and `community-results.json` with
engine details, completed/skipped checks and failures. Failure diagnostics may
include local network addresses; keep artifacts private. Discard the test database
afterward, including any remaining accounts.

Connection failures also record ICE gathering, transceiver state and sanitized
SDP summaries. Raw SDP, ICE credentials, fingerprints and track identifiers are
excluded from these snapshots.

## Informational browser/API performance

With the disposable database still running and the UI already built:

```sh
cargo build --locked --release --bin simplestChat
PERFORMANCE_E2E=1 RUN_LABEL=local-sample \
  TEST_SERVER_BINARY="$PWD/target/release/simplestChat" \
  build/with-test-server.sh node web/e2e/performance.cjs
```

`browser-performance.json` records navigation/paint, Chromium task/heap samples,
bundle identity/transfers, two-user registration and room creation, ten paced
chat deliveries and five seconds of fake-camera decoding. The script checks
frame progress, page errors and cleanup; failures exit nonzero.

For comparisons, follow [performance](../../docs/performance.md): use release
builds, the same pinned browser, fresh databases and alternating revision order.
`TEST_SERVER_WORKDIR` selects the checkout/assets served by `TEST_SERVER_BINARY`;
the test command still runs from the current repository. Label reports with
`SERVER_REVISION`, `FRONTEND_REVISION`, `BUILD_TOOLCHAIN` and `RUN_LABEL`.

Limits: patched headless engines, fake capture and resized desktop viewports do
not establish branded-browser, physical-device or mobile behavior. Performance
samples include automation overhead and unflushed OS caches; they are not
whole-browser resource measurements or production-capacity budgets.

## Other checks

See [testing](../../docs/testing.md) for unit, database and container checks.
`checklist.cjs` is an older, non-CI checklist with potentially stale selectors;
`community.cjs` is the maintained browser gate.
