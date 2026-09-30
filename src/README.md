# Rust server

The server combines HTTP/authentication, WebSocket signaling, room state and
in-process native mediasoup workers. PostgreSQL is optional for guest media,
required for accounts and persistent community data.

## Architecture

```text
HTTP / WebSocket
       │
       ├── auth + room APIs ── PostgreSQL
       │
       └── signaling ── RoomManager ── MediaServer
                                         ├── worker pool
                                         ├── router per room
                                         └── participant transports / producers / consumers
```

| Area | Entry points |
| --- | --- |
| Startup and database pool | `main.rs`, `db.rs` |
| Admission drain and bounded shutdown | `shutdown.rs`, `main.rs` |
| Routes, origin/limits, WebSocket lifecycle | `signaling/mod.rs`, `signaling/connection.rs` |
| Dependency readiness | `signaling/readiness.rs`, `media/worker_manager.rs` |
| Wire contract | `signaling/protocol.rs` and [web/src/protocol.ts](../web/src/protocol.ts) |
| Password/passkey auth and JWT validation | `auth/routes.rs`, `auth/password.rs`, `auth/webauthn.rs`, `auth/jwt.rs`, `auth/limiter.rs` (sign-in failure delays, registration window) |
| Profiles, recovery and refresh sessions | `auth/account.rs`, `auth/session.rs` |
| Membership, lobby, chat and reconnect state | `room/mod.rs`, `room/social.rs` |
| Ordered persistence and uncertain-write handling | `room/control.rs` |
| Persistent room/community API | `room/api.rs`, `room/community.rs`, `room/settings.rs` |
| Roles, invitations and moderation retention | `room/roles.rs`, `room/invites.rs`, `room/moderation.rs` |
| Native worker/router/transport lifecycle | `media/worker_manager.rs`, `router_manager.rs`, `transport_manager.rs` |
| Media configuration and types | `media/config.rs`, `media/types.rs` |
| Prometheus metrics and TURN credentials | `metrics.rs`, `turn.rs` |
| Opt-in local operation traces | `diagnostics.rs`; [capture and interpretation](../docs/diagnostics.md) |

Worker count is configurable (detected CPUs capped at 64 by default), not fixed
at 16. Each worker has a dedicated WebRtcServer UDP port; each room has one
primary router that holds every producer and the audio observers. New routers
select an open worker with an open listener, preferring fewer consumers and then
fewer pending or registered routers. Selection reserves the router count
atomically; cancellation, failed creation, and room removal release it
synchronously. This spreads rooms even before their users start media. Once a
room's primary worker carries 64 consumers, new receive transports are placed by
the same selection, and a room lazily gains one viewer router per other worker,
fed by router-to-router pipes created once per producer and worker; pipes follow
the producer's close, viewer routers close with the room, and a placement drops
with the participant's media. Consumers are created paused and resume after the
browser creates its consumer.
See [configuration](../docs/configuration.md) for allocation limits.

Authorization is server-side. Roles, bans, password access, lobby state and
media/chat permissions cannot rely on what the UI allows. Persistent mutations
must remain consistent with in-memory state; failed or stale async work must not
apply to a replacement room or membership.
Room control operations serialize policy writes and membership changes separately
from chat/media state. The lock order is creation (when needed), control, then
the short-lived room state lock. Never wait for SQL or acquire control while
holding room state. An admitted persistent mutation owns its task through runtime
publication, even if its caller disconnects. See [failure behavior](../docs/configuration.md#room-persistence).
Recovery checks the affected worker identity again after worker recreation. A
retired room reserves its ID until its previous router and participant media
finish cleanup. A new join cannot reuse that ID while old cleanup might still
remove its router; failed router teardown retains the reservation for process
recovery. Snapshot requests capture an owned, typed projection while holding a
shared room read lock and serialize it after releasing that lock. Private-message
visibility and moderator-only lobby entries are filtered during capture.

Essential outbound queue overflow retires the affected WebSocket through
`signaling/outbound.rs`, independently of the full message queue. Registrations
hold weak sender references and leave with the connection handler. The reconnect
path supplies the snapshot needed after a missed event; ephemeral media hints
remain lossy. Normal successful fan-out does not consult the overflow registry.

Room invitation redemptions persist a receipt in the same transaction as the role
grant and usage decrement. Repeating an account/code pair cannot consume a second
use or restore a subsequently removed role. Migration 020 adds these receipts and
retention indexes; production must apply it and grant runtime access before
starting this server version. Membership listings opt into stable room-ID cursor
pages of 100 rows with `paginated=true`, so accounts with more memberships can
retrieve the remainder. Requests without paging parameters retain the legacy
array response for already-open clients during upgrades.

The retention job runs after one minute and every six hours thereafter. Each
statement selects at most 1,000 eligible parent rows, skips locked rows and
commits independently; a sweep processes at most 16 batches within 30 seconds.
Earlier committed batches survive later failure or cancellation. Addresses,
moderation history and resolved reports follow the configured day limits; live
sanctions and open reports remain. Expired invitations and their cascading
receipts are removed seven days after expiry. Each invitation may have up to 100
receipts, so cascading deletion adds work beyond the parent-row batch count.
Sweep changes are logged at info and failures at warning level; a large backlog
may require several sweeps to drain.

The [protocol guide](../docs/protocol.md) documents the browser/server contract,
including request correlation, replay limits and schema changes.
HTTP profiles and directory entries serialize snake_case fields, while room
settings and WebSocket messages use camelCase. Browser decoders rely on these
shapes, including required nullable profile/directory fields and no-content
statuses. Update the endpoint types, browser action/response contracts and
serialization tests together; the guide lists the HTTP method/result mappings.

## Build and run

From the repository root, with the static OpenSSL/Python environment configured
as in [development](../docs/development.md):

```sh
cargo build --locked --release --bin simplestChat
```

Run from the repository root so `web/dist` and development migrations resolve.
Production builds do not enable `load-test`; synthetic clients live separately
under [load_tests](../load_tests/README.md).

No `DATABASE_URL` means anonymous-only operation. Auth also requires a JWT secret
of at least 32 bytes. `RUN_MIGRATIONS` defaults false; production uses a separate
DDL-capable migration role. Registration and ad-hoc rooms are opt-in.
See [deployment](../docs/deployment.md) before opening a service to other users.

## Validation and dependency changes

```sh
build/check.sh --rust
cargo test --locked --all-features -- --test-threads=1
```

The quality check runs formatting, all-target/all-feature Clippy with warnings
rejected, and public/private Rustdoc with warnings denied. It selects the pinned
toolchain and static native environment for its own process; separate Cargo commands still
need the development shell setup. First-party compiler warnings and unfulfilled
lint expectations fail the build, and `unsafe` remains forbidden. See
[contribution standards](../CONTRIBUTING.md).

Generated API documentation is written to `target/doc` by the Rust quality gate.
Its link/HTML checks do not establish documentation completeness. Public lifecycle
methods should explain ownership, side effects, failures and retry/cancellation
semantics, especially when cloning native handles or persisting shared state.
Keep these contracts in Rustdoc near the implementation; use this guide for
architecture and [the protocol guide](../docs/protocol.md) for wire behavior.

Database tests are ignored by default. Use the disposable bootstrap and
`--include-ignored` procedure in [testing](../docs/testing.md); do not treat an
ordinary passing Cargo run as database coverage.

Native tests reserve available UDP ports; serial execution avoids most of the
small reservation-to-bind race and does not require stopping another server.
Keep the macOS release build-script/proc-macro override.

Native source patches and security checks are documented in
[vendor/README.md](../vendor/README.md). Cargo audit alone does not inspect bundled
C/C++ code. Preserve provenance, pinned OpenSSL and the native-version CI gates
when changing dependencies.
