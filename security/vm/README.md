# Disposable deployment validation

`build/security_vm.py` applies the maintained Ansible configuration to one new,
owned Debian 13 QEMU/KVM guest. It takes a canonical immutable application release
for the exact clean checkout revision. It never accepts a remote host, existing
inventory, provider account, arbitrary guest command, or substitute application
image. The manual/scheduled CI workflow verifies release provenance before
invoking this helper; the helper independently verifies the release manifest,
archive checksum, image layout and source revision.

## What a passing run proves

The controller applies `ops/ansible/site.yml` with the `host,docker,benchmark`
tags twice. It requires the second recap to contain zero changed tasks, failed,
rescued, ignored or unreachable tasks. Source cloning is excluded because the
application arrives as an immutable release. The fixture staging playbook copies
the unchanged canonical release helpers and invokes their real staging operation.

It applies `public.yml` twice before starting services. Independent guest checks
compare protected configuration hashes between applications, check exact file
owners/modes and independent generated secrets, inspect the effective Compose
configuration, and check the installed benchmark/image-build units remain
inactive. The actual containers must use the expected nonroot users, read-only
roots, dropped capabilities, no-new-privileges and finite CPU/memory/PID limits.
Only application HTTP is published on the guest loopback address. PostgreSQL and
the temporary migrator use their real isolated network configuration.

The real application image runs every packaged migration against a new PostgreSQL
database. The fixture installs the maintained runtime grants and operations
schema, adds one synthetic account and room plus two synthetic operational
incidents, then starts the application and requires `/ready`. It applies
`backup.yml` twice, validates the effective systemd service sandbox and enabled
timer, runs an actual local backup, verifies its private receipt and complete
migration ledger, and uses the canonical isolated restore verifier. Restored
aggregate counts and verified cleanup must match. The timer is paused only while
serializing the explicitly requested fixture backup and restored afterward.

This covers the selected configuration, migration, application readiness and
local backup/restore paths. It does not validate source-clone access, external
DNS, Caddy/ACME/TLS issuance, TURN, WAN connectivity, media capacity, monitoring
installation, provider firewalls, offsite storage, or a full public release
maintenance transaction. Caddy is configured but never started. There are no
active vulnerability probes or generated traffic workloads.

## Requirements and invocation

Use a fresh Linux amd64 runner as an unprivileged user with read/write access to
`/dev/kvm`, at least 5 GiB available memory and 24 GiB free disk. The controller
requires real KVM and fails explicitly when it is unavailable; emulated or mocked
guest execution cannot produce a passing result. On an Ubuntu GitHub runner the
workflow prepares these packages and grants only its current user access to KVM:

```sh
sudo apt-get update
sudo apt-get install -y qemu-system-x86 qemu-utils cloud-image-utils openssh-client curl acl
sudo setfacl -m "u:$(id -un):rw" /dev/kvm
```

The workflow removes the unused preinstalled Android and .NET SDK directories
from its fresh GitHub runner before installation. The standard runner's
[documented storage allowance](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)
does not guarantee this fixture's required free space. The helper still measures
available space and refuses to start the guest below 24 GiB; SDK cleanup is not
a substitute for that check. This cleanup belongs only to the disposable CI
runner and is not a local workstation prerequisite.

Use the repository's hash-pinned Ansible environment (`ops/ansible/requirements.txt`)
and a supported controller Python (3.12–3.14). GNU `timeout` and `prlimit` must be available. Both inputs below
must be absolute paths; the output directory must not already exist. Keep the
release and evidence outside the checkout so the clean-tree precondition remains
meaningful:

```sh
ops/ansible/.venv/bin/python build/security_vm.py \
  --artifact-dir "$RUNNER_TEMP/verified-release" \
  --output "$RUNNER_TEMP/disposable-vm-evidence"
```

The release directory must contain `release.json` and `image.tar` for the current
checkout revision. Use the maintained release build/fetch and attestation
verification flow to produce that directory. This helper does not acquire an
artifact or authenticate a GitHub signer itself. Archive and manifest validation
also runs before the guest starts, and again through canonical staging inside
the guest.

The dated Debian image and its complete upstream SHA-512 checksum are recorded in
[`debian-cloud.json`](debian-cloud.json), checked against Debian's published
[SHA512SUMS](https://cloud.debian.org/images/cloud/trixie/20260914-2601/SHA512SUMS).
The image is authenticated before QEMU parses it. Updating the pin requires
reviewing a new dated image and its published checksum. Guest package installation
uses the actual maintained signed Debian/Docker repositories and package policy;
the base-image pin does not make those repository contents a historical snapshot.

## Ownership, limits and evidence

Each run generates disposable SSH client and host keys and seeds the host key
before boot. SSH verifies that exact key, disables agents/proxies/multiplexing and
connects only to the owned guest's generated loopback forward. QEMU has no host
filesystem share or container-engine socket. Its user-mode network permits package
installation and immutable dependency pulls; the only incoming forward is SSH on
127.0.0.1. The guest additionally checks root execution, Debian/QEMU identity, a
root-only ownership marker and an exact run/revision selection before its actions.

Limits are 2 vCPUs, 3 GiB guest RAM, a 16 GiB overlay disk, 5 GiB process address
space, 3,600 CPU seconds, 1,024 file descriptors and 512 processes. The controller
has a 30-minute shared deadline, and an independent GNU timeout bounds QEMU to
30 minutes even if the controller is lost. Core dumps are disabled. Input cloud
images and release archives are each limited to 2 GiB; each command/VM output
stream is limited to 2 MiB, and there are at most 64 host commands. Prerequisite
installation and provenance acquisition belong to the separately bounded CI job.

Cleanup addresses only the exact unreaped child process group; it never searches
for VM names or stops unrelated containers. Successful cleanup removes the owned
overlay, seed, keys and inventory. A cleanup failure forces failure and retains
that private directory. SIGINT/SIGTERM follow the same cleanup path. An
uncatchable process/runner failure cannot publish success; the independent QEMU
timeout and preserved GitHub runner tracking identity provide additional process
lifetime bounds. No helper can guarantee a final receipt after an uncatchable
host termination.

Only `summary.json` is suitable for CI artifact upload. It contains source/image
identities, command exit statuses, Ansible recap counts and allowlisted aggregate
guest assertions. Raw SSH/Ansible/QEMU logs are bounded and private but may contain
fixture-only secrets; do not upload the complete evidence directory. No
production credential or SSH agent is forwarded into this run.

## Validation status

The focused Python tests exercise ownership, argument construction, strict
receipts, idempotence failures, filesystem metadata and lifecycle handling. Tiny
owned local processes test output limits and exact process-group cleanup. These
are orchestration tests, not evidence that Debian/KVM deployment succeeded.
Actual guest configuration and local restore must pass on the Linux CI job before
claiming deployment validation for a revision.

```sh
ops/ansible/.venv/bin/python -m unittest discover \
  -s ops/ansible/tests -p 'test_security_vm*.py'
ANSIBLE_CONFIG=ops/ansible/ansible.cfg ops/ansible/.venv/bin/ansible-lint \
  --offline --strict security/vm/stage.yml
```
