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

Media is reachable over UDP only by default, and a deployment without TURN
turns away every client whose network blocks outbound UDP. To offer ICE-TCP as
well, publish the same port range over TCP in Compose (a second `ports` entry
ending in `/tcp`), open it at the provider firewall, and set
`WEBRTC_SERVER_TCP=true`; the weekly performance workflow proves the TCP path
with UDP blocked. TURN over TCP or TLS (coturn, `TURN_URLS`/`TURN_SECRET`) provides
another path when clients cannot reach the media ports. A symmetric NAT alone
does not require TURN when the browser can reach this public ICE-Lite server;
the browser initiates the connection, as described in the
[mediasoup deployment guidance](https://mediasoup.org/faq/#running-mediasoup-in-hosts-with-private-ip-aws-google-cloud-azure).
Networks allowing only outbound TCP 443 need a relay listening on that port on
a separate address from the HTTPS proxy. See the relay controls below.

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

**Saturation.** `simplestchat_cpu_saturated` is 1 while the process's cgroup is throttled for more than half of its enforcement periods or its CPU pressure exceeds the configured level; `/ready` returns 503 and fresh joins are refused (counted by `simplestchat_joins_refused_saturated_total`) until the signals fall below half the thresholds. `simplestchat_cpu_throttled_fraction` and `simplestchat_cpu_pressure_some_avg10` are the raw readings, and the two `_available` gauges say whether the cgroup files were readable. `simplestchat_media_worker_cpu{worker="N"}` is each media worker thread's share of one core over the same window and `simplestchat_media_worker_saturated{worker="N"}` is 1 while it exceeds `CPU_SATURATION_WORKER_UTILIZATION`: a router lives on one worker, so a single busy room can pin a core while the quota still shows headroom; rooms on that worker refuse fresh joins (same counter), new rooms are placed on another worker, and `/ready` fails only when every worker is saturated. Alert on the saturated gauges and on the refusal counter: all mean users were turned away and the host, the quota, the worker count or the connection limit needs revisiting. See [capacity](performance-results.md) for how the limit was chosen.

**Media quality, server side.** The bounded sampler reports SFU transmission scores,
spatial layers, receive-transport loss and outgoing bitrate estimates. Current
score/loss/bitrate distributions are disjoint **gauges** labeled `upper_bound`,
with gauge `_count` and `_sum`; they are not cumulative histogram buckets. Score
boundaries are exact values 0–10; loss/bitrate boundaries describe nonoverlapping
intervals. Never apply `rate()` or `histogram_quantile()` to these snapshots.
Check `simplestchat_quality_sample_available`, sample age/duration/completeness and
participant/transport coverage before interpreting any distribution. Failed or
unrequested transports are unknown, not healthy samples. Partial samples are
expected when the configured budget cannot cover every participant; persistent
budget exhaustion needs investigation. The layer-change counter remains a
monotonic count of worker requests that actually change preferred layers.

**Canary.** The [Canary workflow](../.github/workflows/canary.yml) runs two owned
headless Chromium clients hourly, first on the normal path and then forced through
TURN. Both must decode video at least ten frames per second, receive audio packets and
keep video packet loss at or below two percent with no three-second decoded-frame
stall. The normal scenario verifies a direct current selected ICE pair; the relay
scenario verifies selected
local candidate types. Local impairment tests retain their stricter zero-loss baseline.
Canary clients explicitly leave before closing, preventing reconnect-grace overlap. Synthetic media uses a dedicated reserved room; configure
`CANARY_ORIGIN` and `CANARY_ROOM`. Missing configuration fails visibly. The separate
[availability workflow](../.github/workflows/availability.yml) checks public HTTPS
readiness every ten minutes. GitHub schedules may be delayed and are not a
sub-minute uptime guarantee. No workflow posts chat messages.

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

## Private operational monitoring

Apply the separately owned [monitoring playbook](../ops/ansible/monitoring.yml)
with `scmon_enabled=true` on an already prepared production host. It installs
pinned Prometheus and node-exporter containers without changing the public
application, PostgreSQL or proxy. Prometheus listens only on `127.0.0.1:9090`,
node-exporter on `127.0.0.1:9100`; neither is proxied publicly. Scrapes use the
existing application bearer token through a private credential file. The
monitoring containers never receive the Docker socket or database credentials.
Prometheus has a 512 MB memory limit, 0.5 CPU quota, 15-day retention and a 1 GB
TSDB retention target; allow additional space for its WAL and active head block.
The app scrape permits 15,000 series; HTTP and client instrumentation cap stored
series at 256 and 512 respectively and count discarded observations.
`TelemetrySeriesDropped` reports incomplete instrumentation.
Node-exporter has 64 MB and 0.1 CPU limits. Its host mount is read-only and its
collector list is explicit. See [Prometheus storage bounds](https://prometheus.io/docs/prometheus/latest/storage/)
and the [node-exporter textfile collector](https://github.com/prometheus/node_exporter#textfile-collector).

A hardened root timer samples fixed host/container/database/TLS/evidence signals
each minute and writes an atomic textfile. Docker inspection excludes environment
variables, command lines and log payloads. Database reads are aggregate statistics;
there are no user-row reads. Failed subsystems report explicit failure gauges;
`CollectorStale` detects a failed or stopped timer instead of treating its previous
snapshot as current. Release storage is measured without deleting any release or
incident evidence. Plan cleanup by listing retained revisions, preserving the
current and rollback releases and reviewing backup/evidence obligations first.

Operational incidents are stored separately from application migrations in the
root-owned PostgreSQL `operations` schema. Initial setup takes and checks a private
custom-format database backup, then creates schema version 1 transactionally.
The application and migration roles receive no access. The recorder polls firing
Prometheus rules, retains only fixed rule/severity/resource fields and the release
revision, and tracks first/last observation and resolution. Repeated snapshots
are idempotent. An incomplete Prometheus response cannot resolve existing
incidents. Resolved history is bounded to 30 days and the most recent 10,000 rows;
active incidents are retained. There is no external alert delivery configured.

During database failure the recorder keeps up to 1,440 snapshots, each at most
64 KiB, in a root-only disk spool. It keeps the earliest and latest evidence when
that cap is exceeded and exposes a durable discarded-snapshot counter. Replay is
bounded per invocation; spool depth distinguishes backlog from a successful
individual database write. A database failure itself is retained, including when
a SQL commit succeeded but its response was lost. If the entire host is offline,
local recording cannot run; the external GitHub availability workflow retains its
own failure evidence. Same-host PostgreSQL history is not an off-host backup.

Read incidents privately, without granting the web application access:

```sh
docker exec --user postgres simplestchat-public-postgres-1 \
  psql -X -h /run/simplestchat-postgres -U postgres -d simplestchat \
  -c 'SELECT rule, severity, resource, first_seen, last_seen, resolved_at, release_revision FROM operations.alerts ORDER BY last_seen DESC LIMIT 100;'
```

Use an SSH tunnel to view Prometheus; leave its unauthenticated administrative UI
bound to loopback. Rules cover scrape availability, sample freshness, saturation
refusals, worker deaths, database/container/host pressure, disk space, public
readiness, TURN health, certificate expiry, backup age and collector/recorder
health. Thresholds are operational starting points, not measured capacity claims.
Backup freshness currently measures nonempty release backups; this is not a
scheduled backup policy or proof of recovery. Run the installed restore verifier
against an explicitly selected existing release attempt:

```sh
sudo /usr/bin/python3 -E -B /usr/local/libexec/simplestchat-public/restore_verify.py \
  release.REPLACE_WITH_RECORDED_ATTEMPT
```

The argument names a directory beneath `/srv/simplestchat-public/results`,
containing `database-before.dump` and `outcome.json`. Its recorded SHA-256 must
match a private descriptor-based snapshot, and its release manifest must remain
available. The helper takes the deployment workload lock, uses the running
PostgreSQL image by immutable local ID without pulling, and creates a unique
disposable container with no network, published ports, host data/socket mounts,
or attached application. It has a read-only root filesystem, no capabilities,
0.5 CPU, 512 MB memory without swap and a 256 MB temporary database filesystem.
Archives are capped at 64 MB; larger restores require reviewed resource limits.

The complete archive is restored in one transaction with ownership and ACLs
preserved, then checked against the release migration checksums, required schema,
validated constraints/indexes, incident sequence/uniqueness and application role
privacy. Active and resolved incidents are retained. PostgreSQL
[pg_amcheck](https://www.postgresql.org/docs/18/app-pgamcheck.html) additionally
checks supported heap/TOAST structures and B-tree indexes; its structural checks
do not cover GIN indexes. Restoring their definitions still rebuilds them from
the restored data. The local role stubs have no login or production passwords;
this does not verify recovery of host secrets or a complete production cutover.
Each restore and structural check has a 120-second deadline. Existing backups
from before the operational schema was installed fail the current schema check.

Only successful restore, verification, container removal and snapshot removal
allow the helper to update
`/var/lib/simplestchat-monitoring/restore-verified.timestamp`, clearing
`RestoreEvidenceMissing` on the next collection. Never update it by hand or after
an archive listing. Private command/error evidence and aggregate row counts remain
under `/var/lib/simplestchat-monitoring/restores/restore.*`; terminal output never
includes database rows. These small operator-created records have no automatic
deletion policy. A failed exercise leaves previous success evidence unchanged.
If forced process termination interrupts cleanup, a subsequent exercise refuses
to start while a labeled restore container remains. Inspect its recorded unique
name in `restore.json` and the exact `clinic.research.simplestchat.restore` label
before removing that container; never target the live PostgreSQL container.

### Logging and rollout order

Application logs default to JSON and include the release identity. The maintained
public Compose template uses journald for the application, with a 4 MB
nonblocking Docker buffer. The monitoring playbook configures a persistent host
journal bounded to 256 MB, 15 days and a 2 GB free-space reserve. This includes
other host journal entries; check existing retention requirements before applying
on a shared host. Docker's nonblocking buffer can drop records under sustained
backpressure, and journald can suppress bursts. Neither is an audit-grade guarantee
of complete delivery. Bounded application warning families export occurrence and
suppression counters; inspect journal suppression notices as well.

For an existing deployment, apply monitoring first. Its explicit protected
`application-journal.enabled` marker asks the next verified application release
to change only application logging alongside the immutable image. The release
helper still verifies that the live configuration exactly matches disk before
constructing the candidate, permits only the reviewed logging delta, and restores
the original configuration during rollback. Do not manually edit the live Compose
file to change logging: that correctly triggers the configuration-drift gate.
There is one application replacement and no PostgreSQL or Caddy replacement.

The monitoring playbook installs the reviewed `turn_public.py`, its dependencies
and hardened certificate unit and invokes relay metrics activation. The equivalent
manual action after installing those reviewed files is `python3 -E -B /usr/local/libexec/simplestchat-public/turn_public.py enable-metrics`.
This transaction checks the existing relay identity and binary support, backs up
the configuration, adds only the metrics listener settings, updates its protected
configuration digest and replaces the TURN container once. Auth, certificates and
public ports are preserved. It verifies TLS and the actual loopback-only 9641
listener with native allocation metrics, restoring the previous configuration and
relay on failure. Existing allocations are interrupted by this explicit replacement;
no room-idle wait is imposed. Future full relay provisioning must set
`scpub_turn_metrics_enabled=true` to preserve that configuration. Metrics never
enable username labels. `turn_total_allocations` is a current gauge; finished-session
traffic counters do not measure instantaneous in-flight bandwidth.

### Browser diagnostics and privacy

The Diagnostics checkbox opts the browser into reliability uploads and persists
that preference as `reliabilityTelemetry`. Reports have fixed typed names and
outcomes: no raw console output, exception text, room/account identifiers, chat
contents, credentials, SDP or candidate addresses. The client bounds pending
reports to 64, local history to 80 and batches to 16, flushes every ten seconds,
uses a five-second request deadline and does not retry failed uploads. Per-event
burst limits and dropped-event counts make loss visible. The server validates and
rate-limits this untrusted telemetry; it never affects admission or media control.

Copying a diagnostic summary is a separate user action. The preview contains UTC
generation time, build revision, coarse browser family, fixed events and relative
timings. Its temporary local reference rotates after 30 minutes and is not a
server lookup key. A user can review the summary before sharing it.

Visible opted-in calls sample at low frequency (15 seconds) with bounded native
statistics requests. Unsupported/reset counters and silence remain unknown rather
than fabricated zeroes. First-video-frame timing begins at remote track attachment
and ends at the browser's first presented frame; it is not room-join latency.
Server transport quality and browser receipt/playback measurements describe
different stages and must not be substituted for one another.

Keep `Authorization`, `Cookie`, `Set-Cookie` and `Sec-WebSocket-Protocol` headers,
query strings and URL fragments out of custom proxy/APM/access logs. The supplied
proxy does not enable raw access logging. The WebSocket subprotocol carries an
access token.

Passkey login starts with an empty JSON object and no account lookup or credential
allowlist. The browser selects a discoverable credential; the server binds its
user handle and credential ID to the same account, verifies the assertion with
required user verification, then checks the locked current credential counter.
The retired email selector is rejected regardless of account state. Account
registration still reports duplicate email addresses; this change removes the
passkey-enrollment lookup, not every account-existence signal.

New passkeys request `residentKey: required` and `requireResidentKey: true`, without
restricting authenticator attachment or password-manager choice. The pinned
WebAuthn library's discoverable verifier is reused with an explicit modal browser
policy; signature, challenge, origin, RP and user-verification checks stay intact.
Unsigned browser residency hints are never authorization evidence. Existing
discoverable credentials continue to work; nonresident credentials cannot sign in
through this flow. Before upgrading an installation that must retain such accounts,
validate an alternate sign-in method or a discoverable replacement. There is no
public legacy lookup fallback.
