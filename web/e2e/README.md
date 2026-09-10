# Browser integration tests

The pinned Playwright/Chromium suite runs against a **disposable local database
and server**. It creates unique test accounts and rooms. Never point it at a
production database, even through a local tunnel.

## Install and run

Configure the native build environment using the
[development guide](../../docs/development.md), and start a disposable PostgreSQL
database using the [testing guide](../../docs/testing.md#disposable-postgresql-and-browser-tests).
Its name must end in `_test`. Then, from the repository root in the same shell:

```bash
cargo build --locked --bin simplestChat
npm --prefix web ci --ignore-scripts
npm --prefix web run build
npm --prefix web/e2e ci --ignore-scripts
npm --prefix web/e2e run install:browser
export DISPOSABLE_TEST_DATABASE=1
export DATABASE_URL='postgres://test_owner@127.0.0.1:15434/simplestchat_test?sslmode=disable'
build/with-test-server.sh npm --prefix web/e2e test
```

On Linux, install browser system dependencies with
`npm --prefix web/e2e exec -- playwright install --with-deps chromium`.
`npm ci` installs the exact test-tools lockfile; Playwright is not part of the
application bundle or production image.

The helper starts only its own server, runs SQLx migrations, waits for the
database-backed room API, and stops that server on success or failure. It
requires loopback PostgreSQL, an explicit disposable-database opt-in, and a
database name ending `_test`. It neither creates nor deletes your database:
dispose of that test database/cluster yourself after the run. Existing listeners
are not reused or stopped. Database connection-query overrides are rejected.

Defaults are HTTP `3119`, UDP `41010`, one media worker, registration enabled,
and `target/debug/simplestChat`. Override these with `TEST_SERVER_PORT`,
`TEST_MEDIA_PORT`, and `TEST_SERVER_BINARY` (prefer an absolute path). The helper
exports `BASE_URL`, `COMMUNITY_E2E=1`, and `TEST_DATABASE_URL` to its child command.
Running `build/with-test-server.sh true` just migrates/checks startup and shuts down.

Do not rebuild `web/dist` while browsers are running: changed asset hashes can
invalidate in-flight requests. The suite uses fake media devices and Chromium's
loopback peer-connection flag. This verifies actual browser decoding and media
controls; synthetic RTP load tests alone cannot do that.

## Coverage and diagnostics

`community.cjs` has 17 sequential checks covering registration, owned rooms,
private conversations and isolation, ignore/opt-out, nickname, mentions/emoji,
reports, role changes, bans/unban, local preview, received camera/audio,
viewer-local media controls, profiles/images, recovery-key/password flows,
mobile layout, and room deletion.

The helper prints the temporary artifact directory containing `server.log` and
screenshots. Set `E2E_ARTIFACTS` to choose another directory. CI retains these
diagnostics for seven days. Accounts can remain after a successful run, and a
failed run can leave its test room: discard the disposable database rather than
running broad cleanup queries against a shared database.

For an already-running disposable loopback server only:

```bash
COMMUNITY_E2E=1 BASE_URL=http://127.0.0.1:3119 npm --prefix web/e2e test
```

`PLAYWRIGHT_MODULE` and `PLAYWRIGHT_BROWSERS_PATH` remain available for an
explicitly isolated tooling/browser installation.

## Informational browser/API performance

With the same exported disposable database settings and cluster still running:

```bash
cargo build --locked --release --bin simplestChat
PERFORMANCE_E2E=1 RUN_LABEL=local-sample \
  TEST_SERVER_BINARY="$PWD/target/release/simplestChat" \
  build/with-test-server.sh node web/e2e/performance.cjs
```

`performance.cjs` records new-context navigation/paint timing, Chromium
main-thread task duration and JS heap, hashed bundle bodies/resource transfers,
two registrations, room creation/join, ten paced authenticated chat deliveries,
and five seconds of actual fake-camera decoding. It asserts decoded-frame progress
across that interval, no page errors, and closed peer connections/media elements after leave.
The report is `browser-performance.json` in the printed artifact directory;
failure reports remain marked incomplete/failed and the command exits nonzero.

For comparisons, use release builds, the same pinned browser/harness, separate
fresh databases for every run, and repeated alternating revision order. Supply
`SERVER_REVISION`, `FRONTEND_REVISION`, `BUILD_TOOLCHAIN`, and `RUN_LABEL` metadata.
`TEST_SERVER_WORKDIR` selects the server's checkout (validated to contain
Cargo.toml and migrations), while `TEST_SERVER_BINARY` selects its absolute
executable path. This makes each server serve its own built frontend assets.
The helper's test command still runs from the current repository root.

The browser starts fresh and contexts do not share an HTTP cache, but OS file
caches are not flushed. Chat delay includes browser-automation overhead. CDP
task/heap samples are not whole-browser CPU/RSS, and direct loopback HTTP omits
production TLS/proxy/compression/network effects. Run without competing builds
or load tests. These tiny local samples are informational, not CI performance
budgets, production-capacity claims, or cross-browser/device validation.

## Other checks

- Fast source-level regressions: `npm --prefix web test`.
- Native and database tests: see [testing](../../docs/testing.md).
- Production image startup/migration/API test: after building an image named
  `simplestchat-ci:production`, run `build/test-container.sh`. It creates only
  disposable containers, publishes HTTP on a random loopback port, and removes
  those containers and their temporary database data afterward.
- `checklist.cjs` is the older 27-check room/lobby/moderation checklist. It is
  **not a current CI gate** and its selectors may need updating. It defaults to
  `localhost:3100`; inspect it before manual use, and use only a disposable local
  server/database. The 17-check community suite is the maintained browser gate.
