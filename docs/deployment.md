# Deployment and security

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

Complete the [database migration step](#database-migrations-and-tls) first. Then
start it with both interpolation and service injection enabled:

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
worker count. The supplied Compose mapping starts at port 40000;
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
from PyPI are version-constrained in [`build/pip-constraints.txt`](../build/pip-constraints.txt); review those
pins in the same dependency-update cycle. The media worker and WebAuthn code
are linked to the same static OpenSSL build produced by
[`build/install-openssl.sh`](../build/install-openssl.sh); its source version and SHA-256 are pinned in that
file and must be reviewed whenever OpenSSL publishes a 3.5 LTS security update.

Compose intentionally refuses to start without `ALLOWED_ORIGINS` and
`TRUSTED_PROXY_SECRET`. The application binds a non-loopback container address,
so it requires an exact browser origin allowlist; Docker NAT also makes the
proxy peer non-loopback, so forwarded client addresses are trusted only when
Caddy supplies the matching secret.

## Database migrations and TLS

Run migrations as a separate deployment step with a database role that has DDL
permissions, then start the server with a lower-privilege runtime role that has
only the required table permissions:

```bash
# /etc/simplestchat/migration.env is owner-readable and contains only:
# DATABASE_URL=postgres://migration_user:<password>@db.example.com/chat?sslmode=verify-full
cargo install sqlx-cli --version 0.9.0 --no-default-features --features postgres,rustls
set -a
. /etc/simplestchat/migration.env
set +a
sqlx migrate run
unset DATABASE_URL
```

SQLx 0.9 no longer ships a CLI lockfile, so the example pins the CLI version
without `--locked`. For reproducible deployment tooling, build/package that
version with your own reviewed lockfile. This does not affect the application's
checked-in `Cargo.lock` or its `--locked` builds.

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

## Reverse proxy and metrics

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

## TURN relay controls

Every joined client receives a TURN credential that is reusable until its TTL.
Keep that TTL low, configure coturn per-user/total allocation and bandwidth
quotas, and restrict relay peers—especially loopback, private, link-local, and
cloud-metadata networks unless explicitly required. Monitor allocation and
egress abuse. The application issues credentials but cannot enforce coturn's
relay destinations.

For explicitly authorized load tests, build the isolated client image (not the production image). This host-network example is intended for Linux; see [performance testing](performance.md) before selecting a workload:

```bash
docker build --pull --target loadtest -t simplestchat-loadtest .
load_results="$(mktemp -d "${TMPDIR:-/tmp}/simplestchat-load.XXXXXX")"
docker run --rm --network host \
  --user "$(id -u):$(id -g)" \
  --mount "type=bind,src=$load_results,dst=/results" \
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

## Limitations and operational caveats

- **Secret rooms are only unlisted** - The room ID remains a join capability
  that can be probed or collide; the setting is not authorization. For
  sensitive rooms, use a high-entropy ID and enforce password/registration
  requirements or add an invitation/ACL layer.
- **Maintained mediasoup patches** - Upstream `mediasoup-sys` 0.17.0 still
  bundles vulnerable OpenSSL 3.0.8. This repository patches the exact crates.io
  source to forbid that fallback and statically links checksum-pinned OpenSSL
  3.5.8 LTS, which fixes CVE-2026-54874. It also patches `mediasoup` 0.27.0 to
  use `lru` 0.18.4, removing RUSTSEC-2026-0253, and updates the embedded
  Abseil LTS archive to `20240722.2`, which fixes CVE-2025-0838. Provenance and
  the complete maintained-diff boundary are documented in [the native dependency notes](../vendor/README.md).
  Cargo cannot audit native code: keep the native-version CI checks, rebuild
  for every relevant security update, and remove these patches when fixed
  official crates ship.
- **Ordinary logout access-token revocation is expiry-bound** - Logout revokes the refresh
  session, but an already issued JWT remains valid until its 15-minute expiry.
  Refresh tokens are single-use; replay outside the short multi-tab race window
  revokes the current token family. Password changes and saved-key recovery are
  different: they increment the account auth version, invalidate existing tokens
  and revoke active account connections. Higher-risk deployments may additionally
  need session-backed revocation for ordinary logout.
- **Chat replay is bounded and in-memory** - Public/PM replay is limited to the
  current membership and retained room history (300 entries / 256 KiB per room),
  not a durable inbox or cross-room messaging service. A server restart loses it.
  Chat/device/layout preferences are browser-local; guest ignore entries are
  temporary. Private messages are not application-layer end-to-end encrypted.
- **SFU media is not end-to-end encrypted** - Browser DTLS/SRTP protects each
  hop, but the in-process mediasoup worker terminates those hops and is inside
  the media trust boundary. Claims that the operator cannot access media need a
  separately designed and audited application-layer E2EE scheme.
- **macOS release builds need `build-override`** (in `Cargo.toml`): the release profile's optimize+strip corrupts the sqlx-macros proc-macro dylib ("mis-aligned LINKEDIT string pool") — the override builds proc-macros unoptimized to avoid it. (The old `_LIBCPP_ENABLE_ASSERTIONS` mediasoup-sys failure is resolved on current Xcode CLT.)
- **Container io_uring warnings** - harmless, uses epoll fallback
- **File descriptor limits** - raise `ulimit -n 65536` for 300+ clients
- **Native test port reservation has a small race** - tests release their selected
  UDP socket immediately before the media worker binds it; an unrelated process
  can theoretically claim that port. Serial test execution remains recommended.

See [configuration](configuration.md) for all application environment variables and
[testing](testing.md) for the disposable container startup/migration smoke.
