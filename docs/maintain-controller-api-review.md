# Rust outer maintenance prerequisite controller API review

`xtask ops-artifact maintain-runtime` replaces the optional workflow's invocation
of `build/security_vm_maintain_prerequisite.py`. The old controller stays frozen
as a reference. The Rust entry owns the entire outer operation and the same
four-case caller switch. It does not call `Program::OpsRuntimePython`, import a
Python controller or add a general VM execution interface.

The remaining Python execution is explicit: existing Ansible dependency/tool
bootstrap, the fixed Ops 33 guest, the frozen maintenance guest and its staged
helpers, and unchanged operational launcher helpers. No tracked Python bytes,
installed service callers, backup/DB policies or bootstrap interfaces change.

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
The frozen guest's invocation/output end and
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
