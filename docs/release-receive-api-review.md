# Native host release receiver: affected API review

This prospective record covers the complete explicit `release receive` operation,
its concrete standard-descriptor/HTTPS/ZIP adapters, retained filesystem owners
and immutable publication pair. The local [coding standard](coding-standard.md),
[commit standard](commit-standard.md) and [54-entry inventory](rust-api-checklist.md)
apply. Existing Python references, prepared helper preflight, operational callers
and authenticated Ops bootstrap remain unchanged.

The private shared owner and mature adapters retain the applicability decisions
in the [fresh verification](release-fresh-verification-api-review.md),
[cached verification](release-verification-api-review.md) and
[controller](release-fetch-api-review.md) records. The
[shared signal lifecycle](release-signal-lifecycle-review.md) records atomic
registration, coalescing, SDK removal and after-final-fence limitations.
A passive envelope has no
GitHub freshness, signature, SSH actor or installation authority. The host command
expects the trusted caller to have supplied that authority separately.

- **C-CUSTOM-TYPE/C-NEWTYPE/C-STRUCT-PRIVATE/C-VALIDATE:** the existing typed
  ten-field Envelope retains positive signed 64-bit identities. Host inputs do
  not pass through the narrower local API Metadata type, and no attestation
  placeholder is constructed. The physical LF frames are bounded before the
  shared byte-JSON adapter. A private ZIP pin carries only byte count and SHA;
  it does not establish an authenticated selection or execution capability.
- **C-SERDE:** only the passive envelope/report/journal are serializable. Live
  locks, descriptors, directories, process groups, cancellation and opaque grants
  are neither JSON authority nor Debug output. The established sixteen-field
  report preserves fixed coarse host classes and committed download progress.
  Predicate comparison uses the existing Python-compatible semantic JSON
  equality, including recursive bool/integer/finite-float equivalence.
- **C-CALLER-CONTROL/C-RW-VALUE:** N/M are explicit positive ASCII seconds,
  `0 < M < N <= 300`, with no hidden default or restarted clock. The dedicated
  CLI borrows stdin/stdout and restores their original shared descriptor flags.
  It uses safe nonblocking kernel I/O rather than a blocking Tokio stdin worker.
  Neither ordinary files nor pipe/TTY input require an epoll-only admission.
  Each frame stops at its own LF; no second-message or transport EOF is required.
- **C-GOOD-ERR/C-FAILURE/C-QUESTION-MARK:** typed private causes retain the first
  observation across all known peer finalizers. Host public classes are distinct
  from the controller's coarser mapping: duplicate JSON, malformed syntax,
  HTTPError before ZIP creation, admitted bad responses and receipt persistence
  have separate outcomes. A later receipt-write class can describe settlement
  uncertainty without clearing an earlier private cause. No raw URL, TLS error,
  stderr, credentials or unvalidated native exception text enters a report.
- **C-DTOR-FAIL/C-DTOR-BLOCK:** every admitted original file and process pipe has
  an explicit close path; group retirement precedes the single consuming reap.
  The fixed CA, curl executable and aliases retain original witnesses. Both
  target names are checked before publication and their retained aliases/pins
  are checked after links and before positive completion. Drop is fallback.
  An existing equal-byte foreign inode remains foreign; failed evidence and
  partial publication are preserved rather than adopted or deleted.
- **C-SEND-SYNC/C-COMMON-TRAITS/C-DEBUG:** safe registered atomic signal flags
  remain live independently of runtime progress and are observed at original
  read/hash checkpoints. The dedicated two-worker runtime drives process
  adapters. Private Rc custody stays on the operation thread. No speculative public Clone/Send/Sync,
  arbitrary command flags, root selectors or filesystem callback framework is
  introduced. Test-only lower observations do not become production switches.
- **C-EXAMPLE/C-CRATE-DOC:** the harmless help example documents dedicated
  synchronous use, explicit budgets and the runtime nesting panic precondition.
  Whole-operation success is fenced after actual standard flags, runtime,
  signal-owner completion and umask restoration. Exit status invalidates all
  earlier bytes, including a complete report emitted before a late refusal.
- **C-PERMISSIVE:** this operation reuses the existing tar/rawzip/flate2,
  httparse/getrandom and process-owner dependencies and scoped notices. Curl is
  an external Linux/OpenSSL platform prerequisite, not authenticated Ops
  provenance or a bundled TLS library. The separate shared signal-lifecycle
  repair records its own direct edge, exact notice and dependency inventory.
- **C-STABLE:** the new public surface is only the fixed synchronous `run`
  facade. All filesystem, download, pin and publication helpers remain private.
  Server ten-file/four-subject and Ops fifteen-file/three-subject contracts stay
  separate. The native source route does not replace a prepared Python helper.

The host keeps the reference's ordering: root and envelope, protected paths and
nonblocking workload lock, finalized-journal guards, unique retained evidence,
space check, Ready, fresh URL, transfer, one selected ZIP extraction pass,
archive/evidence/runtime/late-exclusive-UTC binding, manifest-first hardlink pair,
then final outcome persistence while still locked. The receipt file and attempt
sync precede lock close and final stdout. Existing finalized outcomes permit a
new attempt; unknown remote outcomes never authorize automatic retry or rollback.

Publication accepts an existing different equal-byte inode and the narrow racing
same-original-inode alias effect. Generic file witnesses do not accept arbitrary
nlink or ownership changes. Seal metadata may update length/timestamps only after
original type/device/inode/mode/UID/GID/nlink checks. Persistent target replacement
and added CA directory entries are refused. These checks do not eliminate a
same-UID writable-filesystem check/read race, provide an OS lock against that
actor, or prove durable host writes from a passive receipt alone.

The native transport profile is explicit: curl 8.4+ with an OpenSSL backend,
HTTP/1.1, TLS 1.2 minimum, no ambient config/proxy/netrc/redirect/retry/decompression role,
fixed CA plus an owned empty capath and disabled native CA discovery. The fixed
native User-Agent is `simplestchat-release-receive`; urllib's interpreter-minor
User-Agent is not impersonated. Raw headers retain first selected CE/CL values
and trailing whitespace. Curl can reject malformed/conflicting framing that the
Python parser admits; a finite oracle does not prove malformed-wire equivalence.

The fifteen-second connect limit is explicit. Curl whole-process and parent
body limits do not recreate Python's fifteen-second per-socket read idle timer;
a quiet interval can therefore differ while still completing within the original
native body. No speed floor or pipe-idle watchdog is substituted. Exclusive CA
behavior, actual curl version/flags/backend and this timing profile require
separate free-VM runtime evidence before an installed caller switch.

The shared byte-JSON adapter's encoded bounds, Unicode-scalar/surrogate handling,
128-container depth, 100,000-value and decoded-string limits apply. These are
recorded native accepted-domain boundaries rather than full Python parser-domain
parity. Fixed host paths have canonical absolute nofollow custody. Original
admitted File handles have checked-close evidence; on non-Linux test/controller
platforms the mature directory cursor's private extra FD has only an opaque RAII
close. Linux borrowed RawDir opens no such extra descriptor. Blocked kernel
filesystem/random/close/reap calls remain outside a hard wall guarantee.

The source gates below have passed. Actual authenticated musl/SSH/TLS platform
execution remains pending its separate same-head workflow.
The closed producer exercises the actual unchanged receiver with real private
files, ZIP, locks, fsync and publication, adapting only inert lower observations.
It does not establish TLS, signatures, authenticated target bootstrap or actual
host publication. Failed source/gate captures remain separate from later passes.


## Source validation (2026-10-24)

The complete ordinary Ops suite passed 403 checks: 333 library, 62 CLI and
eight documentation tests, in 56.521 seconds wall time. The subsequent test-only
census and cfg-test style corrections preserve that production implementation.
The exact complete receiver comparison passed all 104 cases in 59.12 seconds
test time (65.673 seconds wall time). The four existing prepare/cached/fresh/fetch
comparisons also passed their 359 scenarios in 151.51 seconds test time
(155.000 seconds wall time). These are regression durations, not capacity claims.

The full workspace formatting, all-target/all-feature Clippy and public/private
Rustdoc gate passed in 70.130 seconds. Separate Linux GNU all-target Ops Clippy
passed in 17.477 seconds. The CI integration passed its three Rust automation,
33 Python policy and ten Node container-input tests. Full pinned Ruff, formatting
and strict basedpyright passed across 263 maintained Python sources and stubs;
the receiver's 13 oracle regressions and the VM harness's final 30 inert tests
also passed. Offline lint and syntax checks passed for the new guest playbook.
The VM source underwent an additional narrow first-cause repair and focused
quality/regression checks after the initial whole Python gate.

Failed trials remain distinct. The original symlink case retained matching
effects but reported the wrong coarse class; validation now checks protected
metadata before opening. The first stdio supervisor reached natural EOF before
observing leader termination; its nonconsuming terminal join now uses the same
original deadline before group retirement and one consuming reap. A later
process-directed test signal could still be pending when the handler was
removed; the late injected case uses an actual thread-directed signal whose
handler completes before the sender returns, while external wait-phase INT/TERM
coverage remains. The corrected five-mode parent passed in 0.67 seconds test
time. A comparison's obsolete total of 267 refused the four added VM sources;
it now checks all 267 original paths plus unchanged full current census without
excluding the new maintained files. A cfg-test nested-if lint was corrected
with the pinned let-chain style. No failure was waived or turned into a skip.

The new VM harness's inert tests cover actual owned subprocess and HTTP-resource
settlement, retained verification ordering, closed public projections and paired
body/cleanup faults. Its source and review do not establish a successful
GitHub workflow, current signature validation, Linux musl receive execution,
actual TLS, Docker/service deployment or an installed caller migration. The
[separate runtime proof](release-receive-runtime-proof.md) defines those
authenticated VM observations; production Python receivers and callers stay
available. No dependency was added for this operation.

A subsequent full local Python CodeQL trial found two fixture creation masks
that could request a public final mode during the original open. Both helpers
now exclusively create mode 0600 and apply the selected fixture mode only after
its complete write and flush. Deliberate final-mode refusal cases and the fixed
public guest adapter keep their required final permissions. The original failed
scan is retained; it adds no exception or change to production Python policy.
