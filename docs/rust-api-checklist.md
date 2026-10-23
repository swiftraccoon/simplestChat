# Rust API review inventory

This prospective inventory covers every active checklist entry at
[rust-lang/api-guidelines `19f0f62ecd0f56a0b7adaa332c352bc2476506ea`](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/checklist.md).
There are **54** entries. Links below retain the adopted explanatory sections
exactly. Read those sections for the rule and its qualifications; a trigger is
only a project review prompt, not a replacement rule.

The scope is first-party Rust in the server/protocol, load generator, `xtask`
and Ops tooling. The governing authorities and the host, async, native and
browser mappings are in [coding-standard.md](coding-standard.md). Existing
TypeScript, Python, Ansible and browser/scanner boundaries remain as described
there; this Rust checklist does not turn those ecosystems into Rust libraries.

Every row starts **pending**. No existing crate or module is certified by this
inventory, and this adoption performs no retrospective audit. The user has
reserved that audit for later. Actual affected decisions and their evidence
belong in a change's review record or linked component documentation.

## Review procedure

For a new crate, assess all 54 entries and record its supported targets/features,
compiler and intended API/release surface. For a future change to an existing
component, review the affected rows and reuse its established decisions. A
missing existing record does not require a full retrospective crate review
before the affected change can proceed: create the affected decisions, leave
unreviewed entries pending, and do not claim broader compliance.

Reconsider prior applicability when a public API, ownership model, target,
feature, dependency or release commitment changes. Identify the crate/component
and change with the record. One concise review in the change summary or linked
module documentation can cover the affected entries; do not duplicate a new
54-row checklist for each small edit.

Use **applies** with implementation/check evidence, **not applicable** with the
specific absent construct or scope, **exception** with its narrow reason and
consequence, or **pending** when unresolved. Do not relabel an inconvenient
applicable rule as not applicable. An exception records the source/rule, affected
scope and target, alternatives, consequence and revisit trigger, following the
coding standard. Routine justified decisions create no additional approval
step for work already authorized.

Private application APIs do not need a speculative public-library surface.
They still need the naming, validation, ownership and failure contracts whose
pinned rules apply. Link evidence rather than marking a row complete merely
because a formatter, type checker or test command passed. A pending or unrun
entry is not compliance, and a focused review is not a whole-workspace audit.

| Guideline and pinned explanation | Applicability trigger in this repository | Initial state / evidence |
| --- | --- | --- |
| [C-CASE](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/naming.md#user-content-c-case) | Authored Rust identifiers | Pending |
| [C-CONV](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/naming.md#user-content-c-conv) | Conversion methods | Pending |
| [C-GETTER](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/naming.md#user-content-c-getter) | Accessors | Pending |
| [C-ITER](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/naming.md#user-content-c-iter) | Collection iteration | Pending |
| [C-ITER-TY](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/naming.md#user-content-c-iter-ty) | Named iterator types | Pending |
| [C-FEATURE](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/naming.md#user-content-c-feature) | Cargo feature definitions | Pending |
| [C-WORD-ORDER](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/naming.md#user-content-c-word-order) | Related identifier families | Pending |
| [C-COMMON-TRAITS](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/interoperability.md#user-content-c-common-traits) | Exposed server/tooling types; review ownership and secret exposure | Pending |
| [C-CONV-TRAITS](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/interoperability.md#user-content-c-conv-traits) | Conversions between types | Pending |
| [C-COLLECT](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/interoperability.md#user-content-c-collect) | Collections, queue bounds and explicit capacity failures | Pending |
| [C-SERDE](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/interoperability.md#user-content-c-serde) | Wire DTOs, configuration and safe result/evidence records | Pending |
| [C-SEND-SYNC](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/interoperability.md#user-content-c-send-sync) | Exposed types, Tokio/native callbacks and thread affinity | Pending |
| [C-GOOD-ERR](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/interoperability.md#user-content-c-good-err) | Public failures, error composition, typed statuses and privacy | Pending |
| [C-NUM-FMT](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/interoperability.md#user-content-c-num-fmt) | Numeric, unit and bit-oriented wrapper types | Pending |
| [C-RW-VALUE](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/interoperability.md#user-content-c-rw-value) | Synchronous/asynchronous I/O traits, borrowing and cancellation | Pending |
| [C-EVOCATIVE](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/macros.md#user-content-c-evocative) | Authored macro interfaces | Pending |
| [C-MACRO-ATTR](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/macros.md#user-content-c-macro-attr) | Macros producing items | Pending |
| [C-ANYWHERE](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/macros.md#user-content-c-anywhere) | Item macros and supported invocation scopes | Pending |
| [C-MACRO-VIS](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/macros.md#user-content-c-macro-vis) | Macro-generated public/private items | Pending |
| [C-MACRO-TY](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/macros.md#user-content-c-macro-ty) | Macro inputs containing type expressions | Pending |
| [C-CRATE-DOC](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/documentation.md#user-content-c-crate-doc) | Server, load-test, xtask and Ops library entry points | Pending |
| [C-EXAMPLE](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/documentation.md#user-content-c-example) | Public APIs and examples with actual feature/service prerequisites | Pending |
| [C-QUESTION-MARK](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/documentation.md#user-content-c-question-mark) | Examples with fallible operations | Pending |
| [C-FAILURE](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/documentation.md#user-content-c-failure) | API errors, panics, unsafe preconditions | Pending |
| [C-LINK](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/documentation.md#user-content-c-link) | Related API, feature, target and operator documentation | Pending |
| [C-METADATA](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/documentation.md#user-content-c-metadata) | Package manifests and publication intent | Pending |
| [C-RELNOTES](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/documentation.md#user-content-c-relnotes) | Versioned releases | Pending |
| [C-HIDDEN](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/documentation.md#user-content-c-hidden) | Exported documentation surface | Pending |
| [C-SMART-PTR](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/predictability.md#user-content-c-smart-ptr) | Authored smart pointers | Pending |
| [C-CONV-SPECIFIC](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/predictability.md#user-content-c-conv-specific) | Placement of conversion APIs | Pending |
| [C-METHOD](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/predictability.md#user-content-c-method) | Operations associated with a value | Pending |
| [C-NO-OUT](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/predictability.md#user-content-c-no-out) | Return values and caller-owned buffers | Pending |
| [C-OVERLOAD](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/predictability.md#user-content-c-overload) | Operator implementations | Pending |
| [C-DEREF](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/predictability.md#user-content-c-deref) | `Deref`/`DerefMut` implementations | Pending |
| [C-CTOR](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/predictability.md#user-content-c-ctor) | Initialization interfaces | Pending |
| [C-INTERMEDIATE](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/flexibility.md#user-content-c-intermediate) | Reusable calculations and partial outcomes | Pending |
| [C-CALLER-CONTROL](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/flexibility.md#user-content-c-caller-control) | Ownership and buffer APIs | Pending |
| [C-GENERIC](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/flexibility.md#user-content-c-generic) | Reusable parameter contracts and code size | Pending |
| [C-OBJECT](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/flexibility.md#user-content-c-object) | Traits intended for dynamic dispatch | Pending |
| [C-NEWTYPE](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/type-safety.md#user-content-c-newtype) | Units, account/room/run IDs, modes and semantic wrappers | Pending |
| [C-CUSTOM-TYPE](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/type-safety.md#user-content-c-custom-type) | Ambiguous selectors, authority, parameters and configuration | Pending |
| [C-BITFLAG](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/type-safety.md#user-content-c-bitflag) | Combinable flags | Pending |
| [C-BUILDER](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/type-safety.md#user-content-c-builder) | Complex construction contracts | Pending |
| [C-VALIDATE](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/dependability.md#user-content-c-validate) | Wire/file/process/native input boundaries and invariants | Pending |
| [C-DTOR-FAIL](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/dependability.md#user-content-c-dtor-fail) | File, process, media and other resource finalization | Pending |
| [C-DTOR-BLOCK](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/dependability.md#user-content-c-dtor-block) | Async/blocking cleanup, transfer, joins and shutdown | Pending |
| [C-DEBUG](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/debuggability.md#user-content-c-debug) | Public types; review secrets, native side effects and privacy | Pending |
| [C-DEBUG-NONEMPTY](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/debuggability.md#user-content-c-debug-nonempty) | Useful public debug output without secret/private state | Pending |
| [C-SEALED](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/future-proofing.md#user-content-c-sealed) | Deliberately closed trait implementation sets | Pending |
| [C-STRUCT-PRIVATE](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/future-proofing.md#user-content-c-struct-private) | Public representations and mutation invariants | Pending |
| [C-NEWTYPE-HIDE](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/future-proofing.md#user-content-c-newtype-hide) | Exposed implementation-dependent types | Pending |
| [C-STRUCT-BOUNDS](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/future-proofing.md#user-content-c-struct-bounds) | Generic data structure definitions | Pending |
| [C-STABLE](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/necessities.md#user-content-c-stable) | Stable public API commitments | Pending |
| [C-PERMISSIVE](https://github.com/rust-lang/api-guidelines/blob/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src/necessities.md#user-content-c-permissive) | Project/dependency licensing and distribution | Pending |

## Pinned-source anomaly

The pinned `checklist.md` also contains a link definition named `C-HTML-ROOT`.
It is not an active checklist entry, and the pinned documentation chapter has no
matching section. It is recorded here for completeness and creates no extra
requirement. Do not invent an HTML-root configuration obligation from that
unused definition.
