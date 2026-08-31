# SimplestChat

A WebRTC SFU (Selective Forwarding Unit) server built with Rust and mediasoup, designed for real-time audio/video communication rooms.

## Stack

- **Rust 1.98** - Application logic, signaling, room management
- **mediasoup 0.27** - C++ media workers for RTP packet routing
- **Axum 0.8** - WebSocket signaling server
- **tokio** - Async runtime
- **PostgreSQL + sqlx 0.8** - Users, sessions, persisted rooms, roles (optional — runs anonymous-only without it)
- **webrtc-rs 0.17** - Load test client with real ICE/DTLS/RTP
- **TypeScript + Vite + mediasoup-client 3.21** - Browser client

## Features

- **Auth**: email/password (argon2), passkeys (WebAuthn), JWT access tokens + HttpOnly refresh cookie; guests work without any of it
- **Rooms**: persisted rooms with owner, topic, password (argon2, enforced on join with Admin+ bypass), and 15+ runtime-editable settings (moderated, lobby, guests, media toggles, caps)
- **Roles & moderation**: owner/admin/moderator/member/user/guest hierarchy; kick, ban, cam-ban, text-mute, set-role, voice requests with grant/dismiss. Bans persist (registered users by id, guests by IP) and survive reconnects and room teardown
- **Lobby**: moderated entry — guests wait for moderator approval (admit/deny)
- **Media**: camera, mic (open/push-to-talk), screen share, simulcast, server-side active speaker + audio level detection

## Architecture

```
Client (WebSocket) ──► Axum Signaling Server
                           │
                     ┌─────┴─────┐
                     │ RoomManager│
                     └─────┬─────┘
                           │
                     ┌─────┴──────┐
                     │ MediaServer │
                     └─────┬──────┘
                           │
              ┌────────────┼────────────┐
              │            │            │
         WorkerMgr    RouterMgr   TransportMgr
              │            │            │
         Configurable   1 per room   Send/Recv per
         C++ workers                 participant
```

### Key Components

| Component | File | Purpose |
|-----------|------|---------|
| Signaling | `src/signaling/connection.rs` | WebSocket message handling, event-driven BWE stats |
| Rooms | `src/room/mod.rs` | Room lifecycle, participant management |
| Media | `src/media/mod.rs` | MediaServer orchestrator |
| Workers | `src/media/worker_manager.rs` | Worker pool, WebRtcServer per worker, load-aware selection |
| Routers | `src/media/router_manager.rs` | Per-room media routing |
| Transports | `src/media/transport_manager.rs` | WebRTC transport, producers, consumers |

## Performance

Validated with progressive stress testing (AMD Ryzen 9 8945HS, 8C/16T, 90GB RAM):

| Clients | Rooms | P99 Latency | Errors | Consumers |
|---------|-------|-------------|--------|-----------|
| 100 | 16 | 2ms | 0 | 800 |
| 5000 | 16 | 17ms | 0 | 40K |
| 10000 | 16 | 223ms | 0 | 80K |

- **Comfortable limit**: 10000 clients/server (P99 < 250ms)
- ~1 MB per client; this recorded benchmark used 16 mediasoup workers
- WebRtcServer 5-tuple DEMUX eliminates per-transport file descriptors
- Requires `ulimit -n 65536` for large tests

## Quick Start

### Prerequisites

- Rust 1.98+ (stable)
- A C++ toolchain + `make`, `perl`, `curl`, `pkg-config`, `cmake`,
  `python3-pip`, `meson`, and `ninja` (mediasoup-sys builds C++ workers)
- Linux x86_64 for deployment: `libstdc++-static`, `glibc-static` (static linking)
- Static OpenSSL 3.5 LTS, version 3.5.8 or newer (but below 3.6.0); the
  checksum-pinned helper below avoids trusting an unversioned OS library
- macOS builds too (current Xcode CLT); nuck11/Linux is the canonical deploy host

### Build & Run

```bash
# One-time local dependency build. The prefix is under ignored target/ output.
build/install-openssl.sh "$PWD/target/openssl-3.5.8"
export OPENSSL_DIR="$PWD/target/openssl-3.5.8"
export PKG_CONFIG_PATH="$OPENSSL_DIR/lib/pkgconfig"
export OPENSSL_STATIC=1
export PIP_CONSTRAINT="$PWD/build/pip-constraints.txt"

# VS Code's checked-in Rust Analyzer settings use this same prefix. After the
# first install, reload the VS Code window so background Cargo checks inherit it.

# Build only the production server
cargo build --locked --release --bin simplestChat

# Build the optional load-test client separately
cargo build --locked --release --features load-test --bin load_test

# Start server — ANNOUNCE_IP must be a real IP reachable by clients.
# Do NOT use 127.0.0.1: Firefox rejects loopback ICE candidates → black video.
ANNOUNCE_IP=<your-lan-ip> ALLOW_AD_HOC_ROOMS=true \
  RUST_LOG=simplestChat=info,mediasoup=warn \
  ./target/release/simplestChat

# Run load test (2 clients, 30s)
./target/release/load_test \
  --server ws://localhost:3000/ws \
  --clients 2 --duration 30
```

### Using Scripts

The `scripts/` directory contains deployment and testing helpers. Copy `scripts/` examples and customize server IPs/paths for your environment.

```bash
# Build and run locally — use your LAN IP, not 127.0.0.1
export OPENSSL_DIR="$PWD/target/openssl-3.5.8"
export PKG_CONFIG_PATH="$OPENSSL_DIR/lib/pkgconfig"
export OPENSSL_STATIC=1
export PIP_CONSTRAINT="$PWD/build/pip-constraints.txt"
cargo build --locked --release --bin simplestChat
ANNOUNCE_IP=$(ipconfig getifaddr en0) ALLOW_AD_HOC_ROOMS=true \
  ./target/release/simplestChat   # macOS
# Linux: ANNOUNCE_IP=$(hostname -I | awk '{print $1}') ALLOW_AD_HOC_ROOMS=true ./target/release/simplestChat
```

> **Firefox + `ANNOUNCE_IP=127.0.0.1` = black video.** ICE candidates are the
> media path, and Firefox refuses loopback candidates by default, so media never
> connects (Chromium tolerates loopback, hiding the problem). Announce a real LAN
> IP even for same-machine testing — the browser still loads the page from
> `localhost:3000`; only the WebRTC media targets the announced IP.

## Load Testing

The load test binary (`load_tests/bin/load_test.rs`) creates real WebRTC clients using webrtc-rs that establish genuine ICE/DTLS/RTP connections with mediasoup. Each client:

1. Connects via WebSocket and joins a room
2. Creates send + receive WebRTC transports with real DTLS certificates
3. Produces audio + video tracks (synthetic RTP packets with valid VP8/Opus payloads)
4. Consumes media from all other participants
5. Collects per-client metrics (connection time, packets sent/received, errors)

### Quality Presets

Realistic bandwidth simulation with multi-packet RTP frame fragmentation:

```bash
--quality 480p|720p|1080p    # Video quality preset (default: 480p)
--fps 15|30|60               # Frames per second (default: 30)
```

| Preset | Video Bitrate | Packets/Frame | Audio |
|--------|--------------|---------------|-------|
| 480p | 1.0 Mbps | 4 | 128 kbps |
| 720p | 2.5 Mbps | 10 | 128 kbps |
| 1080p | 4.5 Mbps | 17 | 128 kbps |

### Bandwidth Results

| Clients | Quality | P99 | Send/Client | Aggregate Send |
|---------|---------|-----|-------------|----------------|
| 100 | 480p | 1ms | 1.07 Mbps | 0.11 Gbps |
| 100 | 1080p | 1ms | 4.38 Mbps | 0.44 Gbps |
| 500 | 1080p | 1ms | 4.20 Mbps | 2.10 Gbps |
| 1000 | 720p | 1ms | 2.14 Mbps | 2.14 Gbps |
| 1000 | 1080p | 2ms | 3.76 Mbps | 3.76 Gbps |

Output: `load_test_results.json` (per-client) and `load_test_summary.json` (aggregate).

See `load_tests/README.md` for full documentation.

## Project Structure

```
src/
├── main.rs                    # Server entry point
├── lib.rs                     # Library re-exports
├── db.rs                      # PostgreSQL pool + optional migrations (via DATABASE_URL)
├── metrics.rs                 # Prometheus metrics (AtomicU64, histogram)
├── turn.rs                    # TURN credential generation
├── auth/
│   ├── routes.rs              # /api/auth/* endpoints (register, login, refresh, passkeys)
│   ├── jwt.rs                 # JWT create/validate (jsonwebtoken 10, HS256)
│   ├── password.rs            # argon2 hashing
│   ├── webauthn.rs            # Passkey registration/login
│   └── session.rs             # Refresh-token sessions
├── signaling/
│   ├── mod.rs                 # HTTP routes, /health, /metrics, WS upgrade, serves web/dist
│   ├── connection.rs          # WebSocket message handler
│   └── protocol.rs            # Client/Server message types
├── media/
│   ├── mod.rs                 # MediaServer orchestrator
│   ├── config.rs              # MediaConfig, transport options
│   ├── types.rs               # MediaError, ParticipantMedia, stats types
│   ├── worker_manager.rs      # mediasoup worker pool
│   ├── router_manager.rs      # Per-room routers
│   └── transport_manager.rs   # Transports, producers, consumers
└── room/
    ├── mod.rs                 # Room management, lobby, broadcasts
    ├── api.rs                 # /api/rooms REST (list, create)
    ├── roles.rs               # Role hierarchy + permission checks
    ├── moderation.rs          # Punitive states (camban, mute, ban)
    └── settings.rs            # RoomSettings persistence (camelCase wire format)

migrations/                    # sqlx migrations (run separately in production)

load_tests/
├── bin/load_test.rs           # Load test binary
└── clients/
    ├── mod.rs                 # Module declarations
    ├── webrtc_client.rs       # Real WebRTC client (webrtc-rs)
    ├── media_generator.rs     # RTP packet generation
    └── metrics.rs             # Atomic metrics collection

web/                           # Browser client (Vite + TypeScript)
├── src/
│   ├── main.ts               # UI wiring (auth, room browser, moderation, settings)
│   ├── auth.ts               # AuthManager (JWT in memory, cookie refresh)
│   ├── signaling.ts          # WebSocket client (optional bearer subprotocol auth)
│   ├── media.ts              # WebRTC media handling (mediasoup-client)
│   ├── room.ts               # RoomClient (join/lobby/moderation events)
│   └── protocol.ts           # Message types (mirrors signaling/protocol.rs)
├── e2e/                      # Playwright E2E suite (22 checks, see e2e/README.md)
├── index.html
└── vite.config.ts

scripts/                       # Deployment, server management, testing
```

## Configuration

| Env Var | Default | Purpose |
|---------|---------|---------|
| `ANNOUNCE_IP` | `127.0.0.1` | Server's advertised IP for ICE candidates; set it to the client-reachable address in production |
| `BIND_ADDR` | `127.0.0.1` | HTTP/WebSocket bind address; Compose overrides this inside its isolated network namespace |
| `PORT` | `3000` | HTTP/WebSocket listen port for direct runs; the supplied Compose/Caddy path pins host and container port 3000 |
| `DATABASE_URL` | (none) | PostgreSQL URL; remote hosts must use `sslmode=verify-full`; without it the server is anonymous-only |
| `RUN_MIGRATIONS` | `false` | Apply migrations from `./migrations` at startup; intended only for local development |
| `JWT_SECRET` | (none) | HS256 secret of at least 32 bytes; **auth endpoints return 503 without it** |
| `WEBAUTHN_RP_ID` | (none) | Passkey relying-party ID (domain); passkeys disabled without it |
| `WEBAUTHN_ORIGIN` | (none) | Expected origin for passkey ceremonies (e.g. `https://chat.example.com`) |
| `ALLOWED_ORIGINS` | same host only | Exact WebSocket origin allowlist; required for non-loopback binds, such as Compose |
| `MAX_CONNECTIONS` | `10000` | Max concurrent WebSocket connections and retained reconnect-grace sessions |
| `MAX_CONNECTIONS_PER_IP` | `50` | Max concurrent WebSocket connections accepted from one client IP |
| `WS_HANDSHAKES_PER_MINUTE` | `120` | Per-client IP (IPv6 `/64`) rate limit for WebSocket upgrade attempts |
| `AUTH_REQUESTS_PER_MINUTE` | `60` | Per-IP rate limit for authentication endpoints |
| `AUTH_REQUESTS_PER_ACCOUNT_PER_MINUTE` | `20` | Per-account rate limit for authentication endpoints |
| `AUTH_MAX_CONCURRENCY` | `16` | Global cap on concurrent expensive authentication operations |
| `REGISTRATION_ENABLED` | `false` | Allow new password/passkey accounts; must be explicitly enabled on every bind address |
| `MAX_USERS` | `100000` | Global database-backed account cap enforced transactionally |
| `ROOM_API_REQUESTS_PER_MINUTE` | `120` | Per-IP rate limit for room-management HTTP endpoints |
| `ROOM_API_MAX_CONCURRENCY` | `32` | Global cap on concurrent room-management HTTP requests |
| `ROOM_CREATIONS_PER_ACCOUNT_PER_MINUTE` | `10` | Per-account persisted-room creation limit |
| `TRUSTED_PROXY_SECRET` | (none) | Shared proxy-authentication secret of at least 32 bytes; required for accurate client IPs through a container bridge |
| `MEDIA_WORKERS` | detected CPUs, capped at 64 | Worker count from 1 through 64; invalid or non-UTF-8 values fail startup |
| `MAX_ROOMS` | `1000` | Maximum rooms held by one server process |
| `MAX_PERSISTED_ROOMS` | `10000` | Global database-backed room cap enforced transactionally |
| `ALLOW_AD_HOC_ROOMS` | `false` | Permit joins to create ephemeral rooms; must be explicitly enabled on every bind address |
| `MAX_PASSWORD_WORKERS` | `2` | Global concurrent room-password verification cap (valid range 1–32); hashing uses a separate single-worker lane |
| `MAX_PRODUCERS_PER_PARTICIPANT` | `8` | Server-side media producer cap per participant |
| `MAX_CONSUMERS_PER_PARTICIPANT` | `16` | Server-side consumer cap per participant |
| `METRICS_TOKEN` | (none) | Bearer token of at least 32 bytes; `/metrics` returns 404 when unset |
| `RUST_LOG` | `simplestChat=info,mediasoup=warn` | Tracing filter |
| `TURN_URLS` | (none) | Comma-separated TURN server URLs |
| `TURN_SECRET` | (none) | TURN shared secret of at least 32 bytes; required when `TURN_URLS` is set |
| `TURN_TTL` | `600` | TURN credential TTL in seconds; constrained to 60–3600 |

Hardcoded in `src/media/config.rs`:

- **External WebRTC UDP ports**: worker N uses 40000+N, through 40063 at the maximum; Compose publishes that full range by default
- **Worker RTC allocation range**: 10000–59999
- **Workers**: `MEDIA_WORKERS` when set; otherwise detected CPUs, always constrained to 1–64

## Secure Deployment

Password registration does not verify email ownership. Treat the email as a
self-asserted login identifier only—not proof of identity, a trust signal, or a
recovery channel. Production identity assurance requires an ownership-verification
flow and outbound mail before making any email-based trust or recovery claim.
All deployment modes, including direct loopback development, default registration
closed. Set
`REGISTRATION_ENABLED=true` only during a controlled enrollment window (or
after adding verification/abuse controls), then turn it off again. Ad-hoc rooms
also default off, so arbitrary joins cannot churn new media
routers; authenticated users can create bounded persisted rooms through the
room API.

WebAuthn challenges and request-rate counters are process-local. Run a single
application replica unless you add shared challenge/rate-limit state (and design
the room/media state topology); multiplying replicas also multiplies each local
limit.

The supplied Compose configuration builds the `production` target, runs as an
unprivileged user with all Linux capabilities dropped, uses a read-only root
filesystem, and publishes the HTTP/WebSocket port only on host loopback. The
separate UDP mapping is the client media path and must remain reachable at the
address configured by `ANNOUNCE_IP`.

1. Replace `simplestchat.example.com` in `Caddyfile` and set
   `ALLOWED_ORIGINS=https://chat.example.com`. Expose TCP 80/443 and only the
   required 40000-and-up UDP worker ports in the host firewall; do not expose
   TCP 3000.
2. Store runtime secrets outside the repository in a root-readable file (mode
   `0600`). Generate independent values with `openssl rand -base64 48`; never
   reuse the JWT, metrics, TURN, or trusted-proxy secrets.
3. Supply that file through a private Compose override rather than committing
   secrets to this file or the repository.

For example, `/etc/simplestchat/runtime.env` can contain:

```dotenv
ANNOUNCE_IP=203.0.113.10
ALLOWED_ORIGINS=https://chat.example.com
REGISTRATION_ENABLED=false
ALLOW_AD_HOC_ROOMS=false
JWT_SECRET=<independent-random-value-at-least-32-bytes>
METRICS_TOKEN=<independent-random-value-at-least-32-bytes>
TRUSTED_PROXY_SECRET=<independent-random-value-at-least-32-bytes>
WEBAUTHN_RP_ID=chat.example.com
WEBAUTHN_ORIGIN=https://chat.example.com
DATABASE_URL=postgres://app_user:password@db.example.com/chat?sslmode=verify-full
TURN_URLS=turns:turn.example.com:5349
TURN_SECRET=<independent-random-value-at-least-32-bytes>
TURN_TTL=600
```

Keep a Compose overlay such as `/etc/simplestchat/compose.runtime.yml` outside
the checkout:

```yaml
services:
  simplestchat:
    env_file:
      - /etc/simplestchat/runtime.env
```

Start it with both interpolation and service injection enabled:

```bash
docker compose \
  --env-file /etc/simplestchat/runtime.env \
  -f docker-compose.yml \
  -f /etc/simplestchat/compose.runtime.yml \
  up --build -d
```

The Compose CPU/memory/PID limits are defensive defaults. Tune
`SIMPLESTCHAT_CPUS`, `SIMPLESTCHAT_MEMORY_LIMIT`, and the published UDP range
for the intended load, keeping the UDP range at least as large as the maximum
worker count. The range always starts at the application's fixed port 40000;
set `RTC_PORT_END` to its last published port.

The Dockerfile pins the official Node and Fedora multi-architecture manifests
by digest. Review those pins at least monthly and immediately after relevant
base-image advisories: inspect the same readable tags with
`docker buildx imagetools inspect`, update the verified index digests and
`FEDORA_REFRESH_EPOCH`, then rebuild all targets. The shared epoch invalidates
both Fedora package layers together; final-image loader checks reject a
builder/runtime ABI mismatch. Package versions are resolved from Fedora's
signed repositories when those layers refresh, so these images are not
bit-for-bit reproducible without an external repository snapshot. The Python
tools that `mediasoup-sys` installs
from PyPI are version-constrained in `build/pip-constraints.txt`; review those
pins in the same dependency-update cycle. The media worker and WebAuthn code
are linked to the same static OpenSSL build produced by
`build/install-openssl.sh`; its source version and SHA-256 are pinned in that
file and must be reviewed whenever OpenSSL publishes a 3.5 LTS security update.

Compose intentionally refuses to start without `ALLOWED_ORIGINS` and
`TRUSTED_PROXY_SECRET`. The application binds a non-loopback container address,
so it requires an exact browser origin allowlist; Docker NAT also makes the
proxy peer non-loopback, so forwarded client addresses are trusted only when
Caddy supplies the matching secret.

### Database migrations and TLS

Run migrations as a separate deployment step with a database role that has DDL
permissions, then start the server with a lower-privilege runtime role that has
only the required table permissions:

```bash
# /etc/simplestchat/migration.env is owner-readable and contains only:
# DATABASE_URL=postgres://migration_user:<password>@db.example.com/chat?sslmode=verify-full
cargo install sqlx-cli --locked --version 0.8.6 --no-default-features --features postgres
set -a
. /etc/simplestchat/migration.env
set +a
sqlx migrate run
unset DATABASE_URL
```

Migration 011 enables the trusted PostgreSQL `pg_trgm` extension. Ensure the
migration role is allowed to create that extension, or have a database
administrator install it before running the migration.

For a private CA, add `sslrootcert=/path/to/ca.pem` and mount that certificate
read-only into the container. The server rejects non-loopback database hosts
unless `sslmode=verify-full` is set, preventing SQLx's permissive default from
silently downgrading TLS or skipping hostname verification. Loopback and Unix
socket connections remain available for local development. `RUN_MIGRATIONS=true`
is an explicit local convenience; leave it false in production.

Runtime pool connections set PostgreSQL statement, lock, and idle-transaction
timeouts to 10, 5, and 15 seconds respectively so a stalled database cannot
hold room operations indefinitely. The separate `sqlx migrate run` step does
not inherit those application timeouts.

### Reverse proxy and metrics

Validate and run the supplied Caddy configuration after replacing its example
hostname. It adds TLS and browser security headers, proxies to the loopback-only
backend, and returns 404 for public `/metrics` requests. Prometheus should
scrape the backend locally instead:

```bash
caddy validate --config Caddyfile

curl --fail \
  -H "Authorization: Bearer ${METRICS_TOKEN}" \
  http://127.0.0.1:3000/metrics
```

Set the same `TRUSTED_PROXY_SECRET` in the application environment and Caddy's
service environment. The proxy overwrites `X-SimplestChat-Proxy` with this
secret before forwarding. This lets the application trust Caddy's appended
client IP across a Docker bridge without trusting a client-supplied
`X-Forwarded-For`. Keep the value out of the Caddyfile, process arguments, and
logs; for a system service, use a root-readable `EnvironmentFile` containing
only this proxy secret.

The application also returns 404 when `METRICS_TOKEN` is unset and requires a
token of at least 32 bytes when metrics are enabled. Restrict access to the
loopback scrape path and avoid putting bearer tokens in command history or
monitoring logs.

### TURN relay controls

Every joined client receives a TURN credential that is reusable until its TTL.
Keep that TTL low, configure coturn per-user/total allocation and bandwidth
quotas, and restrict relay peers—especially loopback, private, link-local, and
cloud-metadata networks unless explicitly required. Monitor allocation and
egress abuse. The application issues credentials but cannot enforce coturn's
relay destinations.

To build the isolated load-test image (not the production image):

```bash
docker build --pull --target loadtest -t simplestchat-loadtest .
docker run --rm --network host \
  -v "$PWD/load-test-results:/results" \
  simplestchat-loadtest --server ws://127.0.0.1:3000/ws --clients 2 --duration 30
```

## Server Hardening

- **Input validation**: Room ID (1-128 chars), participant name (1-64 chars), chat (1-4096 chars)
- **WebSocket size limit**: 64KB max message size prevents OOM attacks
- **Lock poisoning recovery**: All std::sync locks recover from panics instead of cascading
- **Error responses**: All operations return errors when not in a room (no silent drops)
- **Health endpoint**: `GET /health` returns only `{"status":"ok"}`
- **Graceful shutdown**: Ctrl+C drains all rooms, closes transports/FDs, removes routers
- **Panic-free startup**: Invalid `ANNOUNCE_IP` returns an error instead of panicking

## Efficiency & Protection

- **Bounded channels**: 64-message capacity per client prevents OOM from slow consumers
- **Connection cap**: Semaphore-based limit (default 10K), returns HTTP 503 when exhausted
- **Per-IP cap**: 50 simultaneous WebSocket connections by default
- **Authentication guard**: Per-IP request throttling plus a global expensive-work concurrency cap
- **Room API guard**: Per-IP request throttling plus a global concurrency cap; auth and room requests have a 15-second total timeout and a 5-second request-body idle timeout
- **Origin validation**: WebSocket upgrades require the same host unless `ALLOWED_ORIGINS` supplies exact alternatives
- **Rate limiting**: Token-bucket per connection (100 msg/s burst), prevents message flooding
- **Idle timeout**: 5-minute WS idle timeout prevents Slowloris-style resource exhaustion
- **Reconnect tokens**: Session hijacking prevention via per-session UUIDv4 tokens

## Observability

- **`GET /metrics`**: Prometheus text exposition format when `METRICS_TOKEN` is configured; otherwise 404
  - Counters: connections, messages sent/received, errors, rooms created, joins, leaves, producers, consumers
  - Gauges: active connections, active rooms, active participants
  - Histogram: message handling latency with 10 buckets (1ms to 5s)
- **Bearer auth**: A 32-byte-or-longer `METRICS_TOKEN` and `Authorization: Bearer <token>` are required
- **Zero dependencies**: Uses `std::sync::atomic::AtomicU64` — no prometheus/opentelemetry crates

## End-to-End Testing

`web/e2e/checklist.cjs` drives two Chromium instances (registered owner + guest) with fake
media devices through 27 checks: auth, room browser, room creation, password enforcement,
lobby admit/deny, all moderation actions, persistent bans, role hierarchy, live settings
enforcement, active-speaker highlighting, and real bidirectional WebRTC video (asserted
via `videoWidth > 0`). See `web/e2e/README.md`.

## Known Issues

- **Secret rooms are only unlisted** - The room ID remains a join capability
  that can be probed or collide; the setting is not authorization. For
  sensitive rooms, use a high-entropy ID and enforce password/registration
  requirements or add an invitation/ACL layer.
- **Maintained mediasoup patches** - Upstream `mediasoup-sys` 0.17.0 still
  bundles vulnerable OpenSSL 3.0.8. This repository patches the exact crates.io
  source to forbid that fallback and statically links checksum-pinned OpenSSL
  3.5.8 LTS, which fixes CVE-2026-54874. It also patches `mediasoup` 0.27.0 to
  use `lru` 0.18.3, removing RUSTSEC-2026-0253, and updates the embedded
  Abseil LTS archive to `20240722.2`, which fixes CVE-2025-0838. Provenance and
  the complete maintained-diff boundary are documented in `vendor/README.md`.
  Cargo cannot audit native code: keep the native-version CI checks, rebuild
  for every relevant security update, and remove these patches when fixed
  official crates ship.
- **Access-token revocation is expiry-bound** - Logout revokes the refresh
  session, but an already issued JWT remains valid until its 15-minute expiry.
  Refresh tokens are single-use; replay outside the short multi-tab race window
  revokes the current token family. Higher-risk deployments should additionally
  use session-backed access-token revocation.
- **SFU media is not end-to-end encrypted** - Browser DTLS/SRTP protects each
  hop, but the in-process mediasoup worker terminates those hops and is inside
  the media trust boundary. Claims that the operator cannot access media need a
  separately designed and audited application-layer E2EE scheme.
- **macOS release builds need `build-override`** (in `Cargo.toml`): the release profile's optimize+strip corrupts the sqlx-macros proc-macro dylib ("mis-aligned LINKEDIT string pool") — the override builds proc-macros unoptimized to avoid it. (The old `_LIBCPP_ENABLE_ASSERTIONS` mediasoup-sys failure is resolved on current Xcode CLT.)
- **Container io_uring warnings** - harmless, uses epoll fallback
- **File descriptor limits** - raise `ulimit -n 65536` for 300+ clients
- **`cargo test` needs `-- --test-threads=1`** - media tests bind fixed UDP ports (40000+) and collide in parallel (and with a running server)
