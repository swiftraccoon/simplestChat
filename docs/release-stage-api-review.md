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

Local validation passed full Rust formatting, all-target/feature Clippy and
warning-free public/private Rustdoc (51.143 seconds on the final Rust sources),
GNU Ops all-target Clippy (20.488 seconds), and 430 ordinary Ops checks: 356
library, 65 CLI and nine doctests (63.235 seconds). All six whole Rust/reference
comparisons passed together in 273.321 seconds: 81 staging scenarios and
463 preserved prepare, cached/fresh verification, fetch and receiver scenarios.
These are finite source/inert checks, not an engine or deployment verdict.

The final Python helper suite passed 1,811 tests in 148.064 seconds wall time
(147.437 test seconds), with 17 existing platform/optional skips. The changed
Python sources passed pinned Ruff, formatting and strict basedpyright. Earlier
full maintained-source quality covered 268 sources/stubs. Thirteen Rust/Python
receipt checks, five command-policy checks, four actual local-evidence CLI checks,
ten Docker-input checks, 34 CI policy checks, two Ops signing-workflow checks,
and offline VM playbook lint/syntax also passed. The VM harness has twenty inert
checks; their success does not establish actual KVM or authenticated musl execution.

The complete automation plan is partitioned into checks and three fixed reference
groups. `foundation` owns VPS controller/host, builder, prepare and cached verify;
`fetch` owns fresh acquisition and fetch; `host` owns receive and stage. The full
local route and bare references route preserve their original complete order.
Every group retains its whole comparisons. The matrix uses numeric parallelism
three, disables fail-fast, keeps the ten-minute limit and has no result cache.
The eleven required job results, including its aggregate, remain mandatory before
signing. The existing automation receipt proves only checks; workflow/xtask
changes invalidate prior keys. Local completion additionally requires eleven
execution receipts, including each reference group; missing, duplicate, unknown
or filtered groups cannot inherit aggregate success.

The first published integration, `326184a`, was refused before remote deployment.
Its automation ran 1,810 Python tests and found two stale expectations for the
checks selector and ten-job roster. Its reference job passed eight comparisons
before exceeding ten minutes during staging. Those completed comparisons took
24.49, 3.88, 16.28, 27.47, 44.00, 83.92, 120.65 and 105.66 seconds, after setup
and compilation. These hosted observations motivated the fixed parallel groups;
new hosted timings remain to be measured. No failed or interrupted case is
counted as passed, and no budget, job, assertion or reference case was removed.

Other failed iterations remain in private evidence: insufficient test-fixture
body headroom, duplicate-key and atomic-temp observation mismatches, Python
whitespace/wide-integer regressions, three Clippy findings, stale receipt counts,
and a local Cargo-target environment correctly refused by cache guards. Their
causes were corrected without broad aliases, lint allowances or skipped cases.
A companion fixture fix removes wall-clock ZIP timestamps after a full-suite
wire-byte comparison exposed a two-second tick race. Its new three-scenario
regression failed before the fix; fixed metadata matches the pinned writer's
raw bytes across all 36 ZIP profiles while preserving compression, modes,
malformed cases and byte-equality assertions. Production Python is unchanged.

Inert daemon observations establish policy/effect wiring only. Actual Docker,
authenticated installed musl staging, caller budgets, upgrade/reconciliation and
production route selection require separate evidence. The manual workflow has
its explicitly qualified first/repeat staging proof. The new source and local
gates do not claim that actual runtime result or the aspirational CI time target.
