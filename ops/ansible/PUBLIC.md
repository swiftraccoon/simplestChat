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
UDP 443, and UDP 40000–40001 through your provider firewall. HTTP port 3000 stays
loopback-only; PostgreSQL uses a private Unix socket, not a published TCP port.
Do not publish Caddy's administrative port.

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

Registration is off by default; guests can still join the lobby. Opting into
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

TURN is not configured. Direct UDP media still needs checks from real browsers
and networks; relay support requires separate setup. The two-worker allocation
and connection limits are starting settings, not measured capacity guarantees.

## Maintenance and data

Before base-host provisioning, image builds, private benchmarks, or public
configuration changes, announce downtime and stop the entire public project:

```sh
sudo /usr/local/bin/simplestchat-public stop --timeout 60
```

The automation refuses to compete with a running public project; it does not
stop users' sessions automatically. Finish maintenance, apply `public.yml` when
needed, and run the explicit deployment command again. This is a maintenance
deployment, not a rolling upgrade.

Persistent data lives under `/srv/simplestchat-public`: `postgres` contains the
database, and `caddy-data`/`caddy-config` retain certificate and proxy state.
Protect `/etc/simplestchat-public` as well; it contains credentials and secrets.
Never use `down --volumes` or Docker pruning as a maintenance shortcut.

Deployment creates a custom-format database dump under `backups/initial.*.dump`
after migrations and lobby setup. These are local snapshots, not scheduled or
off-site backups, and are not a pre-upgrade rollback point. Before updating an
existing site, take an independent database backup and provider snapshot; copy
backups off the VPS through a protected channel and test restoration.

Application SQL migrations run only during explicit deployment. PostgreSQL
major-version upgrades, database rollback, secret rotation, and backup scheduling
are not automated; review and plan them separately.
