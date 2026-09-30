# Bootstrap a private benchmark VPS

`build/bootstrap.py` enrolls access to one explicitly selected host and invokes
the canonical `site.yml` playbook. It supports fresh Debian 13 x86_64 hosts with
systemd, including providers that require changing the initial SSH password.
It also supports rerunning against an already enrolled host without reading or
rotating its password. It does not deploy the public application, change SSH
server policy, remove existing authorized keys, or run capacity tests.

Use a clean, committed checkout and the pinned controller environment from
[README.md](README.md#prepare-a-host). The controller uses the system OpenSSH
tools and Python's standard POSIX terminal APIs; no password automation package
or SSH agent forwarding is required. The source revision must equal the clean
controller checkout's full `HEAD`, because Ansible installs helpers from that
checkout as well as checking out the selected revision on the VPS.

## Establish host trust

For a host not already in the selected `known_hosts` file, obtain its Ed25519
SHA256 fingerprint independently through the provider console or another
trusted channel. For example, the provider console can run:

```sh
ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub -E sha256
```

Supply that fingerprint with `--host-fingerprint`. The controller compares the
key returned by `ssh-keyscan` to the supplied fingerprint before saving it.
Scanning alone never establishes trust. Existing trusted entries can be reused
without the flag; if supplied, the fingerprint must match an existing entry.
Host-key checking remains strict. Conflicting keys require explicit operator
investigation; the controller does not remove or replace them.

The default trust file is `~/.ssh/known_hosts`; `--known-hosts` can select a
dedicated file. Existing files must be owned by the controller user and must
not be writable by the group or other users. Publicly readable mode `0644` is
valid for host trust data. The parent directory must already exist and be owned
by the controller user without group/other write permission.

## First setup

The example uses placeholders for an explicitly selected test host and its
independently verified fingerprint. The identity, inventory and evidence paths
must be unused for a first setup; the existing `~/.ssh` and `results` parent
directories must be present.

```sh
ops/ansible/.venv/bin/python build/bootstrap.py \
  --host test.example.net --user debian --name test_vps \
  --revision "$(git rev-parse HEAD)" \
  --identity "$HOME/.ssh/simplestchat_test_ed25519" \
  --known-hosts "$HOME/.ssh/known_hosts" \
  --host-fingerprint 'SHA256:REPLACE_WITH_VERIFIED_FINGERPRINT' \
  --replacement-password-file "$HOME/.ssh/simplestchat-test-password.json" \
  --inventory ops/ansible/inventory.local.test.yml \
  --output "results/bootstrap-test.$(date -u +%Y%m%dT%H%M%SZ)" \
  --provision --initial-maintenance
```

If the selected private key is absent, the controller creates an Ed25519 key
with no passphrase for unattended automation. Its private file is mode `0600`
outside the checkout. Existing keys are verified and reused, never overwritten;
encrypted keys are not supported by this unattended identity path. Protect the
controller account and private key as host administration credentials.

The controller tries the key first. Only if that fails does it request the
initial password through a non-echoing terminal prompt. For unattended input,
`--password-file` accepts a mode `0600`, caller-owned regular file outside the
checkout. The file can contain one password or a JSON object with a `password`
string. Passwords cannot be supplied as command-line values or environment
variables. Do not put them into an inventory, shell history, or a command that
prints the file.

Before attempting password authentication, the controller creates the selected
replacement-password file exclusively, mode `0600`, outside the checkout, or
reuses an existing protected file. That recovery copy exists before any forced
change begins. The supported dialogue is the English OpenSSH/Debian password
exchange, with bounded time, output and prompt counts and terminal echo checked
before each secret write. Unsupported prompts fail with a fixed error code;
remote authentication transcripts are neither logged nor printed.

Debian may close the connection immediately after successfully changing the
expired password. In that case the controller reconnects once with the saved
replacement password and enrolls the public key. A failed authentication is not
retried in a loop. If a connection fails during rotation, preserve the saved
replacement file and use the provider console to establish whether the old or
new password is active before retrying. A replacement file's existence alone
does not prove that the remote password changed.

Enrollment appends the selected public key only if it is absent. Existing
authorized keys remain intact; symlinked SSH paths are rejected. The controller
then verifies a fresh key-only connection. It leaves password login and SSH
server configuration unchanged.

## Preflight and provisioning

The read-only preflight records the OS, architecture, CPU count, kernel, disk
space, Python version, Docker presence and passwordless-sudo availability. It
requires Debian 13 x86_64 with systemd and at least 20 GiB free on the `/srv`
filesystem, or `/` if `/srv` is absent. `--minimum-free-gib` can explicitly change
that planning threshold. It is a reserve for build inputs, layers and evidence,
not a guarantee that an arbitrary image build will fit.

When noninteractive sudo is available, preflight also refuses a busy canonical
workload lock, an unfinished `current.json` ownership record, or an active image
build/benchmark service. Capacity runs use the same ownership record. The
canonical playbook independently checks active services, unfinished ownership,
and a running public deployment before mutations. Use a dedicated benchmark
host and do not start other workloads during provisioning.

`--provision` invokes the existing `site.yml` for the exact inventory name.
`--initial-maintenance` supplies package-upgrade and reboot flags only as
one-run extra variables. The generated inventory always retains both flags as
`false`; there is no manual reset step. The playbook reboots only if its package
upgrade actually changes packages, and installs the reviewed pinned Docker
versions. It does not install host Rust/Node toolchains or start image builds.
Missing package pins or conflicting runtime configuration remain errors.

For a user whose sudo requires a password, the controller securely prompts or
accepts `--become-password-file` under the same private-file rules. It creates a
temporary owner-only password file outside the checkout for Ansible's supported
`--become-password-file` interface and removes that file afterward. No password
value appears in Ansible arguments, environment, inventory or retained logs.
Privileged busy-state inspection then occurs in the canonical playbook after
sudo authentication.

Omit `--provision` to enroll access, run preflight and create/validate the
inventory without installing packages or changing the host configuration.

## Rerun and advance the source revision

Use the same target, identity and inventory with a new evidence directory and
the clean checkout's explicit revision. A working key bypasses all password-file
reads, password generation and rotation. Do not include
`--initial-maintenance` during ordinary source advancement.

```sh
ops/ansible/.venv/bin/python build/bootstrap.py \
  --host test.example.net --user debian --name test_vps \
  --revision "$(git rev-parse HEAD)" \
  --identity "$HOME/.ssh/simplestchat_test_ed25519" \
  --inventory ops/ansible/inventory.local.test.yml \
  --output "results/bootstrap-test.$(date -u +%Y%m%dT%H%M%SZ)" \
  --provision
```

Existing inventories must select one matching host and use the expected SSH
identity/trust settings. They are validated and preserved byte-for-byte,
including provider sizing fields. The old stored source pin is allowed: the
explicit CLI revision takes precedence for this invocation and is recorded in
the evidence. Unexpected transport overrides, other groups or enabled stored
maintenance flags are rejected rather than rewritten.

Continue with [automated private capacity measurements](CAPACITY.md) to prepare
the selected revision, build immutable images, run bounded measurements and
collect their retained evidence. The capacity controller provides the build
start/wait workflow; bootstrap intentionally ends after access/provisioning.

## Evidence and recovery

Each invocation creates a private, new evidence directory. It retains
`plan.json`, non-secret access status, `host-before.json`, and `outcome.json`.
Successful provisioning also records `host-after.json`; Ansible's command,
output and process outcome are retained under `provision/`. Password dialogues
and password values are excluded. A failed operation never overwrites a prior
attempt's evidence, and the controller does not automatically retry a failed
package transaction, interrupted reboot or uncertain remote mutation.

Inspect the retained operation and host state before retrying. An SSH timeout
or a terminated controller is not proof that remote package work stopped.
Likewise, never remove an unfinished ownership record or a workload lock to make
provisioning proceed. No test host should share a production database.

The automated test suite uses fake SSH/Ansible boundaries, disposable local
files and real local PTYs. It covers forced password rotation, reconnects,
terminal echo refusal, time/output limits, secret-free arguments, key-only
reruns, host pinning, key preservation and inventory revision overrides. It
does not claim provider-specific authentication or live provisioning coverage.
