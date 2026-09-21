# Deployment and security

Deploy one application replica behind an HTTPS reverse proxy, with PostgreSQL
and optional TURN. WebAuthn challenges, rate limits and room/media state are
process-local; multiple replicas require a shared-state and routing design.

## Production setup

For repeatable VPS preparation and private container benchmarks, see the
[operations automation](../ops/ansible/README.md). It prepares the host without
starting a public chat service. The application remains packaged by Docker;
Ansible does not install a second native Rust/Node build on the host.
The separate [public VPS playbook](../ops/ansible/PUBLIC.md) adds pinned Caddy,
socket-only PostgreSQL, protected credentials and an initial guest lobby.
For routine updates to that prepared host, use the
[single-command release](../ops/ansible/RELEASES.md#routine-update-use-the-image-ci-tested).
It deploys the exact image retained by successful CI without rebuilding, replacing
only the application while keeping the database and proxy running.

1. Point your domain at the host and set Caddy's `CADDY_DOMAIN` environment
   variable (or edit its example hostname). Set `ALLOWED_ORIGINS` to the exact
   external HTTPS origin. `CADDY_UPSTREAM` defaults to `127.0.0.1:3000`; use
   `simplestchat:3000` for a proxy on the application's Compose network.
2. Expose TCP 80/443 and the media worker's UDP ports starting at 40000. Keep
   TCP 3000 private. Set `ANNOUNCE_IP` to the address clients can reach.
3. Keep runtime secrets outside the repository in an owner-readable file
   (mode `0600`). Generate each with `openssl rand -base64 48`; use independent
   JWT, metrics, proxy and TURN secrets.

Example `/etc/simplestchat/runtime.env`:

```dotenv
ANNOUNCE_IP=203.0.113.10
ALLOWED_ORIGINS=https://chat.example.com
REGISTRATION_ENABLED=false
ALLOW_AD_HOC_ROOMS=false
JWT_SECRET=<independent-random-value-at-least-32-bytes>
METRICS_TOKEN=<independent-random-value-at-least-32-bytes>
TRUSTED_PROXY_SECRET=<independent-random-value-at-least-32-bytes>
DATABASE_URL=postgres://app_user:password@db.example.com/chat?sslmode=verify-full
```

Registration and ad-hoc rooms default off. Open registration only for controlled
enrollment or after adding verification and abuse controls; email ownership is
not verified. Authenticated users can create persisted rooms without enabling
ad-hoc room creation. Add passkey and TURN settings from
[configuration](configuration.md) if needed.

Keep an overlay such as `/etc/simplestchat/compose.runtime.yml` outside the checkout:

```yaml
services:
  simplestchat:
    env_file:
      - /etc/simplestchat/runtime.env
```

Complete [database migrations](#database-migrations-and-tls), then start the
service. Both `--env-file` interpolation and the service's `env_file` injection
are needed:

```sh
docker compose \
  --env-file /etc/simplestchat/runtime.env \
  -f docker-compose.yml \
  -f /etc/simplestchat/compose.runtime.yml \
  up --build -d
```

For deployment of an already-tested image, set `SIMPLESTCHAT_IMAGE` in the private
runtime environment to its registry digest (`repository@sha256:...`) or an exact
local image ID (`sha256:...`). Use the same Compose files with
`up --detach --no-build --pull never`. The image must already exist on that host;
pull or load the chosen immutable artifact explicitly beforehand. This preserves
the tested artifact instead of rebuilding it during deployment. Registry tags
alone, including commit-named tags, are not immutable identities.

The supplied Compose service runs unprivileged with a read-only filesystem.
HTTP/WebSocket is published only on host loopback; media UDP is public.
Compose requires `ALLOWED_ORIGINS` and `TRUSTED_PROXY_SECRET` because the backend
sees the proxy through a Docker bridge.

Set `MEDIA_WORKERS`, `SIMPLESTCHAT_CPUS` and `SIMPLESTCHAT_MEMORY_LIMIT` for your
workload. Keep `RTC_PORT_END`, the published UDP range and firewall rules aligned:
each worker needs one port starting at 40000. Resource limits are not capacity
guarantees. Compose sets the file-descriptor limit to 65536; configure an
appropriate limit separately for native deployments.

## Database migrations and TLS

Run migrations from the repository root using a DDL-capable role. Use a separate
runtime role with only the required table permissions:

```sh
# Owner-readable /etc/simplestchat/migration.env contains only:
# DATABASE_URL=postgres://migration_user:<password>@db.example.com/chat?sslmode=verify-full
cargo install sqlx-cli --version 0.9.0 --no-default-features --features postgres,rustls
set -a
. /etc/simplestchat/migration.env
set +a
sqlx migrate run
unset DATABASE_URL
```

SQLx 0.9 does not publish a CLI lockfile. If deployment-tool reproducibility is
required, package this pinned CLI with a reviewed lockfile; application builds
still use their checked-in `Cargo.lock`.

Migration 011 enables `pg_trgm`. Grant the migration role permission to create
that trusted extension, or have a database administrator install it first.

Non-loopback database connections require `sslmode=verify-full`. For a private
CA, append `sslrootcert=/path/to/ca.pem` to the URL and mount the certificate
read-only at that path. Loopback and Unix sockets are available for development.
Leave `RUN_MIGRATIONS=false` in production; startup migration is a local convenience.

Runtime connections use statement, lock and idle-transaction timeouts of 10, 5
and 15 seconds. The separate SQLx migration command does not inherit them.

## Reverse proxy and metrics

Set the same `TRUSTED_PROXY_SECRET` in the application and Caddy service
environments. Use a root-readable `EnvironmentFile` for Caddy containing only
this secret; keep it out of the Caddyfile, command arguments and logs.

Caddy overwrites `X-SimplestChat-Proxy` with that secret. The backend then trusts
the proxy's appended client address instead of arbitrary client-supplied
`X-Forwarded-For` values. Validate the edited configuration before starting or
reloading Caddy:

```sh
caddy validate --config Caddyfile
```

Caddy provides TLS/security headers and blocks public `/metrics` access.
Configure Prometheus to scrape the loopback backend with bearer authentication:

```sh
curl --fail \
  -H "Authorization: Bearer ${METRICS_TOKEN}" \
  http://127.0.0.1:3000/metrics
```

`METRICS_TOKEN` must be at least 32 bytes; without it the endpoint returns 404.
Keep tokens out of command history and monitoring logs. Monitor active
connections, rooms, participants, errors and message latency. The
`simplestchat_media_workers_live` gauge counts open workers with open WebRTC
listeners. A timed-out snapshot omits that gauge and reports
`simplestchat_media_workers_snapshot_complete 0`; it is unknown capacity, not
zero live workers. A worker that dies is recreated and its rooms are told to
rejoin; `simplestchat_media_worker_deaths_total` counts those interruptions.
If recreation fails the gauge stays low until the server is restarted.

Alert on the rejection counters as well: `simplestchat_api_requests_rejected_total` (HTTP 429/503 from rate limits, concurrency caps, the password lane or a busy service) and `simplestchat_upgrades_rejected_total` (WebSocket upgrades refused by handshake, connection or per-IP limits), and on `simplestchat_media_worker_deaths_total`, because each death interrupts that worker's calls even though the worker is recreated. `simplestchat_connection_permits_in_use` is the quantity `MAX_CONNECTIONS` is enforced against, including handshake authentication work, and can exceed `simplestchat_connections_active`.

**Saturation.** `simplestchat_cpu_saturated` is 1 while the process's cgroup is throttled for more than half of its enforcement periods or its CPU pressure exceeds the configured level; `/ready` returns 503 and fresh joins are refused (counted by `simplestchat_joins_refused_saturated_total`) until the signals fall below half the thresholds. `simplestchat_cpu_throttled_fraction` and `simplestchat_cpu_pressure_some_avg10` are the raw readings, and the two `_available` gauges say whether the cgroup files were readable. Alert on the saturated gauge and on the refusal counter: both mean users were turned away and the host, the quota or the connection limit needs revisiting. See [capacity](performance-results.md) for how the limit was chosen.

**Media quality, server side.** Every `QUALITY_SAMPLE_INTERVAL_SECS` the server samples what the SFU itself knows from RTCP and its send path, with no client-reported data: `simplestchat_quality_consumer_score` and `simplestchat_quality_producer_score` (mediasoup's 0–10 transmission scores as histograms), `simplestchat_quality_video_consumers_by_spatial_layer{layer}` (which simulcast layer viewers are actually receiving), `simplestchat_quality_downlink_loss` (loss viewers reported through transport-cc, as a histogram over sampled receive transports) and `simplestchat_quality_available_outgoing_bitrate` (the estimate toward each sampled viewer). Alert on the share of consumers below score 7, on the share of transports above 2 percent loss, and on a falling share of viewers at the top layer while the estimate histogram is unchanged; these are the field signals behind the [adaptive path](performance.md#production-shape-and-impaired-networks) checks. `simplestchat_consumer_layer_requests_total` counts the worker requests that changed a consumer's preferred layers: the viewer's tile-size or manual ceiling and the server's bandwidth tier are merged server-side, so the count only rises when the applied layers change.

**Canary.** The [Canary workflow](../.github/workflows/canary.yml) runs two owned headless Chromium clients against the deployment every six hours when the `CANARY_ORIGIN` and `CANARY_ROOM` repository variables are set: one publishes a fake camera, the other must decode video at the top layer without loss, and the evidence is retained as an artifact. Create a room reserved for it; the publisher's synthetic video is visible to anyone who joins that room.

Use `GET /health` for process liveness and `GET /ready` for load-balancer
readiness. `/ready` returns 503 when no media capacity remains, a configured
database fails its one-second probe, or shutdown has begun. Probe concurrency
is capped at four (additional probes return 503).
Guest-only mode does not require a database. This does not verify client ICE or
TURN connectivity; see
[readiness details](configuration.md#health-and-readiness).

Stop the application with `SIGTERM` (as Compose does). It closes admission,
notifies existing sockets, cancels reconnect grace, and drains room/media/DB
resources. Cleanup stages have 16 seconds of total asynchronous budgets, up to
200 ms for an enabled diagnostic recorder to close, and one second for runtime
teardown; incomplete cleanup stages log errors and exit nonzero.
Keep Compose's 30-second hard-stop allowance for stalled native code. Existing
requests may finish during drain; shutdown is not a delivery or transaction
completion guarantee. See [shutdown behavior](configuration.md#shutdown).

## TURN relay controls

Joined clients receive TURN credentials reusable until expiry. The lifetime is
also the longest a relayed call can last, because coturn refuses allocation
refreshes with an expired credential and an undisturbed call never restarts
ICE; the default is one day. Bound abuse with coturn's per-user/total
allocation and bandwidth quotas rather than with a short lifetime.
Restrict relay destinations, especially loopback, private, link-local and
cloud-metadata networks unless explicitly required. Monitor allocations and
egress: the application issues credentials but cannot enforce coturn's relay
destinations.

## Updates and validation

Review Dockerfile image digests and the
[Python build-tool pins](../build/pip-constraints.txt) regularly and after security
advisories. Verify replacement multi-architecture digests with
`docker buildx imagetools inspect <image-tag>` before updating them. When refreshing
Fedora packages, update `FEDORA_REFRESH_EPOCH` and rebuild builder/runtime layers
together. Repository-resolved packages are not bit-for-bit reproducible without
a package-repository snapshot.

Keep the pinned static OpenSSL build and maintained mediasoup patches current;
Cargo audit does not cover native code. Follow the
[native dependency notes](../vendor/README.md) and
[OpenSSL helper](../build/install-openssl.sh) rather than replacing them with
arbitrary system libraries.

Run the [container startup/migration smoke](testing.md#container-and-ci-checks)
before rollout. Use [performance testing](performance.md) for workload selection
and [development](development.md) for native build requirements.

## Limitations and operational caveats

- Email addresses are self-asserted login identifiers, not verified identity or
  a recovery channel. Email-based trust/recovery requires verification and mail
  delivery first.
- Secret rooms are unlisted, not access-controlled. Use high-entropy IDs plus
  password/registration requirements for sensitive rooms, or add invitations/ACLs.
- Ordinary logout revokes refresh sessions, but issued access tokens remain valid
  until their 15-minute expiry. Refresh-token replay outside the multi-tab race
  window revokes the token family. Password changes and saved-key recovery revoke
  existing tokens and active account connections; higher-risk deployments may
  need immediate session-backed revocation for ordinary logout too.
- Public/private chat replay is in memory, limited to current membership and
  300 entries / 256 KiB per room. Restart loses it; this is not a durable inbox.
  Preferences are browser-local and guest ignore entries are temporary.
- Private messages and SFU media are not application-layer end-to-end encrypted.
  DTLS/SRTP protects media hops, but the server terminates them and is inside the
  trust boundary.
