# Single-host releases

Build before maintenance, keep the database and proxy running, and replace only
the application. This path is for an already deployed public site with unchanged
SQL migrations and runtime configuration. Use [public deployment](PUBLIC.md) for
initial setup or reviewed schema/configuration changes.

CI exercises this path using the real release helper, public templates and an
already built production image on a disposable Linux host. It checks successful
replacement and startup-failure rollback without restarting PostgreSQL or Caddy.
See [release integration checks](../../docs/testing.md#app-only-releases-and-rollback)
for prerequisites and evidence limits. This is not a deployment or reboot test of
your VPS; measure the first controlled rollout before relying on its timings.

Use Docker Compose 5.5.1 for these release workflows, matching the provisioned
host and CI. Older versions can omit service `env_file` values from configuration
hashes and reject unchanged containers. CI verifies the same version for both
the runner user and the root-run release helper.

## Build on your Mac or CI

Use Python 3.12+, Git, and a running local Docker Engine with Buildx. Docker API
1.48+ is required for platform-specific image export. Docker Desktop can build
Linux/amd64 on Apple Silicon using emulation; a native Linux/amd64 CI runner is
usually faster. No registry or additional VPS is required.

The manual **Build production release artifact** GitHub Actions workflow runs
this same builder on Linux. Select the reviewed commit/branch and require the
normal CI checks to pass. Download its `simplestchat-production-<commit>` artifact
or use the direct GitHub transfer below.
It has no registry credentials, push permission, SSH access, or deployment step.
Before staging, check that `outcome.json` reports `passed: true` and the artifact
contains `release.json`. Failed build evidence may also be retained by the
workflow; the staging validator checks image integrity, not the build outcome.

Commit the release first, then select a fresh output directory:

```sh
python3 build/build-release.py --output "$PWD/results/release-candidate"
```

The builder takes the exact clean Git revision, normalizes source permissions,
builds only the production target, and exports `image.tar` with `release.json`.
The manifest records the archive checksum, platform, source revision, and SQL
checksums. Build logs and the outcome remain beside the artifact. Existing
directories are never overwritten; inspect failures before starting a new attempt.

The builder does not push images or deploy services. It refuses remote Docker
endpoints, custom builders, running public/benchmark containers, and unfinished
local benchmark work. Keep native dependency pins and the existing Docker cache;
never compile on the live VPS as part of this release path.

## Stage while chat stays online

From the controller environment described in [setup](README.md):

```sh
ANSIBLE_CONFIG=ops/ansible/ansible.cfg \
  ops/ansible/.venv/bin/ansible-playbook \
  -i ops/ansible/inventory.local.yml ops/ansible/release.yml \
  -e "scpub_release_directory=$PWD/results/release-candidate"
```

This verifies the artifact locally, transfers it to private release storage,
checks the exact destination bytes, imports it, and validates its runtime identity
and packaged SQL checksums. Importing an image uses disk and CPU but does not
stop public containers. Reusing a commit with different artifact bytes is refused.
Nothing is published to a registry. The destination's own immutable image ID is
recorded; image IDs need not be portable between Docker image stores.

### Fetch a GitHub artifact directly onto the VPS

This avoids sending the image archive through your controller's SSH connection.
Use Python 3.12+ and GitHub CLI on the controller, authenticated with
`gh auth login --hostname github.com`. Only GitHub.com is supported; no GitHub
token is installed on the VPS.

Select the exact artifact ID, reviewed 40-character commit, and successful push CI run
ID for that commit. Review the repository's build and CI workflows before trusting
their results. The controller verifies the artifact identity and successful build
and CI runs before requesting a short-lived download URL. To list artifact IDs
from the selected build run:

```sh
gh api repos/OWNER/REPOSITORY/actions/runs/BUILD_RUN_ID/artifacts \
  --jq '.artifacts[] | {id, name}'
```

Replace the uppercase placeholders, and omit `scpub_release_directory`:

```sh
ANSIBLE_CONFIG=ops/ansible/ansible.cfg \
  ops/ansible/.venv/bin/ansible-playbook \
  -i ops/ansible/inventory.local.yml ops/ansible/release.yml \
  -e scpub_release_repository=OWNER/REPOSITORY \
  -e scpub_release_artifact_id=ARTIFACT_ID \
  -e scpub_release_expected_revision=COMMIT_SHA \
  -e scpub_release_ci_run=CI_RUN_ID
```

The inventory must use OpenSSH with explicit `ansible_host`, `ansible_user`, and
an absolute private-key file in `ansible_ssh_private_key_file`; `ansible_port`
defaults to 22. Hosts may be DNS names, IPv4 addresses, or SSH aliases, not IPv6.
Use root SSH or passwordless `sudo -n`. Password authentication, other Ansible
connection plugins, and nonempty `ansible_ssh_common_args`/`ansible_ssh_extra_args`
are unsupported. Normal OpenSSH host aliases work, but proxy/jump commands, local
commands, forwarding, and connection sharing are disabled. The controller does
not use Ansible's global `ansible_ssh_args`. Host-key checking remains strict.

The URL is sent only over SSH after the receiver is ready; the API token remains
on the controller. The receiver has a 300-second deadline and the controller a
420-second deadline. Downloaded ZIP and image bytes are verified before the common
image-staging checks run. Existing release bytes are never overwritten. Private
evidence is retained under the controller's `results/` and the VPS's release
storage; console output omits credentials and signed URLs. This still stages only:
deployment requires the explicit choice below. Inspect any failure before another
attempt; a timeout is not proof that remote work has stopped.

ZIP archives and images are each limited to 2 GiB. Every attempt keeps its ZIP
and extracted image for inspection, even when the selected release is already
present and identical. Budget disk space for both; no retained evidence is
automatically deleted. Check mode skips the download and does not validate GitHub
access or remote runtime behavior.

### Faster runs on a prepared host

After a successful staging run, add `-e scpub_release_prepared=true` to either
release command. This skips helper installation and checks every required helper
against the exact SHA-256 of your controller checkout, including file types,
ownership and permissions. A missing or changed helper fails before transfer or
staging; it is never silently reused. GitHub fetching creates its new private
release directory itself. Local archives still use the verified SSH-copy path.

If helpers need updating, review the mismatch, then run the same stage-only
command without prepared mode. This reconciles the helpers and release storage;
it does not require rerunning public provisioning or stopping chat. Run only one
release, helper update, or maintenance operation at a time. The prepared check is
a snapshot; runtime workload locks and unfinished-operation journals remain the
authority for staging and deployment.

The release playbook uses a focused Debian/platform/configuration check instead
of broad fact gathering, and enables
[SSH pipelining](https://docs.ansible.com/projects/ansible/latest/collections/ansible/builtin/ssh_connection.html#parameter-pipelining)
to reduce module-transfer round trips. Sudo configurations that require a TTY
may need `-e ansible_pipelining=false`; the playbook does not modify sudo policy.
Host-key and artifact checks stay enabled in either mode.

To measure your own run, set `ANSIBLE_CALLBACKS_ENABLED=release_timing` alongside
`ANSIBLE_CONFIG` in the command's environment. The optional callback reports
bounded JSON task/playbook durations and completion status, without arguments,
command output, inventory names or credentials. Timings include controller and
transport overhead; they are not service downtime or an availability guarantee.
Check mode performs the read-only host/helper checks but skips download, staging
and deployment. Local-archive check mode requires an already-retained matching
artifact, since it does not copy files. Normal prepared mode still stages only
unless deployment is explicitly enabled.

## Deploy explicitly

Repeat the command above with `-e scpub_release_deploy=true`. Alternatively, use
the exact staged 40-character commit on the VPS:

```sh
sudo systemd-run --unit=simplestchat-app-release \
  --property=Type=exec --property=RuntimeMaxSec=900 \
  --property=TimeoutStopSec=240 --wait \
  /usr/bin/python3 -B /usr/local/libexec/simplestchat-public/release-public.py \
  deploy <40-character-commit>
```

Use a new unit name for a later attempt. Do not automatically retry a failed job.
The command checks running configuration against Compose, requires a healthy
application/database, and rejects missing, changed, failed, or additional SQL
migrations. It takes a live consistent PostgreSQL dump before stopping the app.
The dump is local and its contents listing is checked; off-host retention and
restoration drills remain separate responsibilities.

Only the application is replaced. Caddy and PostgreSQL must retain their IDs,
start times, and restart counts. The candidate must have the selected image and
pass both backend and trusted public HTTPS readiness. Startup failure triggers
one bounded rollback to the previous image/configuration; the release still
reports failure. The database is never restored or migrated automatically.

Each attempt retains private logs and `outcome.json` under
`/srv/simplestchat-public/results/release.*`. Deployment adds a database dump,
selection backups and timing as those phases complete. These files
can contain credentials or user data: do not publish or indiscriminately copy
them. An unfinished journal blocks new releases, builds, benchmarks, and host
maintenance. Inspect the named attempt and owned containers before recovery;
deleting the journal is not proof that daemon-side work has stopped.

After success, verify normal browser use. The bounded
[`build/public-smoke.mjs`](../../build/public-smoke.mjs) check writes one labeled
public message and verifies two guest connections; it does not test media.

## Reboots and user recovery

Use the explicit [reboot playbook](reboot.yml), not a full provisioning or initial
deployment run, to restart an already configured VPS:

```sh
ANSIBLE_CONFIG=ops/ansible/ansible.cfg \
  ops/ansible/.venv/bin/ansible-playbook \
  -i ops/ansible/inventory.local.yml ops/ansible/reboot.yml \
  -e scpub_reboot=true
```

The workflow records the running selection, stops gracefully, verifies a changed
host boot ID, then starts the existing database, application and proxy in order.
There is no image build/pull, migration, enrollment, or competing boot manager.
If the reboot request fails without a new boot, the explicit cancellation path
can restore the saved containers; it does not report a successful reboot.

The controller must remain available through this workflow. There is no new boot
service that silently resumes a deliberately stopped site. If the controller is
lost after preparation, inspect the private journal and run the installed
`reboot-public.py resume` after the boot changed, or `reboot-public.py cancel` on
the original boot, in a bounded systemd job. The helper rejects the wrong boot
identity, changed containers/configuration, and another unfinished operation.
Do not rerun the entire reboot playbook as a recovery shortcut.

A reboot of the only host necessarily interrupts service. Updated browsers show
recovery state and retry for up to two minutes, then offer manual retry. Room
intent and the unsent public draft are retained; microphone, camera, and screen
sharing stay off after a fresh rejoin. Runtime chat history and old media
transports do not survive process replacement. A page loaded before this client
update needs a refresh to gain that recovery behavior.

Independent frontend releases and room-aware overlapping app instances are not
part of this first release path. UI changes still ship with the app image. Do not
add random load balancing: live rooms and media ownership remain process-local.
