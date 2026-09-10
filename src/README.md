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
| Routes, origin/limits, WebSocket lifecycle | `signaling/mod.rs`, `signaling/connection.rs` |
| Wire contract | `signaling/protocol.rs` and [web/src/protocol.ts](../web/src/protocol.ts) |
| Password/passkey auth and JWT validation | `auth/routes.rs`, `auth/password.rs`, `auth/webauthn.rs`, `auth/jwt.rs` |
| Profiles, recovery and refresh sessions | `auth/account.rs`, `auth/session.rs` |
| Membership, lobby, chat and reconnect state | `room/mod.rs`, `room/social.rs` |
| Persistent room/community API | `room/api.rs`, `room/community.rs`, `room/settings.rs` |
| Roles and moderation state | `room/roles.rs`, `room/moderation.rs` |
| Native worker/router/transport lifecycle | `media/worker_manager.rs`, `router_manager.rs`, `transport_manager.rs` |
| Media configuration and types | `media/config.rs`, `media/types.rs` |
| Prometheus metrics and TURN credentials | `metrics.rs`, `turn.rs` |

Worker count is configurable (detected CPUs capped at 64 by default), not fixed
at 16. Each worker has a dedicated WebRtcServer UDP port; each room has one
router. Consumers are created paused and resume after the browser creates its
consumer. See [configuration](../docs/configuration.md) for allocation limits.

Authorization is server-side. Roles, bans, password access, lobby state and
media/chat permissions cannot rely on what the UI allows. Persistent mutations
must remain consistent with in-memory state; failed or stale async work must not
apply to a replacement room or membership.

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
cargo fmt --all -- --check
cargo test --locked --all-features -- --test-threads=1
```

Three database tests are ignored by default. Use the disposable bootstrap and
`--include-ignored` procedure in [testing](../docs/testing.md); do not treat an
ordinary passing Cargo run as database coverage.

Native tests reserve available UDP ports; serial execution avoids most of the
small reservation-to-bind race and does not require stopping another server.
Keep the macOS release build-script/proc-macro override.

Native source patches and security checks are documented in
[vendor/README.md](../vendor/README.md). Cargo audit alone does not inspect bundled
C/C++ code. Preserve provenance, pinned OpenSSL and the native-version CI gates
when changing dependencies.
