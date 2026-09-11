# Deployment and security

Deploy one application replica behind an HTTPS reverse proxy, with PostgreSQL
and optional TURN. WebAuthn challenges, rate limits and room/media state are
process-local; multiple replicas require a shared-state and routing design.

## Production setup

1. Point your domain at the host and replace `simplestchat.example.com` in
   `Caddyfile`. Set `ALLOWED_ORIGINS` to the exact external HTTPS origin.
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
connections, rooms, participants, errors and message latency. `GET /health`
reports process liveness only, not database readiness.

## TURN relay controls

Joined clients receive TURN credentials reusable until expiry. Keep `TURN_TTL`
low and configure coturn per-user/total allocation and bandwidth quotas.
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
