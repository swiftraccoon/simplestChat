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
| `WEBRTC_SERVER_TCP` | `false` | Also listen for ICE-TCP on each worker's port and offer TCP candidates (UDP stays preferred). Clients on networks that block UDP can then still connect; the deployment must publish the same port range over TCP and open it at the firewall first, or clients are offered candidates that cannot connect. TURN over TCP/TLS offers another fallback when clients cannot reach these media ports; a symmetric NAT alone does not require a relay to a reachable public ICE-Lite server |
| `MAX_ROOMS` | `1000` | Maximum rooms held by one server process |
| `MAX_PERSISTED_ROOMS` | `10000` | Global database-backed room cap enforced transactionally |
| `ALLOW_AD_HOC_ROOMS` | `false` | Permit joins to create ephemeral rooms; must be explicitly enabled on every bind address |
| `MAX_PASSWORD_WORKERS` | see note | Concurrent Argon2 verification cap (valid range 1–32) for both the room-password lane (default `2`) and the account-password lane (default `min(AUTH_MAX_CONCURRENCY, CPUs ÷ 2)`, at least 1, where CPUs honours a container quota); room-password hashing uses a separate single-worker lane |
| `MAX_PRODUCERS_PER_PARTICIPANT` | `8` | Server-side media producer cap per participant |
| `MAX_CONSUMERS_PER_PARTICIPANT` | `64` | Server-side consumer cap per participant. Each peer with camera and microphone costs two consumers (three with screen video, four with screen audio too), so the default covers 32 camera/microphone publishers. Consumer and frame burst budgets cover creation, resumption and a layer request for every allowed consumer; sustained limits and the room-wide IPC limit still apply. |
| `METRICS_TOKEN` | (none) | Bearer token of at least 32 bytes; `/metrics` returns 404 when unset |
| `RUST_LOG` | `simplestChat=info,mediasoup=warn` | Tracing filter |
| `DIAGNOSTICS_PATH` | (none) | Opt-in, new absolute path for private local operation records; see [limits and report definitions](diagnostics.md) |
| `MEDIA_DIAGNOSTICS_ENABLED` | `false` | Enable authenticated, on-demand native media snapshots; requires `METRICS_TOKEN`. See [collection bounds and interpretation](diagnostics.md#server-forwarding-snapshots) |
| `TURN_URLS` | (none) | Comma-separated TURN server URLs |
| `TURN_SECRET` | (none) | TURN shared secret of at least 32 bytes; required when `TURN_URLS` is set |
| `LIBWEBRTC_FIELD_TRIALS` | (mediasoup default) | libwebrtc field trials for the media workers' congestion controller as `Name/Value/` pairs, replacing mediasoup's default `WebRTC-Bwe-AlrLimitedBackoff/Enabled/`; the weekly impaired-network job is the way to evaluate a candidate before setting it |
| `WEBRTC_MIN_OUTGOING_BITRATE` | `100000` | Floor in bit/s for each viewer's send-side bandwidth estimate: `0` or at least `30000`, up to the outgoing cap (3000000 by default). `0` keeps mediasoup's own 30 kbit/s floor; the default keeps the lowest simulcast layer flowing at a 150 kbit/s cap where mediasoup's floor loses frames (see the performance results of 2026-09-21). Invalid policies fail at startup. |
| `WEBRTC_MAX_INCOMING_BITRATE` | `3000000` | REMB ceiling in bit/s sent to each publisher (0 = none, at most 50000000). Must cover the largest simulcast ladder the client publishes (1080p: about 2.9 Mbit/s) or the browser starves the top layer |
| `CPU_SATURATION_DISABLED` | `false` | Disable the CPU saturation monitor. When enabled it reads the process cgroup's `cpu.stat` throttling counters and `cpu.pressure`, and each media worker thread's scheduler time, every 2 s; while the process is saturated, `/ready` returns 503 and fresh room joins are refused with a retry message (existing calls, reconnects and lobby admissions continue) |
| `CPU_SATURATION_THROTTLED_FRACTION` | `0.5` | Share of cgroup enforcement periods throttled over the last 10 s that counts as saturated (0.05–1); saturation clears once the share falls below half of this |
| `CPU_SATURATION_PRESSURE_AVG10` | `50` | cgroup CPU pressure `some avg10` percentage that counts as saturated (5–100); clears below half of this |
| `CPU_SATURATION_WORKER_UTILIZATION` | `0.85` | Share of one core a media worker thread may use over the last 10 s before it counts as saturated (0.2–1); clears below 80 % of this. A room's router lives on one worker thread, so new rooms avoid a saturated worker, rooms already on it refuse fresh joins, and `/ready` fails only when every worker is saturated. Once a room's worker carries 64 or more consumers, new receive transports are placed on the least loaded worker and the room's producers are piped to a viewer router there, so a large one-to-many room spreads its viewers across workers while its producers stay on the primary worker Linux only (reads `/proc/self/task/<tid>/schedstat`); elsewhere the per-worker gauges are absent |
| `QUALITY_SAMPLE_INTERVAL_SECS` | `15` | Seconds between server-side media quality samples exported on `/metrics` (5–300) |
| `QUALITY_SAMPLE_MAX_TRANSPORT_STATS` | `100` | Receive transports asked for statistics per sample, round-robin across samples (0–10000); consumer and producer scores cost no worker requests |
| `TURN_TTL` | `86400` | TURN credential lifetime in seconds (60–86400). Relay allocations are refreshed with the credential they were created with and coturn rejects an expired one, so this bounds how long a relayed call can last; ICE restarts mint fresh credentials |

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
It does not test external ICE reachability, TURN, or existing room health.
A media worker that dies is recreated automatically, and every room whose
router lived on it is closed with the temporary `serverRestarting` notice so
its members rejoin onto live capacity; their calls are interrupted, not
retained. `simplestchat_media_worker_deaths_total` counts these events. If
recreation itself fails, capacity stays reduced until restart and the
`simplestchat_media_workers_live` gauge shows it.

## Authentication and database availability

The application pool has 20 connections and a three-second acquisition timeout.
Runtime PostgreSQL statement, lock and idle-transaction limits are 10, 5 and 15
seconds respectively; they are not an end-to-end request deadline.

Each active authenticated socket checks its account credential version every
five seconds after the previous check completes. Checks are not shared between
sockets for the same account. With fast queries, 10,000 active authenticated
sockets would therefore approach 2,000 periodic queries per second, before
handshakes, renewals, disconnect/grace checks and other application SQL. The
connection limit includes guests and grace sessions; it is not a database capacity
guarantee. Periodic checks use the pool but not the handshake/renewal semaphore.

An established session tolerates unavailable validation for one 15-second window
from the first observed failure, never beyond its accepted token expiry. The
deadline follows it into disconnect grace and repeated failures cannot extend it.
Authoritative revocation remains immediate; lost revocation notifications cannot
be excused by the allowance. Missing database configuration and initial
authentication still fail closed. Renewals can request bounded same-socket retries
without accepting a new token; see [authentication continuity](protocol.md#connection-and-authentication).
Database failure still makes `/ready` unavailable. This policy does not make
database-backed account or room operations available during an outage.

## Room persistence

Room settings, moderation, reports and identity writes release the chat/media
state lock during SQL. A separate per-room control gate preserves permissions,
membership and write order. Chat and routine media control remain available
under the current policy while a write is pending; joins, leaves, reconnect
rebinding and other control operations wait. Persisted writes are published to
runtime state only after database success.

Control admission waits at most five seconds; identity edits separately allow
five seconds for creation admission. The complete write phase has a 15-second
deadline, including pool acquisition and transaction commit. Caller cancellation
after admission does not cancel mandatory runtime publication. These are phase
limits, not an end-to-end request deadline: authorization reads, state publication
and native media cleanup are separate.

A confirmed rejected write leaves the existing policy unchanged. A timeout,
uncertain commit result, or missing backing row makes that room unavailable:
memberships close, media cleanup is attempted with bounded waits, and the room ID
stays reserved until restart reloads durable state. The database row is not deleted
or automatically retried. Inspect the room-persistence and cleanup logs before
restarting; incomplete media cleanup is explicitly reported.

## Shutdown

`SIGTERM` and Ctrl-C start a one-way drain. Readiness becomes unavailable,
new WebSocket upgrades and room creation/join/reconnect admissions are refused,
and HTTP stops accepting connections. Existing sockets receive a best-effort
`serverRestarting` message (`Server shutting down`) and close code 1001. Reconnect
grace is cancelled; live and lobby memberships are cleared without deleting
persisted rooms or accounts.

The browser treats this as temporary maintenance, retaining established room or
lobby intent for a bounded automatic rejoin. Local media stops and must be
explicitly enabled after rejoining. Permanent `roomClosed` still cancels recovery.
See [connection recovery](protocol.md#reconnection-and-ownership) for retry and draft limits.

HTTP requests already in progress may finish, including database writes already
started. HTTP/WebSocket, outstanding password jobs, and room cleanup share an
eight-second window, followed by two seconds each for transports, routers,
workers, and the database pool:
16 seconds of asynchronous cleanup budgets, up to 200 ms for an enabled local
diagnostic writer to close, then at most one second waiting for runtime teardown.
Recorder incompleteness is reported separately and does not change the server's
exit outcome. Later cleanup stages run even after a timeout; incomplete stages
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
