# Runtime configuration

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
| `WEBRTC_SERVER_PORT_BASE` | `40000` | First dedicated WebRTC UDP port; worker N uses base+N. Range must fit within 1–65535; use a separate base for isolated local instances |
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

Media allocation in `src/media/config.rs`:

- **External WebRTC UDP ports**: worker N uses `WEBRTC_SERVER_PORT_BASE`+N, defaulting to 40000–40063 at the maximum worker count; Compose publishes the default range, so update its published ports/firewall if changing the base there
- **Worker RTC allocation range**: 10000–59999
- **Workers**: `MEDIA_WORKERS` when set; otherwise detected CPUs, always constrained to 1–64


## Health and readiness

`GET /health` returns `200 {"status":"ok"}` for process liveness only.
`GET /ready` returns `200 {"status":"ready"}` when at least one media worker
and its WebRTC listener are open, and a configured database answers `SELECT 1`.
Without `DATABASE_URL`, readiness checks only media capacity. Failed checks,
probe saturation, or explicit drain state return `503 {"status":"not_ready"}`.
Responses are uncached and contain no backend errors.

Each readiness probe has a one-second total deadline; at most four run at once.
It does not test external ICE reachability, TURN, or existing room health, and
does not automatically replace failed workers. Restart the server to restore
lost worker capacity.

## Shutdown

`SIGTERM` and Ctrl-C start a one-way drain. Readiness becomes unavailable,
new WebSocket upgrades and room creation/join/reconnect admissions are refused,
and HTTP stops accepting connections. Existing sockets receive a best-effort
`roomClosed` message (`Server shutting down`) and close code 1001. Reconnect
grace is cancelled; live and lobby memberships are cleared without deleting
persisted rooms or accounts.

HTTP requests already in progress may finish, including database writes already
started. HTTP/WebSocket, outstanding password jobs, and room cleanup share an
eight-second window, followed by two seconds each for transports, routers,
workers, and the database pool:
16 seconds of asynchronous cleanup budgets, then at most one second waiting
for runtime teardown. Later stages run even after a timeout; incomplete stages
are logged and produce a nonzero exit. A socket that is already closed or does
not read cannot be guaranteed delivery of its shutdown notice.

The deadlines bound asynchronous waits, not a stalled native thread or OS call.
Keep a supervisor hard-stop deadline (Compose uses 30 seconds). Timed-out HTTP
tasks and unfinished blocking jobs may be abandoned when the runtime/process
exits; a shutdown does not guarantee every in-flight write or message completed.

## Additional fixed admission limits

Room joins also have code-level limits in `src/room/mod.rs`: 30 attempts per
client IP per 60 seconds and 10 attempts per room/IP per 60 seconds. These apply
to authenticated and guest joins. They are separate from WebSocket connection
and upgrade limits; raising those environment settings does not raise room-join
limits. Reconnect attempts and multiple people behind one address share these
budgets. They currently have no environment override.

Plan local load ramps and room distributions around these protections; a rejected
join is an admission-policy outcome, not a media throughput measurement. Do not
disable production safeguards to make a benchmark pass.

The table describes application defaults, not a capacity guarantee. The supplied
Compose configuration adds resource limits and binds inside its isolated network;
review [deployment](deployment.md) before changing exposure, proxies or TLS.
For local examples see [development](development.md).
