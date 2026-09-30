# Disposable deployment validation

`build/security_vm.py` applies the maintained Ansible configuration to one new,
owned Debian 13 QEMU/KVM guest. It takes a canonical immutable application release
for the exact clean checkout revision. It never accepts a remote host, existing
inventory, provider account, arbitrary guest command, or substitute application
image. The manual/scheduled CI workflow verifies release provenance before
invoking this helper; the helper independently verifies the release manifest,
archive checksum, image layout and source revision.

The manual workflow also provides `boot_only`, defaulting to `false`. This
diagnostic scope boots the same checksum-pinned Debian image, authenticates the
new guest's SSH host key, waits for cloud-init and verifies owned cleanup. It
accepts no application artifact, skips release acquisition, inventory generation
and Ansible, and cannot establish deployment, migration or restore success.
Scheduled runs always retain the full signed-release path. The workflow job name
distinguishes boot diagnostics from deployment validation, and every summary
records `bootOnly`, `scope` and `fullDeploymentValidated`; a boot-only pass always
has `scope: "boot"` and `fullDeploymentValidated: false`.

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

For a boot failure, run the explicit diagnostic scope without waiting for an
application build; it still requires the exact clean source checkout and all
KVM, ownership and resource checks:

```sh
ops/ansible/.venv/bin/python build/security_vm.py \
  --boot-only --output "$RUNNER_TEMP/disposable-vm-boot-evidence"
```

`--boot-only` and `--artifact-dir` are mutually exclusive. The diagnostic mode
never accepts an unsigned or alternate application artifact. Its Python path
uses only the standard library; the workflow installs the pinned Ansible
dependencies only for a full deployment run.

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

The seed supplies its exact `ssh_keys` and omits `ssh_genkeytypes`. The pinned
[Debian cloud-init schema](https://sources.debian.org/data/main/c/cloud-init/25.1.4-1%2Bdeb13u1/cloudinit/config/schemas/schema-cloud-config-v1.json)
requires a nonempty list when that optional property is present; the supplied-key
path does not use generated-key types. Full schema validation of the generated
seed confirms this correction without changing the enrolled host key.

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

The pinned Debian cloud image configures GRUB for both `gfxterm` and serial
output. QEMU retains `-nodefaults` and supplies an explicit VGA device while
`-display none` keeps the guest headless. A bounded local TCG comparison of the
exact checksum-pinned image stalled at its GRUB Debian-selection banner without
VGA; changing only that device reached Linux and cloud-init. The baseline serial
bytes matched the KVM failure's retained digest. This supports the device
correction, but local emulation does not establish KVM boot success; the actual
Linux workflow must still authenticate SSH and complete cloud-init.

Only `summary.json` is suitable for CI artifact upload. It contains source/image
identities, command exit statuses, Ansible recap counts and allowlisted aggregate
guest assertions. Its `vmLifecycle` distinguishes the exact owned leader's exit
or signal observed before cleanup from the reaped cleanup return code, and records
a fixed drainer failure class and numeric I/O errno. Output failures discovered
during the final drain still fail validation even when resource cleanup succeeds;
an incomplete drain cannot publish success.

After SSH authentication, the controller requests `cloud-init status --wait
--format=json`. The pinned CLI emits clean JSON in this mode. Exit codes 0, 1 and
2 are captured before validation; success still requires exit 0, completed
healthy status, no active stage, all four stage records and zero fatal or
recoverable errors. JSON is limited to 64 KiB and rejects duplicate keys;
bounded error groups/messages yield only fixed status/stage labels, counts and
allowlisted diagnostic categories. Unknown warnings still count as errors.
`vmLifecycle.cloudInit` preserves that projection before a failure is raised;
malformed output retains only a fixed failure code and exit status. Raw error
messages, datasource details, names, keys and paths remain private.

Before the first authenticated SSH connection only, lifecycle evidence includes
stderr byte count, SHA-256 and a diagnostic projection of at most the first 8 KiB,
12 lines and 1 KiB per line. The projection reconstructs only fixed error classes,
OS reasons and emulator component names; it never copies input text. It covers
KVM, block/backing formats, sandbox, boot/device, memory/resource limits,
GLib/thread startup failures (including QEMU's `qemu_thread_create` fatal format),
and QEMU/libc or GLib assertion failures, including GLib's unreachable-code form.
Exact filename-and-line tokens can add one of four fixed component hints:
`thread-pool.c`, `qemu-thread-posix.c`, `async.c` or `gmem.c`. Assertion diagnostics
expose only fixed labels; source paths, line numbers, function names and assertion
expressions remain private.
Unknown, oversized or control-bearing lines are withheld, with truncation/count
metadata retained. Pre-auth guest serial stdout
contributes only a byte count, digest and fixed milestone labels for firmware,
disk boot, missing boot disk, GRUB, Linux, kernel panic, initramfs, reboot,
disk resizing, poweroff, cloud-init and SSH startup. Matching uses a 128-byte
overlap across bounded reads; no serial text or extracted field is published. These labels are
diagnostic observations, not authenticated guest health assertions. SSH
authentication clears buffered startup text and suppresses both stderr and
serial diagnostics before cloud-init or Ansible commands run.
The exact ``Booting `Debian GNU/Linux'`` selection banner is classified as GRUB,
not as evidence that the Linux kernel has started.

The controller also observes the owned QEMU instance through a private Unix QMP
socket in the generated directory (at most 100 path bytes). QEMU waits for this
connection; the observer connects within ten seconds and sends only the fixed
`qmp_capabilities` handshake. It sends no guest-control or arbitrary monitor
commands. Parsing is limited to 64 KiB, 128 newline-delimited messages and 8 KiB
per message, within the shared deadline. Public evidence retains connection,
negotiation and EOF flags, counts, fixed failure codes and only `RESET`/`SHUTDOWN`
events with allowlisted reasons and a boolean guest-origin flag. No raw QMP text,
paths, error descriptions or timestamps are published. Observer failures fail
validation; incomplete observer cleanup also fails the cleanup assertion.

Boot-only [run 36765241078](https://github.com/swiftraccoon/simplestChat/actions/runs/36765241078)
observed a `SHUTDOWN` event with `reason: "guest-reset"` on the owned QMP socket,
followed by process exit zero before SSH. QEMU's former `-no-reboot` option converts such a reset into a
[shutdown](https://www.qemu.org/docs/master/interop/qemu-qmp-ref.html#event-SHUTDOWN).
The controller now permits guest resets within the unchanged SSH retry budget,
shared deadline, independent process timeout and output/QMP bounds. It does not
restart a terminated QEMU process or infer why the guest reset. Boot acceptance
still requires authenticated SSH and successful cloud-init; allowing a reset is
not evidence of a successful boot or deployment.

[QMP events are unavailable before capability
negotiation](https://www.qemu.org/docs/master/interop/qmp-spec.html#capabilities-negotiation),
so an absent event cannot prove that no reset or shutdown occurred. The report
states this limitation explicitly. Boot milestones and QMP observations narrow
the diagnosis; they do not substitute for authenticated SSH and cloud-init.

Raw SSH/Ansible/QEMU logs are bounded and private but may contain
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
