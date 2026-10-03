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

## Routine update: use the image CI tested

Run the [complete local CI gate](../../docs/testing.md#run-ci-locally) on the clean
committed `main` revision, then push it directly under the
[main branch policy](../../docs/security.md#main-branch-rules). A pull request is
optional. From a clean checkout at that exact published revision, run:

```sh
python3 build/deploy.py \
  --inventory ops/ansible/inventory.local.yml \
  --repository OWNER/REPOSITORY \
  --origin https://chat.example.com
```

Use your repository and public origin. The command selects the exact checkout
revision's push CI run, waits for success, and finds its production artifact.
Normal CI builds the production image once, tests it by immutable image ID, and
retains that same image. Deployment does not rebuild it or rerun the test suite.
The command does not commit, push, dispatch builds, or retry failed operations.

The controller needs Python 3.12+, authenticated GitHub CLI, Ansible, Node 22.12+,
and trusted SSH access. Select exactly one inventory host; use `--limit` if needed.
Its `scpub_domain` must match `--origin` so verification targets the deployed site.
The default CI wait is one hour; `--wait-seconds` changes that bound. Changed
checkout state, failed CI, expired/missing artifacts, or mismatched helpers stop
the command before deployment. Every deployable artifact must carry the current
trusted CI attestation; unsigned and manually built artifacts are rejected.

Prepared-host checks are enabled by default. After reviewing helper changes,
`--install-helpers` explicitly installs the current helpers instead of requiring
an exact existing match. It does not run host provisioning or database migrations.

The existing release playbook transfers and validates the image while chat stays
online, takes the backup, and replaces only the application. After readiness,
the command runs the public smoke, which writes one labeled message in `lobby`.
Private command logs and the selected revision/run/artifact identities are kept
under `results/deploy.*`; remote deployment evidence remains on the VPS.

Build and CI time are release latency, not service downtime. The interruption is
limited to application replacement and startup; database and proxy stay running.
Before taking the backup and replacing the app, the release waits up to
`--quiet-seconds` (default 600, at most 600) for `simplestchat_rooms_active` to
reach zero on the loopback metrics endpoint. Admission remains open, so a new call
may arrive during the backup and be interrupted; this is a quiet observation,
not a zero-interruption guarantee. The report records `quietWaitSeconds` and
`roomsActiveAtQuietCheck`, then samples again immediately before stopping the app
as `roomsActiveBeforeStop` with `roomsObservedBeforeStopAt`. Even the final sample
can race a join. A deadline reached with rooms still active proceeds. Missing
`METRICS_TOKEN` or an unreadable endpoint records `null` rather than blocking the
release. Pass `--quiet-seconds 0` to skip the wait; the final observation still runs.
An unsuccessful deployment stops the command. Its existing bounded rollback and
unfinished-operation checks remain authoritative; inspect evidence before retrying.
A failed post-deployment smoke does not trigger another restart or rollback; the
report distinguishes a successful release from failed public verification.

## Trusted artifact requirements

The deployment artifact is produced only by `.github/workflows/ci.yml` on a
`push` to `refs/heads/main`. Its signing job waits for the release smoke and all
required CI, security and CodeQL jobs. Pull-request caches or artifacts are never
promoted. The separate manual image-build workflow is useful for diagnostics;
its output has no deployment eligibility, even if another run passed for the
same commit. Local `build/build-release.py` output likewise cannot be staged as a
public release without the trusted signer contract.

The controller downloads and verifies the exact API-digested artifact locally
before SSH, helper installation, remote preflight, image import or service
operations. `gh attestation verify` validates the signed bundle, GitHub OIDC
issuer, exact CI workflow/main-ref identity, signer and source revision, and
GitHub-hosted runner. Additional checks bind the certificate's exact run and
attempt, the archive, SPDX SBOM, runtime proof and VEX subjects, and every release
metadata file. Required runtime source reviews must remain unexpired at verification.
A saved success receipt is not trusted: cached candidate bytes are verified again.

The current ten-file artifact contains `image.tar`, `release.json`, `outcome.json`,
`source.json`, `sbom.spdx.json`, `image-security.json`, `runtime-proof.json`,
`vex.openvex.json`, `release-predicate.json` and `release-attestation.jsonl`. The
predicate binds the original export and successful image-security outcome by
SHA-256. The separate `runtime-license-evidence.json` notice report stays in the
production-security CI artifact, outside this release ZIP. Its hash is bound by
the signed `image-security.json`, and signing verifies the report's exact bytes.
The receiver accepts the verified claim through authenticated controller SSH
stdin and independently compares the downloaded file hashes before publishing
anything. No artifact-provided `verified` marker can authorize a release.

The controller needs disk space for its ZIP and extracted image in addition to
the VPS's retained evidence. GitHub CLI must support the exact verifier flags;
unsupported versions fail instead of relaxing verification. See the maintained
[GitHub verifier contract](https://cli.github.com/manual/gh_attestation_verify)
and [image security evidence](../../docs/image-security.md).

## Stage while chat stays online

### Fetch a GitHub artifact directly onto the VPS

This avoids sending the image archive through your controller's SSH connection.
The controller still downloads its own copy for cryptographic verification.
Use Python 3.12+ and GitHub CLI on the controller, authenticated with
`gh auth login --hostname github.com`. Only GitHub.com is supported; no GitHub
token is installed on the VPS.

Select the exact artifact ID, reviewed 40-character commit, and successful push CI run
ID for that commit. Review the repository's build and CI workflows before trusting
their results. The controller verifies the artifact identity and successful build
and CI runs before requesting a short-lived download URL. A normal CI artifact
must belong to that exact successful main-push CI run and attempt. The manual
build workflow is not accepted. To list artifact IDs from the selected run:

```sh
gh api repos/OWNER/REPOSITORY/actions/runs/BUILD_RUN_ID/artifacts \
  --jq '.artifacts[] | {id, name}'
```

Replace the uppercase placeholders. The unsigned `scpub_release_directory`
mode is not accepted:

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
1080-second deadline, including local verification. The Ansible controller-only
verification step has a separate 660-second bound. Downloaded ZIP and signed
image evidence are verified before the common image-staging checks run. Existing release bytes are never overwritten. Private
evidence is retained under the controller's `results/` and the VPS's release
storage; console output omits credentials and signed URLs. This still stages only:
deployment requires the explicit choice below. Inspect any failure before another
attempt; a timeout is not proof that remote work has stopped.

ZIP archives and images are each limited to 2 GiB. Every attempt keeps its ZIP
and extracted image for inspection, even when the selected release is already
present and identical. Budget disk space for both; no retained evidence is
automatically deleted. Check mode still performs controller-side signature
verification, then skips remote transfer and runtime changes.

### Faster runs on a prepared host

After a successful staging run, add `-e scpub_release_prepared=true` to either
release command. This skips helper installation and checks every required helper
against the exact SHA-256 of your controller checkout, including file types,
ownership and permissions. A missing or changed helper fails before transfer or
staging; it is never silently reused. GitHub fetching creates its new private
release directory itself. A previously downloaded signed candidate may be supplied
as `scpub_release_verified_directory`; it is reverified against the selected
GitHub artifact/run before remote work, with no unsigned-directory fallback.

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
The dump is first written privately as a partial file. Its contents listing is
checked, its bytes are flushed before rename, and the containing directory is
flushed before a durable SHA256 receipt is published. Headroom reserves twice
the current database size plus 1 GiB. Off-host storage and restoration are
separate opt-ins; the [scheduled encrypted workflow](BACKUPS.md) covers nightly
archives, while this release dump remains private rollback evidence.

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

## Schema and configuration changes: the maintenance release

An app-only deployment refuses a candidate whose packaged migrations differ from
the database ledger. When the candidate adds migrations (or the runtime grants
changed), release it through the maintenance launcher instead:

```sh
ops/ansible/.venv/bin/python build/deploy.py --inventory ops/ansible/inventory.local.yml \
  --repository swiftraccoon/simplestChat --origin https://the.research.clinic \
  --limit public_vps --quiet-seconds 0 --maintenance [--install-helpers]
```

Migration `022_require_current_sessions.sql` deliberately deletes all existing
refresh sessions before requiring a nonnullable refresh-family hash. Release it
through this maintenance path: every signed-in client must sign in once afterward.
Accounts, passwords, passkeys, recovery keys and room memberships remain intact.
There is no compatibility parser for older refresh formats or sessionless access
tokens. Review this sign-in impact alongside the usual pre-release backup; do not
edit the already-published migrations 010 or 013 to avoid the reset.

The controller waits for the commit's CI and stages the image while chat stays
online, exactly as a routine release does, then runs `ops/ansible/maintenance.yml`:
it checks out the revision's source under `/srv/simplestchat-bench/sources/`
(the launcher checks the packaged SQL against it), renders `app.env.candidate`
from the current template with the host's facts (sizing and new settings may
change; the secrets, the origin, the address, the image and `RUN_MIGRATIONS`
may not, or the helper refuses it before the backup), renders the runtime
grants, and runs `release-public.py maintain <commit> --candidate-env …` as a
transient unit. That action requires the applied ledger to be a prefix of the
candidate's packaged migrations, takes the live dump with its receipt, stops
the app and then the proxy (Caddy's grace period would otherwise wait on the
app's sockets), installs the candidate configuration with the new image
(`outcome.json` lists the changed keys as `environmentChanges`), and hands
over to `/usr/local/bin/simplestchat-public-deploy`,
which migrates, applies the grants, restarts the app and the proxy, and keeps
its own evidence under `results/deploy.*`. The helper then requires the staged
image to be running on the same database container with the candidate's
ledger, and records `interruptionStartedAt`/`FinishedAt` in its `outcome.json`
(about 10 s on 2026-09-28). A failed launcher keeps the new selection and the
stopped containers for inspection: the schema may already have moved, so there
is no automatic rollback. Do not rerun `maintain` against stopped services: its
preflight expects a live application. Inspect the failed release and launcher
`outcome.json`, selected configuration, migration ledger and retained migration
container. After resolving the recorded cause and establishing that no migration
is still running, use the explicit bounded launcher command in
[PUBLIC.md](PUBLIC.md#deploy-explicitly) to finish the selected deployment. The
launcher refuses a retained migration container until its state has been reviewed
and recovered. Preserve its evidence; deleting the journal or blindly removing
containers does not establish a safe retry. A later successful launcher does not
change the original failed release outcome.

A settings-only maintenance can reuse the selected image when the candidate
environment has validated changes. An identical image and unchanged configuration
are refused before backup or interruption. `backupMigrations` in the release
outcome records the applied ledger before migration separately from the target
revision; restore verification checks the archive against that older ledger.
Every restore requires this recorded ledger. Development evidence created before
this contract is rejected; the verifier does not substitute the target schema.

Pass `--install-helpers` whenever `release_public.py` changed, as for any release.

The managed application and migration services retain the image's audited
`/app/simplestChat` command, non-root user and working directory. Release selection,
the initial deployment launcher and reboot preparation reject alternate commands,
healthchecks, lifecycle hooks, unreviewed mounts and dynamic-loader overrides.
`LD_*`, `OPENSSL_*`, `GLIBC_TUNABLES`, `GCONV_PATH` and `LOCPATH` are forbidden even
when their values are empty. The only additional application mount is the
read-only PostgreSQL socket; `/tmp` remains bounded and `noexec`. The validators
inspect the resolved Compose model, including environment files, and retain
fixed diagnostics instead of configuration values. These restrictions define
the supported managed profile; an administrator's separate command or modified
container is outside its image-specific applicability evidence.

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
