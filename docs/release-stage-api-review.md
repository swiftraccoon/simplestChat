# Native host staging: affected API review

This prospective review covers the complete `release stage` operation, its fixed
Docker process adapter, original filesystem ownership and atomic records. The
[coding standard](coding-standard.md), [commit standard](commit-standard.md) and
[54-entry inventory](rust-api-checklist.md) apply. This is an affected-component
review, not a retrospective certification of the crate or its dependencies.

The only new public entry is the synchronous `release::stage::run` facade.
The installed Python runtime, prepared-helper hash set, deployment controller,
Ansible callers and first-install/exact-repeat installer remain unchanged.
Staging assumes separately trusted input and local Docker authority. It neither
authenticates the server artifact nor authenticates its own executable.

- **C-CASE, C-GETTER, C-WORD-ORDER, C-METHOD**: Private role/options/ownership methods use the
  existing names and conventions. The crate-private `accepts_runtime_id` boolean query exposes
  no mutable archive representation.

- **C-COLLECT, C-CALLER-CONTROL**: Explicit N/M own one original clock. Each child stream is
  bounded to 2 MiB, durable read buffers are 64 KiB, and the fixed command/record/capture
  collections have finite operation bounds. Capture overflow is a refusal, not truncation
  permission.

- **C-SERDE, C-STRUCT-PRIVATE, C-NEWTYPE-HIDE**: Only passive reports and journal projections
  serialize. Live file/lock/process/signal owners, callbacks and Docker authority do not.
  Existing public archive identity JSON remains unchanged; selected runtime-manifest identity
  stays private.

- **C-CUSTOM-TYPE, C-NEWTYPE, C-VALIDATE**: Validated options distinguish revision,
  whole/reserve durations and fixed command roles. Raw signed status, natural complete stdout
  and all-peer settlement are separate facts. Config IDs and selected manifest IDs come from the
  mature verified archive graph.

- **C-GOOD-ERR, C-FAILURE**: Private typed causes preserve the first observation. Public policy
  messages use fixed strings; fallback classes are explicit. No tool stderr, socket error, image
  configuration, raw command or exception string is emitted. Exit status invalidates earlier
  passive bytes.

- **C-RW-VALUE, C-DTOR-FAIL, C-DTOR-BLOCK**: Both streams drain concurrently and retain their
  buffers across cancelled reads. Group retirement precedes one consuming reap. Every retained
  original and new atomic inode has an explicit finalizer; Drop is fallback. Positive process
  completion requires checked pipe closes. A consuming Tokio pipe-conversion refusal has an
  SDK-private close boundary, remains negative, and still settles every other retained peer.
  Tests retain observations until all known peers have settled.

- **C-INTERMEDIATE**: Verified archive metadata, semantic stdout completion, signed exit status
  and independent close results remain available separately. A successful create's complete ID
  can be owned despite a later stderr close refusal. Unknown/truncated create output never
  grants cleanup authority.

- **C-CTOR, C-BUILDER, C-NO-OUT**: Private constructors admit one fixed operation, and execution
  returns an owned outcome. There is no public builder, output-parameter family, daemon command
  selector or root override.

- **C-GENERIC, C-OBJECT, C-SEALED**: The private executor exists for fixed
  native/whole-reference observations. No public implementation contract, dynamic plugin API,
  callback registry or general command framework is introduced.

- **C-CRATE-DOC, C-EXAMPLE, C-QUESTION-MARK, C-LINK, C-HIDDEN**: The public harmless help
  example needs no host effect. Rustdoc states the runtime nesting panic and operator
  prerequisites; the README links this record. There is no fallible service example disguised as
  an offline doctest.

- **C-STABLE, C-RELNOTES, C-METADATA**: The package remains unpublished tooling with its
  existing targets and release policy. This source feature does not switch an installed route or
  certify a different target. Operational changes require their own release evidence.

- **C-PERMISSIVE, C-FEATURE**: No dependency, version, feature or lock change is required. The
  scoped existing notice route is reused; it is not a new complete dependency-license audit.


C-CONV/C-CONV-TRAITS/C-CONV-SPECIFIC introduce no new public conversion family.
C-ITER/C-ITER-TY introduce no named iterator API. C-COMMON-TRAITS/C-SEND-SYNC/
C-DEBUG/C-DEBUG-NONEMPTY reuse the existing public owner decisions: there is no
new public data or resource-owner type to make Clone, Copy, Send, Sync or Debug.
Private `Rc` custody stays on the operation thread; the registered atomic signal
owner remains independent of Tokio runtime progress. The private diagnostic
enum is fixed and does not expose secret state.

C-NUM-FMT/C-BITFLAG add no numeric wrapper or combinable flag type.
C-OVERLOAD/C-DEREF/C-SMART-PTR add no operator, dereference or smart-pointer API.
C-STRUCT-BOUNDS adds no public generic structure. C-EVOCATIVE/C-MACRO-ATTR/
C-ANYWHERE/C-MACRO-VIS/C-MACRO-TY introduce no macro interface; existing SDK and
test-local macros retain their established roles. Unaffected inventory entries
remain outside this focused review rather than being retrospectively marked green.

The operation preserves the reference order: root/options, protected layout,
nonblocking workload lock and unfinished-journal guards, manifest/revision,
unique retained attempt, whole archive verification, repeat check or import,
runtime identity, isolated migration validation, atomic staging record and
finalized journal. Outcome persistence occurs **after the lock closes**, as in
the unchanged stage action. It is not the receiver's lock-held receipt protocol.

The daemon check independently verifies platform, runtime user, revision label,
command, entrypoint, working directory, healthcheck and environment names.
It additionally binds Docker's actual immutable ID to either the config digest
or selected verified image-manifest digest in that same archive graph. The latter
supports the current containerd-backed engine. Arbitrary index/blob digests and
otherwise similar images outside that graph are refused. This is an explicit
stronger binding than the Python stage policy, not unrestricted parity.

One original N includes parsing, setup, hashes, all children, records, output,
runtime, signal completion and mask restoration. Body ends at B=N−M. While a
validator may exist, its distinct checkpoint ceiling leaves 40 seconds inside B
for exact-ID removal (30 seconds) and the process adapter's TERM/reap (5+5).
Before-create admission refuses when that known tail cannot fit. Small otherwise
well-formed N/M values therefore need not admit an operation. M remains available
for local finalization; it never restarts a child or daemon clock.

Known failed captures are bounded and persisted after the known validator's
retirement attempt, while the workload lock remains held. Semantic stdout
completion requires natural EOF fenced in the live command body, before any
local group retirement. EOF created by TERM/KILL drainage is only a settlement
fact and cannot grant a validator identity. A complete natural body stdout and
raw zero status can identify the created validator even when an
independent stream close fails. Its exact-ID finalizer is still attempted and
cannot clear that first Close cause. A known zero removal effect is recorded
before later pipe/log failure. The reference may retain a validator after a
failure; the native exact-ID cleanup is a separately recorded negative-path
difference. An uncertain create remains unresolved and is never cleaned by name.

Atomic records retain the original prior generation through temporary write and
fsync, then check it immediately before replacement. Every created temporary FD
is owned before subsequent fallible metadata. A changed target remains foreign;
failed evidence and partial records are preserved. Namespace/hash observations
do not eliminate a same-UID writable-filesystem check/read race or authenticate
the Docker daemon. Blocked kernel filesystem, random, close and reap calls remain
outside a hard wall guarantee.

Fixed Docker stdout follows the pinned reference's string stripping (including
C0 1C–1F), six-byte ASCII stripping and Unicode splitlines/CRLF behavior.
Migration versions normalize decimal leading zeros without a u64 conversion.
The pinned default 4300-digit Python conversion limit counts leading zeros;
over-limit conversion fails before normal removal, while an admitted map
mismatch remains after it. Reconfigured Python integer limits are a separate
reference-runtime profile, not a native operator switch. These adapters borrow
the bounded capture and do not allocate an array of split lines.

The native byte-JSON limits, duplicate/nonfinite/surrogate rules and bounded
archive profile remain those of their existing adapters. The fixed original Docker executable
is bounded to 256 MiB before hashing; this is an explicit native prerequisite.
Protected paths have canonical nofollow custody. Native coarse IO/archive/input/close classes and
the graph binding are compared through named finite cases; they are not blanket
aliases for arbitrary reference failures. Private inspection/log output is
bounded and never becomes a public exception message.

Local validation of the complete feature passed Rust formatting, all-target/
feature Clippy and warning-free public/private Rustdoc (61.406 seconds), GNU
all-target Ops Clippy (20.488 seconds), and 430 ordinary Ops checks: 356 library,
65 CLI and nine doctests (63.235 seconds). The independently frozen Python
composition passed full maintained-source Ruff/format and strict basedpyright
(268 sources/stubs), 16 staging-oracle tests covering all 81 reference scenarios,
20 VM-harness tests, and offline playbook lint/syntax. All six exact Rust/reference
comparisons passed together (260.531 seconds):
81 staging scenarios and 463 preserved prepare, cached/fresh verification, fetch
and receiver scenarios. Docker-input checks (ten), CI policy checks (33) and xtask
operations-command checks (three) also passed.

Failed iterations remain in private evidence: insufficient test-fixture body
headroom, duplicate-key and atomic-temp observation mismatches, Python whitespace
and wide-integer regressions, three Clippy findings, and a CI expectation missing
the existing --skip flag. They were corrected and rechecked; no production budget,
lint allowance, reference skip or successful-runtime claim was substituted.
The final source also distinguishes natural body EOF from EOF caused by cleanup,
so descendant-held pipes cannot grant a Docker cleanup identity.

Inert daemon observations prove policy/effect wiring only. Actual Docker execution,
authenticated installed musl staging, caller budgets, upgrade/reconciliation and
production route selection require separate evidence. The manual workflow has
its own explicitly qualified first/repeat staging proof; no local source gate
claims that runtime result.

The final integration also partitions the complete automation plan into parallel
checks and reference jobs after observing 537 seconds of the prior hosted
600-second job limit. The full local route retains its original order; both
partial routes together retain every command exactly once. Reference comparisons
have no result cache. The typed required-job policy includes the new job before
signing. The final integration passed four partition/failure-order checks,
four required-job checks, three actual local-evidence CLI checks, 34 CI policy
checks and two Ops signing-workflow checks. Changed Python quality also passed;
full Rust quality passed again in 60.000 seconds. Independent review confirmed
the disjoint command owners, required signing dependencies and cache boundary.
Hosted timings remain to be measured on the new head.
