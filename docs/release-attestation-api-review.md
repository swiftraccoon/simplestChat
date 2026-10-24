# Server attestation preparation: affected API review

This prospective review covers the new offline `release attestation prepare`
operation and its direct CLI integration. It does not assess the existing Ops
inventory retrospectively. The normative authorities remain
[coding standard](coding-standard.md), [commit standard](commit-standard.md),
and the [54-entry API inventory](rust-api-checklist.md).

## Surface, types and scope

The new public surface is `release::attestation::prepare::run(&[OsString])`,
returning an `ExitCode` for one dedicated CLI invocation. Internal selectors,
run/attempt newtypes, parsed metadata, original files, directory references,
clock/signal owner and lifecycle state stay private. Success creates unsigned
metadata; no decoded value or success bit establishes cryptographic verification,
producer authentication, installation or deployment permission.

- **C-CUSTOM-TYPE, C-NEWTYPE, C-STRUCT-PRIVATE — applies.** Selection validates
  repository/revision domains and constructs distinct run/attempt types only
  after the producer's strict ranges. Whole/reserve budgets use `Duration` and
  joint validation. Internal custody and its private constructors retain actual
  handles and witnesses; data decoding never creates live authority.
- **C-DEBUG, C-COMMON-TRAITS — applies by type.** Small non-secret causes and
  selectors have useful debug output. Descriptor/directory/owner types expose
  no path/content diagnostics and do not gain speculative `Copy`, `Default` or
  public serialization. Only private directory references clone an existing
  shared owner; final positive closure requires exclusive remaining ownership.
- **C-SERDE — applies to data only.** The predicate is constructed from validated
  known scalars and a sorted digest map, preserving the reference serialization.
  Raw input encodings/ignored fields are hashed/copied unchanged. Shared Rust
  trust decoding validates claim shape without conferring signature authority.

## Readers, writers and native lifetime

- **C-RW-VALUE — applies.** The informational/status writer borrows the existing
  stdout descriptor while holding and first flushing its standard lock. Its
  private `Write` adapter preserves actual kernel write errors, without raw-FD
  adoption, duplication, flag changes or ownership transfer. Writes and flush
  are checked; fixed diagnostics never format private I/O errors.
- **C-FAILURE, C-QUESTION-MARK, C-GOOD-ERR — applies.** Public status is 0/1/2;
  operational failure uses one fixed JSON class and usage is fixed. The private
  first cause is latched before later callbacks/finalizers can replace it.
  Every admitted file and directory receives its close attempt even after body,
  deadline, signal or close failure. Partial/reference output prefixes remain.
- **C-SEND-SYNC — applies to actual contexts.** The two-worker Tokio runtime
  drives the existing signal registration while synchronous bounded-chunk I/O
  runs under one original owner. Private `Rc` custody remains on that operation's
  thread. No file work is detached and no additional child/group framework is
  introduced. Blocked kernel I/O is outside a hard cooperative wall bound.
- **C-VALIDATE — applies.** Canonical absolute/nofollow regular
  inputs are an explicit new Rust profile. Existing phase-local byte limits,
  byte-aware JSON bounds, strict booleans/numbers and late exclusive UTC expiry
  are documented alongside Python differences. There is no new arbitrary
  depth/path/FD ceiling, source/SBOM schema or VEX reachability policy.

## Documentation, dependencies and evidence

- **C-EXAMPLE, C-CRATE-DOC — applies.** The public function documents budgets,
  prerequisites, statuses, cooperative limits and unsigned scope, with harmless
  help rustdoc. The operator guide explains exact original-four/copies-four/
  predicate-one files and post-required/read-only preparation before data-only
  signing. Testing documentation distinguishes real native/CLI/whole-reference
  fixtures from actual signing, host or deployment evidence.
- **C-PERMISSIVE — applies to the affected change.** This operation introduces
  no dependency, manifest, feature or licensing change. The existing locked
  graph and embedded affected Tokio/runtime MIT notices remain. This is not a
  complete license inventory or a retrospective compliance claim.
- **C-STABLE — not applicable to a stable-crate promise.** The unpublished 0.1.0
  crate gains a documented binary operation; this record does not promise stable
  representation for private state or certify unrelated APIs.

The original Python implementation remains unchanged. On 2026-10-24, the
source-bound native Rust 1.99.0 `aarch64-apple-darwin` gate passed seventeen
operation tests and four actual CLI tests. The exact ignored whole-reference
test passed all 72 scenarios in 11.868 seconds; its independent Python driver,
nine tests, fourteen unchanged attestation tests and strict lint/types also
passed. GNU Linux default all-target Clippy passed, as did the affected xtask
automation, required-gate and local-CI suites (3, 4 and 28 tests respectively).
A shared-target cache trial with zero matched tests is retained as unproven;
source timestamps were refreshed and the successful rerun required actual
matched counts. It is not counted as runtime evidence.

The native CLI also processed copied original production inputs with a
300,304,896-byte image in 6.003 seconds. All five metadata outputs matched the
unchanged Python operation byte-for-byte; the original signed cache was not
modified. This is actual local offline I/O, not a new signature, API selection,
host installation or deployment. The source controller and evidence file hashes
are retained in the private validation record.

The adjoining Git source-metadata fix was then validated in the same candidate.
The complete native all-features Ops gate passed 248 library tests, 43 CLI tests
and four doc tests, with the four owned/reference helpers explicitly ignored.
Both exact reference tests were run separately: 112 builder and 72 preparation
scenarios passed. GNU Linux all-target Clippy and the full workspace Rust check
passed; the latter included formatting, warning-free Clippy, generated contracts
and private Rustdoc, in 165.843 seconds. These GNU checks compile the target;
they do not establish Linux execution.

The full Python run completed 1,659 cases with seventeen explicit opt-in skips
and one stale workflow-skip expectation. That expectation was corrected and its
complete 33-test suite passed. Strict lint/format/types and 57 focused Python
cases also passed; the additional corrected fixture passed lint/format. The
helper gate passed all 575 cases after admitting the new Rust directory through
the exact deny-by-default Docker context, and the final fast security gate
passed in 32.965 seconds. Failed trials and exact source hashes remain separate
in the private validation record.

Actual hosted read-only preparation, signing and host/runtime evidence remain
separate completion requirements. Local unsigned metadata and native tests do
not establish those authorities or close the remaining Tier 2 operations.
