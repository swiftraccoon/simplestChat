# Rust outer maintenance prerequisite controller API review

`xtask ops-artifact maintain-runtime` replaces the optional workflow's invocation
of `build/security_vm_maintain_prerequisite.py`. The old controller stays frozen
as a reference. The Rust entry owns the entire outer operation and the same
four-case caller switch. It does not call `Program::OpsRuntimePython`, import a
Python controller or add a general VM execution interface.

The guest session now executes the authenticated installed Ops binary's explicit
`release maintain-prerequisite guest` role. Its closed request admits exactly
52 project source pins and 15 operational helper pins; the former four proof-only
Python guest/material sources are excluded from that guest set. Existing Ansible
bootstrap, the fixed Ops33 guest and operational launcher helpers keep their
Python roles. No tracked Python bytes, installed service callers, backup/DB
policies or bootstrap interfaces change. The optional crypto feature and static
producer closure are described in the [native API review](maintain-native-prerequisite-api-review.md).

The affected guideline decisions use the
[adopted API inventory](rust-api-checklist.md). `C-CTOR`, `C-FAILURE` and
`C-GOOD-ERR` apply to the fallible signal registration and fixed existing error
type; its Rustdoc documents failures and includes a compiled usage example
(`C-EXAMPLE`, `C-QUESTION-MARK`). `C-STRUCT-PRIVATE` keeps registrations and flags
opaque. `C-COMMON-TRAITS`, `C-DEBUG` and `C-SEND-SYNC` apply: debug output reveals
only the type, compiled assertions check thread traits, and unique registration
ownership rules out `Copy` and `Clone`. Fallible registration provides no valid
`Default`; equality, hashing and serialization do not describe this live owner.
`C-DTOR-FAIL` and `C-DTOR-BLOCK` apply through explicit checked finalization and
the blocking qualifications below. `C-VALIDATE` applies to the fixed wire and
file profiles, with malformed-neighbor and actual namespace-mutation tests.
This is an affected-component review, not a completed inventory for the workspace.

The entry captures its origin before pure argument parsing. One opaque
`CompletionSignals` instance retains the existing safe atomic registrations
through runtime destruction, umask restoration, summary/stdout and the final
clock/signal fence. Its sticky observation uses INT precedence when both flags
are set; `Control` latches its first observed cause/status. Signal status never
authorizes child ownership. Checked `finish` attempts every registration and
keeps the original flags for the final sample. Drop is negative fallback only.

The outer profile remains N1800/M120, with 510 seconds for setup and 1,170 seconds
required before GO. Native run/init share their own original 1050/60 interval.
The native guest's invocation/output end and
the outer body remain independent authoritative backstops; GO does not transport
the outer absolute clock. There is no 4200 profile, second outer clock, default
budget, timeout increase or fallback retry.

Acquisition and current verification execute fixed source-built native CLI
children. Each child's existing profile stays intact. The parent clips actual
body plus settlement to its live setup ceiling, rechecks after all returns and
closes, and may refuse cold/slow setup. The acquisition API's 1,325-second
worst-case schedule is not asserted to fit the 510-second setup cap. The fixed
fresh server CLI uses N900/M60 and the outer 300-second role cap; cached
verification preserves the reference floor(remaining−40), maximum N900/M60
profile, with a 100-second minimum. Parent deadlines
remain authoritative even if an independent child would otherwise have time.

An opaque authenticated payload owner is constructed only after current owned
child exit zero and settled streams, with every original input admitted before
current verification and checked after it. All 15 Ops files, all 11 server files,
source inputs, original parent namespaces and selected bytes stay held through transfer
and post-VM finalization. Current API identity and actual verifier completion are
separate from passive result JSON. Saved receipts or paths cannot instantiate
positive custody. Retention does not authorize controller execution of musl.

The fixed three-member USTAR writer retains the pinned TAR library's two
512-byte zero terminator blocks. It does not add padding to a 10,240-byte whole
record. Such extra padding can create a 10,752-byte tail when the final member
boundary is 9,728 modulo 10,240, causing both the controller and frozen guest
readers to refuse the writer's own output. The actual d540 signed binary has
that alignment. Removing the extra writer padding preserves the existing
1,024-to-10,240-byte zero-tail admission range, fixed names/order/headers,
per-member hashes, archive digest/length and all original custody/deadline
checks. The transfer identity is computed from the actual new archive bytes.
The frozen Python writer and readers remain unchanged; byte-for-byte writer
equivalence is not claimed. Regression cases exercise the production Rust writer
at every possible 512-byte member alignment and retain malformed-tail refusals.

The manual workflow installs headless controller prerequisites without APT's
recommended packages. QEMU, image/cloud tools, SSH, curl, ACL, CA certificates,
SeaBIOS and iPXE firmware are explicit. The fixed QEMU invocation uses
local QCOW2/raw drives, VGA with display disabled, and virtio/user networking;
it does not select GUI, UEFI or remote block drivers. The resolved Ubuntu 24.04
QEMU package requires both firmware packages, its system data and common tools.
APT repository signature and package-hash policy are unchanged. A minimal-image
resolver comparison retained every common package version and required firmware,
trust, ISO and network components while reducing the planned closure from
308 packages/167,846,296 archive bytes to 80/36,634,508. No packages were installed
and no VM was booted. It is not a hosted byte saving, speed or boot proof;
the fresh signed runtime remains required. The workflow's 45-minute ceiling and
all original controller/guest deadlines remain unchanged.

Completed Ops 3 and server 4 observations are recorded immediately after the
corresponding current child succeeds, even if a later retained check refuses.
The exact four-subject verification-digest object is decoded before custody is
admitted. Evidence JSON/bin, transfer and executable limits match the frozen
reference per role, rather than inheriting a generic binary limit.

Immutable inputs must be nonempty and not group/world writable. The initial link
count, including accepted hardlinks, stays part of the full unchanged stamp.
Only the explicit system-tool role may admit root ownership; other inputs require
the current UID. Artifact namespaces retain complete original mutation stamps
and private mode 0700 admission. Mutable owned work instead binds original output
and work directory FDs, permissions and identity while allowing authorized contents
to grow. Fixed native tool links retain full original symlink stamps, text and
canonical targets. After all borrowers close, work retirement is descriptor
relative and permits only the fixed native-tools nested directory, without
following symlinks or deleting a substituted foreign namespace. Failed or expired
retirement stays negative while every original directory close is attempted.
Known-work cleanup may ignore an already latched body error only for its fixed
original namespace, under the same active phase. It never enters the finalization
reserve early or clears a cause/status. Actual local checked-close or retirement uncertainty is
latched even for owners that never reach the outer registry; a body policy
refusal alone is not a fabricated cleanup failure. Failed local VM/Session
startup finalization is conservatively negative when the returned constructor
state cannot establish complete cleanup; this flag is not a syscall-failure
counter. Actual close errors retain the outer I/O failure category.
Rustix directory cursors close through their opaque RAII destructor; this is a
specific SDK-close qualification, not proof that their close syscall succeeded.

Each child uses one reserved `Group`, nonconsuming terminal observation, exact
group retirement, one consuming reap and attempted checked pipe/log closes.
Metadata/namespace live checks are separated from full pre/post/final hashing.
The SDK-owned duplicate transferred to `Stdio` has no checked parent-close return;
the retained original is checked and explicitly closed separately. There is no
full stdin-consumption acknowledgment. No unsafe/raw-FD recovery is introduced.
QEMU readers and QMP retain the immutable outer end across phase changes. Their
typed first failure is recorded before a later clock check or generic unhealthy
refusal. Direct/group facts do not establish arbitrary descendant retirement.

The fixed wire rejects duplicate keys, trailing JSON, nonfinite values, unknown
fields, impossible status/phase joins and secret-bearing extra fields. It keeps
nullable incomplete hashes and actual signed statuses. Expected native 1/143/124
remain separate from the guest experiment's verdict. Public summaries contain
only closed metadata, hashes and actual status/lifecycle facts; credentials,
raw streams, config, keys, dumps and paths stay private.

Server discovery handles retained artifacts from repeated CI attempts through
one private pure selector (`C-VALIDATE`, `C-GOOD-ERR`). It bounds the listing and
matching timestamp strings, compares parsed RFC3339 instants with full precision,
and refuses a tied newest instant or malformed matching metadata. A creation
instant only locates the next download. Existing signature, current artifact
ID/digest, successful workflow and exact run-attempt checks remain authoritative;
an expired or unauthentic newest candidate cannot trigger an older fallback.
Reordered rows, offset-equivalent instants, subsecond ordering, older ties and
malformed neighbors have source tests. This adds no API call, runtime authority,
deadline extension or claim that acquisition occurred.

Installation failures retain a closed `installDiagnostic` checkpoint and the
actual child exit when available. Guest receipt admission validates the exact
identity, ordered prefix of at most 33 fixed cases, byte-range recorded exits,
nullable boolean install facts and frozen failure vocabulary. Only admitted
facts are copied into the report before positive completion is required;
malformed or secret-bearing neighbors publish no candidate content. A valid
failed guest retains its closed `opsGuestFailure` and stays an ordinary failed
prerequisite, including inner 124/143 statuses. A case row with recorded
`exitStatus: 0` and `passed: false` remains valid negative evidence. Positive
completion still requires all 33 successful cases, the original install facts
and actual exit zero. Successful summaries retain their existing shape. This applies
`C-VALIDATE` and `C-GOOD-ERR` without promoting diagnostics into execution
authority or changing outer signal ownership.

The guest's fixed renderer retires only the admitted inert `images.json` with
exact `{}\n` bytes, root ownership and mode0600 under its original root0700
parent. The maintenance firewall uses its own `sc_maintain_` table identity;
the frozen Caddy-only helper's `sc_caddy_` restriction is not reused. Inert
mutation/rule tests cover those composition corrections. Neither the source
fixes nor local tests establish real nftables behavior or the complete signed
PostgreSQL/Caddy/900-second launcher transaction. The latest frozen-guest run
passed Ops33 then failed at rendering; the new native guest still needs fresh
four-case runtime evidence.

The generated `.git/shallow` administrative input is validated and explicitly
closed before Git receives the namespace. The first fetch can create `.git/logs`
and change the parent directory's link count, so that input cannot remain an
immutable source owner across import. Authenticated bundle and project-source
owners retain their existing checks, alongside the final revision, tree,
one-commit boundary, connectivity and clean-worktree checks. Generic file and
parent bindings remain unchanged. A local Git reproduction covers this ownership
handoff; it does not establish a successful signed VM trial.

Known blocking limits remain: synchronous filesystem/hash/close, OS process APIs
and SDK operations are cooperatively fenced, not cancellable per syscall. A
same-UID swap-and-restore wholly between witnesses is outside the claim. Failed
finalization stays negative even if a contained VM eventually exits. QMP EOF,
QEMU exit zero, SSH status, pipe closes, source freshness, the original deadline
and final signals are separate required observations. New Rust native tests and actual signed four-case
free-KVM runtime must pass before any runtime-success claim; source/static checks
alone do not prove them.

Outer exit statuses preserve the reference domain: syntax 2, ordinary proof or
deadline failure 1, actual observed outer INT 130/TERM 143, or complete success 0.
Inner native/SSH/QEMU signed statuses remain separate passive observations and
are never promoted into an unrelated outer exit status. All actual input
originals remain open until after the VM finalizers; an earlier current-role
error is latched before later close or clock observations.
