# Public test deployment

This opt-in deployment adds HTTPS, persistent PostgreSQL, and a guest-accessible
`lobby` to the [prepared Debian VPS](README.md). Build and retain the application
images first; `scbench_revision` must identify their verified manifest. Deployment
reuses that exact application image and pinned PostgreSQL/Caddy images.

This is the initial/full-maintenance workflow. For routine same-schema updates,
use [prebuilt app-only releases](RELEASES.md), which keep the database and proxy
running and do not rebuild on the live VPS.

## Prepare configuration

Add these host variables to your ignored `inventory.local.yml`:

```yaml
scpub_enabled: true
scpub_domain: chat.example.com
scpub_announce_ip: 192.0.2.10
scpub_registration_enabled: false
```

Replace the example domain and reserved address with your domain and the VPS's
public IPv4 address. Point DNS at the VPS before deployment. Allow TCP 80/443,
UDP 443, and one UDP port per media worker from 40000 (40000–40002 on a 4-vCPU
host; `RTC_PORT_END` in the rendered `app.env`) through your provider firewall.
HTTP port 3000 stays loopback-only; PostgreSQL uses a private Unix socket, not a
published TCP port. Do not publish Caddy's administrative port.

Sizing follows the host's facts: the app gets all but one vCPU, up to 64 media
workers, and the memory beyond a quarter (at least 1 GiB) kept for PostgreSQL,
Caddy, TURN and the system. `MAX_CONNECTIONS` starts from reference workload
estimates, including 175 webinar viewers per worker, then accounts for the
application memory budget and advertised network port. `MAX_ROOMS` follows the
connection limit, with a minimum of 64; room membership is capped at 10,000.
The Ansible filter in `filter_plugins/sizing.py` calls the same model as
`build/capacity.py suggest`. These are planning estimates from reference
measurements, not measured capacity for your host or a guarantee that runtime
CPU admission guards prevent memory or network exhaustion.

`group_vars/benchmark_hosts.yml` holds the defaults; set any of `scpub_app_cpus`, `scpub_media_workers`,
`scpub_app_memory_mib`, `scpub_max_connections`, `scpub_max_rooms`,
`scpub_max_participants_per_room` or `scpub_max_broadcasters_per_room` in the
inventory to override one (for example `scpub_media_workers: 2` behind a
firewall you cannot change that opens only UDP 40000–40001). Compose publishes
the range itself; a host needs no firewall rule of its own for it.
`build/capacity.py suggest --vcpus N --memory-gib M --port-mbps P`
previews the defaults for the same host facts and port, and the server logs its
sizing at startup. Explicit inventory overrides remain operator policy. A
128-vCPU, 256-GiB host with a 40-Gbit/s port, for example, still receives at most
64 media workers and a 10,000-member room limit; adding CPUs does not remove
those implementation limits. Measure the intended workload before increasing
admission limits.

The reserve itself is a setting (`scpub_reserved_cpus` 1, a
`scpub_reserved_memory_share` of 0.25 with at least
`scpub_reserved_memory_min_mib` 1024), and the services in it get ceilings that
grow with it: PostgreSQL half of the reserve within 1–4 GiB
(`scpub_postgres_memory_mib`, `shared_buffers` a quarter of that), Caddy an
eighth within 256 MiB–2 GiB (`scpub_caddy_memory_mib`), each with the reserved
CPUs. TURN's capacity follows `scpub_port_mbps` (1000; set the port the
provider sells, since a virtio interface does not report its speed):
Coturn interprets `bps-capacity` and `max-bps` as **bytes per second**.
`bps-capacity` is half the port because a relay carries every stream twice
(62,500,000 bytes/s, or 500 Mbit/s, on a 1-Gbit/s port),
`total-quota` matches `MAX_CONNECTIONS`, the relay port range grows with it,
and `max-bps` defaults to 500,000 bytes/s (4 Mbit/s) per session per direction.
`scpub_max_users` and `scpub_max_persisted_rooms` (100 each) are policy, not
capacity.

The addresses follow the host's default routes: `scpub_announce_ip` is the
IPv4 default (the IPv6 one on an IPv6-only host) and `scpub_announce_ipv6` the
IPv6 default beside it, so a dual-stack host announces both, the project
network carries IPv6 natively (`scpub_ipv6_network`, a private range Docker
translates to the host's address). The managed TURN playbook requires a public
IPv4 primary address and can add an IPv6 listener; IPv6-only TURN is rejected
before preparation or activation. Direct application media supports an IPv6-only
host independently of that relay limitation. The inventory
overrides either; an empty `scpub_announce_ipv6` keeps a host IPv4-only. Adding
or changing an announced address is an identity change: the maintenance
release refuses such a candidate, so apply it with `public.yml`.

The provider's monthly transfer allowance is not visible from inside the host:
set `scpub_transfer_allowance_tb` (and `scpub_transfer_reset_day` when the
billing period does not start on the first). The monitoring collector totals
the default interface's traffic per period in both directions, which is how
providers usually count, and `TransferAllowanceNearlyUsed` fires at 80 % and
`TransferAllowanceExhausted` at 100 %. `build/capacity.py suggest
--included-egress-tb N` says what an allowance carries in participant-hours;
for most hosts that, not CPU or the port, is the sustained-use ceiling.

From the project root, using the controller environment from the setup guide:

```sh
ANSIBLE_CONFIG=ops/ansible/ansible.cfg \
  ops/ansible/.venv/bin/ansible-playbook \
  -i ops/ansible/inventory.local.yml ops/ansible/public.yml
```

This prepares directories, generates secrets once, renders configuration, pulls
missing pinned dependency images, and installs commands. It does not start the
public service. The public project must be stopped when applying configuration.
Do not print resolved Compose configuration: it contains secrets.

The image comes from the host's own build of `scbench_revision` by default. To
apply configuration changes with the image CI tested instead, stage it while
chat is live (`release.yml` with `scpub_release_deploy: false`, as
`build/deploy.py` does), stop the project, and pass the same revision as
`-e scpub_release_revision=<rev> -e scbench_revision=<rev>`: the playbook then
selects the staged image, checks its revision label, and the launcher below
starts it.

## Query statistics and slow queries

PostgreSQL runs with `pg_stat_statements` preloaded (`track=all`,
`track_io_timing=on`) and logs statements slower than 500 ms. Both
`log_parameter_max_length=0` and `log_parameter_max_length_on_error=0` explicitly
suppress bound parameter values in ordinary and error logging. These settings
do not redact literals embedded in SQL text or every server error message; keep
database logs private. See the [PostgreSQL logging controls](https://www.postgresql.org/docs/18/runtime-config-logging.html#GUC-LOG-PARAMETER-MAX-LENGTH). Read the heaviest
queries from the container as the operator (the app role has no access):

```sh
cid=$(docker ps -q --filter name=^simplestchat-public-postgres-1$)
docker exec --user 999:999 "$cid" psql --host /run/simplestchat-postgres --username postgres \
  --dbname simplestchat --command "SELECT calls, round(mean_exec_time::numeric, 2) AS ms,
  rows, left(query, 100) AS query FROM pg_stat_statements ORDER BY total_exec_time DESC LIMIT 20"
docker logs --since 24h "$cid" 2>&1 | grep 'duration:'
```

`SELECT pg_stat_statements_reset()` starts a fresh window. A new host gets the
extension from the init SQL; the 2026-09-28 host was switched by hand with
`ALTER SYSTEM SET shared_preload_libraries = 'pg_stat_statements'`, a container
restart (about ten seconds without the database) and `CREATE EXTENSION`.

Registration is off by default; guests can still join the lobby, and any
account can hand out registration invite codes (Account → Invite someone to
register). Opting into
registration permits at most 100 total accounts, including the owner. Email
addresses are not verified. Treat this as a public test site, not an invitation
to store sensitive information.

## Deploy explicitly

On the VPS, start the deployment independently of your SSH session:

```sh
sudo systemd-run --unit=simplestchat-public-deploy \
  --property=Type=exec --property=RuntimeMaxSec=900 \
  --property=TimeoutStopSec=90 \
  /usr/local/bin/simplestchat-public-deploy
sudo journalctl -fu simplestchat-public-deploy.service
```

The launcher validates Caddy, starts the private database, applies the packaged
SQL migrations, and checks their source checksums. A separate database role runs
migrations; the normal application cannot alter the schema.

The proxy remains stopped while registration is temporarily enabled on the
private backend to create the owner and lobby. The launcher restores the
inventory's registration policy before starting Caddy. Repeated deployment
preserves existing owner credentials and lobby settings.

Owner login credentials are retained in `/etc/simplestchat-public/owner.json`,
readable only by root (`0600`). Retrieve them privately; never paste them into
logs, issues, or the repository. The owner email is `owner@<your-domain>`.

Each attempt retains its outcome and bounded logs under
`/srv/simplestchat-public/results/deploy.*`. Starting the systemd job is not proof
of success: inspect its exit status and `outcome.json`, then verify HTTPS. If an
attempt fails or times out, inspect retained containers and logs before retrying;
do not delete migration evidence to bypass a failed deployment.

## Verify and operate

Open `https://chat.example.com` in your browser. For a bounded protocol check,
run from your checkout with Node 22.12+:

```sh
node build/public-smoke.mjs --origin https://chat.example.com --room lobby
```

This joins two guests and **writes one public test message**, then leaves and
closes both connections. It checks trusted HTTPS, assets, private-endpoint denial,
and WebSocket text delivery. It does not use a camera or microphone or validate
media, browser rendering, or capacity. Run it only against your own test site.

The root-only Compose wrapper always selects this project's private configuration:

```sh
sudo /usr/local/bin/simplestchat-public ps
sudo /usr/local/bin/simplestchat-public logs --tail 100 simplestchat caddy
```

TURN is opt-in; see the managed relay below. Direct and relayed media still need
checks from real browsers and networks. The sizing derived from the host is a
starting point, not a measured guarantee: `build/capacity.py run` measures it.

## Managed TURN on the same VPS

Set `scpub_turn_enabled: true` in the ignored inventory. The host must already
serve trusted HTTPS and have a staged app-only release matching its running
image. Allow inbound UDP/TCP 3478 and TCP 5349 at the provider firewall. The relay
uses the configured public addresses and accepts peer destinations only on
those addresses. Its UDP allocation range starts at 49160 and grows with
`scpub_turn_total_quota`, ending at most at 65000. The rendered relay ports
communicate with the media server on the same host; they do not need public
inbound firewall access.

Prepare and start the separate, checksum-pinned coturn 4.18.0 project:

```sh
ANSIBLE_CONFIG=ops/ansible/ansible.cfg ops/ansible/.venv/bin/ansible-playbook \
  -i ops/ansible/inventory.local.yml ops/ansible/turn.yml --limit public_vps
```

Preparation does not change the application's advertised ICE servers. It
generates a dedicated secret, copies and validates Caddy's hostname certificate,
starts the unprivileged relay with resource limits, and installs an hourly
certificate timer. The timer selects a validated certificate/key pair together
and sends SIGUSR2; active allocations are retained. Configuration preparation
requires the relay to be stopped, but the application and HTTPS remain running.
Serialize this operation with releases and other host maintenance.

Verify authenticated allocations externally over UDP, TCP and trusted TLS, then
enable the relay for clients:

```sh
ANSIBLE_CONFIG=ops/ansible/ansible.cfg ops/ansible/.venv/bin/ansible-playbook \
  -i ops/ansible/inventory.local.yml ops/ansible/turn.yml --limit public_vps \
  -e scpub_turn_activate=true
```

Activation waits up to ten minutes for active rooms, then backs up the database
and replaces only the application with the **same image**, adding the three
reviewed TURN environment values. PostgreSQL and Caddy stay running. It uses the
release journal, readiness checks and bounded rollback, and retains evidence in
`/srv/simplestchat-public/results/turn.*`. Inspect its `outcome.json`; starting a
systemd unit is not proof of success. Repeated activation of the same settings
does not restart the app. Secret rotation is separate maintenance.

The relay permits four allocations per credential; its total allocation quota
defaults to `MAX_CONNECTIONS`. The default per-session ceiling is 500,000
bytes/s (4 Mbit/s) per direction, and the aggregate ceiling is 62,500,000
bytes/s (500 Mbit/s) on a 1-Gbit/s port. Inventory can override both ceilings.
These limits bound relay use; they are not measured capacity.
Credentials expire after one day. Networks allowing only outbound TCP 443 still
need a separate relay address on that port: this VPS uses it for HTTPS.

Check certificate renewal with
`systemctl status simplestchat-turn-certificate.timer` and inspect the matching
service journal for failures. Keep `scpub_turn_enabled` in inventory so later
full maintenance preserves the relay settings. Protect `/etc/simplestchat-turn`
alongside the other private configuration; it contains the shared secret and a
copy of the TLS private key.

## Maintenance and data

Before base-host provisioning, image builds, private benchmarks, or public
configuration changes, announce downtime and stop the entire public project:

```sh
sudo /usr/local/bin/simplestchat-public stop --timeout 60
```

The automation refuses to compete with a running public project; it does not
stop users' sessions automatically. Finish maintenance, apply `public.yml` when
needed, and run the explicit deployment command again. This is a maintenance
deployment, not a rolling upgrade. Sizing and other `app.env` changes that keep
the secret and identity lines need no `public.yml` run: a maintenance release
(`build/deploy.py --maintenance`, [RELEASES.md](RELEASES.md)) re-renders the
file from the host's facts and installs it with the image. A validated settings
change can reuse the selected image; unchanged image and settings are rejected
before interruption.

Persistent data lives under `/srv/simplestchat-public`: `postgres` contains the
database, and `caddy-data`/`caddy-config` retain certificate and proxy state.
Protect `/etc/simplestchat-public` as well; it contains credentials and secrets.
Never use `down --volumes` or Docker pruning as a maintenance shortcut.

Deployment creates a custom-format database dump under `backups/initial.*.dump`
after migrations and lobby setup. These are local snapshots, not scheduled or
off-site backups, and are not a pre-upgrade rollback point. Before updating an
existing site, take an independent database backup and provider snapshot; copy
backups off the VPS through a protected channel and test restoration.

A nightly local dump is a separate opt-in. Set `scpub_backup_enabled: true` in
the ignored inventory and apply [backup.yml](backup.yml). Its 04:00 UTC timer
runs the reviewed worker under the same workload lock as releases and benchmarks.
An active or unfinished operation blocks a new backup. The worker clears only
recognized private stale partials, reserves twice the current database size plus
1 GiB, bounds `pg_dump` inside the database container, and validates the archive
with `pg_restore --list`. It then flushes the dump, renames it, flushes the
parent directory, and durably publishes its receipt. The receipt contains size,
SHA256, completion time, the exact PostgreSQL image and applied migration ledger.
The live-size reserve is a conservative estimate, not a guarantee against later
filesystem growth.

`scpub_backup_keep_days` accepts integers from 1–3650 (default 14), and
`scpub_backup_keep_at_least` accepts integers from 1–365 (default 3). Pruning
retains the newest verified archives and removes older eligible pairs only after
a new successful archive. Private nightly command logs share the age limit.
Release evidence is separate and is never automatically pruned. The collector
requires current receipt schema, a regular root-owned 0600 receipt and dump,
matching size and SHA256, and a valid completion time before reporting backup
freshness. It reuses a verified digest for at most 24 hours while the file's
identity, size and modification/change timestamps remain unchanged, checking
ownership and permissions on every collection. Changed files are hashed again.
An archive listing does not advance restore-drill freshness.

Normal failure cleanup removes the worker's own partial dump. The wrapper has an
EXIT cleanup trap; startup also checks for stale partials. An abrupt termination
retains `operation=nightly_backup` in the canonical ownership journal. Its cleanup
command may clear only that operation after the recorded same-boot safety deadline
(eight minutes) or a reboot, while holding the shared lock. This allows the bounded
daemon-side dump to finish before other work is admitted:

```sh
sudo /usr/bin/python3 -E -s -B /usr/local/libexec/simplestchat-public/backup_public.py cleanup
```

Application SQL migrations run only during explicit deployment. PostgreSQL
major-version upgrades, database rollback, secret rotation and off-host backup
replication are not automated; review and plan them separately. Nightly local
scheduling is provided by the opt-in backup playbook above.
