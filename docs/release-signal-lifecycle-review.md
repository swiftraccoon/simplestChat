# Release signal lifecycle: affected review

This prospective correction covers the existing server attestation prepare,
cached verification, local fresh verification and SSH fetch commands. It is
separate from the new host receiver operation. It reuses the applicability
records in [prepare](release-attestation-api-review.md),
[cached verification](release-verification-api-review.md),
[local verification](release-fresh-verification-api-review.md), and
[fetch](release-fetch-api-review.md). It does not audit the workspace
retrospectively or change the adopted coding and commit standards.

## Original owner and final completion

Previously each command dropped its Tokio signal receivers with the inner
owner, then joined runtime workers and, where applicable, restored umask. Its
last outer test sampled only time. An INT or TERM delivered during that outer
restoration could therefore leave a previously computed success unchanged.

The same original `control::Owner` now registers two safe atomic signal actions
before the command creates runtime workers. It remains alive through complete
body/output/peer finalization, actual runtime destruction and umask restoration.
`Owner::finish(self)` attempts removal of every admitted registration, samples
its retained flags and checks the original deadline. The existing outer clock
fence then runs before returning success. Registration admission, unregister or
late signal failure cannot produce exit zero. No deadline, phase or reserve is
restarted. Existing output shapes, usage/operational statuses and command
selectors remain unchanged; a nonzero exit invalidates earlier passive output.

Atomic bits retain delivered INT/TERM observations until final destruction.
Local seen bits permit the existing opportunity to write a negative journal
after the first cancellation observation; they do not clear an atomic bit or
the primary cause. Repeated identical signals after an already known primary
coalesce. This is not a claim of indefinitely identical Tokio event consumption.
If both kinds are pending before a check, the existing INT-before-TERM sampling
order remains; this does not establish their kernel arrival order.

This remains a dedicated synchronous CLI API. The safe SDK removes our actions;
it does not promise to restore the operating system's previous default signal
handler. There is no promise about signals after the final fence, undelivered
pending signals or arbitrary blocking kernel I/O. The existing runtime-entry
panic prerequisites remain documented on the public functions.

## Dependency and distribution boundary

The direct dependency is exact `signal-hook =0.4.5`, with default features off.
The pinned crate archive is 46,951 bytes with SHA-256
`b29ffa27d3d59a049bb328258de100f8012848d1b08d444e08bbc02ac468435e`.
Its declared license is MIT OR Apache-2.0. This distribution selects its exact
MIT notice: 1,068 bytes, SHA-256
`503558bfefe66ca15e4e3f7955b3cb0ec87fd52f29bf24b336af7bd00e946d5c`.
The notice is embedded in the existing cached/fresh/fetch notice chain;
`release attestation verify --licenses` can display it without an operation runtime.
It is an affected dependency notice, not a complete binary license inventory.

The resolved lock adds only this package and the Ops dependency edge.
The default musl normal/build package graph changes from 59 to 60 packages;
`signal-hook` has no enabled features. Its two dependencies, libc and
signal-hook-registry, already occur in that graph. The retained upstream
build script is a no-op without `extended-siginfo-raw`; optional cc is disabled.
No TLS library, authenticated subject set, installer contract or binary artifact
layout changes. Package resolution alone proves neither target execution nor
static/native compatibility.

## Affected guideline decisions

- **C-CUSTOM-TYPE, C-STRUCT-PRIVATE, C-VALIDATE — applies.** The consuming owner
  and its registration IDs stay private. Only fixed INT/TERM numbers are
  registered; no caller supplies an arbitrary handler, registry or lifecycle.
- **C-GOOD-ERR, C-QUESTION-MARK, C-FAILURE — applies.** SDK admission maps to the
  existing fixed I/O cause. Earlier primary causes remain latched. Final
  unregister failure is checked, all known IDs are attempted, and public
  failures contain no handler state, credentials, paths or SDK exception text.
- **C-DTOR-FAIL, C-DTOR-BLOCK — applies.** Explicit consuming completion decides
  success. Drop only attempts refusal/unwind cleanup; its ignored result never
  establishes successful completion. No extra worker or detached waiter is
  introduced. Existing cooperative kernel I/O qualifications still apply.
- **C-COMMON-TRAITS, C-SEND-SYNC, C-DEBUG, C-SERDE — applies to actual scope.**
  Registration ownership is neither cloned nor serialized, and no new public
  handle or representation commitment is introduced. Signal callbacks use the
  maintained safe SDK and `Arc<AtomicBool>`; first-party code adds no unsafe
  implementation, raw pointer callback or unsafe trait assertion. Compiler
  evidence for changed code remains pending below.
- **C-RW-VALUE — applies.** The affected exact MIT notice uses the existing
  checked writer-by-value chain. No new generic reader/executor framework is
  introduced. Existing bounded process/file owners retain their interfaces.
- **C-EXAMPLE, C-CRATE-DOC, C-STABLE — applies.** Existing public function
  examples and flags remain intact. This record narrows the changed lifecycle
  and dependency claim. It grants no host installation, caller migration,
  cryptographic or deployment authority.

## Validation status

Pinned Rust 1.99.0 formatting, all-target compilation, workspace all-feature
Clippy and private Rust documentation checks passed. Original Python references
remain unchanged. Native and reference results are recorded below; real
signatures, TLS, installed-host execution and deployment remain separate.

Four new ordinary tests supervise only their exact owned self-test child for
both real INT and real TERM. Each child completes an inert operation, closes
its files/process groups/pipes and drops the actual runtime. Fresh verification
and fetch restore their temporary umask; prepare and cached verification leave
the inherited mask unchanged. The signal is raised after that teardown at the
final owner boundary. Prepare verifies all
four copies and the predicate; cached verification emits a complete passive
envelope; local verification's cached branch emits the unchanged four-key
result; fetch emits its completed passive report and observes its SSH fixture
retirement. The final signal must make the operation status nonzero while
preserving its typed cause and original clock. The supervisor must itself
retire, reap and close every peer before assertions. There are no new ignored
worker routes or silent skips.

Focused execution uses the ordinary Ops library test filter
`late_real_signals_after_runtime_restore_refuse_`. The existing original-end
and retained-hash signal tests remain part of the required Ops suite. These are
inert native process and policy checks, not real signatures, TLS or host proof.

Root validation on 2026-10-24 passed all-target compilation and 391 ordinary
Ops checks: 325 library checks, 59 CLI checks and seven documentation tests.
The four new owned-child routes exercised both actual INT and TERM after
runtime shutdown and applicable umask restoration. The four preserved Python
comparisons also passed all 359 scenarios: 72 prepare, 111 cached verification,
103 fresh verification and 73 fetch. The independent source review covered
this backport separately from the host receiver.

These executions include the separate original-file identity correction.
They establish local native and inert-reference behavior; hosted Linux checks,
signatures, installed-binary runtime and deployment require separate evidence.
