# Coding standard

This standard applies prospectively to newly authored and changed first-party
simplestChat code: the Rust server and protocol, load generator, `xtask`, Ops
tooling, browser TypeScript and maintained JavaScript, Python, shell and Ansible
integration.
Existing code has not been retrospectively audited against this adoption. Such an
audit is separate work; this document is not a compliance claim.

Upstream and vendored code retains its own conventions. Keep our changes
identifiable, record patch provenance, and avoid unrelated restyling.

## Adopted sources and authority

| Source | Adopted revision | Authority |
| --- | --- | --- |
| [Rust Style Guide](https://doc.rust-lang.org/nightly/style-guide/) | [rust-lang/rust `0ef4e1154335de75d5c333bf61e192ec99758fb3`, style-guide sources](https://github.com/rust-lang/rust/tree/0ef4e1154335de75d5c333bf61e192ec99758fb3/src/doc/style-guide) | Source and manifest conventions. |
| [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/) | [rust-lang/api-guidelines `19f0f62ecd0f56a0b7adaa332c352bc2476506ea`](https://github.com/rust-lang/api-guidelines/tree/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src) | Every applicable entry in the checklist, identified by its `C-*` identifier. |
| [Google Rust Style Guide](https://google.github.io/styleguide/rustguide.html) | [google/styleguide `3cf4f72013826804b7442896ce84d3777cf89ae0`](https://github.com/google/styleguide/blob/3cf4f72013826804b7442896ce84d3777cf89ae0/rustguide.md) | Supplemental implementation and design guidance; the repository interpretation retained from the adopted baseline. |

The commit links are normative; live links are convenient reading copies.
Toolchain updates do not update these guide revisions. Record a deliberate
source-adoption change when changing the baseline.

Precedence is the user's instructions, the two explicit authority assignments
above in their respective domains, then compatible Google guidance. Google's
precedence rule for its own codebases does not override these assignments. For
an actual conflict, cite both sections, retain the explicit authority, and record
the resolution. A platform or dependency limitation needs a narrow documented
exception with evidence, rather than an unrecorded relaxation.

This adoption adds no permission gate to already authorized work. Routine
applicability decisions and implementation choices proceed within the task's
existing scope; changes to the adopted authorities or language boundaries need
an explicit decision in that scope.

## Language and implementation boundaries

Rust is the default for new backend and maintained tooling implementation,
including small host automation when it merits maintained code. Use existing
tools for routine research and inspection; this is not a reason to recreate
shell commands, mature parsers or upstream tooling in Rust.

Preserve the established language boundaries:

- Browser application code remains TypeScript. Playwright suites and browser
  harnesses remain in their browser/Node ecosystem; Vite-adjacent scripts remain
  there when that integration is their purpose.
- Existing Python references and executable oracles remain available until their
  replacement operation has its own complete contract and evidence. A pure
  parser or policy helper does not replace a runtime controller.
- Ansible playbooks, templates, plugins and installed Python operations retain
  their authorized roles. Source-level Rust implementation alone does not
  authorize a host callsite switch or prove authenticated binary distribution.
- Scanner drivers, authenticated installers, platform bootstrap and other
  deliberate keepers retain their existing boundaries. Workflow YAML, Dockerfile,
  Caddy configuration and SQL remain appropriate integration formats.

These are maintained implementation boundaries, not blanket style or correctness
exemptions. New work inside them follows their existing strict quality gates.
When a genuinely new capability lacks viable Rust integration, document the
missing capability and alternatives: an existing binding, a minimal safe
dependency adapter, an upstream integration or another implementation language.
Resolve the choice within the authorized task rather than silently broadening
the language policy or reopening already settled decisions.

Keep reusable protocol, validation, calculation and workload logic separate from
HTTP/WebSocket adapters, database access, browser presentation, process ownership
and native bindings. Preserve intended behavior across adapters: payloads,
capacities, validation, deadlines, errors and completion conditions. Prefer a
small direct adapter over a general framework without a concrete need.

## Formatting, toolchains and dependencies

Follow the pinned official chapters for
[source layout](https://github.com/rust-lang/rust/blob/0ef4e1154335de75d5c333bf61e192ec99758fb3/src/doc/style-guide/src/README.md),
[items and imports](https://github.com/rust-lang/rust/blob/0ef4e1154335de75d5c333bf61e192ec99758fb3/src/doc/style-guide/src/items.md),
[statements](https://github.com/rust-lang/rust/blob/0ef4e1154335de75d5c333bf61e192ec99758fb3/src/doc/style-guide/src/statements.md),
[expressions](https://github.com/rust-lang/rust/blob/0ef4e1154335de75d5c333bf61e192ec99758fb3/src/doc/style-guide/src/expressions.md),
[types](https://github.com/rust-lang/rust/blob/0ef4e1154335de75d5c333bf61e192ec99758fb3/src/doc/style-guide/src/types.md),
and [Cargo manifests](https://github.com/rust-lang/rust/blob/0ef4e1154335de75d5c333bf61e192ec99758fb3/src/doc/style-guide/src/cargo.md).
Use `rustfmt` for mechanical formatting and review manifests and documentation
it does not cover. Do not add nightly-only formatter options merely to match a
supplemental preference.

The current workspace declares Rust `1.99.0` and language edition `2024` in
`Cargo.toml`; `rust-toolchain.toml` pins that compiler with rustfmt, Clippy and
rust-analyzer, and `rustfmt.toml` selects style edition `2024`. Preserve these
distinct decisions. The pinned
[style-edition chapter](https://github.com/rust-lang/rust/blob/0ef4e1154335de75d5c333bf61e192ec99758fb3/src/doc/style-guide/src/editions.md)
contains historical release-status wording, not today's compiler-support matrix.
Declared package `rust-version`, the selected compiler and tested target/feature
combinations are different facts. Do not infer additional MSRV, cross-target,
static-linkage or runtime compatibility evidence from those declarations.

Before introducing another target or toolchain, record its exact compiler,
edition, components, native libraries and runtime requirements. Prefer stable
tools; scope a necessary nightly or platform requirement to the component that
needs it. Host-only optional TLS or native functionality must not silently change
the default bootstrap dependency graph.

Commit the workspace lockfile for maintained applications and record tool,
submodule, native-library and binary-input versions or hashes where required.
Do not treat a cache hit, caller-supplied digest, successful compilation or static
CRT flag as authenticated provenance or runtime proof. Set real package metadata;
do not invent authors, licenses or websites to satisfy a checklist. Review
licensing deliberately before distribution without changing upstream terms.

## Commit messages and coherent changes

Use Conventional Commits 1.0.0 under the local
[commit standard](commit-standard.md). Group a completed coherent purpose with
its tests, documentation, lockfiles, generated outputs and required review
bindings. A feature's parser, owner, fault tests and runtime callers belong to the
same theme; a chain of helper-only checkpoints is not a completed operation.
Keep unrelated changes separate. Publication, release, tag, push and deployment
actions follow their existing task authorization; commit syntax grants no new
authority and adds no new confirmation requirement.

## API review and typed contracts

The pinned API checklist contains **54 active entries**. Its complete inventory
and review procedure are in [rust-api-checklist.md](rust-api-checklist.md).
For future changes, review applicability and record evidence for affected
entries. Reuse a component's applicability record rather than repeating all
54 rows for every small change. The initial inventory remains pending; it is
not an assessment of the current workspace.

Use **applies**, **not applicable** with a concrete reason, **exception** with
scope and evidence, or **pending**. Pending is not compliance. Private code needs
no speculative general-purpose public surface, but privacy does not waive
naming, safety, correctness or any guideline whose actual scope applies. Read
the pinned explanatory section and its qualifications, not only its short title.

Document public behavior and useful examples, sharing one realistic example
across related items where permitted. State the target, feature, native service
and execution prerequisites of an example. Distinguish compiled, simulated and
actually executed examples. Keep implementation details private unless callers
need a representation commitment.

Use types to distinguish identifiers, credentials, durations, byte counts,
protocol versions, lifecycle states and result statuses where confusion can
affect correctness. Do not force one universal status or ownership model across
different runtimes. Validate external data before constructing trusted types.
Retain numeric, boolean, omitted, null and supplied-value distinctions required
by the contract; bounds and narrower accepted domains must be explicit.

In TypeScript, accept external JSON as `unknown`, validate the relevant shape
and then narrow it. A type assertion or caller-selected generic is not runtime
validation. Preserve the existing action-selected request/response contracts,
exact optional properties, checked indexed access and separate browser/Node
ambient environments. Generated protocol types do not replace receiver-side
validation or serialized-form tests.

## Host, async and native applicability

| Situation | Repository application |
| --- | --- |
| Server and protocol | Separate wire validation, authentication/authorization, room state and database transactions from transport delivery. Preserve compatibility and distinguish accepted work from delivered or persisted work. |
| Load generator and benchmarks | Keep reusable workload definitions and calculations separate from engine, browser and sampling adapters. Preserve units, capacities, completion checks and failure evidence. |
| `xtask` and controller tooling | Use normal `std` facilities and bounded private adapters. Separate source inspection from execution, acquisition from authentication, and retained artifacts from install authority. |
| Ops binary and host adapters | Record the actual target, ABI and runtime requirements. Preserve bootstrap, provenance, lock/journal and installation ownership contracts before changing installed callers. |
| Browser ownership | Account/room/device intents own asynchronous results. Validate the owner after an await before applying state, while retaining and retiring every resource already acquired. |
| Reusable records | Evaluate `C-SERDE` for configuration, wire and result records. Do not serialize live handles, secret fields or incidental internal state merely because a derive is available. |
| Error and diagnostic types | Preserve `C-GOOD-ERR` through meaningful composition and fixed privacy-safe public failures. Keep sensitive context in appropriately controlled evidence, not public `Display`, `Debug` or receipts. |
| Reader/writer interfaces | Apply `C-RW-VALUE` to the actual synchronous or asynchronous trait contract, borrowing and cancellation behavior. Do not invent a generic I/O framework solely to resemble an example. |
| Resource handles | Review common traits individually. Do not derive `Copy`, `Clone`, `Default` or sharing traits when they duplicate ownership, expose authority or construct invalid states. |
| Bounded collections | Evaluate `C-COLLECT` against explicit overflow behavior. Use checked operations and a narrow recorded exception when an infallible collection trait would hide failure. |

These mappings are review questions, not blanket exemptions. Record relevant
component, target and compiler evidence with the affected decisions.

## Safety, concurrency and resource lifetime

First-party Rust **forbids `unsafe`** and **denies warnings**, including unmet lint
expectations. This adoption preserves those workspace rules. Use maintained safe
interfaces; unsafe operations inside native dependencies do not authorize unsafe
first-party wrappers. A need that cannot be expressed under this policy requires
a separately justified policy/dependency decision, not an inline bypass.

At native and FFI dependency boundaries, document ABI/layout assumptions,
ownership, pointer/buffer lifetimes, callback context, error conversion and
teardown order relevant to the safe interface. Validate foreign statuses rather
than manufacturing Rust enum values. State panic behavior where a foreign
boundary is involved; do not assume unwinding across it is supported. Keep
generated bindings identifiable with their input/version provenance.

Treat Tokio tasks, blocking workers, callbacks, browser documents, subprocesses
and native media workers as distinct execution contexts. Check reentrancy,
thread affinity, `Send`/`Sync`, ordering and shared-state assumptions for the
actual adapter. A single application thread does not eliminate callback or
native concurrency. Volatile access is not a synchronization protocol.

State which operations may allocate, block, wait, take locks or require a
particular execution context. Review lock ordering and contention; do not hold
a blocking lock or a `RefCell` borrow across `.await`. Add async when the contract
benefits from it, rather than for uniformity alone.

Every asynchronous operation needs an owner. Specify cancellation behavior and
what dropping an in-progress future does to borrowed buffers, queued messages,
files, processes, tracks or partial evidence. Register acquired resources before
accepting a late result; stale-owner refusal must not discard cleanup ownership.
An aborted timer does not cancel an already-running operation. Detached UI work
needs an explicit rejection boundary; background Rust work needs an owned
completion or shutdown path.

Make shutdown explicit when it can fail or block (`C-DTOR-FAIL`, `C-DTOR-BLOCK`).
Attempt all relevant peer finalizers even after one fails. Preserve the first
known failure and report cleanup uncertainty. A destructor or forced drop can be
a fallback without being positive evidence of successful cleanup. Distinguish
process exit, process-group settlement, pipe completion, descriptor close,
protocol closure and application success; none automatically proves the others.
Claims about runtime internals or escaped processes need their own evidence.

For bounded operations, use the original monotonic deadline, reserve the known
remaining work/finalization tail and fence positive observations after awaits.
Do not reset a deadline after setup, retry or cancellation. State any scheduling
allowance honestly: a timeout does not guarantee that OS I/O, reaping or close
cannot stall.

## Diagnostics, privacy and measurements

Represent unsupported, skipped, unavailable, timed out, cancelled, failed and
passed distinctly when the operation distinguishes them. Missing evidence is not
a numeric zero or a successful result. Preserve useful typed status at adapter
boundaries while keeping raw external errors and private data out of public
diagnostics. Use `Result` and `Option` according to the contract; panic is not
ordinary input, connection, capacity or device-error handling. Explain rare
`expect` calls justified by an invariant. Tests may assert expected behavior.

Keep retry and recovery outside measured regions unless explicitly part of the
workload. A recovered connection or restarted process does not erase a failed
trial. A prepublication report may describe earlier observations while a later
write, close, replay, signal or deadline still makes the operation fail; document
which final result is authoritative.

Record logging destination, level, buffering, dropped events and whether logging
is disabled during timing. Avoid formatting, extra I/O, heap growth and control
round trips inside measured regions unless they are the workload. Bound queues
and captures, define overflow behavior, and distinguish accepted/postparse bounds
from raw parser or whole-process resource limits.

Never expose access tokens, cookies, ICE credentials, private messages, raw SDP,
secret-bearing native errors or private configuration in logs, debug formatting,
public receipts or commits. Keep private evidence bounded and access-controlled;
retain it only under the operation's evidence policy. A caller-supplied string
must not become an unreviewed public diagnostic.

Define workloads with explicit inputs, relevant seed, warmup, repetitions,
synchronization points and completion validation. Record timer source, units,
resolution, conversion, wrap handling and observer overhead. Distinguish
deterministic inputs from network, browser, hardware and scheduler variability.
Record build profile, optimization/LTO, target/CPU, enabled services, executor
configuration and workload identity when they affect comparisons. Disclose
different compilers or implementations rather than attributing every effect to
one component. Preserve samples and outliers under the privacy/evidence policy;
use [performance guidance](performance.md) for comparisons. Compilation and
correctness tests alone do not establish capacity, latency or media quality.

## Verification, exceptions and review records

Run the relevant formatters, compiler/lint and documentation checks, and
meaningful tests for the affected supported targets/features. Use the commands
and prerequisites in [testing.md](testing.md) and [CONTRIBUTING](../CONTRIBUTING.md).
Record the exact commands and outcomes; generic test instructions do not prove
an execution strategy. Default and optional-feature bootstrap graphs need their
own relevant checks when changed.

Use pure tests and inert executable references for parsing, calculations,
contracts and fault handling. Bind reference bytes and interpreter/provider
domains where they affect results. Fake process or network fixtures do not
prove actual engine, native-media, TLS, database or browser behavior. Use those
runtime checks and manual device journeys when relevant and authorized; identify
unrun checks explicitly. Source-level ownership proofs do not authorize unrelated
workloads, installations or releases.

Fix lint causes. Keep a needed expectation or exception narrow, named and
reasoned; do not weaken unsafe/warning policy, widen a ratchet or edit vendor
warnings to hide an unrelated finding. Do not add tests that merely restate
implementation details; choose checks capable of finding meaningful failures.

A future change's review record identifies affected components and contexts,
relevant `C-*` decisions, ownership/native/blocking changes, commands/results,
limitations and exceptions. A change summary or linked component documentation
can carry that record; a new long checklist file is not required for every edit.
An exception names the rule/source, reason, smallest scope, alternatives,
consequence and revisit trigger. Routine justified applicability decisions do
not add a user approval step. Existing implementation outside the change is not
silently certified or scheduled for retrofit by this prospective adoption.

## Supplemental Google guidance

Apply compatible guidance from the pinned Google source without asserting that
this workspace or the entire guide has received an independent compliance audit.

| Topic | Repository interpretation |
| --- | --- |
| Google precedence | The user-assigned official style/API authorities above prevail in their domains. |
| Chromium/Fuchsia layout, imports, testing and threading | Product-specific mechanisms are not imported automatically; review the actual server, browser and Tokio context. |
| Formatting options | Stable rustfmt and the official conventions govern. |
| Trait implementations | Retain `C-COMMON-TRAITS` with explicit ownership/privacy review. |
| Lifetime/generic naming | Retain official/API conventions where recommendations differ. |
| Constructors and examples | Retain `C-CTOR`/`C-EXAMPLE`, including their qualifications. |
| Initialization | Keep startup, side effects and resource ownership explicit; do not invent an embedded startup requirement or silently add pre-main work. |

Relevant pinned sections include
[API context](https://github.com/google/styleguide/blob/3cf4f72013826804b7442896ce84d3777cf89ae0/rustguide.md#user-content-api-design-context),
[unsafe](https://github.com/google/styleguide/blob/3cf4f72013826804b7442896ce84d3777cf89ae0/rustguide.md#user-content-unsafe),
and [initialization](https://github.com/google/styleguide/blob/3cf4f72013826804b7442896ce84d3777cf89ae0/rustguide.md#user-content-global-initialization).
