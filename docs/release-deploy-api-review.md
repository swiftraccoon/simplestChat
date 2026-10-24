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

The VM controller/guest source has 22 passing inert tests. Actual authenticated
signed-musl HostDeploy, Docker, PostgreSQL, Caddy/TLS and image rollback execution
remain unrun for this fixture. Installed callers, bootstrap and production
migration policy remain unchanged; no runtime or adoption proof follows from the
local comparison.
