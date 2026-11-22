# Disposable deployment validation

`.github/workflows/infrastructure-security.yml` runs daily and on dispatch. Its Rust
controller, `simplestchat-release ops-artifact infrastructure-runtime`
(`crates/release/src/ops_artifact/maintain_runtime/infrastructure.rs`), applies the
production provisioning playbooks to one new, owned Debian 13 QEMU/KVM guest, stages
and checks the signed release of the checkout's own revision through the production
commands, and destroys the guest. It accepts no remote host, inventory, provider
account, arbitrary guest command or substitute application image. It shares its VM,
QMP, process, deadline and file owners with the three manual maintenance KVM proofs
(`maintain_runtime/{vm,qmp,process,control,files}.rs`).

The manual dispatch also offers `boot_only`, defaulting to `false`. That diagnostic
scope requires the same clean checkout, boots the same checksum-pinned image,
authenticates the guest's SSH host key, waits for cloud-init and verifies owned
cleanup. It selects and authenticates no release, runs no playbook and cannot
establish deployment, migration or restore success. Scheduled runs always take the
full scope. The job name distinguishes the two, and every summary records `bootOnly`,
`scope` and `fullDeploymentValidated`; a boot-only pass has `scope: "boot"` and
`fullDeploymentValidated: false`.

## What a passing run proves

The controller requires a clean checkout whose `HEAD` is the workflow's revision, then
selects that revision's one successful `ci.yml` push run and its application and Ops
artifacts with the production `release deploy` selection (waiting at most 900 seconds
for CI), and authenticates both: the Ops artifact with `ops-artifact acquire` and
`verify-retained`, the application with `release verify` and `release attestation
verify` of the runner-side GNU `simplestchat-ops`. Then, in the guest:

1. `ops/ansible/site.yml` with the `host,docker,benchmark` tags, twice. Every run must
   report one recap for the generated host with something done and nothing
   unreachable, failed, rescued or ignored; the second must change nothing. Source
   cloning is excluded because the application arrives as a signed release.
2. The production delivery of the signed Ops executable:
   `ops/ansible/security-vm-ops-install.yml` imports `ops/ansible/tasks/signed-ops.yml`
   unchanged, so the guest's installed `simplestchat-ops` is the signed musl build the
   release uses, and the playbook requires its digest to be the one the controller
   authenticated.
3. `security/vm/stage.yml`: the authenticated `release.json` and `image.tar` are copied
   into the release store and staged by the installed executable's production
   `release stage`, through the same `systemd-run` properties `release.yml` uses.
4. `ops/ansible/public.yml` twice, each followed by the guest's `snapshot` check.
5. The guest's `database` check, then `ops/ansible/backup.yml` twice and the guest's
   `backup-restore` check.

The guest checks are the role `release infrastructure-guest ACTION --run-id RUN` of the
installed signed executable (`ops/src/release/infrastructure/`), run over SSH as root.
Each first requires root, Linux x86_64, Debian 13, the QEMU vendor, the private
fixture directory, the run's cloud-init marker and selection, the installed
executable as the running image and the root-owned Docker socket.

- `snapshot` checks every protected directory and configuration file for its exact
  owner, mode, single link and size, the generated secrets for their schema and
  independence, the rendered application, migration and proxy environments for the
  fixture's exact values, the inactive benchmark and image-build units for their
  identity and private runtime directory, an empty public project, and the effective
  Compose model: the four services, their non-root users, read-only roots, dropped
  capabilities (only Caddy may bind low ports), no-new-privileges, bounded memory,
  processes and CPUs, no network for PostgreSQL and the migrator, an init process and
  loopback-only application HTTP. The second snapshot must find the first's record
  unchanged.
- `database` requires both snapshots, the staged release's manifest, recorded images
  and staging record to agree on the revision and the inspected image, and an
  immutable database image; then it starts PostgreSQL, runs the real migrator until
  the live ledger is the manifest's, removes exactly that container, installs the
  runtime grants and the operations schema, adds one account, one room and two
  operational incidents, requires `restore-verify.sql` to count exactly them, starts
  the application, requires `/ready`, and checks every container's actual identity,
  isolation and limits; Caddy stays absent.
- `backup-restore` checks the backup target, script, units, the service's effective
  sandbox and the enabled timer, pauses the timer, starts the installed backup service
  once, requires exactly one new receipt whose archive, database image and recorded
  migrations are the fixture's, restores it in the isolated restore verifier
  (`simplestchat-ops`'s own, on memory filesystems, no network), requires the same
  counts and its cleanup, and restarts the timer.

This covers the selected configuration, the signed delivery, staging, migration,
application readiness and the local backup and restore paths. It does not validate
source-clone access, external DNS, Caddy/ACME/TLS issuance, TURN, WAN connectivity,
media capacity, monitoring installation, provider firewalls, offsite storage, or a
full public release transaction. Caddy is configured but never started. There are no
active vulnerability probes or generated traffic workloads.

## Requirements and invocation

The workflow prepares a fresh Linux amd64 runner: the pinned Rust toolchain, the
controller (`target/debug/simplestchat-release`) and the runner-side GNU
`simplestchat-ops` it verifies with, QEMU, `cloud-image-utils`, OpenSSH and `curl`, read
and write access to `/dev/kvm` for its user, the hash-pinned Ansible environment
(`ops/ansible/requirements.txt`) and the pinned `gh`, both first on `PATH`. It removes
the unused Android and .NET SDKs first; the controller still measures at least 24 GiB
free disk and 5 GiB available memory and refuses below them. Both scopes need these
tools: the controller retains each one before it starts. Only the full scope receives
`GH_TOKEN`.

```sh
target/debug/simplestchat-release ops-artifact infrastructure-runtime \
  --output "$RUNNER_TEMP/security-vm"
target/debug/simplestchat-release ops-artifact infrastructure-runtime --boot-only \
  --output "$RUNNER_TEMP/security-vm"
```

The output directory must not exist. The controller refuses to run outside the
workflow on a hosted `main` runner of this repository.

The dated Debian image and its complete upstream SHA-512 checksum are recorded in
[`debian-cloud.json`](debian-cloud.json), checked against Debian's published
[SHA512SUMS](https://cloud.debian.org/images/cloud/trixie/20261001-2618/SHA512SUMS).
The image is authenticated before QEMU parses it. Updating the pin requires reviewing a
new dated image and its published checksum. Guest package installation uses the
actual maintained signed Debian/Docker repositories and package policy; the image pin
does not make those repository contents a historical snapshot.

## Ownership, limits and evidence

Each run generates disposable SSH client and host keys and seeds the host key before
boot. SSH verifies that exact key, disables agents, proxies and multiplexing and
connects only to the owned guest's loopback forward. QEMU has no host filesystem share
or container-engine socket; its user-mode IPv4 network permits package installation
and image pulls, and the only incoming forward is SSH on 127.0.0.1.

Limits are 2 vCPUs, 3 GiB guest RAM, a 16 GiB overlay disk, 5 GiB process address
space, 3,600 CPU seconds, 1,024 file descriptors and 512 processes; the main loop's
disk I/O pool has at most eight workers and only QEMU's environment sets
`MALLOC_ARENA_MAX=2`, because guest RAM shares the address-space limit with thread
stacks and allocator arenas. The whole run has 54 minutes: selection and
authentication must end within 20, at least 30 then remain for the guest, and two are
reserved for cleanup; an independent `timeout` bounds QEMU to the run. Boot diagnosis
has 32 minutes. Core dumps are disabled. Each command's standard output and error are
limited to 2 MiB, there are at most 64 commands besides at most 32 CI polls, playbooks
have 600 seconds, guest checks 400. The pinned Debian image configures GRUB for
`gfxterm` and serial output, so QEMU keeps `-nodefaults` with one explicit VGA device
under `-display none`. A guest reset within the SSH retry budget is allowed; it is
never evidence of a successful boot.

Cleanup addresses only the exact owned QEMU process group; it never searches for VM
names or stops unrelated containers. A cleanup failure fails the run. SIGINT and
SIGTERM take the same cleanup path. The private work directory (disk, seed, keys,
inventory) stays in the runner's output directory, which only the runner's user can
read and which the workflow never uploads.

Only `summary.json` is uploaded. It holds the revision, the selected run, attempt and
artifact ids, the image digests, every command's name and exit status, each playbook
run's recap counts, the guest checks' aggregate receipts and, for a failed check,
only its four fixed labels (phase, context, code, exception class), the QEMU lifecycle
(`qemu`: exit and retirement, readers, QMP observations, and the cloud-init
projection), and for a failed startup before SSH `vmStartupDiagnostic`: the emulator's
exit status, a fixed classification of its first standard error lines and the serial
boot milestones seen. Each projection keeps only fixed labels, counts and digests;
command output, task names, paths, keys and secrets stay private. `failure` names the
step that failed, or a fixed refusal code, and `failureKind` its class.

## Validation status

The Rust tests exercise the controller's steps with scripted commands
(`maintain_runtime/infrastructure_tests.rs`), replay the recorded answers of the
retired Python controller for recaps, the cloud-init and startup projections, the
serial milestones, guest receipts, the inventory, the seed, the pin and the QEMU
command line (`infrastructure_oracle_tests.rs`), replay the retired Python checker's
answers for every guest decision (`ops/src/release/infrastructure/{model,policy}_tests.rs`)
and run each guest action over a scratch tree with scripted commands (`guest_tests.rs`).
The deployment CI job renders the real templates with the pinned Ansible, runs the
provisioned Compose and requires the guest's Compose check to pass
(`crates/ci/src/automation/pins/public_templates.rs`). These are not evidence that a
Debian deployment succeeded; only the workflow's own run is.

```sh
cargo test --locked -p simplestchat-release --bin simplestchat-release -- infrastructure
cargo test --locked -p simplestchat-ops --features maintain-guest --lib -- infrastructure
ANSIBLE_CONFIG=ops/ansible/ansible.cfg ops/ansible/.venv/bin/ansible-lint \
  --offline --strict security/vm/stage.yml
```
