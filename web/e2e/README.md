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
`server-shutdown.json` records the owned child's wait status and requested signals;
forced termination, nonzero exit or missing shutdown evidence fails the command.
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

## Homepage layout without a backend

After building `web/dist` and installing the browser tooling above:

```sh
npm --prefix web/e2e run test:layout
```

This check renders production assets with intercepted room/account fixtures;
Chromium is the default, with `E2E_BROWSER=firefox` or `E2E_BROWSER=webkit` selecting
the other installed engines. It contacts no application server, grants no capture
permissions, and fails if the homepage requests media. Populated, empty, unavailable and filtered
directories are checked at 320, 375, 768 and 1440px, plus short landscape.
Long names, thumbnails, counts and badges must fit the card without overlaps or
horizontal scrolling; the last room and direct-join controls must remain reachable.
This is layout coverage, not backend integration or native mobile-browser proof.

The printed private artifact directory retains asset hashes, rendered geometry,
screenshots and browser cleanup results. `E2E_ARTIFACTS` may select a new directory;
an existing directory is rejected. CI retains the separate `homepage-layout` report.

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

Signaling recovery closes only the owned browser WebSocket while fake audio/video
is active, separately for publisher and receiver. The check requires successful
session recovery without a fresh join, new media peers or recapture, plus decoded
video and audible audio progress during the interruption and after recovery.
This does not simulate UDP failure, a network outage or expired reconnect grace.

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

Connection failures also record transceiver state, sanitized SDP summaries and
the latest 128 ICE events per peer, including candidate address classifications.
These snapshots exclude raw SDP, candidate addresses, ICE credentials,
fingerprints and track identifiers.

To isolate native ICE gathering without the app or database:

```sh
ICE_ISOLATION_E2E=1 E2E_BROWSER=webkit node web/e2e/ice-isolation.cjs
```

The check uses fresh browsers for receive-only offerer and answerer roles, with
no capture or ICE servers. Each must publish a candidate and finish gathering
within 15 seconds. `ice-isolation-results.json` contains sanitized snapshots;
this does not test connectivity or media. CI runs it after the community check,
including when that check fails.

For a same-host comparison, run **Actions → ICE isolation → Run workflow**.
It runs WebKit and Chromium on one hosted Mac without building the app, retaining
both reports even when a probe fails. Each engine keeps its existing launch options.

## Accessibility

With the same pinned browser tooling, built UI and disposable-service setup:

```sh
ACCESSIBILITY_E2E=1 build/with-test-postgres.sh build/with-test-server.sh \
  node web/e2e/accessibility.cjs
```

The separate `accessibility.cjs` runner uses axe-core WCAG 2 A/AA, 2.1 A/AA and
2.2 AA tags without disabling rules or excluding elements. It scans public join,
sign-in/register, room creation, joined chat, account and all personal/room
settings tabs on desktop and 320px layouts, plus light/dark Help. Keyboard checks
exercise actual chat overflow and the newest-message button, native dialog
Escape/focus restoration, and Help opening in a separate tab while the room stays
connected. It does not turn on capture.

`accessibility-results.json` records rule IDs, impacts, bounded selectors and
computed contrast styles for violations **and incomplete findings**, not DOM HTML.
Text-fragment rectangles and center hit tests aid review; they do not prove full
visibility. Evidence is limited to 50 targets, 32 text nodes/fragments per target
and six ancestors, with no text, form values or image URLs retained. A separate
rendered-avatar check enforces opaque initial/background contrast of at least
[4.5:1](https://www.w3.org/WAI/WCAG22/Understanding/contrast-minimum.html).
Violations or functional failures exit nonzero. `incomplete`
findings remain in the report for manual review; a passing run is not full WCAG
conformance or screen-reader/mobile-browser coverage. CI preserves its separate
`accessibility-e2e` artifact for seven days. Use a unique `E2E_ARTIFACTS` directory
to keep local runs separate.

## Repeated-session lifecycle check

After building the server/UI and installing Chromium, run the optional gate with
fresh owned services:

```sh
LIFECYCLE_E2E=1 build/with-test-postgres.sh build/with-test-server.sh \
  node web/e2e/lifecycle.cjs
```

Two synthetic-media clients reuse the same pages across join, publish, stop and
leave cycles. Defaults are six cycles with 30 seconds of measured media per
cycle. `LIFECYCLE_CYCLES` accepts 3–10; `LIFECYCLE_MEDIA_SECONDS` accepts 30–120.
This takes several minutes and is not part of `npm test` or default CI.

The check covers signaling resume and actual expiry of the server's unchanged
30-second grace period. It holds one replacement WebSocket handshake, observes
membership expiry, then permits the real server response and fresh rejoin.
It does not simulate a UDP outage, change OS networking or forge protocol replies.
Rejoining must release old media without automatically recapturing devices.

`lifecycle-results.json` retains bounded native media counters, resource snapshots,
recovery evidence and server counts. After each leave, media resources and room
memberships must be released; the join screen intentionally retains one signaling
socket per page. Server connections must reach zero after browser closure.
Browser cleanup must observe an unforced, zero-status process exit. Graceful
closure has a 10-second limit, forced cleanup a further 5 seconds, and finalization
a 25-second watchdog; any escalation fails the run.
The helper supplies its own temporary metrics credential; reports do not retain it.
Use a fresh `E2E_ARTIFACTS` directory and keep reports private.

JS heap, DOM counters and server RSS are informational trends, not hard leak or
capacity budgets. Sampled decode/playback progress does not prove uninterrupted
media, and zero membership gauges do not audit every native allocation. Real
browser/device checks in the [manual checklist](../../docs/testing.md#manual-release-checklist)
remain necessary.

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

First-frame timing requires positive native decoded-frame counters and remote
video dimensions; it includes automation/polling delay. The five-second counter
delta confirms decode progress, not uninterrupted playback. Use fresh artifacts:
existing reports or pending writes are rejected. A nonpassing report is retained
before browser cleanup; only completed cleanup and report writes permit success.

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
