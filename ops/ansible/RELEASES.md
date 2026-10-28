# Single-host releases

For routine updates, build before deployment, keep the database and proxy running,
and replace only the application. App-only releases require unchanged SQL
migrations and runtime configuration. Use the [maintenance release](#schema-and-configuration-changes-the-maintenance-release)
for migrations and supported settings changes, and [public deployment](PUBLIC.md)
for initial setup or changes to deployment identity.

CI exercises this path using the real release helper, public templates and an
already built production image on a disposable Linux host. It checks successful
replacement and startup-failure rollback without restarting PostgreSQL or Caddy.
See [release integration checks](../../docs/testing.md#app-only-releases-and-rollback)
for prerequisites and evidence limits. This is not a deployment or reboot test of
your VPS; measure the first controlled rollout before relying on its timings.

Use Docker Compose 5.6.0 for these release workflows, matching the provisioned
host and CI. Older versions can omit service `env_file` values from configuration
hashes and reject unchanged containers. CI verifies the same version for both
the runner user and the root-run release helper.

## Inspect local release metadata

The read-only [operations policy tool](../README.md) validates the current
`release.json` schema and prints a diagnostic projection plus the original file's
SHA-256 and byte count:

```sh
cargo run --locked -q -p simplestchat-ops -- release manifest --path /path/to/release.json
```

This validates metadata only; it does not authenticate an image archive or
signature and cannot authorize deployment. It starts no host operation.
Application staging and routine app-only releases retain their Python
runtime. Prepared-host maintenance uses the separately authenticated Ops
executable described below; it is not part of the production image.

The same tool can inspect the original uncompressed image archive against that
validated manifest without extracting, loading or running it:

```sh
cargo run --locked -q -p simplestchat-ops -- release archive \
  --manifest /path/to/release.json --path /path/to/image.tar
```

Its fixed diagnostic projection includes the original archive SHA-256 and byte
count, config SHA-256, revision, tag and platform. It omits the image config,
which may contain private environment values. This checks transfer integrity
and the existing Docker/OCI metadata policy; it does not authenticate signatures,
validate filesystem-layer contents independently or authorize deployment. The
[archive domain and resource limits](../README.md#offline-image-archives) are
explicit. Diagnostic inspection does not change the selected runtime route.

## Browser assets across replacement and rollback

The managed proxy serves the selected app first. If that app returns 404 for an
exact fingerprinted public `/assets/<name>` GET or HEAD, Caddy may serve the
verified retained file. This keeps an open tab's deferred JavaScript imports
available through app replacement and rollback, including rollback to an app
that predates retention. HTML always revalidates with `no-cache, no-store,
must-revalidate`; `/app-version.json` is uncached selected-revision metadata.
The browser offers an explicit update action and does not reload an active call.

Install the stable proxy capability once on an existing site through the
[direct maintenance workflow](#schema-and-configuration-changes-the-maintenance-release),
setting the typed boolean `scpub_browser_assets_upgrade` to `true`.

This choice stages the candidate Compose and Caddyfile, compares the Caddyfile with
the exact release Git object, validates it using the already pinned running
Caddy binary, and permits only the fixed readonly public-store mount in the
resolved Compose change. The app and proxy stop for the normal maintenance
launcher; PostgreSQL is retained. Initial provisioning already installs this
capability. Later app-only releases retain the same proxy container. An old
proxy profile rejects app-only deployment with an upgrade instruction rather
than silently restarting Caddy. The signed ten-file release artifact format is
unchanged, and no additional image build is needed.

Before any interruption, the helper captures both selected and candidate assets
from never-started, isolated containers of their verified content-addressed
images. A bounded uncompressed `docker cp` tar reader accepts only flat
fingerprinted JS, CSS, fonts and images, rejecting links, duplicate/traversal
paths, HTML, JSON and source maps. Each public file is SHA256- and size-bound;
different bytes at an existing immutable URL fail deployment. The root-owned
store is `/srv/simplestchat-public/browser-assets`, mounted readonly only into
Caddy. Its `public/assets` pointer is relative and resolves to a validated union
inside that store. Account responses, credentials and private uploads never
enter it. Existing selected assets are captured on the first upgrade too.

Retention is bounded by four generations, 256 files per generation, 8 MiB per
file and 32 MiB of stored generation bytes. Selected and rollback generations
are preserved at publication or the release fails before stopping the app.
Other generations may be evicted sooner than seven days when count or size
limits are reached. The expiration timer runs each minute, removing expired
assets even without another deployment, on the next successful serialized minute
pass after the seven-day age limit. Active releases can delay that pass; host,
timer or unfinished-operation failures can delay it further and require operator
recovery. The age setting is not a strict serving deadline when cleanup is blocked.
The small public union uses hard links to validated bytes rather than another
full copy. Publication may temporarily retain both old and new bounded unions.

The image selection boundary publishes the selected revision independently from
candidate asset capture; rollback restores that metadata while retaining both
editions' immutable files. Retention handles asset delivery, not arbitrary
cross-version API or database compatibility, and tabs older than the retained
set may still need the explicit update action.

The helper settles content before changing the index and relative public
pointer. These two renames are not a single filesystem transaction. Ordinary
pre-switch failures clean only their own new files; a crash or storage failure
can leave owned pending directories, old unions, or an index/pointer mismatch.
Serving from the last switched union can continue, but validation deliberately
blocks subsequent publication instead of adopting or deleting ambiguous state.
Preserve the entire store, release attempt evidence, unfinished-operation journal
and expiration service logs. Resolve any owned container/process first, then
rebuild a fresh validated store from the explicitly selected and rollback image
identities during maintenance, retaining the old store for inspection. Do not
delete unknown directories or guess which index to trust. The expiry unit uses
the same workload lock and refuses unfinished operations.

Hosted release integration keeps an old Chromium document open through two
different immutable fixture images, loads its deferred chunk after replacement,
then loads the other edition's chunk after rollback. It proves direct backend
404s and successful proxy delivery, unchanged document identity, uncached HTML,
selected revision metadata, zero capture calls, and database/proxy continuity.
The fixture changes only public test files and image labels, using the production
binary and pinned proxy. This is hosted evidence; it is not a production probe.

## Routine update: use the image CI tested

Run the focused checks relevant to the change, then push the clean committed
`main` revision directly under the
[main branch policy](../../docs/security.md#main-branch-rules). A pull request is
optional. The [complete local CI gate](../../docs/testing.md#run-ci-locally)
remains available; running it before every push is not required. From a clean
checkout at that exact published revision, run:

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

The controller needs Python 3.12+, authenticated GitHub CLI, Ansible, curl,
and trusted SSH access. Select exactly one inventory host; use `--limit` if needed.
Its `scpub_domain` must match `--origin` so verification targets the deployed site.
The default CI wait is one hour; `--wait-seconds` changes that bound. Changed
checkout state, failed CI, expired/missing artifacts, or mismatched helpers stop
the command before deployment. This default path requires the current trusted CI
attestation. The explicit force path below admits a local build without it.

Prepared-host checks are enabled by default. After reviewing helper changes,
`--install-helpers` explicitly installs the current helpers instead of requiring
an exact existing match. It does not run host provisioning or database migrations.

The existing release playbook transfers and validates the image while chat stays
online, takes the backup, and replaces only the application. After readiness,
the command makes anonymous HTTPS GETs to `/health`, `/ready`, and `/`; it does
not join guests or write chat messages. `--public-chat-smoke` explicitly opts into
the Node 22.12+ smoke that joins two guests and writes one labeled message in
`--room` (default `lobby`). Do not select that option without authorization to post.
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

## Explicit force deployment

When the operator explicitly requests deployment without waiting for CI, use:

```sh
python3 build/deploy.py \
  --inventory ops/ansible/inventory.local.yml \
  --repository OWNER/REPOSITORY \
  --origin https://chat.example.com \
  --force --quiet-seconds 0
```

`--force` builds the clean current commit off-host through the existing production
Dockerfile and `build/build-release.py` implementation. Docker/Buildx must already
be available on the controller; the command does not start a VM or install tools.
It uses the builder's caches and bounded one-hour build/export deadline. To reuse
an already built exact-HEAD export, add `--artifact-dir results/YOUR-RELEASE`.
That option is rejected without `--force`. On first use or after reviewed helper
changes, add `--install-helpers` to install the complete force helper set.

Force skips CI waiting and GitHub attestation verification only. A clean checkout,
matching repository/revision, source input and migration hashes, successful build
evidence, archive digest, production image identity, and packaged migrations are
still required. The host rechecks the transferred bytes and uses the same workload
lock, checked backup, readiness, application rollback, and database/proxy continuity
checks. Schema and configuration changes require the separate
[direct maintenance workflow](#schema-and-configuration-changes-the-maintenance-release),
which requires a signed Ops artifact. The frozen wrapper
`--force --maintenance` route does not supply its required inputs.
App-only `--force` supports unpushed commits. No failed operation is retried
automatically.

Controller and host outcomes record `forced: true`, `githubAttested: false`, and
`ciVerification: skipped-explicit-force`. This does not certify CI, image scanning,
or signature verification. The host retains the authorization and exact build
evidence alongside the canonical release and in its private operation directory.
Existing release bytes are immutable: a different local or CI build of the same
revision is refused. Use a new reviewed commit for a later differing signed build;
never overwrite retained artifacts or relabel the forced deployment as signed.

## Trusted artifact requirements

The default signed deployment artifact is produced only by `.github/workflows/ci.yml` on a
`push` to `refs/heads/main`. Its signing job waits for the release smoke and all
required CI, security and CodeQL jobs. Pull-request caches or artifacts are never
promoted. The separate manual image-build workflow is useful for diagnostics;
its output has no deployment eligibility, even if another run passed for the
same commit. Local `build/build-release.py` output requires the explicit force path
above; it does not satisfy the trusted signer contract.

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
Check mode still verifies the signed artifact on the controller, downloading it
there unless `scpub_release_verified_directory` supplies an existing candidate.
It performs read-only host/helper checks but skips transfer to the host, image
staging and deployment. Normal prepared mode still stages only unless deployment
is explicitly enabled.

## Deploy explicitly

Repeat the command above with `-e scpub_release_deploy=true`. Alternatively, use
the exact staged 40-character commit on the VPS:

```sh
sudo systemd-run --unit=simplestchat-app-release \
  --property=Type=exec --property=RuntimeMaxSec=1800 \
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
[`xtask public-smoke`](../../docs/testing.md#manual-public-protocol-smoke) check writes one labeled
public message and verifies two guest connections; it does not test media.

## Schema and configuration changes: the maintenance release

An app-only deployment refuses a candidate whose packaged migrations differ from
the database ledger. For migrations, runtime grants, supported settings changes
or the browser-asset proxy transition, use the direct playbook sequence below on
an already provisioned public host. Run one release, helper update or maintenance
operation at a time. Deployment identity changes still require the separate
[public deployment workflow](PUBLIC.md).

This is a breaking maintenance caller change. The frozen `build/deploy.py` /
`build/release_deploy.py` wrapper supplies none of the new Ops inputs; its
`--maintenance` and `--force --maintenance` routes cannot drive this contract.
Use the direct playbooks below. Routine app-only and explicit force app-only
releases keep their existing entry points. This maintenance path requires a
signed Ops artifact and has no unsigned force bootstrap fallback. Existing
Python remains in application staging, Ansible and retained reference helpers.

### Prepare the controller before credentials

Use the caller-trusted checkout and pinned toolchain. Build the default `xtask`
before obtaining or providing GitHub credentials. For an arm64 macOS controller:

```sh
/usr/bin/env -u GH_TOKEN -u GITHUB_TOKEN cargo build --locked -p xtask --bin xtask \
  --target aarch64-apple-darwin --target-dir target/native-maintenance-controller
```

On an x86_64 GNU/Linux controller use `--target x86_64-unknown-linux-gnu` instead.
The resulting executable is
`target/native-maintenance-controller/<target>/debug/xtask`; supply its absolute
path below. The explicit target directory avoids relying on an older default
build. The caller trusts the checkout, compiler and resulting controller; the
playbook does not establish their provenance.

Select an absolute caller-trusted GitHub CLI **2.102.0** executable whose canonical
basename is `gh`. Its version check is separate from trusting the client binary.
Put that executable's directory first on `PATH` for application staging. After
the source build, authenticate it with `gh auth login --hostname github.com`, or
provide `GH_TOKEN` to the playbook. The maintenance caller uses `GH_TOKEN` when
supplied and otherwise reads the user's token through the selected `gh` client;
it never sends that token to the host. Existing controller Python and pinned
[Ansible dependencies](README.md) are still required.

### Select both signed artifacts explicitly

Choose one reviewed revision and its successful main-push CI **run and attempt**.
Select the application and Ops artifact IDs from that exact run, rather than
using the latest artifact or treating the two IDs as interchangeable:

```sh
gh api repos/swiftraccoon/simplestChat/actions/runs/CI_RUN_ID/attempts/RUN_ATTEMPT \
  --jq '{id, run_attempt, head_sha, event, conclusion}'
gh api repos/swiftraccoon/simplestChat/actions/runs/CI_RUN_ID/artifacts \
  --jq '.artifacts[] | {id, name, expired}'
```

The signed application name is `simplestchat-production-REVISION`; the separate
Ops name is `simplestchat-ops-REVISION-x86_64-unknown-linux-musl`. API listings
help select coordinates; the subsequent current signature verification is still
required. Re-run attempts are distinct identities even for the same commit.

Create an owner-readable `maintenance-vars.json` outside version control. Replace
every uppercase placeholder below before execution. IDs, `buildRunId` and
`runAttempt` must be unquoted JSON integers, never strings or booleans. The native
decoder preserves and checks those distinctions; the playbook does not coerce
them. The template uses the current workspace package version `0.1.0`; update it
only to the version actually declared by the selected checkout and signed Ops
distribution. The repository, protocol and target are the current supported
export profile.

```json
{
  "scpub_release_repository": "swiftraccoon/simplestChat",
  "scpub_release_artifact_id": APPLICATION_ARTIFACT_ID,
  "scpub_release_expected_revision": "REVISION_40_LOWERCASE_HEX",
  "scpub_release_ci_run": CI_RUN_ID,
  "scpub_release_deploy": false,
  "scpub_release_revision": "REVISION_40_LOWERCASE_HEX",
  "scpub_ops_xtask": "/absolute/controller/target/native-maintenance-controller/TARGET/debug/xtask",
  "scpub_ops_gh_executable": "/absolute/trusted/bin/gh",
  "scpub_ops_artifact_id": OPS_ARTIFACT_ID,
  "scpub_ops_selection": {
    "repository": "swiftraccoon/simplestChat",
    "revision": "REVISION_40_LOWERCASE_HEX",
    "packageVersion": "0.1.0",
    "protocolVersion": 1,
    "target": "x86_64-unknown-linux-musl",
    "buildRunId": CI_RUN_ID,
    "runAttempt": RUN_ATTEMPT
  },
  "scpub_browser_assets_upgrade": false
}
```

Keep the existing reviewed inventory and its host-key/SSH settings. The selected
revision must match in both contracts and contain this maintenance implementation.
Do not combine a newer caller with an older signed Ops executable lacking it.
The expected installed binary's digest and length are collected from the fixed
host path only as custody for a possible replacement; they do not authenticate
the old binary or establish its signing history.

### Stage, then run native maintenance

Stage the signed application while chat stays online:

```sh
ANSIBLE_CONFIG=ops/ansible/ansible.cfg \
  ops/ansible/.venv/bin/ansible-playbook \
  -i ops/ansible/inventory.local.yml ops/ansible/release.yml \
  -e @maintenance-vars.json
```

`scpub_release_deploy: false` is the existing stage-only interface; there is no
`build/deploy.py --stage-only` option. The release playbook independently verifies
the selected application artifact and its exact CI provenance. Omitting prepared
mode reconciles the existing release helpers without stopping chat. On later
attempts, add `-e scpub_release_prepared=true` only when those helpers match this
checkout; a mismatch refuses instead of silently reusing them. A successful
stage does not run maintenance.

Successful signed staging also retains the original verified
`release-predicate.json` beside the staged manifest, as root:root 0600.
An existing retained predicate must have the exact same bytes and private
profile; staging refuses a different run or attempt's predicate at the same
revision rather than overwriting it. Existing staging without this retained
predicate must be repeated with the selected signed application before maintenance.

Before exporting Ops, the maintenance caller joins the application variables,
retained predicate and actual staged manifest to the Ops repository, revision,
CI run and attempt. The predicate's `fileDigests['release.json']` must match the
published manifest's SHA256. These are passive original-byte and intended-identity
joins under the trusted controller/root/SSH keepers. The earlier completed signed
application verification and the current native Ops verification remain the
respective authentication authorities. Parsing retained JSON or a generic saved
success flag does not establish a fresh API observation or signature verification.
The application verifier still derives its selected attempt from the current CI
run; the retained signed attempt must equal the explicitly selected Ops attempt.

After staging succeeds, run the maintenance playbook directly:

```sh
ANSIBLE_CONFIG=ops/ansible/ansible.cfg \
  ops/ansible/.venv/bin/ansible-playbook \
  -i ops/ansible/inventory.local.yml ops/ansible/maintenance.yml \
  -e @maintenance-vars.json
```

For the one-time legacy browser-asset proxy transition, set the JSON boolean
`scpub_browser_assets_upgrade` to `true`. It stages only the fixed reviewed
Compose/Caddy changes and keeps the selected dependency images. Leave it false
for ordinary schema or settings maintenance; it permits no arbitrary proxy change.

The playbook invokes the source-built Rust exporter synchronously and requires
its actual exit zero before consuming its current receipt. Opaque authenticated
acquisition verifies the exact signed Ops revision/run/attempt/artifact and
produces a pinned three-member transfer plus an original installer request.
Trusted Ansible/SSH transfers these into a fresh private host nonce directory.
It joins the received tar's hash and length before parsing, then joins the
candidate's hash and length before making it executable. The authenticated
candidate's native `distribution install` or `distribution upgrade` must finish
successfully before the fixed canonical executable runs. Native installation
owns the input, expected-old, workload-lock, journal and canonical-publication
checks. A JSON success flag or saved receipt never substitutes for this current
verification and actual process result. These separate transport/stat/chmod/exec
calls do not claim retained-descriptor atomic execution against a hostile root.

The existing tasks obtain the exact source, render a protected candidate
environment from current host facts and secrets, and render runtime grants.
The final transient unit invokes
`/usr/local/libexec/simplestchat-public/simplestchat-ops release maintain` with the
exact revision, candidate environment, explicit proxy choice and **1800/60**
whole/finalization seconds. Export has its own original 1860-second profile;
bootstrap and staging are separate operations. These are operation limits,
not a measured downtime or hard wall-clock guarantee. Slow preparation can
refuse before interruption when the source-derived first-stop debt no longer
fits; retained stage or delivery evidence may already exist.

Maintenance admits the applied migration checksum subset, including holes,
then requires the exact candidate ledger after the native launcher. It validates
configuration identity and settings, retains browser assets, publishes a listed
live backup and receipt before stopping the app, then stops the proxy. Listing
the dump is not a restore test. It durably publishes the new selection and
candidate configuration, runs the native launcher under the original owner,
then reacquires the actual workload lock for final readiness, selected image,
unchanged database container and exact ledger checks. `environmentChanges` in
the outcome lists admitted changed setting names. See the
[native maintenance contract](../README.md#explicit-prepared-host-maintenance)
for deadlines and ownership details.

A failed launcher keeps the selected state for inspection; the schema may have
changed, so maintenance starts no application rollback. Do not rerun against
stopped services: preflight expects a live application. Inspect the original
release and launcher outcomes, selected configuration, migration ledger and
retained migration container. After resolving the recorded cause and establishing
that no migration is still running, use the explicit bounded launcher command in
[PUBLIC.md](PUBLIC.md#deploy-explicitly) to finish that selected deployment.
Preserve the evidence; deleting journals or blindly removing containers does not
establish a safe retry. Later recovery cannot turn the original failure into a
successful maintenance outcome. A nonzero final process exit invalidates an
earlier complete report.

A settings-only maintenance can reuse the selected image with validated changes.
An identical image and unchanged configuration are refused before backup or
interruption. `backupMigrations` records the applied ledger before migration
separately from the target revision; restore verification requires that recorded
ledger and never substitutes the target schema.

### Retained delivery evidence and sign-in impact

The caller deliberately retains the new private controller export and the
root-only host bootstrap nonce directory on success and failure. Paths are
reported after successful installation; the original controller request also
records the remote nonce for earlier failures. Each successful remote bootstrap
payload is below 514 MiB: tar at most 257 MiB, extracted binary at most 256 MiB,
manifest at most 64 KiB, selection at most 16 KiB and installer request at most
16 KiB. This is a per-attempt payload bound, not an aggregate disk quota, a bound
on transient Ansible copy staging or a retention lifetime. Repeated attempts keep
separate nonces. The host path is beneath `/run` and may disappear at reboot;
copy needed evidence into protected operational storage before then. Application
staging, backups and other controller evidence require additional disk space.
There is no automatic bootstrap cleanup or owned native retirement command yet.
Do not substitute recursive unknown-path deletion for such an owner.

Migration `022_require_current_sessions.sql` deliberately deletes existing
refresh sessions before requiring a nonnullable refresh-family hash. Every
signed-in client must sign in once afterward. Accounts, passwords, passkeys,
recovery keys and memberships remain intact. Older refresh formats and
sessionless access tokens have no compatibility parser. Review that impact with
the pre-release backup; do not edit published migrations 010 or 013 to avoid it.

The managed application and migration services retain the image's audited
`/app/simplestChat` command, non-root user and working directory. Release selection,
the initial deployment launcher and reboot preparation reject alternate commands,
healthchecks, lifecycle hooks, unreviewed mounts and dynamic-loader overrides.
`LD_*`, `OPENSSL_*`, `GLIBC_TUNABLES`, `GCONV_PATH` and `LOCPATH` are forbidden even
when empty. The additional application mount is the readonly PostgreSQL socket;
`/tmp` stays bounded and `noexec`. Validators inspect the resolved Compose model,
including environment files, and retain fixed diagnostics instead of values.
These restrictions define the supported managed profile; an administrator's
separate command or modified container is outside its applicability evidence.


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
It also records existing managed TURN, Prometheus and node-exporter container
identities and their running state. Recovery starts only unchanged containers
that were running before maintenance; absent or intentionally stopped companions
are not started. Their state is part of the private durable reboot journal.
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


### Pinned Docker runtime upgrades

For a reviewed Docker/containerd/runc/Compose/Buildx update on an existing public
host, use
`reboot.yml -e scpub_reboot=true -e scpub_upgrade_runtime=true`. Update the exact
inventory package and upstream binary pins first; local inventory overrides group
defaults. The candidate Compose executable must match its separately pinned official SHA-256.
Before stopping services, the playbook compares its resolved configuration and
all three service hashes against the installed Compose. Any difference fails
before maintenance and requires a separate configuration migration.

After preparation saves container identities and gracefully stops the public
services, the existing signed apt workflow installs the exact packages with
service auto-start disabled. Where that package channel lags, authenticated
upstream
containerd 2.4.1 and runc 1.5.2 binaries live under a versioned root-controlled
`/usr/local/lib/simplestchat-runtime/` directory. A managed containerd service
drop-in selects the executable and matching shim/runc search path; unknown service
overrides are refused. Buildx 0.37.2 uses Docker's standard global CLI plugin
directory, ahead of the older packaged plugin. Archive and executable SHA-256s
remain pinned in `group_vars/benchmark_hosts.yml`; the Docker service and socket
retain their packaged Unix-socket configuration. One explicit reboot activates
the runtime; the running engine, containerd, runc, Compose and Buildx version
checks precede the existing recovery/readiness checks. Containers, volumes and
selected application/dependency images are retained. A failure is
still a failed maintenance operation even if same-boot recovery succeeds.
This interrupts active chat/media and also reboots the host's TURN/monitoring
services. App-only releases do not upgrade Docker or reboot the host.
