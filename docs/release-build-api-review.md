# Production release builder: affected API review

This is a prospective review of the release build/export change in the existing
`simplestchat-ops` crate. It covers the affected decisions below, not the crate or
workspace as a whole. Unreviewed entries remain pending in the
[54-entry inventory](rust-api-checklist.md). No retrospective audit is performed.

The governing [coding standard](coding-standard.md) adopts API Guidelines revision
`19f0f62ecd0f56a0b7adaa332c352bc2476506ea`. This review read the applicable pinned
explanations in its [source chapters][a], including their scope and qualifications.
Guideline IDs below refer to the exact explanation links in the inventory.
**Applies** records an affected design and its source evidence; it does not turn an
unrun gate into compliance. **Pending** identifies an unresolved decision or check.
**Exception** is limited to the stated target and behavior.

## Surface and runtime boundary

- `release::build::run`: explicit local unsigned build/export, with CLI statuses
  0/1/2. Evidence: [controller][build] and [CLI tests][cli].
- `archive::verify_retained_archive`: borrow an original `File` and verify the
  existing server archive profile. Evidence: [archive implementation][archive].
- Public `owned_process`: reserved group, signal registration, terminal
  observation and retirement/reap. Evidence: [owner][owner] and [cleanup][cleanup].
- Private `Owner`, `Dir`, `Held`, `Executor`: one operation clock, original handles
  and fixed tool roles. Evidence: [control][control], [files][files], [controller][build].

The source-built controller supports the reviewed Linux/macOS context and emits
Linux/amd64 production images. It does not broaden the distributed Ops artifact's
separate Linux x86_64 musl target, install a host binary, authenticate a signature,
or authorize deployment. The [operator guide][operators] distinguishes the native
controller from attested distribution and preserves the server's ten-file release
contract. Windows portability is not claimed by this review.

The implementation remains in the existing crate. It adds no executor framework,
public arbitrary-program command or new crate. The private `Executor` permits
inert observations in tests; production dispatch fixes the Git/Docker role graph.
Moving the small shared owner downward avoids an Ops-to-xtask dependency cycle.
Existing xtask callers retain their adapter paths.

## Affected decisions

- **C-CASE, C-GETTER, C-METHOD, C-CTOR — applies.** `Owner`, `Group`, `Signals`,
  `Dir` and `Held` use ordinary type/method naming. `pid`, `path`, `file` and
  `file_mut` name borrows or observations; `new`, `open` and `create` identify
  construction. Resource operations remain associated with their owners. This is
  evidence for the authored surface, not a naming audit of existing modules.

- **C-COMMON-TRAITS — applies.** Fixed failure
  enums have value traits, `Display` and `Error`. Unique process/signal/operation
  owners deliberately have no `Copy`, `Clone` or `Default`: copying would duplicate
  responsibility, and default construction cannot establish fallible native
  registration or a child reservation. Private `Dir::clone` shares one original
  chain through `Rc`; checked finish requires all its borrowers to retire.
  Ordering, hashing or equality of live owners has no useful value contract.

- **C-DEBUG, C-DEBUG-NONEMPTY — applies.** Manual [owner implementations][owner]
  expose stored group PID/reap state and nonempty signal-registration kinds.
  They do not poll notifications or format native internals, argv, paths or
  captures. The [owner tests][owner-tests] include
  `disarmed_group_debug_reads_only_stored_identity_and_owner_state` and
  `signals_debug_exposes_only_registered_kinds_without_polling_events`.
  These regressions passed in the native all-features gate;
  diagnostic state never becomes an observation authorizing another action.

- **C-GOOD-ERR — applies.**
  `owned_process::Error` has meaningful fixed variants and implements `Error`;
  its public `Display` now uses lowercase messages without terminal punctuation.
  `fixed_owner_errors_use_lowercase_privacy_safe_display` checks that fixed surface.
  The [xtask compatibility adapter][xtask-owner] intentionally maps variants to
  its existing `ToolingFailure` text. That preserved CI diagnostic contract is
  not an exception for the new Ops `Display` or an audit of the older formatter.
  Release `Failure` and
  `ArchiveError` already return fixed codes without private external text.
  Child exit 124/130/143 remains a subordinate failure; actual owner deadline,
  interrupt and terminate causes remain separate in private evidence. The CLI
  maps operational failure, deadline and INT/TERM to 1; usage is 2 and success 0.

- **C-SERDE — applies to passive records; not applicable to live authority.**
  Existing manifest/source/outcome schemas remain closed. Measurement/result data
  is serialized; original file handles, group reservations, signals and operation
  ownership are not. Decoding JSON never reconstructs spawn, publication or retry
  authority. Internal CLI options are consumed configuration, not a new wire format.

- **C-SEND-SYNC — applies.** No unsafe auto-trait implementation is introduced.
  `public_owners_preserve_send_and_sync_contracts` asserts `Send + Sync` for
  `Group` and `Signals`; it passed on the native target and compiled in the GNU
  Linux target check. Private `Dir` uses `Rc` and is
  intentionally confined to its controller thread; it is not advertised as a
  transferable concurrent capability. Runtime worker count does not make that
  controller future safe to move between threads.

- **C-CALLER-CONTROL, C-NO-OUT — applies.** `Group::new` consumes its child;
  dispatch consumes each `Invocation`. Retained archive verification borrows the
  original `File` and manifest, returns measured identity/configuration, and leaves
  final close with the caller. The mutable file is an existing I/O resource whose
  cursor is used, not an output parameter for constructing an unrelated result.

- **C-RW-VALUE — not applicable to the concrete retained-descriptor signature.**
  Its by-value prescription concerns generic `Read`/`Write` function parameters.
  `verify_retained_archive(&mut File, ...)` deliberately requires file metadata and
  caller custody. The private `Read`/`Seek` implementation must use those traits'
  signatures. This does not justify rewriting other generic I/O APIs by reference.
  The private child invocation also consumes an optional original archive-writer
  loan. It is an admitted file, not a path chosen by the child. Completion retains
  a passive byte/digest/metadata seal and explicit close observation; neither
  reconstructs ownership. The controller compares them against its retained file.

- **C-GENERIC, C-INTERMEDIATE — applies.** Archive verification accepts only the
  checkpoint closure it needs and returns identity/configuration already measured.
  Private static `Executor` dispatch separates supplied observations from the
  controller's actual source, file and publication work. `Completion` retains
  partial signed exit/capture/cleanup observations even when its result fails.

- **C-OBJECT, C-SEALED — not applicable to a downstream trait surface.**
  `Executor` is private and used through a generic bound, with no intended trait
  objects or external implementation set. Visibility already closes that scope.

- **C-STRUCT-PRIVATE, C-NEWTYPE-HIDE — applies.** Public `Group` and `Signals`
  retain private state. Existing archive result accessors expose measured data;
  they do not expose a live authority representation. `Dir`, `Held` and `Owner`
  remain private. Test-only metadata/descriptor observations create no production
  constructor or bypass.

- **C-NEWTYPE — applies to the affected ownership and unit distinctions.**
  `Instant`/`Duration`, fixed `Role`/`Failure` choices and separate file/group/clock
  owners prevent interchange of different operational responsibilities. Original
  hashes, names and JSON numeric tokens remain data and cannot replace those owners.

- **C-CUSTOM-TYPE — applies.** `Owner::new` takes private `Mode::Build` or
  `Mode::Export`, replacing its ambiguous positional boolean. The [control][control]
  constants and existing [budget cases][budgets] preserve the same arithmetic;
  the enum/caller delta passed the native budget and whole-operation tests. This does not force
  one universal status model across unrelated operations.

- **C-COLLECT — not applicable to new general collection APIs.** The source ledger
  and output registry are private owned-resource journals, not public collections.
  An infallible `FromIterator`/`Extend` constructor would not establish original
  directory authority or express their admission failures. Their actual entry/name
  and stream limits are assessed under validation below.

- **C-VALIDATE — applies, with explicit accepted-domain limits.** Selectors are
  checked before effects. The Rust command accepts full option names once and
  an ASCII-decimal timeout; the unchanged Python reference remains available.
  The source journal admits 100,000 logical entries and
  16 MiB of summed normalized relative-name bytes, without a new depth or 8 GiB
  source cap. Every created directory has exact membership plus metadata seals;
  files retain original identity and intended-byte readback checks. Directory
  refresh does not admit unrecorded members. See [custody tests][custody] and
  [whole transactions][transactions]. These are observed mutation fences, not an
  immutable filesystem or complete compiler-input-closure proof.
  Source extraction also admits one leading 52-byte Git global PAX commit comment
  bound to the selected revision. It creates no filesystem or journal entry for
  that record and refuses repeated, late or overriding global metadata. The
  existing headerless fixture profile remains accepted.
  The fixed Docker-save command now omits `--output` and streams binary stdout
  into the admitted original writer in 64 KiB chunks, bounded at 8 GiB. It never
  enters textual command logs or inspection capture. Stderr retains its 2 MiB
  limit. Flush, synchronization, metadata observation and explicit writer close
  precede the controller's independent original-file readback and seal comparison,
  including on failed commands. The ten-file server release contract is unchanged.
  Shared PID helpers additionally require the documented original-child/private-
  group and exclusive-wait preconditions. A numeric PID or successful record alone
  cannot establish them; callers retain the actual `Group` reservation.

- **C-DTOR-FAIL, C-DTOR-BLOCK — applies with the narrow exception below.**
  Positive operation completion uses explicit process retirement/reap and checked
  original handle closes. Failure paths attempt admitted peers while preserving
  the first known cause. `Group::Drop` is a fallback that may poll for five seconds;
  explicit `finish_before` plus joined `reap` is its alternative. Drop alone never
  establishes successful cleanup, and cancelling a consuming reap is not completion.

- **C-CRATE-DOC, C-LINK — applies.** The [library overview][lib], module comments,
  [operator guide][operators] and [testing guide][testing] describe effect, target,
  trust and completion boundaries. They link the original Python reference and
  distinguish policy fixtures from actual native/Docker/signing evidence.

- **C-EXAMPLE, C-QUESTION-MARK, C-FAILURE — applies to the shared owner.** Its new
  module-level `no_run` example shows original deadline/signal ownership, group
  retirement before joined reap and fallible `?` propagation after peer settlement.
  Formal `Errors`/`Panics` sections document registration, supported positive PID,
  exclusive-wait/private-group preconditions and disarmed child/reap behavior.
  The example passed as a `no_run` doc test; that establishes compilation, not execution.
  No new Rust `unsafe` API is introduced.

- **C-EXAMPLE, C-QUESTION-MARK, C-FAILURE — applies to retained verification/build.**
  Public [retained verification][archive] documents `Errors`/`Panics`, callback
  refusal and propagation, moved file cursor and caller checked close on either
  outcome. Its `no_run` example borrows the original file and namespace/deadline
  checkpoint and propagates errors with `?`; links distinguish the full controller
  and anchored inspection API. [Build entrypoint rustdoc][build] connects its
  0/1/2 contract to the operator guide and requires a dedicated CLI process for
  temporary umask/stdio-flag ownership and restoration. Its runnable `--help`
  example requires no Git, Docker, configuration or writable directory. These
  source changes passed Rust quality and all three doc tests; the `--help` example ran.

- **C-STABLE — not applicable to a stable-crate declaration here.** The workspace
  remains version 0.1.0 and unpublished as crates. This does not waive changes to
  the documented binary contract or promise stability for every exposed source
  type. Reassess public dependency commitments before a stable library release.

- **C-PERMISSIVE — applies to the builder's explicit Tokio/runtime use.**
  The declared Tokio minimum is 1.53.2; the existing lock selects 1.53.3 (MIT).
  Actual default musl normal/build trees for the public baseline and candidate
  both contain the same 57 package versions, with no additions or removals.
  Tokio 1.53.3, Mio 1.2.4 and signal-hook-registry 1.4.8 offer MIT; this limited
  change selects and embeds their exact authenticated MIT notices in
  `ops/src/release/build_licenses.rs`. `release build --licenses` prints them
  before runtime/environment/filesystem setup, using a private `Write` value
  with checked writes and flush. Binary-only installation retains those texts
  without changing signing subjects or payload schemas. Crate/license SHA-256
  provenance is recorded below. The notice/hash unit and CLI tests passed, including
  actual read-only stdout refusal through the checked native descriptor adapter.
  This affected review neither relicenses the project nor certifies the rest of
  the Ops/workspace license inventory. Existing Cargo-deny policy and source
  declarations alone were not treated as notice delivery.

  - `tokio 1.53.3`:

    Archive: `ce3335fa71841cda333a58d7615b03901380ecf09d59b3296d21f8bbac0dde4e`.

    MIT notice: `253cd04c6714889df2d32f3f64d669179a1c95c76ac43c40882c52eb06bc3552`.

  - `mio 1.2.4`:

    Archive: `1788edb87fdc09c7e26304471e2f5be8cdefb1b6930d6e3985fc02ff53bf86ee`.

    MIT notice: `07919255c7e04793d8ea760d6c2ce32d19f9ff02bdbdde3ce90b1e1880929a9b`.

  - `signal-hook-registry 1.4.8`:

    Archive: `c4db69cba1110affc0e9f7bcd48bbf87b3f4fc7c61fc9155afd4c469eb3d6c1b`.

    MIT notice: `503558bfefe66ca15e4e3f7955b3cb0ec87fd52f29bf24b336af7bd00e946d5c`.

## Narrow native teardown exception and residual limits

On Linux, membership enumeration borrows a fresh admitted cursor `File` through
`RawDir`; its explicit close result is checked. The portable macOS
`rustix::fs::Dir::read_from` path also owns an opaque libc cursor. The library drops
that cursor through RAII and exposes no fallible `closedir` result to this caller.
Its implementation is in [directory membership][files].

For **C-DTOR-FAIL**, Drop itself does not propagate an error, as the pinned rule
permits for fallback teardown. The narrower project exception is that a successful
metadata operation cannot report that opaque cursor's close status. For
**C-DTOR-BLOCK**, the opaque destructor may enter a kernel close and has no exposed
caller preparation/finish API. Explicit checked-close claims therefore cover
admitted original `File` handles, including the fresh cursor, not every hidden
library descriptor. Replacing this with unchecked raw descriptor ownership or a
new directory parser would broaden the risk; rejecting supported macOS enumeration
would narrow the approved platform domain. Revisit when rustix exposes a suitable
explicit close result or another mature safe adapter supplies it.

The same original owner is checked around archive reads/seeks and between bounded
chunks. It cannot bound a kernel I/O or callback that itself blocks indefinitely.
Original namespace, socket, membership and seal checks also leave a same-UID
check-to-read race while Docker reads pathname inputs. Neither limitation is an
authentication, immutable-context or complete-input-closure claim.

The metadata profile keeps UTF-8 string input, rejects raw NUL/leading BOM,
decoded duplicate keys and lone surrogates, and bounds all ignored fields too:
128 containers, 100,000 values, 4300-digit integer tokens, finite float conversion
with underflow allowed. Large integers keep raw spelling beyond `u64`/finite
`f64`. Image inspection uses its caller's 2 MiB cap; current/persistent state use
64/16 KiB. This is an explicit Rust acceptance boundary, not full Python parity.
The unchanged Python reference remains available. See [profile tests][json-tests].

## Validation state and completion evidence

The initial builder implementation was checked against public base
`70ec4ab495db30e0838bcb4cf17b7271e39f2907`: 229 native Ops library tests,
39 CLI tests, 839 xtask tests, three doc tests and the 112-case whole-reference
comparison passed. Those results predate the source-metadata correction below.

At `98d6dd8`, the hosted native controller compiled in forty seconds, but source
extraction rejected Git's global PAX commit comment before Docker build/save or
final-image verification. A separate read-only development workflow reproduced
that phase. The focused correction admits one bounded commit-comment record,
bound to the selected revision, without creating a filesystem or journal entry.

A real 31,805,440-byte archive produced by Git 2.56.0 then passed both whole
build/export controller paths with CI's existing 300-second export setting and
unchanged 120-second setup allowance. The two-loop native debug diagnostic took
102.864 seconds. Tool and image observations in that replay were inert: it proves
actual source extraction/custody/publication, not a physical Docker build. An
older replay using the fixture's 120-second export setting exhausted its clipped
46-second setup phase; that failed trial is retained separately.

Before the streaming and lifecycle corrections, the PAX/preparation candidate
passed the native Rust 1.99.0
`aarch64-apple-darwin` all-features Ops gate: 248 library, 43 CLI and four doc
tests, with four explicit ignored helpers. Both exact reference comparisons
passed separately (112 builder and 72 preparation scenarios). GNU Linux
all-target Clippy passed; this is cross-compilation, not Linux execution. The
full workspace Rust quality gate passed in 165.843 seconds, including generated
contracts and private Rustdoc. All 575 helper tests and final fast security
passed. The Python suite ran 1,659 cases with seventeen opt-in skips and one
stale workflow fixture; its corrected 33-test suite passed. Current source
hashes, commands, failed trials and scoped review evidence are retained privately.

The builder's minimal Tokio feature request and locked dependency graph remain
unchanged: the same 57 default musl normal/build package versions as the baseline.
The scoped notice/hash/CLI tests include actual read-only stdout refusal. These
facts do not certify the whole workspace's license inventory.

Hosted CI at `09c9193` passed the real production Docker build in 176 seconds
and all migration/static/PWA/health/DB/registration/restart smokes. Native export
then refused `release_build_original_changed` after thirteen seconds. This is a
separate failed trial from the earlier source-header refusal. The Ops producer
also completed, but preparation and both signers were skipped; its unsigned Ops
artifact does not establish signed runtime or installation.

The output-transport cause was reproduced before this correction: both whole
build/export paths refused an untouched empty original after a pure sibling-file
write and rename, with no candidate or release manifest published. One exact test
covering both modes passed in 11.45 seconds. An actual Docker 29.9.2 CLI paired
with a private inert Unix API independently produced the same 4,097-byte payload
through both transports: `--output` replaced the original inode, while stdout
written through the original handle preserved it. Both commands exited zero,
stderr was empty, and the inert server shut down and joined. There was no engine
or actual image export in that experiment. The versioned
[Docker save implementation](https://github.com/docker/cli/blob/v29.9.2/cli/command/image/save.go)
selects an atomic writer for `--output` and ordinary stdout otherwise.

The corrected source passed all 256 Ops library tests, 43 CLI tests and four
Rustdoc tests, plus the separate 112-case Python comparison. Six new native
process tests cover a 3 MiB original-inode stream, the archive limit before write,
partial failed output, actual write failure, failed close and metadata failure
preceding a later close failure. Two new whole-controller tests require the
closed original seal before publication and refuse missing-close or mismatched
seal observations in both modes. The initial focused run passed 65 cases and
failed one new fixture created outside Tokio; entering its owned runtime corrected
that fixture before the successful complete run. The failed trial is retained.

GNU Linux all-target Clippy passed in 31.719 seconds. Workspace Rust quality,
helper quality and fast security passed in 184.551, 61.418 and 30.430 seconds,
respectively. The complete Ops run took 42.936 seconds; the separate 112-case
comparison took 26.016 seconds. Independent source reviews covered the process
adapter and whole-controller wiring. These are source, inert-child and local
filesystem checks; they do not establish actual engine export or deployment.

The CLI teardown correction also fences the final runtime boundary. One checked
calculation preserves the original start and existing build/export mode overheads.
The outer positive fence runs after Owner/Signals and the actual runtime are gone
and the original process umask is restored. A body failure stays negative; help,
notices and invalid arguments return before runtime or umask effects. The public
entrypoint documents valid operations as synchronous dedicated-CLI calls outside
an entered Tokio runtime; it makes no panic-unwind restoration guarantee.

The combined streaming/lifecycle source passed all 258 Ops library tests, 43 CLI
tests and four Rustdoc tests in 39.927 seconds. The focused builder group passed
68 cases with two explicitly ignored helpers in 19.751 seconds. The separate
112-case Python comparison passed in 22.133 seconds. GNU Linux all-target Clippy,
workspace Rust quality and fast security passed in 26.032, 166.150 and 34.882
seconds, respectively. These are native inert-process/filesystem checks and a
Linux compilation check; actual Linux execution and release gates remain separate.

The unchanged Python reference retains its original transport. Its 112-case
comparison admits only the exact removal of
`--output` and the known archive path from the Docker-save argv, requires the
original writer loan there, and keeps other argv, environment and selectors exact.
Published streaming commit `6a3acc8` subsequently passed actual Docker export
in 22 seconds, native preparation and both signers. Normal signed deployment
and independent HTTP/container revision checks passed. Its separate signed Ops
Debian runtime trial failed a keeper gate; the stale global-help expectation and
failed-case evidence retention are being corrected. The new CLI teardown
correction still needs its own hosted release/runtime gates. Preserve the independent
signer and unchanged ten-file server contract; unsigned records and native
compilation do not establish authenticated distribution or host installation.

[a]: https://github.com/rust-lang/api-guidelines/tree/19f0f62ecd0f56a0b7adaa332c352bc2476506ea/src
[build]: ../ops/src/release/build.rs
[archive]: ../ops/src/release/archive.rs
[owner]: ../ops/src/owned_process.rs
[owner-tests]: ../ops/src/owned_process_tests.rs
[xtask-owner]: ../xtask/src/owned_process.rs
[cleanup]: ../ops/src/owned_process_cleanup.rs
[control]: ../ops/src/release/build_control.rs
[files]: ../ops/src/release/build_files.rs
[lib]: ../ops/src/lib.rs
[cargo]: ../ops/Cargo.toml
[operators]: ../ops/README.md#production-release-build-and-export
[testing]: testing.md#production-release-builder
[cli]: ../ops/tests/release_build_cli.rs
[transactions]: ../ops/src/release/build_tests.rs
[custody]: ../ops/src/release/build_custody_tests.rs
[budgets]: ../ops/src/release/build_budget_tests.rs
[native]: ../ops/src/release/build_process_tests.rs
[json-tests]: ../ops/src/release/build_json_tests.rs
