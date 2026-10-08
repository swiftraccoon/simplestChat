# SimplestChat

Audio/video chat rooms built with Rust, mediasoup, and a TypeScript web client.
Run guest rooms on their own, or add PostgreSQL for accounts and persistent rooms.

## Features

- Camera, microphone, push-to-talk, screen sharing, and private device preview.
- Public room directory, owned rooms, passwords, lobby admission, invitations and moderation.
- Room chat and private messages with replies, reactions, mention completion, unread
  markers, drafts, ignore controls, personal chat colors and timestamp formats.
- Message editing, moderator removal and room pins; optional retained room history;
  an account PM inbox with search and read positions shared across devices.
- An installable web app and optional private-message push notifications.
- Password/passkey accounts, profiles, and saved recovery keys.
- Responsive layout and per-participant volume, mute, and video controls.
- Sizing derived from the host it runs on (workers, quotas, ceilings, addresses,
  TURN capacity), every value overridable.

Shared watch sessions, recording and email verification are not implemented.

## Quick start

Locally ([prerequisites](docs/development.md#prerequisites)):

```sh
build/run-local.sh        # http://localhost:3000, guest rooms; --skip-web reuses the web build
```

On a Debian 13 VPS, sized from the host's facts ([details](ops/ansible/PUBLIC.md)).
DNS points at the host; TCP 80/443, UDP 443 and one UDP port per configured media
worker, starting at 40000, are open.

```sh
# Controller
python3 -m venv ops/ansible/.venv && ops/ansible/.venv/bin/pip install -r ops/ansible/requirements.txt
cp ops/ansible/inventory.example.yml ops/ansible/inventory.local.yml
$EDITOR ops/ansible/inventory.local.yml   # host, ssh key, scbench_revision (full commit), scpub_enabled: true, scpub_domain
export ANSIBLE_CONFIG=ops/ansible/ansible.cfg
ops/ansible/.venv/bin/ansible-playbook -i ops/ansible/inventory.local.yml ops/ansible/site.yml
# VPS: build the image once per commit
sudo systemctl start simplestchat-image-build.service && sudo journalctl -fu simplestchat-image-build.service
# Controller: render the configuration from the host's CPUs, memory and addresses
ops/ansible/.venv/bin/ansible-playbook -i ops/ansible/inventory.local.yml ops/ansible/public.yml
# VPS: migrate, create the owner and lobby, start the app and Caddy
sudo systemd-run --unit=simplestchat-public-deploy --property=Type=exec --property=RuntimeMaxSec=900 --property=TimeoutStopSec=90 /usr/local/bin/simplestchat-public-deploy
sudo journalctl -fu simplestchat-public-deploy.service
# Verify; owner credentials are in /etc/simplestchat-public/owner.json on the VPS
curl -s https://chat.example.com/ready
```

Update after pushing to `main` ([releases](ops/ansible/RELEASES.md); add `--maintenance` for migrations or sizing changes):

```sh
ops/ansible/.venv/bin/python build/deploy.py --inventory ops/ansible/inventory.local.yml --repository OWNER/REPO --origin https://chat.example.com
```

This default update waits for successful CI and verifies its signed artifact.
An explicitly requested deployment without that wait uses
[`--force`](ops/ansible/RELEASES.md#explicit-force-deployment).

Preview host sizing before provisioning. For a plain Compose deployment, follow
the [deployment guide](docs/deployment.md#production-setup), which covers the private
configuration file, database or guest-room policy, and HTTPS proxy.

```sh
python3 build/capacity.py suggest --vcpus 4 --memory-gib 8 --included-egress-tb 5
```

## Project guide

| Topic | Guide |
| --- | --- |
| Build, local launch, and troubleshooting | [Development](docs/development.md) |
| Quality standards and review requirements | [Contributing](CONTRIBUTING.md) |
| Frontend development and modules | [Web client](web/README.md) |
| Server architecture and protocol | [Rust server](src/README.md) · [Protocol](docs/protocol.md) |
| Environment variables and limits | [Configuration](docs/configuration.md) |
| Production setup and security | [Deployment](docs/deployment.md) |
| Host preparation, image builds and private benchmarks | [Operations](ops/ansible/README.md) |
| The managed public site: HTTPS, PostgreSQL, TURN, monitoring | [Public deployment](ops/ansible/PUBLIC.md) |
| Routine and maintenance releases | [Releases](ops/ansible/RELEASES.md) |
| Unit and integration tests | [Testing](docs/testing.md) |
| Benchmarks and measurements | [Performance](docs/performance.md) · [Results](docs/performance-results.md) |
| Sizing a server and comparing hosts' cost | [Sizing a host](docs/performance.md#sizing-a-host) |
| Browser diagnostics | [Diagnostics](docs/diagnostics.md) |
| Native dependency updates | [Vendor notes](vendor/README.md) |

## Checks

After [build setup](docs/development.md):

```sh
build/check.sh
```

This runs Rust, web and Python quality gates plus web/helper tests. Native Rust,
database and browser integration tests are documented in [testing](docs/testing.md).

## Deploying

Follow the [deployment guide](docs/deployment.md) for TLS, database migrations,
origins, and secrets. The server currently supports a single application replica.
Private messages and media are not end-to-end encrypted; secret rooms are
unlisted, not access-controlled. See [security limitations](docs/deployment.md#limitations-and-operational-caveats).
