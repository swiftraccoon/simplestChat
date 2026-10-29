# Native host application deployment: affected API review

This prospective review covers the complete fixed-host `release deploy` action,
its embedded Stage operation, direct borrowed asset store, retained input/bulk
process adapter and file generation checks. The local
[coding standard](coding-standard.md), [commit standard](commit-standard.md) and
[54-entry inventory](rust-api-checklist.md) apply. Existing installed Python,
controller, Ansible, maintenance, database restore and bootstrap remain unchanged.

The only new public operation is synchronous `release::deploy::run`. It requires
root on the fixed prepared Linux/amd64 Docker host, separately trusted release
inputs and explicit N/M. No report, saved JSON, filesystem hash or daemon ID
authenticates the binary or grants installation authority.

- **C-CASE/C-GETTER/C-WORD-ORDER/C-METHOD**: Existing role, file, guard and owner
  names are retained. New APIs remain private to the concrete composition.
- **C-CUSTOM-TYPE/C-NEWTYPE/C-NEWTYPE-HIDE/C-VALIDATE**: Whole/reserve durations,
  actual flock Lease, retained file generations, fixed child roles, natural body
  EOF, signed status and all-peer settlement are distinct. A saved report cannot
  reconstruct a guard, input descriptor, Docker effect or recovery permission.
- **C-SERDE/C-STRUCT-PRIVATE**: Only passive journals/reports serialize. Metrics
  integers retain the pinned Python Nd/separator/4300-digit domain and original
  numeric meaning without conversion through f64. Live callbacks, files,
  processes, credentials and owners never serialize.
- **C-COLLECT/C-CALLER-CONTROL**: Explicit positive 0<M<N<=1800 owns all phases.
  Ordinary child captures remain 2 MiB per stream, physical buffers 64 KiB.
  Browser capture remains 33 MiB; dump output is at most 4 GiB and current free
  space minus 1 GiB. Poll loops use the original source intervals and clocks;
  overflow refuses rather than silently dropping evidence.
- **C-RW-VALUE/C-INTERMEDIATE**: Bulk output borrows the original RW file. The
  subsequent list role inherits a separately admitted read-only capability to
  that same inode. Position-independent hashes preserve custody despite the
  child's shared input offset. Completed `pg_restore --list` is neither full
  input-consumption acknowledgement nor proof of an actual database restore.
- **C-DTOR-FAIL/C-DTOR-BLOCK**: Every known first-party original has an explicit
  finalizer, including negative paths. The borrowed asset arena closes before
  its Lease clone drops; only the sole parent unlocks. Exact group retirement
  precedes one consuming reap. Late close/output/runtime/mask/signal refusal
  invalidates earlier complete or partial positive output.
- **C-GOOD-ERR/C-FAILURE**: The original primary cause remains latched. Fixed
  role/status retry handles readiness and metrics completed refusals only;
  resource, EOF, deadline or close uncertainty stays fatal. Recovery cannot
  clear the primary or turn a failed deployment into success. Public messages
  never interpolate configuration, daemon output, metrics token or raw errors.
- **C-CTOR/C-BUILDER/C-NO-OUT/C-GENERIC/C-OBJECT/C-SEALED**: Constructors admit
  the fixed operation. Private adapters serve this operation and its closed
  oracle; there is no public command builder, callback registry or root override.
- **C-CRATE-DOC/C-EXAMPLE/C-QUESTION-MARK/C-LINK/C-HIDDEN**: Harmless help is the
  executable example. The public entry documents nested-runtime panic behavior;
  examples require no Docker, database, TLS or host privilege.
- **C-STABLE/C-RELNOTES/C-METADATA/C-PERMISSIVE/C-FEATURE**: Existing unpublished
  package metadata, default graph, dependencies and notice scope remain. No new
  dependency or feature is needed; compilation is not target/runtime authority.

C-CONV/C-CONV-TRAITS/C-CONV-SPECIFIC introduce no public conversion family.
C-ITER/C-ITER-TY add no named iterator API. C-COMMON-TRAITS/C-SEND-SYNC/C-DEBUG/
C-DEBUG-NONEMPTY apply to existing public owner decisions: no new public resource
type gains Clone/Copy/Send/Sync/Debug. The private Lease clone borrows the one
actual held flock and cannot unlock it. Private Rc file custody remains on the
operation thread; the original registered signal flags survive runtime teardown.
C-NUM-FMT/C-BITFLAG add no public numeric wrapper/flag family. C-OVERLOAD/C-DEREF/
C-SMART-PTR/C-STRUCT-BOUNDS add no public generic or operator API. The private
ScopedFs borrows one existing implementation; its Deref is not a public promise.
C-EVOCATIVE/C-MACRO-ATTR/C-ANYWHERE/C-MACRO-VIS/C-MACRO-TY add no macro API.
Unaffected entries remain outside this focused review.

## Original order and recovery boundary

One original entry Instant includes parsing, owner/handler/mask setup, Stage,
archive checks, old/new runtime identity, exact running configuration hashes,
healthy DB, absence of a retained migration container, loopback readiness, exact
live migration ledger, isolated packaged migrations, full candidate configuration
comparison, both browser captures, public readiness, quiet observation, database
size/headroom, dump/list/fsync/hash/receipt, immutable selection copies and journal.

Stage executes inside the same State, attempt, Lease and Owner. Its standalone
600-second budget is never restarted. Every validator and capture settles before
the replacement reservation becomes active. Schema equality is exact; maintenance
and DB restore are not fallback deployment policies.

Body B=N-M contains a source-derived recovery reservation R=576 seconds:
diagnostics120, stop55, up70, application inspection80, two readiness loops91 and
dependency inspections160. These include the original role caps and 5+5-second
process settlement. N1800/M60 is an explicit example, not a default, measured
guarantee or caller increase. Filesystem/hash work and blocked kernel calls can
still make recovery refuse. The final room observation and timestamp precede
the admission that the full first stop55 fits before the replacement flag.

Only an error inside that flagged replacement try grants one known-owned
rollback attempt. Recovery uses B and the original signals, ignores the primary
solely for that bounded known work, and never clears it. Late asset close,
unlock, outcome, stdout, runtime or outer lifecycle failure remains negative
without inventing another application rollback after the completed body.

## Ownership and explicit native differences

The fixed previous image is checked against its runtime profile/revision; the
new image is additionally bound to the selected verified archive graph, including
either its config ID or selected image-manifest ID. Asset containers are never
started. Only raw zero plus a valid complete natural-body CID grants exact-ID
retirement. The named `assets-create-unknown` and `assets-old-create` cases differ
from Python's generated-name removal: native keeps unfinished evidence and does
not remove by name after unknown/nonzero creation. A known zero removal effect
is recorded before later log/close failure; it cannot cause a second removal.

Atomic writes retain the previous opened generation, verify it immediately
before replacement, then update only the two generations affected by that exact
rename. Logs close promptly after sealing; final anchored inode/metadata/hash
checks reject persistent replacements. Such observations do not eliminate a
same-UID writable-filesystem swap/restore race or authenticate Docker.

The temporary readonly list input closes at its original command boundary.
Removing that one record relocates later deferred log positions before checked
close; earlier input/writer positions and independent sealed witnesses remain
unchanged. A failed list still preserves both log prefixes and the original
command cause through a later close refusal. The passive backup SHA is assigned
after dump validation, publication and directory sync, before fallible receipt
publication. That digest can survive receipt failure without proving that a
receipt or completed backup was published.

The narrow Snapshot checkpoint extension borrows FnMut only for each synchronous
call; it is never stored. Existing standalone APIs delegate to the same bodies.
Live Owner checks surround fixed chain metadata/open/read/seek/close boundaries,
and admitted descriptors remain tracked if a positive checkpoint refuses.

**C-DTOR-FAIL exception:** stable Rust 1.99 consumes a File in `From<File> for
Stdio` (`library/std/src/process.rs`), stores its owned FileDesc in Unix command
stdio (`library/std/src/sys/process/unix/common.rs`), and closes that SDK-owned
parent duplicate through OwnedFd Drop (`library/std/src/os/fd/owned.rs`). The
safe API exposes no take-stdin/checked-close result. This one transferred clone's
close is not positively observed. The separately retained original is checked
and explicitly closed; child/group/reap/output facts remain required. A pipe pump
would change original inherited-file/early-reader semantics and require a new
EPIPE/consumption policy; unsafe adoption or retrying a consumed descriptor is
excluded. Revisit if a safe SDK checked-close interface becomes available.

The existing Stage negative consuming `ChildStdout`/`ChildStderr::from_std`
conversion boundary also remains: an Err consumes that SDK argument, so the
rejected handle's close status is opaque. Refusal stays negative and every other
retained peer is explicitly settled; it is not checked-close proof for that
consumed descriptor.

Canonical nofollow/protected host paths, the existing bounded byte-JSON and
archive profiles, 256 MiB Docker/curl executable limits and current target assumptions remain
explicit. OS filesystem/random/read/close/reap stalls are not a hard wall.

`Caddyfile` is a distinct readonly input: the operational playbook installs it as
root-owned `0644`, while private fixtures may retain `0600`. Only that fixed name
admits those two approved modes and readonly positive hardlink counts, preserving
the exact observed owner/group/mode/link/length/time/hash and namespace through
final checks. Every private selection/credential/writer retains its existing
`0600`/single-link admission. Other Caddy modes/owners are a documented native
refusal profile, rather than permission to change installed configuration modes.

Four named comparisons preserve established typed helper/checked-close diagnostics:

- `archive-digest`: Python reports exactly
  `Image archive SHA256 does not match release manifest`; native reports exactly
  `native_archive_refused` from the shared Stage archive verifier.
- `assets-old-invalid`: Python reports exactly `AssetError`; native reports exactly
  `asset_tar_path` from the shared browser asset capture validator.
- `candidate-up-close` and `candidate-up-close-rollback-up`: Python reports exactly
  `OSError`; native reports exactly `native_close_refused`. The native checked-close
  cause remains distinct from an ordinary I/O error, which still reports `OSError`.

Each comparison requires its exact scenario and both exact raw failure strings
before projecting only that field on cloned report/outcome comparison records.
Raw reports/traces, first causes and every other field, effect, file and verdict
remain intact. No other scenario, helper code or failure-class alias is admitted.
The two close fixtures model post-completion refusal; their comparisons also
require native primary `Close` and an observed original fault injection.

Browser mount mismatches report only the sorted fixed reviewed field names,
with `bind` appended last, preserving the original diagnostic for those fields.
A differing unknown field uses the fixed native diagnostic `Browser asset proxy
mount contains unreviewed fields`; unlike Python's raw-key suffix, it cannot echo
an arbitrary private key or value. An extra null retains Python's missing/null
comparison behavior. This is a diagnostic profile, not an accepted unsafe mount.

## Validation state

The complete source and closed 97-case unchanged-Python producer are authored.
The producer's 17-test inert suite and all 97 original raw observations completed.
The complete 97-case native comparison passed locally on macOS in 365.12 test
seconds (367.785 seconds wall), with exactly one selected ignored comparator and
stable source throughout. It compares the unchanged full Python action using
real private files, locks and owned inert child processes. Docker, database and
readiness replies are inert adapters. Ordered calls, original inode/byte custody,
reports, journals, retained prefixes, explicit closes and final verdicts remain
strict except for the specifically documented profiles above.

The dedicated local command was:

```sh
cargo test --offline --locked -q -p simplestchat-ops --lib \
  release::attestation::deploy::oracle_tests::whole_deploy_matches_unchanged_python \
  -- --ignored --exact --test-threads=1
```

The focused list-input regression passed both failed-list branches, including
later checked-close refusal. Those production bodies passed full workspace Rust
quality in 20.789 seconds before the final test-only fixture/documentation updates.
GNU all-target Clippy passed on the corrected fixture in 2.504 seconds. The
corrected ordinary Ops suite passed 484 tests: 406 library tests and 78 CLI/documentation
tests, with ten owned helpers/reference tests ignored. The 576 helper tests also
passed. The final fixture correction changes only test code; all production
bodies remain identical to the passing 97-case comparison.

The earlier ordinary run retained 405 passes and one suffix-test failure at its
group-retirement/reap assertion. Fresh-process diagnostics observed EOF before
kernel terminal in 12 of 100 trials; the exact initial group failure was not
reproduced. The fixture now derives its original window from the existing
five-second TERM and five-second reap phases and joins nonconsuming terminal
status before expecting natural zero. It retains every prior byte/close assertion
and calls the same production finalizers. The failed trial remains retained.

The VM setup transition checks its original setup deadline before granting the
full body. Epoch-independent tests cover the strict boundary and an observed
clock advance after a successful two-call budget check; a late grant remains
refused with setup still active. This also avoids a test's captured real-clock
default factory accidentally using host uptime instead of its intended epoch.

The VM controller/guest source has 23 passing inert tests. Actual authenticated
signed-musl HostDeploy, Docker, PostgreSQL, Caddy/TLS and image rollback execution
remain unrun for this fixture. Installed callers, bootstrap and production
migration policy remain unchanged; no runtime or adoption proof follows from the
local comparison.


## Failed rollback diagnostics

An attempted rollback that returns failure now includes the optional closed
`rollbackFailure` object in its existing report and `outcome.json`. Its fields
are `stage`, `boundary`, `cause` and nullable `logOrdinal`. The first three use
private finite enums; no exception text, arguments, configuration, identifiers,
paths or output is copied. The original application failure, rollback booleans,
process status and final owner verdict remain authoritative and unchanged.
Successful operations and successful rollbacks omit this field.

The trace is active only during mandatory recovery. Recovery admission, the
stop, individual selection reads and writes, asset selection, startup, image
checks, readiness, dependency continuity and final journal have fixed stages.
Command boundaries distinguish original checks and role admission, retained
log creation, execution, settlement, nonzero status, evidence sealing and final
witnesses. A returned executor error or completed nonzero is retained before
later evidence writes. The deliberately best-effort diagnostic command block
cannot become this mandatory recovery failure, and the trace never replaces
the original owner's first cause or starts a further recovery attempt.

`logOrdinal` names an admitted stdout/stderr log pair, not proof of dispatch or
command completion. Pre-admission and noncommand failures have no ordinal. The
actual positive native integer is retained without introducing a production
command cap. The fixture's separate last-eight log reader still recognizes only
ordinals 1 through 192; its scan bound does not constrain this passive field.
Malformed, late or unknown fixture observations never supply cleanup authority.

This is an explicit additive native diagnostic profile. The unchanged Python
reference has no such object. The genuine comparison validates the exact
addition for twelve named failed-rollback cases before removing only that field
for the legacy comparison, independently for stdout reports and `outcome.json`.
Actual native reports and raw retained tree observations remain intact. All
other fields, selected files, modes and command effects remain strict. A
successful or unrelated case cannot use this profile.

The Python VM harness and its tests remain frozen at the published reference.
Their failed-report decoder does not project this new diagnostic. A Rust
replacement for that orchestration is required for its signed runtime capture;
no additional Python implementation is part of this change. The native
extension does not establish the cause of the retained signed `47094af` failure
or a corrected signed runtime. Native compiler/unit checks and the genuine
reference comparison are recorded separately from that runtime requirement.

The native validation passed GNU/musl all-target Clippy, warning-free private
Rustdoc, the full host Ops library suite (444 passed, 11 existing ignored
fixtures), seven current comparison-contract tests, and all 97 whole-operation
reference cases. The genuine comparison took 420.808 seconds wall locally. The
reference producer and all tracked Python files remain unchanged. These are
inert/native filesystem and process checks, without a real engine or signed VM
trial of this diagnostic extension.

## Native runtime controller and guest composition

The manual deploy fixture now has a Rust controller entry,
`xtask ops-artifact deploy-runtime`, and a distinct signed guest entry,
`release maintain-prerequisite deploy-guest`. This is a fixture orchestration
change. The production `release deploy` operation, installed caller policy and
all frozen Python references retain their established roles. The retained
`47094af` / `40237650754` trial remains a failed rollback trial; the new source
and its local tests do not establish its cause or a corrected signed result.

The controller keeps current Ops and current server selectors separate from an
explicit previous server revision, run, attempt and artifact ID. Both server
acquisitions use the existing verifier and retained-tree owners; their original
packaged migration maps must be equal before fixture effects. Expected hashes,
JSON observations and completed-role counters do not construct authenticated
input authority. Source-built GNU tools remain trusted controller prerequisites;
the retained current signed musl executable is the guest operation authority.

`C-CUSTOM-TYPE`, `C-VALIDATE`, `C-STRUCT-PRIVATE` and `C-SERDE` apply to the closed
deploy purpose, explicit selectors, opaque acquisition owners and exact passive
request/result domains. The shared private `Owners`, `Runner`, `Original`, VM,
prepared-overlay, QMP and session adapters remain the lifetime owners. The deploy
purpose retains one 4500-second original interval, a 180-second final reserve,
a 600-second setup ceiling and the 3720-second continuation debt for two complete
N1800/M60 calls plus observation. No phase, restart or diagnostic starts another
outer clock. Existing maintenance purposes retain their own profiles.

`C-DTOR-FAIL`, `C-DTOR-BLOCK` and `C-RW-VALUE` apply through explicit original
replays, all-known finalization and the terminal session/VM verdict. A result
frame remains passive until the final session exit, stream closure, original
witnesses, QMP/VM settlement and signal/clock checks succeed. Earlier JSON cannot
certify later closure. Same-UID swap-and-restore limitations and blocking-kernel
limitations of the reused owners remain; this is no new isolation claim.

`C-GOOD-ERR` and `C-FAILURE` apply to capture before ordinary success validation.
The guest and controller retain a closed 14-field `failureCapture` on negative
results, including independent stdout/outcome decode states and rollback
objects. Each rollback object has the existing four producer fields. Two
independently valid but differing objects remain available in a negative
capture; disagreement still refuses ordinary report/outcome validity. Native
stdout flags are never inferred from an independently decoded outcome. Unknown,
invalid and actual zero values stay distinct. The old command classification's
1..192 range does not limit the producer's nullable positive `logOrdinal`.
No raw command, exception, configuration, credentials or stream bytes are public
fields, and diagnostics never add cleanup or retry authority.

The source contracts cover the manual/read-only workflow boundary, credential
exposure after unprivileged compilation, exact selectors, fixed preparation
paths, private copies, pins and original outer/inner budgets. They execute no
workflow, Ansible, Docker or KVM operation. Actual signed execution and successful
rollback qualification remain separate evidence requirements.

The existing optional `maintain-guest` feature also enables the already locked
Rustix dependency's `mount` API for the fixed Linux namespace adapter. The default
controller dependency graph retains its previous features and needs no native
TLS or mount implementation. This narrow safe binding replaces the fixture's
Python mount client; it introduces no package or first-party unsafe code.
The inherited executable handoff uses an owned read-only duplicate under the
same process-wide gate as every guest spawn. Only that duplicate temporarily
changes CLOEXEC; its flags and checked close settle before releasing the gate.
The source descriptor retains its flags, and a spawned child remains registered
if later flag restoration or close fails. The namespace entry checks and closes
the inherited duplicate before any further subprocess, then executes its own
separately admitted canonical executable original.

The fixture's dump-list observation passes a read-only duplicate through standard
process stdin while retaining and replaying the original dump. Standard-library
ownership makes that duplicate's close status opaque; the original close and all
returned child/group/pipe outcomes remain checked. A successful list is still
neither a restore rehearsal nor acknowledgement of complete dump consumption.

Deploy's routine command/health boundaries compare retained file and parent
metadata, matching the frozen deploy controller's original checks. This applies
only to the closed deploy profile. Initial admission, actual reads and copies,
prepared/native/trial acceptance, final successful acceptance and descriptor
finalization retain full hash replays. Existing maintenance profiles keep their
full boundary checks. The distinction avoids adding repeated whole-archive
hashing to the unchanged 20-second inspection service; it is source parity,
not a measured speedup or an explanation of any historical failed trial.

The first signed Rust trial exposed a request-profile mismatch before guest
state construction: a newly hard-coded PostgreSQL 17 prefix rejected the actual
configured and frozen-reference PostgreSQL 18.6 pin. `C-VALIDATE` applies to the
corrected fixed-profile predicate and its configuration-backed request regression,
including obsolete reference and malformed/altered digest refusals. Existing
maintenance request semantics, source ownership and all clocks retain their
previous contracts. The [runtime proof record](release-deploy-runtime-proof.md#first-signed-rust-trial-request-profile-refused-before-a-prepared-result)
separates the actual failed trial, independent finalization facts and joined
source diagnosis from the fresh signed execution still required after correction.

The second signed trial admitted that request but captured a baseline setup
deadline before either native transaction. Review found that the new baseline
waited for the long-running migration server to exit before stopping it. The
correction restores the frozen readiness, stop, zero-exit and removal sequence
under its original clocks. `C-VALIDATE`, `C-DTOR-FAIL` and `C-DTOR-BLOCK` apply to
the held migration network namespace, revalidated container identity, bounded
readiness subprocesses and explicit descriptor/process settlement. The
[second trial record](release-deploy-runtime-proof.md#second-signed-rust-trial-baseline-setup-deadline)
keeps the actual coarse deadline observation separate from the source diagnosis
and does not attribute measured time to an unrecorded subcommand.

Further baseline review found that the grants input contains both SQL and a psql
meta-command. It must reach psql on stdin, as in the frozen fixture; `--command`
cannot represent that input. The correction uses the existing retained-file stdin
owner and an interactive Docker exec with the fixed PostgreSQL actor. It keeps
the exact input bytes, error-stop setting, child deadline, original-file checks
and checked child settlement. As with the existing dump-list input, the standard
library owns the transferred stdin duplicate and its close status is opaque;
the retained original's checks and explicit close remain observable. This is a
source-parity correction before another signed trial, not a diagnosis from an
observed grants failure.
The baseline app readiness call also uses the frozen 45-second cap instead of
the adapter's 120-second local allowance. This narrows that phase without changing
the original setup clock or the separate continuity profile.

The third signed trial completed baseline and positive deployment, then exposed
an unsuccessful native rollback. `C-GOOD-ERR`, `C-FAILURE` and `C-RW-VALUE` apply
to separating a command's current result from the operation owner's retained
first failure. The shared process completion remains cumulative; the deploy
adapter must independently establish recovery command closure and fresh failure
observations before accepting that command. A successful recovery command never
clears the original operation failure. Inert tests execute the real native
adapter and process owner; source or fixture success remains distinct from signed
rollback qualification. The [third trial record](release-deploy-runtime-proof.md#third-signed-rust-trial-positive-deployment-passes-rollback-is-refused)
retains actual positive and negative facts without inferring the unexported raw
stop status from a cumulative executor error.
