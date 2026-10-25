# Standalone release-backup verification: affected API review

This record covers the complete native `release backup verify` operation and its
private file/process adapters. The unchanged reference is
[`restore_verify.py`](../ops/ansible/files/restore_verify.py); its installed
callers, nightly/offsite programs and authenticated Ops bootstrap remain separate.
Source compilation or a passive private report does not authorize a caller switch.

The prospective [coding standard](coding-standard.md) and
[commit standard](commit-standard.md) apply. This is an affected-change review,
not a retrospective whole-workspace or licensing audit.

## Entry, authority and complete result

The public entry is `release::backup::verify::run(&[OsString])`. The CLI requires
`--release-attempt release.NAME`, `--timeout-seconds N` and
`--finalization-seconds M`; there are no budget defaults or short/abbreviated
flags. Each flag appears once, with a separate value or `--name=value` spelling.
Seconds use positive ASCII decimal digits and require `0<M<N<=1800`. Syntax and
native budget refusals return2; a parsed invalid release selection returns1.
Help and scoped licenses return before runtime, host-file or engine effects.

An admitted operation requires root on the fixed prepared Linux host and the
trusted local Docker Unix socket. It neither authenticates the selected release
manifest nor upgrades Ops. Its root-owned paths and current PostgreSQL image are
existing host authority. The generated disposable name is private and never a
caller-supplied cleanup selector. The original backup and live database remain
untouched; only an owned snapshot and disposable database are used for restore.

Success emits exactly the reference's fixed success line. Operational refusal
emits its fixed failure line; SQL, database values, paths and command output stay
in private evidence. Final nonzero status invalidates any earlier line or partial
bytes, including late output/runtime/mask/signal/clock refusal. Private outcome
and freshness records are passive evidence, not reconstructed signing authority.

The operation includes all of these phases:

1. Retain the fixed root/config/state/source namespaces and actual workload lock;
   reject unfinished benchmark/release journals without a recovery exception.
2. Admit the release outcome, revision/hash and matching validated release manifest.
   Accept a failed release's valid pre-upgrade backup and a nonempty exact subset
   ledger; do not require `outcome.passed`, a receipt or the complete new ledger.
3. Copy/hash/fsync one positive backup of at most64MiB through original descriptors.
   Bind the readonly reader to the written original before closing its writer.
4. Inspect the current actual production PostgreSQL image; write initial private
   `restore.json`; refuse any prior restore-label container before create begins.
5. Create/start the fixed isolated database, wait for readiness, establish the
   two fixed roles, restore all sections in a single transaction, check the exact
   recorded ledger and fixed aggregate/schema/privacy SQL, and run `pg_amcheck`.
6. Inspect and retire only the generated name/label/image/optional-CID join, using
   the returned exact CID for removal; require the final selector empty.
7. Retire the owned snapshot, then publish outcome and freshness in that order.
   Keep the sole lock through output, then close/unlock and sample the original
   owner after runtime/mask restoration and signal unregistration.

The helper `restore_verify.verify` alone does not perform the source outcome,
manifest,64MiB snapshot or freshness admission. The offsite program invokes that
helper with a different configured archive/resource domain. Existing VM backup
coverage also calls helpers, so it is not whole-CLI evidence for this new route.

## One clock, cleanup debt and native profiles

The entry `Instant` precedes pure option parsing. Exactly one Owner is constructed
from that Instant and decoded N/M before effects; parsing time is not refreshed.
Body B=N-M and final end N remain fixed. The isolated-create phase reserves R=160
inside B: selector30 + inspection30 + exact-ID remove30 + final selector30, plus
four existing5-second TERM/5-second reap intervals. This is minimum command debt;
filesystem, scheduler and blocked OS/daemon latency remain additional constraints.
Required explicit1200/120 is an example, not a product default, measured capacity
or a guarantee that every reference-positive workload fits.

Normal create/restore work uses live B-R. Known joined cleanup can use original B
without clearing a previous failure, resetting the clock or borrowing M. M is for
local file/lock/runtime/stdio/signal finalizers. A deadline, new signal, changed
witness, cleanup error or blocked kernel call can still prevent retirement. Keep
attempted/dispatched/raw effect/reaped/closed facts separate; local client death
does not prove daemon work stopped. In-container120-second restore/amcheck timeouts
remain unchanged, as do the server SQL limits.

The reference has no operation-wide Python clock. Its readiness loop tests a
30-second boundary only after errors and can accept a later success; the native
profile is deliberately stricter. Each readiness child and its original
settlement debt must fit the same live30-second phase. Post-observation fences
refuse late success and no new attempt is admitted without remaining debt. Only
a completed settled nonzero readiness result may retry; I/O, EOF, close, group,
role-deadline or signal uncertainty stays a native refusal. Named oracle rows
must retain the actual reference-positive late result alongside native refusal.

`NativeIO.timed_out` keeps its existing inherited-primary meaning. The new
`role_timed_out` fact records current invocation admission/body/settlement/reap/end
observations only. A prior Owner Deadline remains the final negative primary but
cannot fabricate a timeout in a later actually settled cleanup role. New local
timeout, natural EOF, raw0, group/reap, close and live-B predicates remain required.
An actual nonconsuming terminal observation can skip optional TERM only; exact
reserved-group retirement still precedes the single consuming reap, including
live descendants of a terminal leader. Query errors stay negative. No PERM error
is ignored and no already recorded primary is cleared.

Reference cleanup deliberately recovers an ambiguous create through its fresh
internally generated nonce name, fixed label, current exact image and optional
observed CID. The native route preserves only that join, then removes the exact
returned CID and checks absence. This is not Stage/deploy name cleanup and is not
a public arbitrary-name or image selector. A complete actual raw0/naturally
fenced stdout CID is retained before later log/pipe/close failures; saved JSON or
cleanup-generated EOF cannot create such an effect fact or make final success.

Readonly source inputs accept existing positive link counts and retain their
original binding/hash; private writers keep their one-link rule. The fixed
installed SQL has an explicit root0644 readonly profile, distinct from private
root0600 files and Caddy admission. Canonical absolute/no-follow path custody,
fixed SQL permissions and the existing bounded JSON/numeric/Unicode profile are
native acceptance qualifications. Any reference-positive exception must be an
exact named paired oracle row, with raw original effects retained, not a blanket
failure alias or inferred universal Python compatibility.

## Safe adapters and failure semantics

The implementation reuses private retained `Record`/`Records`, byte JSON, manifest,
actual flock Lease and existing NativeIO bodies. Visibility changes reach only
attestation siblings. Restore-labelled Owner methods forward to the existing
exclusive B-R/B machinery; no additional logical owner, shared atomics, global
scope, clock or public callback framework is introduced.

Both actual0600 stdout/stderr originals are admitted before each child and
written during its live body under existing2MiB caps. The optional stderr sink
and duplicate stdout capture are private, explicit restore opt-ins; old callers
retain None/false. Duplicate capture is forbidden above2MiB and the two sink
originals must differ. Extra capture uses at most another2MiB; it never duplicates
a4GiB bulk archive. Each observed prefix/hash, natural EOF, raw signed status,
pipe close, reserved group and one reap remains independent. No sink append occurs
from refusal/cleanup drainage. Checked-close refusal does not erase an already
observed complete create identity; unknown output never becomes a known ID.

Inherited input uses a readonly original. Stable std1.99 Command/Stdio consumes
one clone whose parent close has no safe checked result. That SDK-owned duplicate
close is explicitly opaque (C-DTOR-FAIL); every retained original still receives
its own checked close, stamp and position-independent hash. The child may advance
the shared cursor. No full-input-consumption acknowledgement or successful live
restore is inferred from that cursor or client status alone. Alternatives require
a pipe pump and an explicit EPIPE/full-consumption policy; neither is silently
introduced. Fixed role/ledger SQL uses checked original anonymous readonly files;
its bytes and private preparation effects are retained in the source contract.

Original file generations are bound before atomic replacement and parent fsync.
Failed writes retain their actual bounded prefix; partial/invalid temporary JSON
must not be normalized away. The snapshot may be unnamed only after its original
binding and known written-prefix size/hash are established. Refused cleanup still
attempts every known original close. The sole actual lock is released after all
payload input/log owners and output. Lock aliases and readonly directory
capabilities settle afterward, before the final Owner fence. No duplicate unlock
or second flock is used as proof of the original acquisition.

The known-CID effect-observation comparison exception is the named
`create-timeout-after-effect` row: the original lower child actually returns raw0
and a full CID, then its delivery wrapper raises TimeoutExpired before the caller
receives it. Native first Deadline plus an observed hook, actual raw0, naturally
fenced body EOF, valid exact CID and bulk facts may retain that known CID in the
final passive `restore.json`, where the reference remains empty. Both operations
must fail and remove the same exact joined CID in the same order. The comparator
retains both raw reports/calls and may normalize only that one field on cloned
records for that exact row. Generic timeout, nonzero, unknown IDs and cleanup-only
EOF are excluded. The report grants no authority and native known effects are
not erased to pretend a matching reference observation.

The genuine restore oracle has a finite counter-only source-order table for
`backup-oversized`, `backup-mode` and `backup-symlink`. The first two require their
exact deterministic regular-file size/hash/mode/link pins (64MiB+1 at 0600, or
4128 bytes at 0755), with Native's first cause Policy. The symlink row requires the
literal `database-before.dump.original` target and one link, with first cause Io
at the no-follow open. Its complete actual original pin, including the platform's
observed mode, must equal Native's; no portable symlink mode is invented.

Every table row additionally requires both statuses 1, no daemon commands, absent
final snapshots and exact raw Native 0/original 1 unlink-attempt counts. All source
pins, full product trees, directory modes, lower effects and delivered original
log pins still compare strictly. Native refuses before snapshot admission. Python
opens the target before its metadata/size require for the regular-file rows; on
symlink-open failure it never opens the target, but its outer finally still calls
unlink(missing_ok=True). No native cleanup action or ownership is fabricated for
an uncreated snapshot. Every other named row retains exact unlink-count equality.
There are no missing/FIFO/directory/wrong-owner backup-input rows in this 90-case
catalog; no generic unsafe-input profile is inferred from the table.

Native first-cause preservation is deliberate. Exact paired body+cleanup or close
faults keep the known native primary while still recording/attempting later peer
failures. The Python finally error can replace an earlier exception. Only finite
named paired rows may express that difference; all raw reference records and
command/FS/known-ID/finalization facts stay available. Unrecognized failure text
is never public output.

## Applicable API decisions and validation boundary

The affected components apply the adopted checklist as follows:

| Checklist area | Affected decision |
| --- | --- |
| C-CASE, C-GETTER, C-WORD-ORDER, C-METHOD | Fixed route and role names. |
| C-CUSTOM-TYPE, C-NEWTYPE, C-VALIDATE | Typed roles and actual retained capabilities. |
| C-NEWTYPE-HIDE, C-STRUCT-PRIVATE | No live resource reconstruction from shape. |
| C-RW-VALUE, C-INTERMEDIATE | Borrow original inputs/sinks; retain typed partial facts. |
| C-CALLER-CONTROL, C-COLLECT | Explicit capacities, overflow and one original clock. |
| C-GOOD-ERR, C-FAILURE | Fixed public failure, first cause and fallible projections. |
| C-DTOR-FAIL, C-DTOR-BLOCK | Explicit finalizers and the narrow SDK-close exception. |
| C-SERDE | Passive records alone serialize. |
| C-SEND-SYNC, C-COMMON-TRAITS | No new public auto-trait or resource-type promise. |
| C-DEBUG, C-DEBUG-NONEMPTY | No public resource debug surface or secret interpolation. |
| C-CTOR, C-BUILDER, C-NO-OUT | One fixed operation, no public builder/root override. |
| C-GENERIC, C-OBJECT, C-SEALED | Concrete private adapters, no callback registry. |
| C-CRATE-DOC, C-EXAMPLE, C-QUESTION-MARK | Harmless informational example. |
| C-LINK, C-HIDDEN | Scoped guide, prerequisites and final-status authority explicit. |
| C-STABLE, C-RELNOTES, C-METADATA | Existing unpublished package/release decisions. |
| C-PERMISSIVE, C-FEATURE | No dependency/feature delta or complete license-audit claim. |

C-CONV/C-CONV-TRAITS/C-CONV-SPECIFIC add no public conversion family.
C-ITER/C-ITER-TY add no named iterator API. C-NUM-FMT/C-BITFLAG add no public
numeric-wrapper or flag family. C-OVERLOAD/C-DEREF/C-SMART-PTR/C-STRUCT-BOUNDS add
no public operator, pointer or generic data structure. C-EVOCATIVE/C-MACRO-ATTR/
C-ANYWHERE/C-MACRO-VIS/C-MACRO-TY add no macro API. The adopted
[54-entry inventory](rust-api-checklist.md) remains the authority; unchanged
components and unrelated rows are not retrospectively certified here.

Generic collection abstractions, extension traits, public builders, new FFI/unsafe,
network/TLS/database drivers, arbitrary command execution and installed bootstrap
APIs are outside this change. Existing external Docker and PostgreSQL utilities
retain their explicit trusted-host roles. No new default TLS or dependency graph
is adopted.

The first full production snapshot compiled for Linux GNU. Nine real inert
pipe regressions passed on their exact source; the same nine cases against the
old TERM branch produced eight passes and one prior-Deadline cleanup failure.
That combined old assertion did not record an errno, so it is not an observed
PERM claim. The final test separates the EOF and cleanup-error diagnostics.
The corrected Python producer passes all 90 scenarios in 15 tests. The exact
isolated Rust comparison also passes all 90 scenarios (92.25 seconds of test time),
with no ignored scenario or blanket failure alias. Its five comparator regressions,
423 ordinary Ops unit tests, 71 integration tests across 19 CLI targets, 11 doc
tests and GNU all-target Clippy passed on the bound source. These are local inert
process/filesystem comparisons; actual signed musl/full host CLI/isolated
PostgreSQL/amcheck/free-VM validation remains unrun. CI registration configures
one sole exact ignored process in the existing `host` reference group, after
receive and stage, with `--test-threads=1`. The ordinary Ops command adds
`release_backup_cli`; broad all-features execution skips the unique oracle
basename instead of bypassing its sole-process guard. Existing four groups,
local execution receipts and ten-minute job caps remain unchanged. The tracked
workflow/command plan and both native/external oracle inputs already participate
in the existing fingerprints; regression tests bind changed inputs to refusal of
old success receipts. Repository integration passed full workspace Rust quality,
the ordinary Ops/CLI gates, 2,001 Python tests with 17 expected skips, five
automation tests, 20 fingerprint tests, 11 doctests, the container input tests
and workflow lint. Full local automation took 525.854 seconds; the performance
target remains unmet. A new same-head hosted and signed runtime run is pending.

The first repository comparison also passed all 90 scenarios in 169.64 test
seconds, while its dynamic census included 1,829 installed controller-environment
Python files. Both oracle implementations now prune only `ops/ansible/.venv`.
All 292 explicit archived originals and the reference declarations remain
required. New real-file properties preserve adjacent and newly maintained
source inclusion, mutation sensitivity and missing-original refusal. The
corrected 16 Python tests, six Rust comparator/census tests, Ops Clippy and exact
90-scenario comparison passed; the latter took 91.39 test seconds locally.
These timings are local observations, not a hosted capacity guarantee. This
test-only census correction changes no operational Python or native input.

The recorded failed final-file-sync comparison exposed a test-fixture error:
the producer lost the captured initial/created/final phase before its fsync
adapter ran. The fixture now retains the exact path and phase, and its regression
checks all nine final write/sync targets after the full 14 actual lower roles.
The operational Python and native implementation were unchanged by that repair.
Earlier failed trials remain retained. No source, mock or helper-only result is
claimed as real-host runtime proof.
