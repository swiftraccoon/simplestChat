# Testing

Run commands from the repository root after [development setup](development.md).
Use disposable local services, never a shared or production database.

## Fast checks

```sh
cargo fmt --all -- --check
cargo test --locked --all-features -- --test-threads=1
npm --prefix web test
npm --prefix web run build
node --test build/tests/*.test.mjs load_tests/benchmark-local.test.mjs
```

Rust tests include native media checks; database tests are opt-in. Serial
execution reduces UDP port-allocation races. Web tests use mocked browser APIs;
the web build runs TypeScript checking and produces the production UI.

## Disposable PostgreSQL and browser tests

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

[CI](../.github/workflows/ci.yml) also runs formatting, locked Rust builds/tests,
web tests/build, pinned Chromium integration tests, dependency audits, native
dependency guards and production-image non-root/loader checks.
The [browser compatibility workflow](../web/e2e/README.md#ci) adds weekly and
manual Firefox/Linux and WebKit/macOS runs.

## Media correctness and performance

See [performance](performance.md) for the optional RTP generator and comparison
workflow, or [browser/API measurements](../web/e2e/README.md#informational-browserapi-performance)
for startup, chat delivery and decoded media. The short CI RTP smoke checks
delivery correctness; it does not set a performance budget.

## Manual release checklist

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

Headless tests use fake devices and resized desktop viewports. Complete the
manual checklist on real browsers and devices before a release.
