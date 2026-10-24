# PID namespace topology prerequisite probe

This is an unexecuted, test-only Python reference prototype. Production maintain work will use
Rust. The prototype changes no installed caller, launcher, bootstrap or product timeout and has
no production authority. It executes no Docker command, server image, application, database or
real launcher.
Static checks cannot establish that Debian permits the required namespace and descriptor roles.

The only controller input is a fresh absolute output directory:

```sh
python3 -I -B build/maintain_namespace_probe.py --output "$RUNNER_TEMP/maintain-namespace-probe"
```

Run this in an unprivileged Linux amd64 environment with real local KVM and the existing
security-VM prerequisites. The driver reuses the existing pinned Debian cloud image validation,
private cloud-init keys, loopback-only authenticated SSH, QMP and actual QEMU finalizers. There is
no emulation, paid provider, remote host, image, command, case or timeout selector. The controller
has one 1800-second envelope, with body 1680/finalization 120. Every guest duration and 30-second
case ceiling is clipped from that original window; no case or handshake refreshes it. Kernel
close/reap latency has no hard wall bound: an observed late finish remains negative.

Review the actual observed timeout/unshare/kernel identities and matching Debian source
before closing this experiment. Latest man pages are research context, not binary authority. The
guest records SHA256/resolved path/version for timeout, unshare, bash and dpkg-query; it records
coreutils/util-linux/bash package versions and actual Debian 13/amd64 identity before the cases,
checks tool identities again after them, and samples the kernel release when building the final
report. These are observations, not proof that a package name identifies its bytes.

The private guest has fixed `run`, `init` and `leaf` roles. Only the controller runs `run`; it
creates fresh paired SEQPACKET channels and invokes the fixed PID1 supervisor through:

```text
/usr/bin/unshare --pid --fork --kill-child=KILL \
  /usr/bin/python3 -I -B <held-helper> init <fixed-frame>
```

There is deliberately no `--mount-proc`. The original host `/proc` mount must remain unchanged:
the operational launcher uses Docker host PIDs in `/proc/$pid/ns/net`. The probe opens only its
own known host-PID namespace handle and `/proc/self/fd` alias; it neither enters a foreign network
namespace nor executes Docker. It does not demonstrate the complete unchanged 900-second launcher.
The immutable Bash fixture preserves representative original timeout 180/kill 5 and timeout 15/kill 2
forms with inert leaf programs. It contains no externally supplied shell text or command.

Before GO, PID1 sends its original namespace FD and self pidfd with SCM_RIGHTS. The receiver checks
kernel SCM_CREDENTIALS, NS_GET_NSTYPE=CLONE_NEWPID and the pidfd's kernel NSpid mapping in receiver
namespace coordinates. It opens the actual sender's PID namespace through the original host-proc
directory and joins its device/inode to the received namespace FD. The lookup PID comes only from
the original pidfd's kernel record. Signal 0 and poll on that same pidfd bracket the lookup and
checked close. This independent join refuses a real self pidfd paired with a different namespace;
reported JSON PIDs never authorize the lookup. Python signal 0 and poll are prototype operations;
production Rust support for those operations remains a separate decision. Leaf/orphan
readiness transfers the same original kernel roles before its own GO. Reported JSON PIDs are
never opened as pidfds or adopted for signaling. Only an exclusive actual Child PID is used for
reserved-group signaling. Newly received rights become close-on-exec before another admission.

Leaf entry requires a descendant namespace, root, a live original deadline and a PID1-created
control socket. GO additionally carries kernel sender credentials for PID1. The namespace-wide
signal function checks getpid() = 1, a different original namespace and the unchanged proc mount
before every call. No outer controller path issues host `kill(-1)`.

The fixed catalog contains eleven named cases and fourteen fresh namespace trials:

- natural-zero: completed original wrapper exit 0 and actual EOF, closes and reaps;
- completed-nonzero: actual inert exit 7 remains observable;
- default-timeout-pgid: compare actual groups and original pidfd liveness after launcher-group TERM;
- term-during-inner: retire the nested group after signaling the reserved launcher group;
- terminal-leader-live-descendant: a session-changing orphan stays live after its leader exits;
- eof-with-live-descendant: actual stdout/stderr EOF cannot stand in for process settlement;
- ignore-term: bounded escalation retires an inert TERM-ignoring peer;
- exit-trap-new-child: a real Bash EXIT trap starts a fresh timeout during cleanup;
- late-input-or-signal: expired GO, GO-channel EOF and actual pre-GO INT all refuse admission;
- helper-or-close-failure: a real admitted FD closes before an injected secondary close failure,
  preserving the original body object. Before work, its first trial also sends a real self pidfd
  paired with a network-namespace FD and then the original parent PID-namespace FD, each with its
  matching reported inode. Both must be refused and all received rights closed before an exact
  acknowledgment permits progress. A second trial sends an oversized packet with duplicated
  rights after the valid supervisor mapping, requires actual MSG_TRUNC refusal and EBADF close
  facts, then aborts before any work and proves ECHILD;
- host-proc-netns-role: original host-PID lookup and retained namespace-FD alias remain usable.

PID1 stays alive while it consumes every direct/adopted child to actual ECHILD. WNOHANG returning 0
means a live child, never an empty realm. Repeated namespace KILL covers a cleanup fork concurrent with an
earlier signal. Even past the observed deadline, known children are consumed rather than detached;
that late cleanup cannot make the test positive. Namespace-init death and unshare kill-child are
fallback containment only, not positive ECHILD or userland-reap evidence. Received truncated or
malformed rights close even when the frame is refused. All original channels, rights, returned
stdout/stderr
pipes, wrapper reaps, signal restoration and final clock fences must succeed. The exact Popen
owner is disarmed at its consuming wait observation, before subsequent record/validation failures;
no reserved PID/PGID signal follows that consumption. Namespace peer cleanup remains separate.
Earlier JSON is passive;
the actual final exits and VM lifecycle are required. The driver explicitly requires QEMU exit 0.

This prototype passes control endpoints through explicit `pass_fds` and fixed argv while Python
owns a DEVNULL stdin. It does not test the proposed Rust control-on-fd0 transport. Python's internal
DEVNULL/exec duplicates remain an opaque subprocess SDK boundary; the checked-close statements
above concern the actual returned pipes, channel and capability originals only. The guest checks
its original subordinate end and signal status after stdout write/flush. The controller observes
the original body before opening its reserved finalization window, attempts every registered
handler/mask restoration, then checks its original end. No restoration creates a new deadline.

The guest/driver keep bounded private receipts and failure logs. Source bytes are transferred only
through an owned two-file archive and checked before and after execution; same-UID swap/restore
races are not eliminated by those witnesses. There are no credentials or signing grants in this
prototype. Controller work disks and SSH keys are removed only after proved VM cleanup; uncertain
retirement retains the private directory for the operator. The manual job retains only the closed
metadata files described below.

An actual utility may be refused because the selected Debian kernel lacks PID namespaces/pidfds,
root lacks CAP_SYS_ADMIN, a service applies RestrictNamespaces/SystemCallFilter/PrivateUsers or
proc visibility restrictions, inherited descriptors are filtered, or the namespace/capability
mapping differs from the reviewed profile. Refusal must stay negative; do not add privileges,
change product mounts, increase timeouts or use a reported PID as a fallback. A later production
systemd profile must be observed separately from an unrestricted disposable guest.

A green prototype would establish only these finite topology and ownership observations for its
exact tools/kernel/profile. It would not prove the full launcher, native maintain, migrations,
rollback, application health, signed inputs or Caddy TLS. Docker daemon-side containers/execs are
outside a client PID namespace and need their own exact actor/outcome proof. Complete production
maintain authoring remains held until the maintainer closes the actual probes and their source
bindings.

The cross-platform unit tests exercise only bounded frame decoding, real local FD retirement,
explicit-epoch body/final transition, fixed Debian metadata-profile refusal, partial process-state
registration/restoration and supplied
wait-result ownership ordering. They execute no namespace helper role, Linux namespace operation,
VM, process launcher or network. A mocked wait record is a negative decoder-order regression;
it is not a kernel-reap or namespace-positive proof. The actual 11-case/14-trial run and the
unchanged operational 900-second launcher remain unexecuted until a separate authorized trial.

The optional manual job uploads the positive summary only after success and always
attempts a separate exact `failure-summary.json` artifact. Negative metadata is a
closed fixed projection of the controller checkpoint, named failed case, exception
family, actually returned signed SSH status or null, row SHA/count and cleanup
facts. Raw streams, unknown guest fields, private work, configuration and keys are
never eligible. The original first cause survives diagnostic/cleanup failures.
Missing, partial or late metadata remains unavailable, not proof; the original end
still fences new output admission and final nonzero exit invalidates passive bytes.
The 14 inert tests include hostile unknown-field projection, real local partial-file
closure and late-output handling without executing any actual helper role or VM.
