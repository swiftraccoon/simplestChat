# Testing

Run commands from the repository root, after [development setup](development.md).
Keep tests on disposable local services; ignored operator scripts are not the
supported test workflow.

## Fast checks

```sh
cargo fmt --all -- --check
cargo test --locked --all-features -- --test-threads=1
npm --prefix web test
npm --prefix web run build
node --test build/tests/*.test.mjs load_tests/benchmark-local.test.mjs
```

The Rust command runs native media tests too, but skips three database tests.
Native tests use available UDP ports; serial execution reduces a small
reservation-to-bind race without requiring an existing server to stop.
Clippy is useful during review; existing warnings are not justification to
silence broad categories or apply unrelated automated rewrites.

Web source tests mock DOM/capture/signaling. The production typecheck and Vite
build are separate checks; neither establishes real browser media behavior.

## Disposable PostgreSQL and browser tests

Use a fresh, dedicated database whose name ends in `_test`. The tracked helper
rejects remote URLs and effective-host query overrides, and requires an explicit
disposable-database opt-in. It does not create or delete the database for you.

One native PostgreSQL example, after checking port 15434 is unused:

```sh
test_cluster="$(mktemp -d "${TMPDIR:-/tmp}/simplestchat-pg.XXXXXX")"
initdb -D "$test_cluster/db" -U test_owner --auth=trust --encoding=UTF8 --no-locale
pg_ctl -D "$test_cluster/db" -l "$test_cluster/postgres.log" \
  -o "-h 127.0.0.1 -p 15434 -k $test_cluster" start
createdb -h 127.0.0.1 -p 15434 -U test_owner simplestchat_test
export DATABASE_URL='postgres://test_owner@127.0.0.1:15434/simplestchat_test?sslmode=disable'
export DISPOSABLE_TEST_DATABASE=1
```

Trust authentication is only for this disposable loopback cluster, never a
shared/deployed database. Stop it when finished using the same validated path:

```sh
pg_ctl -D "$test_cluster/db" stop -m fast
```

Keep the directory/logs for diagnostics; remove it separately only after
confirming it is the temporary cluster you created. Do not stop the user's
existing PostgreSQL instance or execute broad cleanup SQL.

Build the debug server, then run the opt-in Rust tests:

```sh
cargo build --locked --bin simplestChat
build/with-test-server.sh cargo test --locked --all-features -- --include-ignored --test-threads=1
```

The helper starts its own loopback server, applies the real SQLx migrations,
waits for database-backed API readiness and exports `TEST_DATABASE_URL`. It
stops only its own server when the child command finishes or fails. This covers
recovery atomicity/revocation, refresh rotation and persisted social/moderation
behavior in addition to ordinary tests. Alternatively, use a previously
migrated disposable `TEST_DATABASE_URL` directly.

For the maintained real-browser suite:

```sh
npm --prefix web/e2e ci --ignore-scripts
npm --prefix web/e2e run install:browser
npm --prefix web run build
build/with-test-server.sh npm --prefix web/e2e test
```

Linux runners also need Playwright's browser system dependencies. The helper
defaults to HTTP3119/UDP41010 and one media worker, refuses occupied ports and
accepts `TEST_SERVER_PORT`, `TEST_MEDIA_PORT` and `TEST_SERVER_BINARY` overrides.
Do not rebuild assets while browsers are running.

See [web/e2e/README.md](../web/e2e/README.md) for the 17 community checks,
artifacts and exact installation options. Tests use real Chromium decoding with
fake media devices. Test accounts may remain: discard the dedicated database
rather than deleting rows from a shared system by prefix.

## Container and CI checks

[CI](../.github/workflows/ci.yml) runs formatting, locked Rust production/client
builds, PostgreSQL-backed tests, web unit tests/build, pinned Chromium community
checks, audits and native dependency guards. The production-image job checks
non-root execution, loader dependencies, assets/migrations and startup against
a disposable PostgreSQL container.

With Docker available:

```sh
docker build --target production -t simplestchat-ci:production .
build/test-container.sh
```

The smoke creates and removes only its own disposable containers, applies
migrations, checks database-backed API readiness and publishes HTTP only on a
random loopback port. It is not a deployment command. CI static/build checks are
not substitutes for actually starting the image; report if Docker was unavailable.

## Media correctness and performance

Build the optional generator and follow [performance](performance.md). Require
a completed, passing report, successful expected subscriptions, sustained
receiver packets and clean server state after disconnect. Exit 0 alone from an
old generator is not sufficient evidence.

A short CI media smoke is a correctness gate, not a capacity or latency budget.
Use the same validated generator, release servers and repeated alternating runs
for comparisons. Preserve raw artifacts and provenance; qualify co-located
laptop measurements. Browser startup/playback and authenticated database/chat
workloads must be measured separately from synthetic RTP.

## Manual release checklist

Check these flows manually with an owner and
a guest in separate browser contexts:

- Start/stop preview, cancel its dialog, then publish; ensure preview never reaches
  the other browser. Leave during a pending permission prompt and verify late
  capture is stopped. Try real device changes and screen sharing.
- Adjust viewer volume, mute and hide/restore; verify the publisher and another
  viewer are unaffected. Test fullscreen/PiP only where the browser supports it.
- Exchange public text and PMs, switch conversations with unfinished drafts,
  ignore/unignore, disable PMs, and reconnect after a short network interruption.
  Check unread counts and that no old account's conversation is shown after logout.
- Edit a profile/avatar and owned-room rules/image, inspect member roles and reports,
  and test a timed ban. Create a saved recovery key, redeem it once, then confirm
  reuse fails and the old signed-in browser is disconnected.
- Test mobile tabs and participant action menus, keyboard-only dialog operation,
  desktop panel resizing/collapse, and a room that requires push-to-talk.

Real Safari/Firefox behavior, hardware device hot-plugging, screen-share permission
UX, assistive technology, and mobile OS camera restrictions still require manual
device coverage; mocked media cannot establish those behaviors.


## Remaining validation priorities

- Real Safari/Firefox and mobile devices, permission-denial/hot-plug cases and
  keyboard/screen-reader review.
- A dedicated, explicitly authorized performance runner with stable CPU/network
  conditions; establish noise before adding latency/CPU regression budgets.
- Longer churn/soak and authenticated room/chat workloads at representative scale.
- Recovery and restore rehearsal, migration rollback/roll-forward procedures and
  secret rotation before a production rollout.

Do not describe these as complete merely because unit tests or a short local
benchmark pass.
