# Cached server verification: affected API review

This prospective record covers the complete new `release attestation verify`
operation, its scoped rawzip dependency/notice and private sharing with the
existing preparation operation. The normative sources are the local
[coding standard](coding-standard.md), [commit standard](commit-standard.md) and
[54-entry API inventory](rust-api-checklist.md). It is not a retrospective audit.
The entries below apply to the authored surface; the remaining inventory is pending.

- **C-CUSTOM-TYPE, C-NEWTYPE, C-STRUCT-PRIVATE:** exact passive nine-field
  selection uses distinct private artifact/run/attempt types and validation.
  Caller run/attempt bounds are below `2^53`; preparation retains its distinct
  producer bound below `2^31`. Original File/Dir/tool/process owners remain
  private and never deserialize from metadata or proof JSON.
- **C-SERDE, C-VALIDATE:** strict original claims and envelopes reuse existing
  typed trust data. Successful GH statement equality preserves Python numeric
  semantics using the existing JSON representation; it does not strictly
  reinterpret a signed nested predicate. Whole-operation completion requires
  actual maintained verifier success and exact selected certificate/subjects,
  not a decoded success record. Native JSON/ZIP/path limits are documented.
- **C-RW-VALUE:** the private notice writer accepts a generic writer by value.
  Checked borrowed stdout writes preserve actual kernel failures. ZIP reads
  borrow the original retained File; the mature parser owns no replacement FD.
  Fixed heap buffers persist across cancellable awaits; no raw-FD adoption or
  unsafe directory/ZIP decoder is introduced.
- **C-FAILURE, C-GOOD-ERR, C-QUESTION-MARK:** fixed public 0/1/2 statuses keep
  raw errors and credentials private. The original monotonic owner includes
  setup, all four processes, revalidation, every known peer close, runtime and
  stdout settlement. A failure known before output attempts fixed negative JSON;
  a late refusal can leave an emitted complete/partial passive envelope, which
  any nonzero final status invalidates. First cause survives later cleanup;
  actual signed child status is retained separately. Kernel-blocked latency is not a hard bound.
- **C-DTOR-FAIL, C-DTOR-BLOCK:** positive completion requires explicit checked
  peer settlement, natural pipe EOF and reserved-group retirement before reap.
  Destructors are fallback only. Read/close/conversion errors remain negative;
  independent peers still receive their finalizer attempts before assertions.
- **C-SEND-SYNC, C-DEBUG, C-COMMON-TRAITS:** the dedicated two-worker runtime
  drives the one original signal registration during synchronous checks. Private
  Rc custody stays on its operation thread and is not detached. Passive causes,
  selected identities and observations gain useful traits only; no speculative
  public cloning, serialization or Default creates live authority.
- **C-EXAMPLE, C-CRATE-DOC:** the public function documents standalone sync
  context and its Tokio panic precondition, required budgets, statuses and
  cooperative limits. Harmless help rustdoc and operator/test guides separate
  cached verification from fresh API selection, downloads, signing and hosts.
- **C-PERMISSIVE:** rawzip `=0.5.2` is already locked with crate SHA-256
  `ac9999039f74cd73c03158cddfe4362991703c74efe5ce5c8073026f80a5e48c`.
  Its exact MIT body is 1022 bytes without terminal LF, SHA-256
  `2af4d13698993a15849a869b4c2ed4954cf251768ad145f371774adb96697e0c`,
  embedded in a scoped checked notice route. This adds the existing `std`/`alloc`
  package edge with zero normal dependencies. The default Linux musl normal/build
  graph was checked at 58 packages,
  with only rawzip added to the previous 57. Package versions and checksums
  otherwise remain unchanged; rawzip adds no dependency edges. Cargo-deny alone is
  not attribution, and this limited notice
  does not claim a complete workspace inventory.
- **C-STABLE:** this unpublished binary operation adds no stable representation
  promise for private owners. The signed server ten-file/four-subject and separate
  Ops three-subject payload shapes stay unchanged.

The unchanged production Python files remain the reference. The final closed
111-case driver passed ten Python tests, strict Ruff/format/basedpyright and its
standalone shared-offset/ZIP64 cases. The original 238 Python reference hashes
remain unchanged. An earlier 105/108-case set passed before independent review
found central-versus-local flag handling and equal-offset overlap differences.
Those trials remain separate; the final correction preserves Python's tie order,
warning-only shared-offset exception and arbitrary-precision logical extent while
clamping reads to the original bounded ZIP. CRC, output length and hashes remain
required. The new cases cover both central orders and a ZIP64 compressed extent
of `u64::MAX` without increasing physical input or output limits.

The final native 111-case comparison passed in 20.994 seconds. All 271 Ops library,
50 CLI and five Rustdoc tests passed in 37.414 seconds, including fifteen new
verification unit/process tests and seven new CLI tests. The shared preparation
operation retained its seventeen native tests, four CLI tests and separate
72-case comparison. Native and GNU Linux all-target Clippy, workspace formatting,
private Ops Rustdoc, Python quality and fast security passed after the final ZIP
correction. The earlier complete snapshot also passed workspace Rust quality
(146.579 seconds), all 575 helper tests, the 33-case CI policy regression and
native automation/required/local-CI checks; their unaffected source was retained.
The initial test compile exposed a JSON-macro fixture expression, and initial
production Clippy rejected a constant test-clock fallback. Both were corrected
without lint exceptions; failed trials and source bindings remain recorded.

Actual source-built Rust 1.99.0 `aarch64-apple-darwin` verification passed with four
fresh maintained GH calls on both the currently deployed signed `6a3acc8` release
and the prior signed `a841e2c` release. The measured durations were 25.369 and
25.457 seconds. Both produced 1,825 bytes of stdout, empty stderr and envelopes
identical to the unchanged Python reference; all eleven copied inputs remained
unchanged. These are correctness observations with network/cache variability,
not a controlled performance comparison. Both selections were copied from prior
authenticated release caches, so these checks establish no fresh API selection.

The composed source with the separate builder teardown correction passed 328
Ops tests in 37.943 seconds, native and GNU Linux all-target Clippy, formatting
and private Ops Rustdoc. Its newly built combined binary also verified the current
signed release with four fresh GH calls in 25.593 seconds, again matching the
Python envelope exactly with all eleven inputs unchanged and empty stderr.

Independent reviews cover the final process/CLI/oracle lane and the separate
state, shared custody, ZIP, tool, policy, runtime and output-finalization lane.
The dedicated CLI requires final exit 0: a later output/runtime/deadline failure
can invalidate an already emitted full or partial passive envelope. Source
compilation and these native checks do not establish authenticated Linux musl
distribution or installed-caller readiness. No caller, installer or host route is
switched by this implementation; its distribution/runtime and before-SSH caller
proof remain separate gates.
