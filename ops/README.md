# Operations policy tools

`simplestchat-ops` validates local release manifests, receiver receipts and
Docker/OCI image archives, and inspects local executable identities. Its production
release builder can invoke the selected local Git and Docker tools to build or
export an unsigned image. Local unsigned artifact preparation can write a new
private directory, as described below. Python remains the reference implementation
and the installed release runtime. A separate read-only
release preflight checks the fixed host paths without executing installed helpers.
The browser-asset transaction commands described below can mutate only the fixed
managed asset store while holding the existing workload lock.
The separate Ops first-install command publishes one original-pinned executable
at its fixed helper path. The authenticated Debian runtime owner exercises that
operation in a disposable guest; installed Python callers retain their roles.

```sh
cargo run --locked -q -p simplestchat-ops -- release manifest --path /path/to/release.json
cargo run --locked -q -p simplestchat-ops -- release archive --manifest /path/to/release.json --path /path/to/image.tar
cargo test --locked -p simplestchat-ops
```

The successful diagnostic JSON contains `manifest`, `manifestBytes` and
`manifestSha256`. The digest and byte count describe the original file, including
its whitespace, BOM and encoding. The semantic projection preserves the existing
seven field names; it must never replace bytes covered by release hashes. This
command does not authenticate an archive or signature and does not authorize a
release. Validation failure returns exit 1 and the fixed JSON
`{"passed":false,"failure":"invalid_release_manifest"}`. Invalid arguments return
exit 2 and the usage line; no path or file contents enter error output.

Inputs must be nonempty regular files of at most 1 MiB. The descriptor reader
refuses symlinks in every path component, parent (`..`) components and more than
256 components. It retains directory descriptors, checks file identity, size,
permissions and modification/change timestamps against both the open file and
visible binding, and closes all successful reads explicitly. This is a snapshot,
not a lock against later writes. Use the retained original bytes for a later
transaction, or reopen and verify the digest at its operation boundary.

The parser matches Python's UTF-8/16/32 BOM and null-pattern detection and keeps
integer tokens distinct from floats and booleans. It rejects decoded duplicate
keys at every depth, nonfinite numbers and unknown manifest fields. JSON is
limited to 128 nested containers and 100,000 values, with at most 2 MiB of decoded
UTF-8. Unpaired surrogates are refused. These are explicit bounded-decoder limits,
not a claim to accept every value Python can represent. Calendar checks use the
locked Unicode 16 decimal-digit data, matching the pinned Python oracle; original
timestamp text remains unchanged.

Rust manifest parity tests require CPython 3.14.8 and its Unicode 16 database.
The CI Rust test job selects that interpreter before input fingerprints, cache
reuse and builds. This pins the test oracle's Unicode behavior; the operational
Python helpers retain their Python 3.12 support.

| Module | Responsibility |
| --- | --- |
| `src/main.rs` | Allowlisted CLI forms and fixed output failures |
| `src/json.rs` | Serde grammar, duplicate keys, numeric tokens and bounded decoding |
| `src/files.rs` | Descriptor-bound regular-file snapshot and change detection |
| `src/distribution.rs`, `src/distribution_elf.rs` | Original executable identity and bounded ELF linkage facts |
| `src/distribution_build_plan.rs`, `src/distribution_inventory.rs` | Nonexecuting build recipe and selected original-byte inventory |
| `src/distribution_manifest.rs`, `src/distribution_pair.rs` | Separate Ops declarations, selected identity and pinned local executable pair |
| `src/distribution_assembly.rs`, `src/distribution_assembly_files.rs` | Fresh local unsigned signing inputs and measured payload bytes |
| `src/release/manifest.rs` | Existing release schema, migration checksums and real UTC dates |
| `src/release/archive.rs`, `src/release/archive/` | Whole-transfer identity and bounded offline Docker/OCI metadata validation |
| `src/release/archive/generator*.rs` | Separate generator profile with streamed config/layer graph identity |
| `src/release/preflight*.rs` | Fixed read-only host admission and exact installed-helper byte checks |
| `src/release/assets*.rs` | Ordered browser-asset capture, retention, publication and workload lock |

The crate reuses workspace dependency versions and adds one exact safe signal
adapter.
Its default graph excludes the server, mediasoup and native TLS; capacity logic
is shared with the load-test crate. The manual argument parser follows `xtask`
for this small command surface and avoids adding a CLI framework dependency.

No binary is installed by Ansible or added to the production image. Host
execution requires a separate attested operations artifact, an inspected static
`x86_64-unknown-linux-musl` build, harmless CLI checks on Debian 13, and
controller-side verification before any installed binary executes. Static CRT
selection alone does not prove the final binary's dependencies; inspect the
result as the [Rust linkage reference](https://doc.rust-lang.org/reference/linkage.html#static-and-dynamic-c-runtimes)
recommends. The existing signed server artifact and its ten-file contract stay
unchanged. Python's controller-supplied preflight remains the bootstrap trust
boundary until binary provenance, ABI and installation ownership are established.

## Production release build and export

Build the selected checkout's native controller before starting the operation:

```sh
cargo build --locked --release -p simplestchat-ops --bin simplestchat-ops
mkdir -p results
target/release/simplestchat-ops release build --output "$(pwd -P)/results/release-build-attempt"
target/release/simplestchat-ops release build --image-id sha256:EXACT_LOWERCASE_IMAGE_DIGEST \
  --timeout-seconds 300 --output "$(pwd -P)/results/release-export-attempt"
```

The image selector must be `sha256:` followed by exactly 64 lowercase hexadecimal
digits; the placeholder above must be replaced. Output must be a fresh directory
with an existing parent and no symlink components. An output inside the checkout
must be Git-ignored. Commit or remove checkout changes before invocation; both
tracked and untracked changes block admission. A retry uses a new output path and
repeats every source, workload and image check.

This source-built command supports Linux and macOS controllers, including Docker
Desktop. The produced image remains `linux/amd64`. Windows is refused before
effects. This controller build does not widen the distributed Ops binary's fixed
Linux musl target, authenticate installation or update an installed host helper.
The operation requires the selected local Git executable and a configured local
Unix-socket Docker endpoint. It honors the active Docker context, then fixes the
explicit local endpoint and strips Docker/Buildx endpoint and API overrides from
subsequent commands. Socket alias checks detect observed replacement; they do not
authenticate a daemon or make pathname connection atomic.

`simplestchat-ops release build --licenses` prints the exact MIT notices for
Tokio 1.53.3, Mio 1.2.4 and signal-hook-registry 1.4.8 (MIT option selected).
They are embedded in the executable, so a binary-only installation carries these
notices. This informational route needs no runtime, environment, checkout or
writable directory; output/flush failure returns 1 with a fixed diagnostic.
Mixed or duplicate selectors remain usage errors. This limited notice set covers
the builder's explicit Tokio/runtime use; it is not a complete Ops or workspace
license inventory. The original builder change kept its 57-package musl
normal/build baseline without adding a package. Later verification and signal
adapters have their own dependency reviews; the current default graph contains
60 packages.

The CLI accepts `--output PATH`, optional `--timeout-seconds N` and optional
`--image-id ID`, in split or equals form. `N` uses ASCII decimal digits and may
have leading zeroes; signs, whitespace, separators and Unicode numerals are usage
errors. Selectors may occur once; full option
names and ordinary ASCII decimal integers are required. `N` is 60 through 7200,
default 3600. Python's incidental argparse abbreviations and Unicode integer forms
remain available through the unchanged reference command; the new command does
not accept them. Help and usage errors require no environment, tool or source
lookup. Exit 0 means complete success, 1 means an operational failure, deadline or
interrupt, and 2 means invalid arguments. Private command evidence retains actual
signed child status separately from the operation's cause; a child exit 130 does
not mean the controller received an interrupt.

The builder refuses active public-chat or benchmark containers and unfinished
protected benchmark/release cleanup records. It holds the existing workload lock
when present. Build mode requires the default local Docker Buildx driver, archives
the exact clean Git revision, restores Git source modes, hashes the five source
inputs and migration files, and builds only the production target with its exact
revision label. It may pull build inputs. Export mode inspects the exact immutable
image and its production runtime identity and layer DiffIDs. It reuses an already
matching release tag or creates a missing one, refuses a conflicting tag, and
checks selection before and after save. Export mode never builds, pulls or runs
an image, including after a failed export. Retained images and tags are not
pruned or rolled back.

Docker inspection and protected state records use the existing strict Ops JSON
profile through a UTF-8 string adapter. Inspection keeps its 2 MiB encoded bound;
benchmark `current.json` keeps 64 KiB and persistent `release-state.json` keeps
16 KiB. All fields, including ignored metadata, are limited to 128 nested
containers and 100,000 values. Decoded duplicate keys and lone surrogate escapes
are refused; Unicode scalar strings and valid surrogate pairs are accepted.
Integers retain their raw spelling through 4300 digits, including ignored values
beyond `u64` or finite `f64` range. Float spelling is retained when conversion is
finite; underflow to zero is allowed, while overflow, NaN and Infinity are refused.
Booleans and null retain their own types.

This adapter rejects raw NUL bytes and a leading UTF-8 BOM. The Python reference
receives already decoded strings from strict subprocess UTF-8 or `read_text`;
`json.loads(str)` does not perform the generic byte decoder's UTF-16/32 or BOM
autodetection. Escaped `\u0000` inside a JSON string remains valid. The Rust metadata
profile is an explicit accepted-domain boundary: deeper ignored objects accepted
by Python can be refused here. The unchanged Python builder remains the reference;
these limits are not a claim of full Python metadata parity.

Both modes stream `docker image save --platform linux/amd64 TAG` stdout into
an original writer for the admitted `image.tar`. Docker receives no output path:
its `--output` implementation atomically replaces that path rather than writing
the admitted inode. Binary stdout is capped at 8 GiB and kept out of command
logs; stderr retains its 2 MiB bound. The writer is flushed, synchronized, sealed
and explicitly closed, and its byte count, digest and identity are compared with
the retained original before the command can succeed. Partial bytes and negative
command evidence remain on failure; an exchanged visible path is never adopted.

Both modes then check the final clean revision, verify the whole archive and
existing server metadata contract, then publish `release.json`
by linking its verified candidate. Export additionally requires the archive's
DiffIDs to equal the selected image's; a local Docker image ID is not assumed to
equal the archive's configuration digest. Original file, source, tool and socket
observations are rechecked across command boundaries. Temporary cleanup operates
only on its original created entries. A changed or foreign entry makes completion
fail and is retained; cleanup never adopts an unknown path to delete it.

The source extractor accepts Git's optional first global PAX header only in its
ordinary commit-comment form: `pax_global_header`, exactly 52 bytes containing
one `comment` equal to the already selected lowercase 40-hex revision. It parses
that metadata with the existing tar PAX parser without creating a filesystem
entry or adding it to the logical source ledger. Repeated, late, differently
named or malformed global headers, other keys and path/size overrides are
refused. The comment does not authenticate the revision or replace the original
Git/source witnesses. Archives without that header remain supported.

The native source ledger admits at most 100,000 logical archive entries and
16 MiB of summed normalized relative-name bytes. These are explicit source
acceptance limits using the existing archive resource profile's entry/name
budgets; they do not impose its 8 GiB image-archive cap on source or add a source
depth limit. Original deadlines and filesystem constraints still apply.
Every created directory is checked for its exact journaled members, alongside
full directory seals and original file bytes. An extra file is negative even if
it appears before the first directory seal or directory growth metadata happens
to match. Cleanup retains that unknown member.

Membership enumeration opens a fresh cursor `File` relative to its retained
original directory. Linux's iterator borrows that cursor. The portable iterator
used on macOS owns an additional opaque libc directory cursor; its automatic
`closedir` result is not exposed. Explicit checked-close claims cover the admitted
original `File` handles, including the fresh cursor, and do not claim a reported
close status for that opaque library cursor. Membership and seal checks detect
observed changes. They leave a check-to-read race against the same UID while
Docker reads pathname inputs; they do not make the context immutable or prove a
complete compiler input closure.

The selected timeout remains the upper bound on the active build/export window,
beginning after source setup. The native operation also has an earlier whole
owner, measured from entry:

| Mode | Whole operation upper bound | Live workflow selection |
| --- | --- | --- |
| Build | `N + 300` seconds | Default 3600 gives 65 minutes |
| Exact-image export | `N + 60` seconds | 300 gives 6 minutes |

Setup shares at most 120 seconds. Build verification reserves and shares at most
140 seconds. Exact-image export uses `v = min(140, N)` seconds for both its future
verification reserve and verification phase cap. The whole export remains `N + 60`;
the correction for small `N` preserves the full 40-second settlement/finalization
tail rather than reducing it to admit work.
Commands are admitted only with their remaining original body and settlement
allowances. A live command retains 20 seconds for group retirement/reap,
6 seconds for pipe/close settlement and 14 seconds for remaining file/state/outcome
finalization. Save is capped at 300 seconds and the active time remaining, then
clipped again to preserve those verification and settlement tails. Consequently,
`--timeout-seconds 300` does not guarantee a full 300-second save body inside a
six-minute export.

For an export entering at virtual time zero, the original global body ceiling is
20 seconds at `N = 60`, `139` and `140`, and 180 seconds at `N = 300`. Setup and
active commands share that absolute ceiling; setup does not grant a new body
allowance before export. The first setup query is additionally capped at 30 seconds.
A 60-second selection therefore admits a positive bounded body while retaining
60 seconds of verification and all 40 seconds of tail. Elapsed setup work reduces
that remaining body. The [virtual-clock matrix](../docs/testing.md#production-release-builder)
records exact phase and query endpoints for all four selections.

Owner checks run between chunks and transitions; they cannot
bound a kernel read or filesystem operation that itself blocks indefinitely.
Workflow termination remains a separate outer bound.

Failures after output admission retain the attempt, including available original
command argv, bounded logs, signed exit observations and cleanup diagnostics.
Stdout and stderr each have a 2 MiB live budget. Captured inspection also limits
their combined log to 2 MiB; long uncaptured build/save commands retain both live
stream limits. Cancellation and failure settle owned process groups and all
admitted pipe/file peers, preserving the first operation failure. No automatic
retry, fallback build, image deletion, registry push, service restart or deploy
occurs. Positive record bytes alone do not override a late close or owner failure;
downstream consumers must require exit 0.

The unsigned directory keeps `image.tar`, `source.json`, `release.candidate.json`,
`release.json` and `outcome.json`, plus private command evidence. `source.json`
contains only `revision` and hashes of Dockerfile, .dockerignore, Cargo.lock,
web/package-lock.json and build/pip-constraints.txt. Exported image ID appears
only in `outcome.json`. The manifest and outcome remain schema 1.
Unsigned build success supplies no signature, receiver authorization or deployment
permission. The separate signer and server's ten-file published contract remain
unchanged; the Ops distribution artifact has its own distinct contract.

CI prepares the native controller in a separate bounded step before the existing
six-minute export. It exports the production image already verified and smoke
tested by that job. The manual artifact workflow keeps its 75-minute job and
read-only permissions: pinned compiler bootstrap plus native preparation share
8 minutes, the default operation gets 65 minutes, leaving 2 minutes for checkout
and artifact retention. Cold setup and actual Docker export need workflow runtime
evidence; an offline oracle or successful compilation cannot establish that fit.
Neither caller obtains a same-run signed Ops artifact: that would depend on the
very CI completion that production export helps establish.

The unchanged `build/build-release.py` and `build/release_build.py` remain runnable
references. Installed Python operations and other users of `release_build.Runner`
keep their existing roles. The [testing guide](../docs/testing.md#production-release-builder)
distinguishes whole-reference comparison, actual inert process tests and hosted
Docker/signing evidence.

## Read-only release preflight

The implemented command is available for eventual independently verified Ops
distribution; Ansible still supplies and runs the unchanged Python reference.
Do not install or substitute an unverified development binary on a release host.
The native check requires effective UID 0, kernel Linux/x86_64 and Debian major
13. It starts no process and does not import or execute an installed helper.

```sh
simplestchat-ops release preflight
simplestchat-ops release preflight --helpers-json "$EXPECTED_HELPER_JSON"
```

The first form selects ordinary reconciliation. The second requires an exact
complete mapping of the ten base helpers, the base plus three GitHub-fetch
helpers, or the base plus `force_release.py`, to lowercase SHA-256 digests of
their original bytes. The controller supplies `EXPECTED_HELPER_JSON` with all
names and all 64-digit digests from its checkout, without trimming source. The
14-helper combined fetch/force set is refused. The request cannot choose paths,
identity, a fixture root, file modes or the helper directory.

Both modes require `/etc/simplestchat-public` and `/srv/simplestchat-public` to be
root:root directories with exact mode 0700, plus nonempty regular
`/etc/simplestchat-public/images.json` with exact mode 0600 and at most 65,536
bytes. Its contents are never read. Prepared mode additionally requires
`/srv/simplestchat-public/releases` mode 0700, and every directory from `/`
through `/usr/local/libexec/simplestchat-public` root:root with exact mode 0755.
Selected helper files must be root:root regular files with exact mode 0644,
nonempty and at most 1 MiB each. All permission checks include special bits.
Hardlinks are accepted and their original link counts remain part of the
mutation checks; no exclusive inode ownership is claimed. Helpers are visited
in request order, and hashes cover whitespace and final newlines exactly.

Success is only `{"schemaVersion":1,"runId":"32 lowercase hexadecimal digits","prepared":true}`
(or `false` in ordinary mode). `runId` is fresh UUID-v4 entropy without hyphens.
Failures return exit 1 and only
`{"schemaVersion":1,"passed":false,"failure":"fixed_code"}`. The fixed codes
match `release_preflight.py`; no private paths, configuration, helper bytes or
operating-system error text are printed. Invalid command syntax returns 2.
Successful output writes must complete before the command returns zero; this
does not impose a wall-clock guarantee on a blocked output or filesystem.

The optional argument is UTF-8 JSON at most 4096 bytes. Absent input means `{}`.
Its explicit conservative domain rejects a BOM, decoded duplicate keys,
nonfinite numbers, unpaired surrogates and more than 128 nested containers;
these malformed-input refusals return `preflight_unavailable`. Python's raw
string character count, duplicate-last and nonfinite behavior are broader.
Valid helper names and digests remain the same fixed ASCII domain.

The selected OS data is at most 64 KiB of UTF-8 from `/etc/os-release`, with
`/usr/lib/os-release` used only when the preferred name is absent. The standard
relative link `../usr/lib/os-release` and its absolute equivalent are supported
through that fixed retained target; all other links are refused. Preferred-name
absence, a selected link's inode/target and the final target are rechecked.
The parser follows CPython's selected field behavior: optional whole quotes,
five backslash escapes, universal newlines and last-value-wins keys. Only exact
`ID=debian` and the first dot-separated `VERSION_ID` component `13` pass.
See [CPython's parser](https://github.com/python/cpython/blob/3.14/Lib/platform.py)
and the [os-release standard](https://www.freedesktop.org/software/systemd/man/latest/os-release.html).

The native filesystem profile is deliberately stronger than Python's path-based
snapshot: selected OS data is root:root regular mode 0644, every traversed
directory is root:root without group/other write permission, and other links are
refused. A single shared descriptor tree retains at most 48 owned Files,
including transient probes. Before/after hashes and final namespace checks bind
original file stamps, modes, owners, sizes and link counts. A 64 KiB streaming
buffer reads at most 13 MiB of helper bytes plus 64 KiB of OS data; configuration
contents are excluded. Every acquired File receives an explicit close attempt,
including constructor, read, verification and close-failure paths.

These are optimistic observations, not a filesystem lock, authentication,
durable receipt, helper update serialization or runtime readiness. Operators
must still serialize releases. The existing locked rustix 1.1.5 `process`,
`system` and `rand` APIs supply effective UID, uname and Linux getrandom without
unsafe first-party code or external commands; no new package or version is
introduced. Tests use actual owned files with synthetic ownership/identity and
the retained Python oracle, never the actual host's protected release paths.

## Browser asset filesystem transactions

The complete local transaction API and CLI are available for eventual verified
Ops distribution. The installed Python `release_assets.py`, `browser_assets.py`,
release controller and Ansible tasks remain authoritative and unchanged. This
unit does not copy from an image, run a workload, alter a proxy or establish
installed/runtime compatibility. Never substitute an unverified development
binary on a release host.

```sh
simplestchat-ops release assets capture --path /path/to/assets.tar
simplestchat-ops release assets initialize [--revision REV]
simplestchat-ops release assets inspect
simplestchat-ops release assets publish --capture REV /path/to/assets.tar
simplestchat-ops release assets publish --capture REV /first.tar --capture REV /second.tar
simplestchat-ops release assets select --revision REV
simplestchat-ops release assets prune
```

Only capture input paths are caller-selected. Managed operations require root
on Linux and retain the fixed `/srv/simplestchat-public` and
`/etc/simplestchat-public` directories with exact mode 0700. They create or verify
`/run/simplestchat-bench` mode 0700 and its ordinary root-owned `workload.lock`
mode 0600, then acquire the same nonblocking exclusive flock as Python. The lock
inode and namespace are rechecked after flock. Both optional `current.json` and
`release-state.json` journals must be at most 16 KiB, ordinary root-owned mode
0600, have strict integer schema version 1 and `finalized:true`.
Journal input is UTF-8 JSON text without BOM or raw NUL, matching Python's
text-read gate on the supported host; escaped NUL in an unknown field is valid.
No public root,
owner, clock, recovery or lock-bypass option exists. Library callers borrow the
same live `Workload` guard for `Store`; a future native release controller must
reuse that guard rather than launch an independently locking CLI.

`initialize` creates the store or validates it without rewriting valid existing
bytes. Its receipt is `{"passed":true,"changed":true}` or `false`; an optional
revision explicitly selects the independent public version. `prune` initializes
first and returns only `{"passed":true}`. Flock contention returns zero only
after owned closes succeed, with exactly
`{"passed":true,"busy":true,"changed":false}`. Unlike Python's broad
`BlockingIOError` handler, a later I/O or close failure cannot masquerade as busy.
Other failures return 1 and a fixed private diagnostic; syntax returns 2. New
inspect/publish/capture receipts disclose bounded counts, not paths, asset names,
payloads, raw indexes or operating-system errors.

Captures use the mature bounded tar reader without extracting paths. Limits are
33 MiB per input, 8 MiB per file, 32 MiB of payload, 256 flat fingerprinted files,
1024 physical headers, 128 KiB of accumulated names and one local GNU longname
or PAX extension of at most 8 KiB per member. Effective paths are at most 262
bytes and managed basenames at most 255 bytes. Ordinary and contiguous files
are supported; optional root-directory slash aliases follow Python. Global or
stacked extensions, sparse files, long links, size-changing PAX, special files,
links, legacy null-type slash directories and physically truncated padding are
named unsupported domains. Python accepts some broader tar forms. Existing
shared JSON encoding/depth/value/duplicate/finite limits apply. Timestamps are
unsigned 64-bit seconds with checked seven-day expiry rather than Python's
unbounded integer range; raw integer `-0` remains zero. Schema-version equality
preserves Python's acceptance of `true` and `1.0`, while times and sizes require
integer tokens and reject booleans/floats.

The store admits at most four generations, two required captures, a 256 KiB
index and 32 MiB of declared generation payload before reading any asset.
Every payload is checked against its declared size and SHA-256, including the
hardlinked public union. Captures, generations and asset records preserve source
insertion order. The compact index and final LF determine the union name from
their original bytes. Equal index bytes are a complete filesystem no-op; valid
existing numeric schema aliases are not rewritten by initialization. Historical
generations sort newest first with stable ties. A shared URL must retain the
same bytes; its union link uses the last generation's source inode while keeping
the first insertion position. Hardlinks are intentional and accepted.

Publication stages new mode-0755 directories and mode-0644 files, fsyncs them,
replaces the index first, then the relative public pointer. Before a pointer
rename becomes visible, failure attempts to restore the previous index and
remove only newly admitted owned entries. After visibility, failures never roll
back the live pointer or delete its union; failed final cleanup leaves evidence
that a later read refuses. There is no adoption of unindexed/pending residue or
automatic crash recovery. Existing ordinary asset files/directories need root
ownership and no group/other write permission rather than exact 0644/0755;
existing hardlinks and other protected modes remain valid. New writes explicitly
set their fixed modes without changing the process umask.

Selecting a version is separate from publishing/pruning. An unretained revision
and an empty asset store are valid. Equal canonical version bytes at mode 0644
are a no-op; a protected different mode, such as 0600, is repaired by replacement.
Publish/prune preserve the selected version even after all generations expire.

All reads/writes/removals use fixed components under retained directory
descriptors. File identity, owner, modes, size, link count and timestamps are
checked before/after reads and final namespace validation; expected hardlink and
rename changes update witnesses. Rollback refuses observed foreign replacement
entries. These are optimistic observations under the exclusive-owner assumption,
not an atomic filesystem compare-and-swap or protection from another root.
Final success follows a complete store revalidation, retained File close attempts
and lock release/close; failed operations preserve their first known diagnostic.
The locked rustix directory cursor has only RAII `closedir`, whose result cannot
be observed through its safe API, so checked-close claims cover retained File
owners and transient File probes, not that hidden cursor descriptor.

This is a synchronous local filesystem command, with no signal cancellation,
hard OS/filesystem/output wall-clock bound, crash durability receipt or runtime
proof. A changed index/version may already be visible when a later fsync, close
or output failure returns nonzero. The unchanged Python filesystem oracle and
owned lock/tar/fault fixtures test this source-level contract; no real managed
host paths, browser, container, database or network workload are used.

## Local executable inspection

Inspect a candidate file without executing it:

```sh
cargo run --locked -q -p simplestchat-ops -- distribution inspect-executable --path /path/to/simplestchat-ops
```

Success prints the original whole-file `sha256` and `bytes`, `architecture`,
`elfClass`, `littleEndian`, `fileType`, `programHeaders`, and fixed linkage facts:
`noInterpreter`, `neededCount`, `noNeeded`, `pieFlag`, `bindNow`,
`nonExecutableStack`, `noWritableExecutableLoad`, `relroPresent`, `noRpath`,
`noRunpath`, `noTextRelocations` and `noLoaderHooks`. The digest includes unused
and trailing bytes. No loader path, dependency name, symbol, note, source path or
candidate string is printed. Invalid input returns exit 1 and only
`{"passed":false,"failure":"invalid_executable"}`; invalid arguments return 2.
The command starts no child, even to query a candidate's version.

The supported domain is nonempty regular files of at most 256 MiB containing
ELF64 little-endian x86-64 ET_EXEC or ET_DYN, System V/Linux OSABI, with an entry
point in a file-backed executable load segment. ET_DYN means `dynamic`; the
separate `pieFlag` describes DF_1_PIE and does not infer it from the file type.
Static executables and static PIE with a dynamic table but no interpreter or
DT_NEEDED entries are supported. Each file read and seek checks the retained
snapshot stamp; final file/directory binding checks and explicit successful
closes precede the identity. The library's `verify` also compares an explicitly
selected digest, byte count and architecture. The caller owns authentication of
that selection, and later operations must rebind current bytes.

The exact [object 0.40.0 low-level reader](https://docs.rs/object/0.40.0/object/read/elf/index.html)
uses only `read_core`, `elf` and `std`. Before allocating parser reads, the tool
bounds program headers and dynamic entries to 8,192 each, the dynamic string
table to 1 MiB and the interpreter to 4 KiB. Only five bounded regions can enter
the cache (under 2 MiB total); no section/symbol readers, archive parser,
decompressor or executable mapping is used. Extended program counts, truncated
or ambiguous relevant tables, missing dynamic termination, nonzero hidden
entries after termination, duplicate singleton dynamic fields and ambiguous
virtual metadata mappings are refused. Dynamic-table address/offset identities
must agree within one complete file-backed LOAD mapping; partial overlaps and
zero-fill shadow mappings are refused too. Sections, relocation correctness and
machine instructions are not validated. Python's existing glibc server policy
remains unchanged; inert overlap fixtures compare its parser facts, while new
static forms have explicit local fixture assertions.

The mapping domain explicitly uses Linux x86-64 4 KiB base pages. LOAD file and
virtual offsets must be congruent modulo 4 KiB, and every nonempty LOAD's rounded
memory-page span must be disjoint from all others. This conservative refusal
also applies when no dynamic table is declared: a page replacement can shadow
the program-header table itself. The complete original program-header table
must have one unambiguous file-backed mapping. A present PT_PHDR must be unique
and describe that exact offset, address and size.
[Linux](https://github.com/torvalds/linux/blob/master/fs/binfmt_elf.c) derives
AT_PHDR from LOAD plus e_phoff;
[musl](https://git.musl-libc.org/cgit/musl/plain/ldso/dynlink.c) may use PT_PHDR
to derive a base address. These admission
rules prevent contradictory views without attempting to emulate either loader.

These are linkage facts. `noInterpreter` and `noNeeded` do not establish musl
provenance, kernel/CPU compatibility, source revision, a signed artifact or
permission to install or run the file. Package version currently inherits the
workspace version and is not a CLI compatibility protocol. A future musl build
must also control the existing global `/usr/lib64` RPATH flags. Distribution
readiness remains incomplete until an actual controlled musl artifact, its
separate authenticated attestation, harmless checks on an owned Debian 13 host,
and installation/bootstrap ownership evidence exist. Python remains the runtime.

## Separate Ops artifact manifest and local pair

`ops-release.json` is a new, separate schema. It does not extend the signed
server artifact, server `release.json`, its ten-file allowlist or its predicate.
The prospective Ops artifact name is
`simplestchat-ops-<revision>-<target>`; this slice publishes no artifact or archive.

| Exact field | Schema 1 rule |
| --- | --- |
| `schemaVersion` | Integer 1 |
| `artifactType` | `simplestchat-ops` |
| `repository` | ASCII owner/repository: owner 1–39 alphanumeric/hyphen bytes; repository 1–100 alphanumeric/dot/underscore/hyphen bytes; each starts alphanumeric |
| `revision` | Forty lowercase hexadecimal characters |
| `packageVersion` | Decimal `MAJOR.MINOR.PATCH`, each component 0–4294967295 with no leading zeros except zero itself; at most 32 bytes |
| `protocolVersion` | Integer 1, a declared CLI compatibility version |
| `target` | `x86_64-unknown-linux-musl` |
| `buildRunId`, `runAttempt` | Positive signed 64-bit integers |
| `binary` | Exactly `name`, `sha256`, `bytes`; name is `simplestchat-ops`, digest is lowercase SHA-256, byte count is a positive integer at most 256 MiB |
| `inputDigests` | Object with 1–64 declared name-to-SHA-256 entries |
| `evidenceDigests` | Object with 0–16 declared name-to-SHA-256 entries |

Digest names are canonical relative ASCII paths of at most 128 bytes using only
alphanumerics and `/._-`; empty, `.` and `..` components are refused. Digests
are exactly 64 lowercase hexadecimal characters. These maps are declarations:
validation does not locate/read/hash their named files or assess build-input
coverage. Empty evidence is permitted and does not imply attestation. The
initial package-version profile is narrower than general SemVer: prerelease
and build suffixes are refused. Package, protocol, schema and source revision
remain distinct identities; none is inferred by executing the candidate.

Manifest inputs are nonempty regular files up to 64 KiB, with shared strict
duplicate-key/type decoding, UTF-8/16/32 handling, a 128 KiB decoded UTF-8 cap
and the existing depth/value limits. Unknown fields and floats/booleans in
integer fields are refused. `ArtifactManifest` retains the original encoded
bytes and SHA independently from validated fields. Its manual Debug omits raw
bytes and digest-map names. Use that same object after authenticating its
original SHA: `verify_pair(&manifest, binary_path, &selection)` never reopens
the manifest path. This avoids authenticating one document and then interpreting
a replacement. There is no signature verification in this API.

Inspect a local pair against an explicit caller-owned selection:

```sh
cargo run --locked -q -p simplestchat-ops -- distribution verify-pair \
  --manifest /path/to/ops-release.json --path /path/to/simplestchat-ops \
  --selection /path/to/selection.json
```

Argument order is exact. The selection JSON has exactly the seven source fields
`repository`, `revision`, `packageVersion`, `protocolVersion`, `target`,
`buildRunId` and `runAttempt`, with the same field rules, at most 16 KiB encoded
and 32 KiB decoded. It is caller input, not proof of a signature or observed
GitHub identity. All seven identities must match before the executable is read.
The executable must match the declared original SHA/count through the existing
inspector, with x86-64 architecture and no interpreter, DT_NEEDED or loader-hook
entries. Those limited static-linkage facts are compatible with the declared
target; they do not prove which libc/toolchain built the file or that it runs.

Success prints only `manifestSha256`, `manifestBytes`, bounded `claims` and the
existing `executable` facts. Claims contain the declared identities and
`inputDigestCount`/`evidenceDigestCount`; map names/entries and raw bytes stay
private. Success includes no `passed`, signed, musl-provenance or runtime-ready
assertion. Failure is exit 1 and only
`{"passed":false,"failure":"invalid_ops_artifact"}`; invalid arguments return 2.
All file reads keep the existing snapshot/close guarantees. This is not a lock
against later writes: subsequent execution/installation must rebind its current
bytes. No candidate is executed, fetched, unpacked, installed or transported.
Python remains the supported runtime, and authenticated distribution readiness
is still incomplete. Tests assert this new contract directly; no Python parity
is claimed for a schema Python does not implement.

## Nonexecuting Ops build plan

Prepare a local plan without starting Cargo or executing a candidate:

```sh
cargo run --locked -q -p simplestchat-ops -- distribution build-plan \
  --selection /path/to/selection.json --source-root /path/to/source
```

Argument order is exact. The selection has the same seven-field caller-owned
shape as `verify-pair`; it is a declaration, not an authenticated GitHub/source
observation. The library separates `collect_inputs(source_root)` from the pure
`plan(&selection, &inventory)`. `inspect` composes them. No command is started,
toolchain resolved, registry accessed, artifact generated or candidate run.

The `workspace-ops-capacity-v2` inventory selects original bytes of `Cargo.toml`,
`Cargo.lock`, `rust-toolchain.toml`, `.cargo/config.toml`, the Ops/load-test/xtask
manifests, the two current mediasoup path-patch manifests, and every regular
`*.rs` under `ops/src`, the load-test library entry and capacity dispatcher, and
Rust files under its `capacity`, `capacity_run` and `owned_process` directories.
This covers the current named workspace discovery manifests, not complete
Cargo/compiler I/O closure. The tool does not interpret
TOML, resolve changed workspace members/path patches, or read other workspace,
vendor or registry source. A later builder must audit the complete input closure
instead of treating this fixed selection as one. Implicit `ops/build.rs`, an
`ops/.cargo`, `load_tests/build.rs`, `load_tests/.cargo` and legacy `.cargo/config`
are refused rather than silently omitted from this profile. Other unselected
paths are not source inputs here.

At most 256 file records are selected, each at most 1 MiB except `Cargo.lock`
at 4 MiB, with a 16 MiB original-byte total. Empty selected files are recorded accurately;
the inventory does not establish that a manifest parses or source compiles.
Source traversal has at most eight descendant directory levels and 32 selected
directory owners, including the source root. Selected source names are bounded
ASCII components; links and special files are refused. Enumerations have a
combined 8,192-entry/1 MiB name budget and 255-byte entry-name limit. Source-path
ancestors have at most 32 retained owners, shared across every file. The bound
is 80 retained File handles plus one transient enumeration/binding handle:
32 ancestors, 32 selected directories and 16 regular-file owners. Fixed discovery
files retain their original descriptors. Each Rust leaf is hashed and explicitly
closed before admitting the next; its anchored parent, original full stamp and
digest remain. Final verification opens that same visible inode without following
links, completely rehashes it and checks its unchanged stamp before closing it.
This is a checked new open, not continuous original-file custody. The larger
record inventory therefore does not raise the previous live-descriptor ceiling.

Retained descriptor-relative directories are enumerated with rustix's mature
`Dir` reader. Files, selected directory stamps/membership and visible bindings
are rechecked across collection. Outside ancestors bind inode/device, mode and
owner/group without freezing unrelated siblings. Every retained File close is
attempted explicitly, including after validation failure; rustix's temporary
cursor is RAII-closed internally, with no fallible close API. This detects
observed mutation/replacement; it is neither an atomic snapshot nor a lock.

The canonical SHA-256 frame is the ASCII bytes
`simplestchat-ops-capacity-source-inventory-v2` plus NUL, a big-endian u32 file
count, then entries sorted by relative ASCII path. Each entry is big-endian u16 path length,
path bytes, big-endian u64 original byte count and the 32 raw bytes of that
file's SHA-256. Names are part of the digest but stay private. Public inventory
output and Debug contain only the profile, SHA, count and byte total. This
measures selected bytes; it does not prove their relationship to the declared
revision or cleanliness of a Git tree.

The recipe uses typed unresolved executable/directory slots and literal argv,
never a shell template. It selects Rust 1.99.0, an x86-64 GNU Linux host toolchain,
and `build --frozen --release -p simplestchat-ops --bin simplestchat-ops --target
x86_64-unknown-linux-musl --target-dir` followed by the owned target-directory
slot. Its clear-then-set environment contains explicit Cargo/home/temp, compiler,
linker and authenticated host-tools directory slots. No ambient PATH fallback
is authorized. Target encoded rustflags select static CRT, baseline x86-64 CPU,
self-contained linking and the `ld.lld` flavor. They replace this workspace's
global GNU RPATH through Cargo's documented
[flag precedence](https://doc.rust-lang.org/cargo/reference/config.html#buildrustflags).
Explicit `--target` keeps those target flags away from host proc macros. Rust's
[linker options](https://doc.rust-lang.org/rustc/codegen-options/index.html#link-self-contained)
describe those selections; no successful link or resulting linkage is inferred.

Cargo also reads ancestor and CargoHome configuration, including legacy files
and included configuration. Consequently clearing environment variables alone
does not isolate a build. Before resolving this plan, an owner must create a
fresh staging tree with controlled ancestors/config, rebind inventoried bytes,
authenticate actual Cargo/rustc/rust-lld/target components and host-tool/runtime
inputs, authenticate a prepopulated registry, isolate CargoHome, audit the full
input closure and own an empty target directory. Cargo's
[`--frozen`](https://doc.rust-lang.org/cargo/commands/cargo-build.html#manifest-options)
means locked plus offline; it neither authenticates cached bytes nor proves a
build-script sandbox. Build process, closed outputs, ELF/pair checks, provenance
and Debian runtime evidence are still unmeasured prerequisites.

Success prints only `schemaVersion`, `kind`, bounded `claims`, `inventory`, the
unresolved `recipe` and fixed `prerequisites`. It has no passed/build-success,
binary digest, signature or runtime-readiness claim. Failure is exit 1 and only
`{"passed":false,"failure":"invalid_ops_build_plan"}`; invalid arguments return 2.
The existing Ops artifact and server release contracts are unchanged. This is
a new plan contract tested directly, not a Python producer receipt or oracle.

## Offline lock-bound registry hydration

Prepare a Cargo directory source from explicitly selected local cache archives:

```sh
cargo run --locked -q -p xtask -- ops-artifact registry \
  --request /controlled/registry-request.json --request-sha256 ORIGINAL_REQUEST_SHA256
```

The UTF-8 JSON request has exactly these fields and at most 64 KiB. Its original
bytes must match the selected lowercase SHA-256:

```json
{
  "schemaVersion": 1,
  "lock": {
    "path": "/controlled/materials/Cargo.lock",
    "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
  },
  "archivesRoot": "/controlled/cache/crates",
  "output": "/controlled/fresh-registry-output"
}
```

All paths are absolute, canonical in spelling, and walked without following
links. The three material/output paths have at most 20 components. Output must
be absent, including an existing empty directory, and outside the request,
lockfile and archive-root namespaces. The archive root is one explicitly selected
flat directory; the importer does not discover Cargo Home or acquire materials.

The selected original lockfile is at most 4 MiB. The maintained TOML parser's
80-level recursion guard remains enabled, and duplicate TOML keys are refused.
The supported profile is lock version 4, with `version` and `package` fields;
package fields are `name`, `version`, optional `source`, `checksum` and
`dependencies`. There are at most 4,096 package records and 1,024 registry
selections. Packages with no source/checksum remain external path packages.
Every sourced package must select exactly the ordinary crates.io registry URL
and a lowercase SHA-256 checksum. Git/alternate registries, missing checksums,
duplicate output identities and unknown lock fields refuse. Names are bounded
ASCII alphanumeric/hyphen/underscore tokens; versions are bounded digit-leading
ASCII alphanumeric/dot/hyphen/plus/underscore tokens without `..`. This is a
conservative filename profile, not a general SemVer implementation or resolver.

Each selected `<name>-<version>.crate` is a regular, single-link, nofollow file.
The complete original bytes, count, retained file stamp and visible namespace
binding are checked and the descriptor explicitly closed before gzip or tar
processing. Its actual SHA-256 must equal the lock checksum. A bounded mature
gzip reader validates CRC/ISIZE and consumes exactly one whole member; trailing
bytes, concatenated gzip members and truncation refuse. A mature raw tar pass
bounds physical headers and GNU-longname bodies before interpreted entries are
read. Only ordinary files, directories and bounded GNU-longname extensions are
supported. PAX/global/longlink/sparse/device/link records refuse. Two zero blocks
and entirely zero terminal padding are required; appended tar data refuses.

Every effective name stays below the exact selected crate root. Absolute,
dot/parent/empty components, controls, backslashes, duplicate paths,
file/directory conflicts and a root `.cargo-checksum.json` or descendants refuse.
Trailing slashes are admitted only for directory records. Implied and explicit
directories count before any crate payload is written. The normalized
`Cargo.toml` must declare the selected package name/version; no manifest code,
build script or installer runs. Regular crate-relative `.git` payloads are
ordinary checksum-covered data; this operation never discovers or invokes Git.
The separate source exporter's `.git` refusal remains unchanged.

Limits are 16 MiB per compressed archive, 128 MiB compressed in aggregate,
128 MiB expanded tar per archive, 1 GiB expanded tar/output, 16 MiB per payload,
8,192 effective nodes per archive, 65,536 total output nodes and 16 MiB of member
names. Names have at most 1,024 bytes and 16 components. Raw headers are capped
at 16,384 per archive; GNU-longname bodies at 8 KiB each and 1 MiB per archive.
The complete-tree verification reader additionally retains its existing bounded
entry/name/file profile, with aggregate file bytes narrowed to 1 GiB here.

The fresh 0700 output contains only `registry/`. Descendant directories are
0700 and files 0600, or 0700 when the archive declares an executable bit; host
owner, timestamps, special modes and links are not restored. Exact written
bytes are read back and bound to retained stamps. Each generated
`.cargo-checksum.json` has `package` equal to the original archive checksum and
`files` mapping every actual regular payload's relative name to its SHA-256,
excluding the generated checksum itself. Cargo's
[directory-source checksum metadata](https://doc.rust-lang.org/cargo/reference/source-replacement.html)
detects accidental changes; it is not an origin signature. An independent
full-tree scan must equal the expected `ops-producer-tree-v1` inventory, including
generated checksums and empty directories; added, missing or changed members
refuse. The registry subtree can be selected as `kit/registry` when preparing a
separate curated kit and measuring its complete kit pin.

Standard output is one bounded `ops-offline-registry-observation` with original
request/lock hashes and counts, package count, aggregate archive identity/counts,
and output tree identity. `ops-registry-archives-v1` hashes its NUL-terminated
profile, then sorted crate-directory records: big-endian u32 UTF-8 name length,
name bytes, raw 32-byte archive SHA-256 and big-endian u64 archive byte count.
No input paths, archive content or credentials are printed. No success marker
is written into partial output; final exit 0 is required. Partial output is
retained on refusal for private inspection and is not signing authority.

The importer dispatches no Cargo, compiler, imported crate code, installer or
network command. One Signals/deadline owner covers admission through explicit
input/output close attempts and the existing bounded final replay. The shared
128-descriptor budget covers retained directory/file owners and transient
cursors; inherited/runtime descriptors are outside that accounting. The
18-minute active/2-minute finalization watchdog is not a hard bound on filesystem,
OS scheduling or reap stalls, and hosted timing is unmeasured. Snapshot checks
detect observed mutation; they are neither locks nor exclusion of later
same-user changes. This measures only a caller-selected lock/archive/output
relationship. Registry origin, Git/source authentication, target/feature graph,
complete Cargo I/O closure, authenticated compiler/runtime and a ready builder
kit remain separate obligations. Existing producer/source/server contracts and
production Python callsites are unchanged.

## Selected local Git source export

Export one selected local commit without checking out, compiling or executing its
source:

```sh
cargo run --locked -q -p xtask -- ops-artifact source \
  --request /controlled/source-request.json --request-sha256 ORIGINAL_REQUEST_SHA256
```

The UTF-8 request is at most 64 KiB, bound to its original lowercase SHA-256, and
has exactly these fields:

```json
{
  "schemaVersion": 1,
  "selection": {
    "repository": "swiftraccoon/simplestChat",
    "revision": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "packageVersion": "0.1.0",
    "protocolVersion": 1,
    "target": "x86_64-unknown-linux-musl",
    "buildRunId": 123,
    "runAttempt": 1
  },
  "repository": "/controlled/local-repository",
  "git": {"path": "/controlled/tools/git", "sha256": "GIT_SHA256", "bytes": 12345},
  "output": "/controlled/fresh-source-export"
}
```

Replace the example revision, run identities, Git digest and byte count with
explicit selections. The seven identity fields remain declarations. Paths use
the producer's normalized absolute, control-free profile; output has at most
20 components, must be fresh, and cannot overlap the repository, selected Git
file or request. Exported material is `output/source/`; its measured full-tree
pin can feed the unchanged producer request. The fixed private
`source-observation.json` is outside that tree.

Only an ordinary local repository with descriptor-bound `.git/objects` is
supported. Bare repositories, linked worktrees, `commondir`, `gitdir`, object
alternates and symlinked ancestors are refused. A fresh bare `git-view` has a
literal configuration, empty refs/objects and retained namespace/metadata checks.
`GIT_OBJECT_DIRECTORY` selects the original local objects; original repository
configuration is never loaded. Global/system configuration and inherited Git
overrides are cleared. Fixed
[Git options](https://git-scm.com/docs/git) disable replacement objects, lazy
promisor fetches, optional locks and pagination. Unsupported options fail closed.
There is no checkout, archive attribute expansion, filter, hook, fetch or received
shell/argv stage. Selected Git, its dynamic runtime/helpers and the local object
database are caller-owned trust inputs; this is not an execution sandbox or
upstream repository authentication.

The selected Git file is nonempty, regular, at most 64 MiB, executable,
root/current-user owned, without group/other write or set-ID permissions. Its
original SHA/count are checked through a retained descriptor and the exact
hash-bound metadata is rechecked before and after every command. No PATH
fallback is allowed. Installed hardlinked command aliases are admitted; their
link count is retained in the full stamp checks. Those checks detect observed change; they do not authenticate
Git acquisition or prevent a later same-user/root replacement. No exclusive
inode ownership, descriptor-based exec or atomic runtime snapshot is claimed.

A NUL-delimited raw tree catalogue admits at most 8,192 entries, 4 MiB output,
2 MiB path bytes, 16 descendant levels, 4096-byte paths and 255-byte components.
Directories, including explicit empty trees, are preserved. Links, submodules,
unsafe or `.git` components, duplicate/conflicting names and missing parents
are refused. Each blob is at most 16 MiB and the source total at most 256 MiB.
Raw commit bytes are bounded to 1 MiB, each raw tree to 4 MiB and their combined
retained bytes to 8 MiB. The catalogue is repeated, and the raw commit, root and
every distinct nested tree are captured and explicitly rehashed with selected
Git before and after copying. Recursive `ls-tree` alone does not validate every
nested loose-tree payload against its object ID. Every stored blob is also
rehashed with `hash-object --no-filters` and compared to its catalogue ID.
[Raw object reads](https://git-scm.com/docs/git-cat-file) and
[unfiltered hashing](https://git-scm.com/docs/git-hash-object) keep this boundary
in Git rather than a new Git object parser.

Fresh directories are 0700 and regular files 0600 or 0700 according to the Git
executable bit. Writer-bound byte checks, retained stamps and namespace checks
precede independent complete-tree hashing with `ops-producer-tree-v1`; the result
must match exactly the admitted catalogue and captured blob bytes. The shared
128-descriptor budget includes owners and a sixteen-slot command/replay reserve;
deep path combinations may be refused. File owners attempt explicit closes;
temporary mature directory cursors are RAII-closed. One signal/deadline owner
survives closes and bounded result replay. The producer's 18-minute active plus
two-minute settlement watchdog applies; filesystem/scheduler/reap stalls and
cold xtask compilation have no hard wall-clock guarantee.

Final zero exit and bounded JSON describe an **observed local commit/tree/source
relationship** and selected Git/request identities. Paths, catalogue names,
source bytes and commit author/message data stay private. The output does not
prove GitHub origin/main status, authenticated acquisition, a successful build,
signing readiness, musl provenance or runtime compatibility. This is an optimistic
observation of a caller-owned mutable object database and output, not an atomic
snapshot or lock. Failure can retain private partial material and even an
observation after a later close/replay failure; file presence alone is not success.
Tests use fake/fault processes and tiny owned local Git repositories with isolated
configuration/templates and no hooks or network. No candidate is built or run.

## Controlled local unsigned producer

`xtask` can execute a fixed build recipe using explicitly selected local materials:

```sh
cargo run --locked -q -p xtask -- ops-artifact build \
  --request /controlled/producer-request.json --request-sha256 ORIGINAL_REQUEST_SHA256
```

This command authorizes a build on the current Linux x86-64 host. Its dispatcher
has no material-acquisition or installation stage. It never executes the candidate,
signs or switches a production callsite. Ops remains
process-free; xtask calls its existing inspector, retained-manifest pair validator
and unsigned assembly library directly. The new first-party dependency adds Ops
source to the native-DTLS cache inputs without changing the server ten-file release,
required jobs, attestation policy or Python runtime.

The original UTF-8 request is at most 64 KiB, pinned by its lowercase SHA-256, and
has exactly these fields:

```json
{
  "schemaVersion": 1,
  "selection": {
    "repository": "swiftraccoon/simplestChat",
    "revision": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "packageVersion": "0.1.0",
    "protocolVersion": 1,
    "target": "x86_64-unknown-linux-musl",
    "buildRunId": 123,
    "runAttempt": 1
  },
  "source": {"path": "/controlled/materials/source", "sha256": "FULL_TREE_SHA256"},
  "builderKit": {"path": "/controlled/materials/kit", "sha256": "FULL_TREE_SHA256"},
  "output": "/controlled/new-run"
}
```

The example digest placeholders must be replaced with independently selected
64-character digests. The seven selection fields use the existing Ops schema;
revision/repository/run identities remain declarations. Paths are normalized
absolute UTF-8 without control characters, at most 4096 bytes and 32
components; output has no colon, at most 20 components and must be fresh. Source, kit and
output cannot contain one another. Duplicate/unknown fields, numeric type changes
and wrong original request bytes are refused before material or output access.

The new full-tree pin profile is `ops-producer-tree-v1`, distinct from the partial
`workspace-ops-capacity-v2` inventory. Its SHA input begins with the profile plus NUL, followed
by entries sorted by relative UTF-8 path, including the empty root path. Each entry
contains a big-endian u32 path byte length, path bytes, one directory-kind byte
(1 for directory), one executable-file-bit byte, big-endian u64 file byte count
(zero for directories), and 32 raw file SHA bytes (zero for directories). Directory
permissions are not encoded; original regular-file executable-bit distinctions
are encoded. No self-computed pin authenticates acquisition or the declared revision.

Trees admit only regular files and directories on the selected filesystem:
symlinks, hardlinked files, special files and set-ID modes are refused. The curated
kit has only `toolchain/`, `host-tools/` and `registry/` at its root. Required roles
are `toolchain/bin/cargo`, `toolchain/bin/rustc`,
`toolchain/lib/rustlib/x86_64-unknown-linux-gnu/bin/rust-lld`, `host-tools/bin/cc`,
and the musl target library subtree. Ordinary rustup installations with aliases
need a separately prepared kit. Source is bounded to 1 GiB, kit to 8 GiB, each file
to 512 MiB; together they have at most 131,072 entries, 16 MiB of relative names,
32 descendant levels and 4096-byte relative paths. Private staged directories are
0700, data 0600 and executable files 0700, with current-user ownership and no
special permission bits at admission. Streaming copy checks actual stored
bytes before admitting its exact metadata stamp, then independently re-enumerates
and rehashes original and staged trees before the first tool and after the build.
These are optimistic snapshots, not
an atomic filesystem lock against the caller or root.

The fresh output contains `source/`, `kit/`, empty Cargo/home/temp/target slots,
private logs and observations, then `unsigned/` from the existing assembly API.
Cargo runs from the new parent with `--manifest-path source/Cargo.toml`, so source
`.cargo` files remain measured descendants rather than effective configuration.
Its sole generated `.cargo/config.toml` replaces crates.io with the pinned
`kit/registry` directory source and sets offline mode. Every outside ancestor,
and any existing `.cargo` there, must have root/current-user ownership without
group/other write authority; both legacy and modern ancestor config files are
refused. Fresh CargoHome has neither configuration spelling. These rules follow
Cargo's [discovery and relative-path rules](https://doc.rust-lang.org/cargo/reference/config.html).

The clear-then-set environment supplies only fixed owned path slots, locale,
compiler/linker roles and the plan's encoded target flags. Fixed version/sysroot
probes precede bounded `cargo metadata --frozen` and the fixed package/bin/target
build. Metadata admission covers returned **manifest locations** under source or
registry, not every reported source path or compiler I/O. Its selected package
version must match the explicit selection. Rust 1.99.0's
[pinned target](https://github.com/rust-lang/rust/blob/1.99.0/compiler/rustc_target/src/spec/targets/x86_64_unknown_linux_musl.rs)
supports static PIE; the recipe preserves that default and the unchanged ELF/pair
policy. Fake recipe tests do not establish a successful actual link.

`--frozen` is locked plus Cargo offline, not network isolation for build scripts.
Directory-source [checksum metadata](https://doc.rust-lang.org/cargo/reference/source-replacement.html)
detects accidental modification, not malicious input. Authentication of source,
registry, compiler, host tools and their dynamic runtime is caller-owned. Cargo
metadata may read an external manifest before refusing it; build scripts, proc
macros and host tools can access host files, network and processes. Their complete
I/O/resource closure and filesystem quotas are not proved by this command.

A checked shared budget limits producer-owned descriptors to 128, including
transient directory cursors. Native setup/settlement reserves sixteen slots;
material/config/tool owners close before reserving one hundred for the existing
Ops library calls. Deep combinations may be refused even within individual path
limits. Inherited stdio and runtime signal/poll registrations are outside this
file-owner budget. File owners explicitly attempt close; directory cursors use
the mature reader's RAII close without a fallible-close claim.

The watchdog allows 18 minutes for active work and two for final settlement,
verification, closes and bounded output replay. Commands subtract their grace
and a 20-second scheduling allowance from remaining active time. Signals remain
owned through final output/close/deadline fences, and known nonzero statuses win
over later cleanup errors. Combined build output is privately retained with an
actual 64 MiB pipe-reader cap; probes and metadata have smaller fixed caps. This
is not a hard filesystem/scheduler/reap wall-clock guarantee or measured hosted
build timing; cold xtask compilation precedes its clock.

The final bundle includes measured material, selected-tool, recipe, resolution
and Cargo/log observations, each at most 1 MiB. Raw probe/metadata output and the
combined build log stay private outside it. Observations do not authenticate a
build, prove musl provenance/ABI or authorize signing. Successful CLI output is
bounded counts/hashes and selected claims; require its final zero exit. Failure
can retain private partial inputs and even a completed unsigned inventory after
a later failure; no inventory is a producer-success or signing-readiness marker.
Trusted-workflow attestation, a fresh independent controller verifier, a real
controlled musl build and owned Debian runtime evidence remain outstanding, as
do transport/bootstrap/install ownership. Validation uses inert fake Cargo/tools
and metadata-only nonexecutable ELF fixtures, with no real build or candidate run.

## Local unsigned artifact preparation

Prepare signing inputs from a local pair without building, signing, running or
installing the candidate:

```sh
cargo run --locked -q -p simplestchat-ops -- distribution prepare-artifact \
  --manifest /path/to/ops-release.json --path /path/to/simplestchat-ops \
  --selection /path/to/selection.json --evidence /path/to/evidence-sources.json \
  --output /path/to/new-private-output
```

Argument order is exact. The manifest and seven-field selection keep their
existing schema. `evidence-sources.json` is a separate caller-selected JSON object
mapping each declared evidence logical name to an absolute local source path.
For example, `{"build/receipt.json":"/private/evidence/receipt.json"}` selects
bytes; it does not authenticate the receipt or interpret a build-success bit.
The mapping has at most sixteen entries, 16 KiB encoded and 32 KiB decoded.
Logical names use the manifest's existing conservative relative ASCII grammar.
The exact name set must match `evidenceDigests`; every SHA is compared against
the copied original bytes. Evidence must be nonempty regular nonlink files, at
most 1 MiB each and 16 MiB in total. Empty evidence is allowed and means no
evidence bytes were supplied. `inputDigests` remain unmeasured declarations.

All seven identities are compared before binary or output access. The library
borrows the retained `ArtifactManifest`, never reopens its old path, and copies
its original bytes unchanged. Binary and evidence paths must be absolute,
control-free, nonlink walks of at most 32 components. Sources use the existing
descriptor-bound Snapshot reader and checked streaming. Output is a fresh 0700
directory beneath an existing nonlink parent; even an empty existing directory
is refused. Its names are fixed and flat, with exclusive 0600 files:

- `simplestchat-ops` and original `ops-release.json`;
- zero to sixteen `ops-evidence-00.bin` through `ops-evidence-15.bin`, ordered by
  logical name, plus the private `ops-evidence.json` mapping;
- `ops-predicate.json` and the last unsigned inventory `ops-preparation.json`.

Before writing each payload, its exclusively created empty file's kernel-assigned
creator owner must match the retained fresh directory's owner. Directory/file
identity, modes and owner/group remain bound afterward. This is a local file
ownership check, not authentication of the caller or its selected claims.

The writer explicitly closes each payload, reopens only its exact inode/full
stamp through Snapshot, and hashes the actual stored bytes. This catches a
same-length destination write before the writer's initial metadata sample.
Namespace, file stamps and the complete fixed output inventory are rechecked.
The existing pair/ELF policy then validates the closed copied binary against the
retained manifest. Sources, writer descriptors and output directory owners close
before CLI success. During assembly, at most 32 shared output owners and
sequential bounded source/destination readers keep retained file descriptors
below 100; directory cursors and binding probes add bounded transient handles.

The new predicate type is
`https://github.com/swiftraccoon/simplestChat/attestations/ops-artifact/v1`.
Its `ops-artifact-identity` document binds the original manifest, selected claims,
inspected executable facts, and measured payload/index SHA/counts. The final
`ops-unsigned-artifact` inventory also binds that predicate. Neither document is
a signature, authenticated source/build observation, producer receipt, musl
provenance proof or runtime/installation authorization. Opaque copied evidence
is not assessed for those properties. The current server predicate and ten-file
release contract are unchanged; no workflow or production callsite consumes the
new format.

Success prints only bounded selected claims, manifest/preparation original SHA
and byte counts, payload/evidence counts and fixed outstanding prerequisites.
Logical names, input paths and raw evidence stay out of stdout, Debug and fixed
errors. Failure returns exit 1 and exactly
`{"passed":false,"failure":"invalid_ops_preparation"}`; invalid arguments return
2. Partial private evidence is retained. Earlier failures never create the final
inventory, and observed later failures attempt to unlink only that inventory's
own inode; foreign replacements are preserved. Filesystem errors can prevent
rollback, so the inventory deliberately contains no success/readiness bit and
its presence cannot establish operation success. CLI status includes the final
namespace and close checks. This is not atomic multi-file publication, a lock or
a power-loss durability claim.

A real controlled musl build, authenticated source/toolchain/host/registry inputs,
complete Cargo I/O closure, trusted-workflow attestation and fresh controller
verification, owned Debian runtime checks, and transport/bootstrap/install
ownership remain prerequisites. Tests use inert ELF bytes and owned filesystem
faults for this new contract; no Python parity is claimed.

## Pure release evidence policy

Inspect collected receiver evidence locally:

```sh
cargo run --locked -q -p simplestchat-ops -- release receipt --envelope /path/to/envelope.json --receipt /path/to/fetch-outcome.json
```

The exact argument order is required. Success prints only `receipt` (the sixteen
allowed fields), `envelopeSha256`, `envelopeBytes`, `receiptSha256` and
`receiptBytes`. Both identities describe original snapshot bytes, including their
encoding and whitespace. Exit 0 means the receipt is consistent with the supplied
envelope; a consistent failed receipt remains failed in that projection. It does
not authenticate a signature or prove transport/process success or durable writes.
Failure returns exit 1 with only
`{"passed":false,"failure":"invalid_release_receipt"}`. Invalid arguments return
exit 2. The reader enforces the same regular-file, size, symlink and change guards
as manifest inspection. Python remains the release runtime.

`release::trust` validates the current claim, proof and receiver envelope against
explicit selected identities. The predicate has twelve exact fields and binds
eight file digests; the proof binds the four existing verification subjects.
These are schema and relationship checks. They never invoke `gh`, authenticate
a signature, observe GitHub API identity, or authorize a host operation. Python
remains the operational reference and transport.

`Document<T>` retains the exact original encoded bytes and SHA-256 separately
from its typed projection. Unknown receipt fields remain private in those source
bytes and are omitted from the public sixteen-field `ReceiverReceipt` projection.
Its success consistency check requires matching revision/artifact/ZIP identities,
`passed`, `settled`, `finalized`, complete status/phase, exact byte count, no failure,
and both declared archive/manifest digests. The owning operation must still prove
its process exit status, current byte bindings and durable publication. A decoded
journal `finalized` bit alone proves none of those actions.

`HelperSet` preserves the exact Python base/fetch/force source-helper declarations.
`PreflightReceipt` models the producer's three-field output, including its strict
prepared boolean and selected helper request. This is stronger than the current
Ansible two-field assertion; it does not establish a fresh run, root/OS/path
checks or a held lock. No installed binary or runtime callsite consumes these
new library APIs yet.

Claims/proofs accept at most 64 MiB, envelopes/journals/preflight receipts 16 KiB,
and receiver receipts 64 KiB. Helper requests retain the 4,096-character argument
limit, with a bounded UTF-8/16/32 byte envelope. Each JSON decode has a matching
encoded limit and at most twice that decoded UTF-8 budget, plus the existing
128-container/100,000-value bounds. Numeric tokens retain their original spelling;
strict integer fields never accept booleans or floats. Unknown receipt fields
cannot appear in fixed error text or the public projection.

## Offline image archives

`release archive --manifest PATH --path PATH` validates the manifest first, then
opens one nonempty regular archive of at most 8 GiB through the same retained,
nonlink directory walk. It hashes the entire original transfer before parsing
tar metadata. Indexing checks the open file's stamp before and after every read
and seek so a changed extension header cannot bypass earlier allocation limits.
File and visible directory bindings are checked after hashing, indexing and
validation; all descriptors close explicitly before a successful result. Later
writes by the filesystem owner still require revalidation at the next operation.
No archive member chooses a host path, and no unpack operation is called.

The reader uses the already locked `tar` 0.4.46 with default features disabled.
Raw seekable entries bound physical headers and extension bodies before normal
seekable entries resolve their effective names. Checked offsets must fit the
retained file length. Layers are skipped by seeking; only referenced JSON is
read. The supported domain is uncompressed, well-formed Docker-save/OCI tar
metadata with Unicode-scalar UTF-8 names. The JSON decoder retains its UTF-8/16/32
and numeric rules above.

| Bound | Limit |
| --- | --- |
| Original archive | 8 GiB |
| Logical regular/contiguous files and directories | 100,000 |
| Physical headers, including extensions | 200,000 |
| Each local PAX or GNU long-name body | 8 KiB |
| Each effective member name / sum of effective name lengths | 8 KiB / 16 MiB |
| Each referenced JSON member | Nonempty, at most 1 MiB |
| OCI descriptor traversals | 8, including the final image manifest |

One local PAX **or** GNU long-name extension may precede each logical member.
PAX records use the dependency's decoder; keys must be unique and valid UTF-8,
and a declared `size` must equal the following raw header size. Global PAX,
GNU long links, sparse metadata, repeated/combined extensions, malformed or
hidden PAX records, non-UTF-8 names, nonempty directory bodies and old V7
null-type trailing-slash directory aliases are refused explicitly. Hard links,
symlinks, devices, FIFOs, duplicate normalized names, noncanonical paths and
out-of-bounds bodies are refused. These limits and the mature reader's strict
header checks are an explicit supported boundary, not arbitrary `tarfile`
compatibility. Default first-zero/EOF termination is retained; all original
bytes, including trailing data, are still covered by the transfer digest.

Within that domain, validation follows the Python reference's order: Docker
image count/tag/layers/config, optional OCI layout/index/annotations and hashed
metadata, optional legacy repository keys, then platform, user, revision and
command. Descriptor sizes require JSON integers, while OCI schema version uses
Python numeric equality. Falsey descriptor URLs remain allowed. Unknown fields
remain accepted where the reference accepts them. Ordered OCI layer paths and
sizes must match Docker's selection; layer payload digests and config `rootfs`
are not independently checked. The existing full-transfer digest binds every
original archive byte.

Success prints only `archiveSha256`, `archiveBytes`, `configSha256`, `revision`,
`imageTag` and `platform`. It prints no config, private environment, member name,
path or attestation claim. Failure returns exit 1 and exactly
`{"passed":false,"failure":"invalid_release_archive"}`; invalid arguments return
exit 2. The library retains original config bytes for a later private operation;
its debug output contains their length only. Tiny owned archive/CLI fixtures
compare the actual Python validator and test bounds, supported extensions,
explicit refusals, ordering, mutation and close failures. These checks start no
engine and do not prove deployment, image execution or ABI compatibility.

## Generator image archive identity

The manual load-generator image owner uses the separate library profile
`release::archive::verify_generator_archive(path, &selection)`.
`GeneratorSelection::new(observed_image_id, revision)` admits one canonical
`sha256:` digest and a lowercase 40-digit source revision. The caller owns the
selected daemon/image observation; this constructor supplies no authentication.
The image tag is fixed to `simplestchat-loadtest:ci`, platform to `linux/amd64`,
user to `10001:10001`, work directory to `/results`, Entrypoint to
`["/app/load_test"]` and Cmd to `["--help"]`. The revision label and exactly one
`SOURCE_REVISION=` environment entry must agree with the selected revision.
Other private config/environment values are never returned or logged.

The existing server `verify_archive`/CLI, signed release ten-file contract and
Python policy are unchanged. This generator profile does not create a public
CLI or accept a server manifest as a substitute for a selected image identity.
It returns only measured transfer SHA-256/byte count, observed image ID, config
SHA-256, optional selected root digest, fixed format/platform/tag/revision and
layer count. The transfer getters let the owner compare the subsequent zstd
round trip without decoding a public JSON projection.

For a classic Docker export, the selected ID must equal the hash of the actual
original config bytes. Every ordered `manifest.json` raw layer is streamed and
its hash must equal the corresponding config `rootfs.diff_ids` entry. Layer
names, distribution hints or filenames never substitute for hashing the stored
raw bytes. A present legacy `repositories` map must contain exactly the fixed
repository/tag and its final raw DiffID. Docker's classic exporter writes raw
layer streams, while optional distribution descriptors can refer to compressed
representations; these are different identities.
[Moby's classic exporter](https://github.com/moby/moby/blob/v28.5.1/image/tarexport/save.go)
and [its raw-layer verifier](https://github.com/moby/moby/blob/v28.5.1/layer/ro_layer.go)
describe that distinction.

The Docker compatibility manifest's optional `LayerSources` field is ignored
distribution-registration metadata, not authoritative payload proof. The whole
transfer hash covers its original bytes, but the verifier does not validate,
register, fetch or load these hints. Contradictory hint digests or URLs cannot
replace actual config/raw-layer hashing. This scope does not establish how a
later Docker loader will register distribution metadata.

For a containerd export, the selected ID must instead equal the actual verified
outer descriptor digest in `index.json`. It can identify a parent index rather
than the config. A complete one-branch graph, at most eight index/manifest
descriptors deep, must reach the same Docker-compatible config and ordered
stored layers. Every blob referenced by that selected authoritative OCI graph
must be present
and match both descriptor SHA-256 and integer size. The top descriptor requires
both exact annotations: `io.containerd.image.name` is
`docker.io/library/simplestchat-loadtest:ci`, and
`org.opencontainers.image.ref.name` is `ci`. Alternative values for these keys, multiple
images/branches, foreign platforms, remote URLs, embedded data, artifact subjects,
cycles and incomplete graphs refuse. The supported media types are OCI/Docker
schema-2 indexes and image manifests/configs, raw tar layers, gzip layers and
OCI zstd layers. Raw OCI layers also agree with config DiffIDs. Compressed
payload hashes describe stored bytes and are not equated to uncompressed
DiffIDs. See the
[containerd exporter](https://github.com/containerd/containerd/blob/v2.1.4/core/images/archive/exporter.go)
and [OCI DiffID definition](https://github.com/opencontainers/image-spec/blob/v1.1.1/config.md#layer-diffid).

An optional OCI extension is checked in full even when the selected identity is
a classic config ID. In particular, some classic exports retain compressed
distribution descriptors while saving only raw layer bytes. Those extensions
are not self-contained under this profile and are refused; this is not general
compatibility with every valid Docker export. No `docker save --platform` is
added to change the observed parent identity. The verifier never fetches a
missing descriptor or silently ignores an advertised OCI graph.

The original uncompressed archive is at most 8 GiB, JSON members at most 1 MiB,
and an image has 1–1024 layers. Existing mature tar raw-extension preflight,
100,000-member/200,000-header, 8 KiB individual extension/name and 16 MiB total
name limits apply. The generator additionally refuses file-name trailing slash
aliases and file/parent conflicts, and requires at least two terminal zero
blocks followed only by aligned zero padding. It streams hashes with 64 KiB
buffers; layer payloads cannot enter metadata allocations. The retained snapshot
checks reads/seeks and namespace bindings, then attempts every retained close
even if verification already failed. Failed constructor/probe admissions also
attempt closes before returning. This is optimistic change detection, not a
filesystem lock against later same-user writes.

`verify_generator_archive_with_checkpoint` adds synchronous checkpoints at
hash chunks, headers and graph steps. A false callback returns
`archive_budget_exhausted`; the caller retains any already observed timeout or
signal status as its primary outcome. Final checkpoints occur after explicit
snapshot closure. These admissions do not guarantee a wall-clock bound on OS
I/O, scheduling or close operations. Synthetic tests include inert bytes labelled
as compressed blobs deliberately: the verified fact is the stored-byte graph,
not valid compression, inner-layer decoding, Docker loader acceptance, image
execution, build provenance, signing readiness or ABI/runtime compatibility.

## Flat retained Ops verification

The controller-local transaction is:

```sh
cargo run --locked -q -p xtask -- ops-artifact verify-retained \
  --request /absolute/request.json --request-sha256 ORIGINAL_REQUEST_SHA256
```

The original request is at most 16 KiB and has exactly `schemaVersion: 1`,
`selection`, `directory`, `output` and `authentication: "gh-token"`. `selection`
uses the existing seven Ops identity fields. The repository is fixed to
`swiftraccoon/simplestChat`; run and attempt are positive integers at most
9007199254740991. All received paths are absolute UTF-8, contain no control or
dot components and have at most sixteen ordinary components. Source and output
namespaces must be disjoint. Original source inode identity is also compared
against output-parent ancestors before creation, including filesystem case
aliases. Output is a new leaf; existing paths refuse.
An explicitly captured, nonempty `GH_TOKEN` is mandatory before output or tool
admission. The token never belongs in the request, artifacts or public output.

The retained input is a private current-user 0700 directory with exactly the
unsigned assembly's `simplestchat-ops`, four metadata files (`ops-release.json`,
`ops-evidence.json`, `ops-predicate.json`, `ops-preparation.json`), zero to sixteen
generated evidence files and one additional `ops-attestation.jsonl`. Every file
is regular, current-user, single-link and exactly 0600. The binary is at most
256 MiB, each evidence file at most 1 MiB, each metadata record at most 64 KiB
and the bundle at most 8 MiB. The complete flat input has at most 22 members and
281 MiB. Links, directories, extra files, conflicting names and empty required
files refuse. All seven selected claims and the small-record hash graph must
match before binary admission or output creation.

One selected caller-trusted `gh` executable supporting version 2.102.0 performs
four fixed calls: version, repository API, exact run-attempt API, then attestation
verification of the original copied preparation file and retained bundle. The
compiled repository and owner IDs are 1155881649 and 47705353. Fresh API results
must describe that repository, owner, revision, run and attempt, successful
completed `CI`, `push`, `main`, and matching non-fork repository/head-repository
identities. Workflow paths admit only `.github/workflows/ci.yml` and its exact
`@main` or `@refs/heads/main` suffix. This is a fresh invocation observation, not
a guarantee about HTTP cache freshness.

The signed certificate must independently carry the fixed numeric IDs as
strings, the exact CI/main SAN, GitHub OIDC issuer extension, repository/owner
URIs, selected source and signer digests, source ref, run invocation, push trigger
and `github-hosted` runner environment. The certificate issuer DN cannot replace
the OIDC extension. The returned verified statement must have exactly three
unique fixed subjects: preparation, manifest and binary. Their SHA-256 values
must match the original retained bytes; its predicate must match the retained
predicate graph. Only timestamp-authority or transparency-log timestamp entries
are accepted; current-time entries refuse. Unverified top-level
attestation JSON never supplies authority.

One preparation verification covers the complete returned signed statement;
the controller separately checks every required subject and payload. This
follows the pinned [gh digest policy](https://github.com/cli/cli/blob/v2.102.0/pkg/cmd/attestation/verification/policy.go)
and [Sigstore whole-envelope and digest-membership verification](https://github.com/sigstore/sigstore-go/blob/v1.3.0/pkg/verify/signature.go).
The returned statement is parsed and reserialized, not byte-identical signed
JSON. The pure Ops policy API interprets supplied successful-verifier output;
calling that API with fabricated JSON establishes no cryptographic execution.

Each command uses fresh private HOME, GH_CONFIG_DIR, XDG and temporary
directories with an explicit environment allowlist. Ambient configuration,
debugging, proxies, CA overrides and alternative tokens are excluded.
`GH_TELEMETRY=disabled` prevents the pinned gh runtime's detached telemetry
helper. Anonymous API mode is deliberately unsupported: empty configuration
does not pass gh's API authentication gate, and a host-only configuration can
fall back to an ambient OS keyring. The selected token avoids that fallback.
The default maintained GitHub/Public Good trust providers may access the
network; gh can continue if one provider cannot initialize. This is an online
profile, with no custom-root override, offline or revocation claim. The tool's
retained path/stamps are observed, not authenticated executable provenance.

After policy acceptance, binary and evidence are copied and independently
read back. The unchanged borrowed-manifest ELF pair verifier runs under an
explicit helper descriptor allowance. Full original and staged inventories,
file hashes and sorted member frames are rechecked before publication. Original
directory bindings and full file stamps survive owner closure; re-opening must
match those original witnesses. Directory sibling timestamps may change.
This detects observed mutations optimistically; it does not lock same-user
files or prevent later tool/runtime changes.

The 600-second entry schedule reserves fixed command bodies of 5/30/30/120
seconds, four 45-second settlement slots, one aggregate 120-second filesystem
budget, 25 seconds for bounded replay and 20 seconds for file/final fences:
530 seconds plus 70 seconds of scheduling slack. Admission checks retain the
whole remaining tail and each actual hash pass consumes the same aggregate
budget. The filesystem worker has sticky abort checkpoints and is always
joined, including after cancellation. The existing ELF inspector has admission
and final fences, with no prompt intra-helper interruption claim. OS I/O,
scheduling, close and reap stalls are not hard bounded.

The shared 128-token descriptor budget covers Rust owners, probes/cursors and
reserved helper allowances, excluding inherited/runtime registrations and gh
internals. Native dispatch reserves sixteen tokens only when existing owners
use at most 48; the unchanged ELF helper reserves 64 with at most 24 retained.
Directory cursor closure is RAII and has no observable syscall-close result.
Every retained File owner attempts explicit close. Both command capture seals
come from the original final writer metadata/hash/count after all I/O finalizers;
same-length edits before first read refuse. Existing reserved-group and pipe
retirement is required, with no claim about escaped sessions or all host work.

The fresh output retains copied input, private paired command captures and the
private work/cache tree. Opaque runtime cache names are not recursively deleted
or treated as a complete internal write inventory. Public output contains only
fixed identity/hash/count observations. `verification-observation.json` records
prepublication observations: its own write/close, replay or a final signal or
deadline can still make the command fail. Only final CLI zero includes those
fences. Fixed failures reveal no private paths, logical evidence names, JSON or
token values; observed process/control statuses remain primary.

The `verify-retained` command downloads no artifact and establishes no
artifact-ID/ZIP transport relationship by itself. The hosted producer and
acquisition commands below supply those separate operations. Debian 13 runtime
behavior and installation ownership still require their own evidence. The
transaction and Native verifier fixtures are inert and owned; they run no real
gh verification, GitHub request, trust-root acquisition or candidate.

## Hosted Ops production and signing

Trusted `CI` pushes to `main` build a separate
`simplestchat-ops-REVISION-x86_64-unknown-linux-musl` artifact. The server's
ten-file release artifact and installed Python runtime retain their existing
contracts. A pull request cannot request an Ops signature.

The hosted entry point is:

```sh
cargo run --locked -q -p xtask -- ops-artifact hosted --output /absolute/fresh-run
```

It requires the fixed repository, numeric repository/owner IDs, `CI` workflow,
`push` event, `refs/heads/main`, matching source/workflow revision, and a GitHub
hosted Linux x86-64 environment. The local source exporter, registry hydrator
and fixed producer borrow the same process supervisor and cancellation owner.
The environment check is an admission policy; the eventual GitHub certificate
provides signer identity authority.

[ops-build-inputs.lock.json](ops-build-inputs.lock.json) pins the exact HTTPS
origins, SHA-256 values and sizes of four Rust 1.99.0 archives and 38 Ubuntu
24.04 packages. The registry archives are selected from the original exported
Cargo lockfile. Downloads, extraction, registry copying and complete-tree
readback precede the existing offline producer. Rust installer-v3 declarations
are checked against the selected entry kinds. File-only
manifests allow archive directories that are proper ancestors of declared
entries; this does not admit undeclared files, links or unrelated empty
directories. Explicit directory declarations authorize their bounded subtrees.
Missing declarations and file/directory mismatches are refused.
One exact packaging exception retains the empty
`lib/rustlib/x86_64-unknown-linux-gnu/lib/self-contained` directory from the pinned
GNU `rust-std` 1.99.0 component, whose file-only manifest omits it. The exception
requires that component/root and a zero-byte directory header; it admits no
children, sibling directories, files or links without their own declarations.
The measured host compiler kit contains its package sysroot, fixed GCC/binutils
roles and an execution adapter
that replaces itself with that GCC. Resolver probes check the compiler's owned
program, startup-object and search-directory selections before the build.
The hosted operating system, `/bin/sh` and dynamic loader remain trusted runtime
inputs. Offline Cargo configuration is not a compiler filesystem sandbox or
complete network isolation.

The internal hosted owner has a 3,600-second scheduling profile, beginning after
`cargo run` bootstrap and compilation. The job's external 65-minute watchdog
also includes that setup. Source export, downloads,
registry hydration, kit preparation and production have bounded aggregate
phases. Each subprocess body is clipped to the remaining phase budget with its
settlement allowance retained. Individual per-command maxima are not additive
completion guarantees: a slow cold download or build can time out. Filesystem,
scheduling and reap stalls have no hard wall-clock bound.

Kit preparation has a 540-second slot. All phase slots plus the 300-second final
reserve total 3,540 seconds, leaving 60 seconds for initial setup and gaps within
the unchanged 3,600-second entry. Embedded filesystem work retains its
120-second reserve, so copying and subsequent measurements share the active
cutoff 420 seconds after kit preparation begins. A local Darwin arm64 diagnostic
copied a measured 505,134,204-byte tree with 26,814 files in 134.362 seconds,
including all five payload-hash passes, per-file synchronization and checked
closes. This was a local filesystem measurement with an inert process adapter;
it does not establish completed hosted Linux or cold-build timing.

After all required correctness and security jobs pass, the separate signer job
downloads the same-run unsigned assembly and independently checks the selected
identity, original payload graph, producer outcome and pinned material catalog:

```sh
cargo run --locked -q -p xtask -- ops-artifact signing-inputs \
  --directory /absolute/unsigned-artifact --output /absolute/fresh-signer-inputs
```

The existing GitHub attestation action signs exactly three subjects:
`ops-preparation.json`, `ops-release.json` and `simplestchat-ops`, using the
`ops-artifact/v1` predicate URI documented above. The published flat artifact
also contains its metadata, indexed evidence and `ops-attestation.jsonl` bundle.
Preparing signer inputs performs no cryptographic verification. The controller
must still verify the completed successful run and retained signed statement;
the workflow cannot establish its own terminal result while it is running.

## Acquire one signed Ops artifact

Build the default `xtask` before supplying credentials, then invoke its binary
with an explicit `GH_TOKEN` and one pinned request:

```sh
target/debug/xtask ops-artifact acquire \
  --request /absolute/acquire-request.json --request-sha256 ORIGINAL_REQUEST_SHA256
```

The closed request contains exactly `schemaVersion: 1`, `selection` with the
existing seven identity fields, `artifactId`, a fresh absolute `output`, and
`authentication: "gh-token"`. Artifact ID is a positive integer at most
9007199254740991. The original UTF-8 request is at most 16 KiB. Received request
paths admit at most sixteen ordinary components; output admits fifteen so that
derived verifier paths remain inside the existing sixteen-component limit.
No URL or credential belongs in this JSON. Missing `GH_TOKEN` refuses before
output/tool admission; alternative tokens and keyring fallback are unsupported.

The selected caller-trusted gh 2.102.0 executes five fixed calls: version,
repository API, exact run-attempt API, exact artifact API, then the artifact ZIP
endpoint. Repository/run/attempt/revision checks match retained verification.
Artifact metadata must additionally match its numeric ID, repository, run,
revision, exact target-suffixed name, nonexpired status, SHA-256 and byte count.
The API digest and size bind the downloaded ZIP before extraction.

The ZIP is at most 300 MiB, with at most 22 flat members and 281 MiB expanded.
Only the fixed metadata, binary, bundle and contiguous indexed evidence names
are admitted. Stored and deflate entries are supported, including canonical
ZIP64 framing. Duplicate or conflicting names, links, encryption, directories,
overlaps, preambles, trailing data and local/central disagreements refuse.
Admission checks the complete structure before creating payload sinks; decoding
checks final stream termination, exact sizes and CRCs. The extracted graph must
match the existing Ops metadata before the retained verifier is invoked.

Acquisition has a 1,440-second entry profile retaining five command settlement
slots, an aggregate archive/hash budget, the existing 600-second verifier,
bounded replay and final file checks. Filesystem workers are always joined.
Original request, outer output and verified file witnesses remain bound through
final replay; reopening cannot adopt identical bytes at a replacement inode.
An observation file records progress. Only final CLI zero includes owner
settlement, replay and the final signal/deadline/file fences.

Private paired captures and work directories remain under the new output leaf.
Fixed errors reveal no tokens, private paths or payload JSON. The gh runtime
handles GitHub's archive redirect; its normal redirect and trust-provider policy
remains part of the selected client trust boundary. This operation authenticates
retained bytes. It does not execute a candidate or install or replace a host tool.

## Authenticated Debian runtime and first installation

The manual [Ops runtime workflow](../.github/workflows/ops-runtime.yml) checks one
signed Ops artifact from a successful main CI attempt for the workflow's exact
checkout revision. Supply that CI run ID, attempt and signed artifact ID. The
workflow uses a free Ubuntu 24.04 runner, the pinned caller `gh` bootstrap and a
separate Debian 13 KVM guest. Its checkout is never selected by an input.
Controller compilation and dependency installation precede the credential-bearing
step. This workflow is separate from the full application deployment/restore VM.

The local entry point uses the closed acquisition request schema above:

```sh
cargo build --locked -p xtask --bin xtask
target/debug/xtask ops-artifact runtime \
  --request /absolute/runtime-request.json --request-sha256 ORIGINAL_REQUEST_SHA256
```

Provide `GH_TOKEN` only to the compiled controller invocation. This operation
also requires Linux x86-64, the owned-guest prerequisites, the current package
version, protocol 1 and target `x86_64-unknown-linux-musl`. Its output directory
must be fresh and absolute, with at most thirteen ordinary path components;
the received request path permits sixteen. The request chooses an artifact identity and output;
it cannot choose a host, command, compiler, guest image or executable path.

One original cancellation owner and monotonic deadline cover fresh API identity,
artifact download, certificate verification, transfer, guest work, destruction
and final original-witness checks. Acquisition's retained manifest, binary and
namespace witnesses remain live through final replay. A saved acquisition JSON
or keeper receipt cannot reconstruct that authority.

The controller constructs a bounded uncompressed USTAR with exactly three
0600 members: original `ops-release.json`, controller-selected `ops-selection.json`
and authenticated `simplestchat-ops`. Whole-transfer and individual SHA/count
pins, the original installer-request bytes and the controller's typed executable
identity cross the keeper boundary. The selected first-party keeper dependency
closure is measured against Git HEAD and retained original files. The controller
copies exactly those 28 files into a fresh private source tree and runs the
keeper from that tree. Exact directory inventories, immutable metadata and file
hashes are checked through dispatch, settlement and final replay. Unselected
checkout siblings cannot supply Python imports; additions or substitutions in
the execution tree refuse. Isolated Python also uses a fresh bytecode namespace.
The source tree and any partial construction remain private output evidence.
No candidate runs on the controller.

The guest checks the complete 19-field executable identity, local pair policy,
ordinary/base/fetch/force preflight against the unchanged Python oracle, typed
negative cases, first installation and exact repeat. Its 33 ordered cases use
fixed commands: five VPS execution-boundary probes precede the existing 28
installation and CLI cases. The new probes use a separate closed guest source
directory containing the measured probe, original guest and bootstrap scripts.
They check actual same-descriptor ELF execution, kernel CLOEXEC retirement by
candidate inode, exchanged/unlinked/wrong running-image refusals and the original
remote monotonic deadline. Their expected negative statuses are not success
receipts; each probe separately asserts its kernel/custody observations before
the parent admits that row. They run no measured capacity workload.
Writers close before the original executable inode receives its
checked executable mode; commands execute through the retained read-only file
descriptor. Per-command completion observes the leader and pipes. Final owned
VM destruction supplies guest process closure, with authenticated SSH, QMP,
cloud-init and drainer observations checked separately.

The owner allows 4,200 seconds after entry, reserving 3,900 seconds for the complete
phase sequence and 300 seconds of slack. The keeper slot includes 30 seconds of
setup, a 1,800-second body, 120-second TERM grace and 20-second Native settlement
allowance. The workflow's 90-minute job also covers cold setup/compilation.
These are admission/deadline policies, not measured cold-start promises or hard
bounds on OS I/O, scheduling or reap. Only final CLI zero includes original
readback, all explicit closes, replay and cancellation/deadline fences. The
workflow uploads the runtime summary only after success; a partial local
`summary.json` is not independent success or installation authority.

A separate prebuilt Rust diagnostic operation runs under `always()` after the
runtime step. It starts no process or runtime; the workflow does not forward
`GH_TOKEN` to this operation. It reads only the bounded, no-follow
`ops-runtime/keeper/summary.json`,
checks the selected revision/run/attempt/artifact and fixed package/protocol/target,
and writes a fresh private `ops-runtime-diagnostics/diagnostic.json`. Only a
successful readback, namespace replay and explicit owner closes publish its
`retained` marker. The workflow uploads that exact JSON for one day, including
when the original runtime fails; it never uploads raw reports, logs, caches,
images, sockets or keys. Without a compiled controller it does not compile a
replacement during finalization. Missing, malformed or foreign reports remain
explicitly unavailable. The diagnostic records the last observed fixed case and
reported exit-match facts; it does not identify an unobserved first failure or
authorize installation. The original runtime status and all 33 cases stay intact.

The keeper's global-help check accepts the exact manifest route in the older
manifest-first layout and the current indented layout. It keeps the fixed program
prefix, one Usage line, other required commands, empty stderr, 8 KiB limit and
successful command status. Extra supported routes do not invalidate that receipt.

The affected API decisions are prospective. **C-SERDE/C-VALIDATE apply:** fixed
projected data and typed source identities never deserialize runtime authority.
**C-CALLER-CONTROL/C-RW-VALUE apply:** original file owners remain with the
operation; stdout is borrowed and checked at the kernel writer boundary, including
read-only descriptor failures. **C-DTOR-FAIL/C-GOOD-ERR apply:** independent closes
are attempted, first failures remain primary, and public diagnostics contain only
fixed classes and allowed case/status fields. This reuses the existing namespace
adapter and JSON parser, adds no dependency or permission, and makes no wider API
inventory or runtime-readiness claim.

The corrected source passed 24 guest fixtures, nine focused diagnostic tests,
30 runtime/controller tests, the eight workflow/request tests, native/GNU lint,
formatting, Python quality, 575 helper tests and fast security. The full runtime
set preceded only an import move into its test module; the nine diagnostics and
all production lint checks were repeated afterward. Independent review added
regressions for the guest's clamped negative statuses and real stdout write errors.
The original signed `6a3acc8` runtime trial failed without a retained per-case
report. Its stale help expectation is independently proven; its earliest failing
case remains unproven until a new signed runtime run supplies the required evidence.

The binary's separate host entry is:

```sh
simplestchat-ops distribution install \
  --request /absolute/install-request.json --request-sha256 ORIGINAL_REQUEST_SHA256
```

Call it only after the caller authenticates the artifact. Its closed original
request contains `schemaVersion: 1` and `manifest`, `selection`, `binary` objects,
each with `path`, `sha256` and `bytes`. It checks those original bytes and the
pair policy, root/Linux x86-64/Debian 13, fixed configuration and the existing
workload flock/journals. It publishes only
`/usr/local/libexec/simplestchat-public/simplestchat-ops`, using a no-replace rename.
Repeating the completed installation produces `changed: false` without filesystem
changes. Foreign/different bytes, modes, links, unfinished journals
or substituted namespace entries refuse. Errors after a known publication retain
the visible executable for exact-identity retry; cleanup removes only owned
temporary state. This command performs no certificate verification itself.

The synchronous installer has a 120-second owner with 15 seconds reserved for
finalization. Linux signalfd owns INT/TERM on its calling thread; other application
threads must not leave those process signals unblocked. Observed deadline/INT/TERM
retain statuses 124/130/143 after all-peer finalization. Blocked OS calls, panic,
SIGKILL and process-crash recovery have no cleanup guarantee. This first-install
operation adds no upgrade, uninstall or production caller migration.

## API review for Ops delivery

This affected-component review follows the pinned
[Rust API inventory](../docs/rust-api-checklist.md); unrelated rows remain pending.
`C-VALIDATE`, `C-SERDE`, `C-NEWTYPE` and `C-CUSTOM-TYPE` apply to closed request,
material, artifact and verified-witness boundaries. Decoded requests and opaque
invocation/witness types carry their distinct roles; external IDs, digests,
sizes, paths and JSON shapes are checked before use. Serialization is deliberate
for requests and observations; process owners and credential-bearing contexts
are not deserializable authority.

`C-DTOR-FAIL` and `C-DTOR-BLOCK` apply to owned files, subprocess I/O and workers.
Checked finalizers attempt acquired peers and retain the primary failure;
fallback destructors do not panic. Blocking filesystem workers have abort
checkpoints and are joined before the operation returns. OS close/reap calls
remain outside a hard time bound. Linux source inventories use a borrowed
directory cursor whose enclosing file has a checked close. The non-Linux inert
fixtures use rustix 1.1.5's portable `Dir`, which exposes only destructor closure
for its internal descriptor. This narrow safe-API limitation supplies no checked
cursor-close evidence on those unsupported runtime targets; revisit it if the
dependency adds a consuming close API or runtime target support changes.

`C-DEBUG` and `C-DEBUG-NONEMPTY` apply to the
cross-module invocation, request and retained-observation types, whose fixed
nonempty debug output omits credentials and private paths. Filesystem witnesses
stay internal and have no debug representation. `C-FAILURE`, `C-EXAMPLE` and `C-LINK` are
covered by the command contracts and examples above. These are internal CLI
adapters, with no new public library compatibility promise or package metadata.

## Private VPS capacity operation

The [capacity guide](ansible/CAPACITY.md#rust-controller-and-authenticated-host-operation)
documents `xtask vps-capacity`, its mandatory CI artifact selectors, the fixed
Debian host and the retained Python reference. The public Ops entry points serve
one framed request or one canonical systemd worker; decoded protocol records
alone grant no execution authority. No module import starts a workload.

The load-test library keeps its default `full` feature for the existing
executable. Ops selects only `capacity-runtime` with default features disabled,
sharing workload and cleanup policy without adding the server, media or native
TLS graph. Linux implements the host body; other targets retain protocol and
inspection support and return the fixed unsupported-platform failure for host
entry points. A cross-target type check does not establish kernel behavior.

This affected-component review applies `C-FEATURE` to that explicit feature
boundary; `C-NEWTYPE`, `C-CUSTOM-TYPE` and `C-STRUCT-PRIVATE` to opaque authenticated
acquisition, invocation and filesystem owners; and `C-VALIDATE`/`C-SERDE` to closed
control messages and original private numeric evidence. Mutable wire DTOs remain
data. Invocation admission retains the actual authenticated acquisition and
original transfer witnesses rather than reconstructing authority from JSON.

`C-RW-VALUE`, `C-DTOR-FAIL` and `C-DTOR-BLOCK` apply to borrowed original input,
bounded captures, joined preparation groups and all-peer file finalization.
Readiness is bound to both independently observed progress and the final stdout
seal. Linux signal ownership stays on its calling thread, and safe POSIX spawn
restores the child's original signal mask. Observed deadlines and first control
status survive later cleanup failures; blocking OS calls still have the
limitations described above. `C-GOOD-ERR`, `C-DEBUG` and `C-FAILURE` keep fixed
codes and redacted owners separate from private child evidence.

The prospective review covers these changed contracts. Inert fixtures, actual
local processes, Linux type checks, signed artifact interoperability and measured
workloads provide different evidence; none substitutes for an unrun category.
Unrelated guideline rows and the planned retrospective review remain pending.


## Offline server attestation preparation

`simplestchat-ops release attestation prepare` consumes the existing unsigned
server release and successful scanner evidence, creates four metadata copies,
and writes the unsigned `release-predicate.json`. It performs no signature
verification, signing, Git, Docker, network, installation or deployment action.
The unchanged `build/release_attestation.py prepare` remains the reference.

```sh
target/release/simplestchat-ops release attestation prepare \
  --artifact-dir /controlled/unsigned-release \
  --security-dir /controlled/production-security/image \
  --repository owner/repository --revision EXACT_LOWERCASE_40_HEX_COMMIT \
  --run-id EXACT_CI_RUN --run-attempt EXACT_ATTEMPT \
  --timeout-seconds 300 --finalization-seconds 30
```

Replace the selector placeholders with exact identities. Run IDs satisfy
`0 < run < 2^53`; attempts satisfy `0 < attempt < 2^31`. Every full flag occurs
once and accepts split or equals form. Numeric arguments use ASCII decimal
syntax; Python's incidental plus signs, underscores, Unicode digits and option
abbreviations remain available through the reference command. Artifact/security
paths are canonical absolute paths. This Rust profile refuses symlinks in every
component and admits regular files only, including the five hash-only inputs.
Python's broader symlink/special-file behavior is not claimed as supported here.
The two directories may overlap; fixed create-new destination names still refuse
conflicts. Unrelated directory entries are permitted.

Both budget flags are required and caller-selected: `0 < M < N`, with no default
or arbitrary maximum. The original entry instant defines the whole `N` seconds,
including parsing, runtime/signal setup, input admission and finalization. Body
work stops at `entry + N - M`; the remaining interval is reserved for every
admitted file/directory close. There is no restart or extension. Failure,
INT/TERM, deadline and late finalization all return 1. Even after the first
failure, every admitted peer receives its close attempt. Blocked kernel I/O
cannot be bounded by cooperative checkpoints. The example's 300/30 allocation is
a caller choice, not a default or cold-completion guarantee; measure actual
selected evidence inside the existing CI job budget before switching a caller.

Preparation preserves the reference order: validate the manifest/archive,
validate scanner success and raw config identity, freshly hash all eight
security evidence inputs, make four ordered copies, build the claim, freshly
bind all eight release files and their runtime relationships, then create the
predicate. Initial archive inspection retains its existing 8 GiB profile; the
late release binding admits at most 2 GiB. The release manifest keeps 1 MiB;
other bound metadata keeps 64 MiB. These checks retain their reference phases,
including failures after copies. The five hash-only inputs (`native.json`,
`elf.json`, `secret-paths.json`, `database-status.json`, and
`runtime-license-evidence.json`) may be empty or arbitrary bytes and receive no
new size or JSON limit; they stream through a fixed heap buffer under the same
deadline. Source evidence and SBOM receive byte bindings without new schema
validation. VEX checks here enforce the existing statement count, leaving scanner
reachability policy with its existing owner.

Parsed metadata uses the existing byte-aware JSON decoder: UTF BOM and UTF-16/32
detection remain supported. Original encodings, whitespace and ignored integer
lexemes remain in hashes and copies. The existing Rust profile admits at most
128 nested containers and 100,000 values, refuses decoded duplicate keys and
unpaired surrogates, limits integer conversion to 4300 digits and requires finite
float conversion (including accepted underflow). Booleans, integers, floats and
null stay distinct. These bounds also apply to ignored parsed fields; they are
an explicit difference from unrestricted Python inputs. The existing signed
archive metadata profile remains in force. No complete Python-domain parity is
claimed.

Required runtime reviews must have a real ASCII `YYYY-MM-DD` expiry strictly
after the current UTC date. This check occurs late, after the earlier runtime
identity and VEX checks, in the same order as the reference. Optional runtime
proofs do not require a review. Run this complete preparation only after all
required gates in a contents-read-only CI job. The privileged signer consumes
prepared data; no newly compiled or unsigned helper executes with OIDC signing
permissions. Current verification and receiver boundaries still validate
relationships and expiry afresh before use.

| Source | Destination |
| --- | --- |
| `spdx/sbom.spdx.json` | `sbom.spdx.json` |
| scanner `outcome.json` | `image-security.json` |
| `runtime-proof.json` | `runtime-proof.json` |
| `vex.openvex.json` | `vex.openvex.json` |

The original `image.tar`, `release.json`, build `outcome.json` and `source.json`
remain unchanged. The four copies plus predicate are the five preparation
outputs; a later signer adds exactly one `release-attestation.jsonl`, preserving
the final ten-file server contract. Its four subjects remain image, SBOM,
runtime proof and VEX. The separate Ops three-subject graph is unchanged.

All five outputs use exclusive creation and private mode 0600. A conflict or
copy/write error retains earlier copies and any partial current destination;
retries require a fresh artifact directory with the original four files.
Preparation never overwrites or rolls back an existing entry. Exit 0 has no
stdout and includes checked finalization. Operational failure returns 1 with
fixed JSON `{"passed":false,"failureClass":"release_attestation_failed"}`;
invalid arguments return 2 with fixed usage. Neither path prints input values.
Help needs no runtime, environment, input files or writable directory.

Directory chains and original file descriptors remain owned through preparation.
Visible names and inode/mode/owner/size/timestamp witnesses are checked around
streamed I/O, copied bytes are read back, and the first observed failure remains
primary through all-peer settlement. This detects observed replacement and
mutation; it is not a filesystem lock, signature or authentication mechanism.
Same-UID writable-filesystem/check-read races remain outside that claim.


## Cached server attestation verification

`simplestchat-ops release attestation verify` verifies an already downloaded
candidate with four fresh maintained `gh attestation verify` invocations. It
performs no API selection, download, extraction, signing, SSH, engine or host
operation. The original Python `verify_directory` and installed callers remain
available; caller migration requires the separate recorded authentication and
before-SSH gates.

```sh
target/release/simplestchat-ops release attestation verify \
  --directory /controlled/cached-release \
  --selection-json "$FRESH_SELECTED_METADATA_JSON" \
  --timeout-seconds "$VERIFY_SECONDS" \
  --finalization-seconds "$VERIFY_FINALIZATION_SECONDS"
```

The authenticated caller supplies the exact nine fields `schemaVersion`,
`repository`, `artifactId`, `buildRunId`, `ciRunId`, `runAttempt`, `revision`,
`artifactZipBytes` and `zipSha256`. `--selection-json` is UTF-8 JSON at most
16 KiB, with no `attestation` field or previous proof. The JSON itself does not
prove a fresh API observation. Keep API selection and the trusted controller
channel as their separate authorities. IDs and attempts satisfy
`0 < value < 2^53`; build/CI run IDs agree. Attempts at or above `2^31` are
supported here; preparation's narrower producer selector does not apply.
The ZIP identity is a positive size at most 2 GiB and a lowercase SHA-256.
Repository spelling remains exact in certificate URLs.

Every full flag occurs once; split and equals forms are supported. Numeric
arguments use ASCII decimal syntax. Both budgets are required, with `0 < M < N`
and no default, extension or guessed maximum. The original entry instant covers
parsing, admission, ZIP work, all verifier processes, revalidation, file/runtime
settlement and stdout. Body work ends at `entry + N - M`. Each verifier retains
its reference 180-second body cap, clipped to the remaining original body budget
while preserving five seconds of TERM grace and five seconds of reap time.
No new peer is admitted without that room. The caller's reserve remains for
final file/runtime/output settlement. Four maximum command times need not fit
within a chosen whole budget; measure real inputs and authentication before
selecting caller values. Existing 660-second Ansible and 1080-second controller
guards also cover their other phases and are not increased by this command.

The directory is a canonical absolute path, with no symlink components. Its
`artifact.zip` and the ten fixed selected files are original nofollow regular
files retained throughout verification. Unrelated directory entries are allowed;
there is no directory membership seal or new arbitrary depth/FD limit. This is
an explicit native path profile rather than Python's wrapper path resolution.
Candidate files are never replaced, republished or rolled back.

Verification preserves the reference order: compare the ZIP to its selected
size/digest and the ten retained members once; validate the original claim and
its eight file/runtime relationships; admit the original bundle and verifier;
run each of the four subjects in order; form a new passive proof; then check the
full build/source/manifest/archive and received-file relationships. Outcome
schema/error, the exact five source inputs and the full manifest are late checks.
Required runtime expiry samples current UTC separately in the initial and final
binding phases. Lexical expiry checks precede each UTC sample. There is no added
second decompression pass, cached-proof shortcut or source/SBOM schema expansion.
Original namespace/content witnesses remain checked at handoffs and completion.

The ZIP adapter uses the locked rawzip 0.5.2 parser, existing flate2 and streamed
CRC/SHA checks. It preflights at most 65557 terminal bytes before library metadata
allocation: one-disk EOCD, bounded counts, at most 1 MiB of central metadata and
an exact central-directory endpoint. Fixed heap buffers hold 256 KiB of metadata
and 64 KiB per streamed payload. Each declared and actual entry count is bounded
by 256; they need not equal each other. Declared uncompressed totals stay within
2 GiB + 128 MiB, with image at most 2 GiB and each other entry at most 64 MiB.
All ten selected regular members must be nonempty; safe extra entries remain
allowed and their payloads are not extracted or unnecessarily decompressed.

Central metadata remains authoritative as in Python. Small member ZIP64 extras,
local metadata differences, data descriptors, safe extra directory payloads and
file/prefix-child names are not replaced with the acquisition adapter's stricter
layout policy. Changed Unicode Path names are refused; the reference's ignored
or equivalent extra fields retain their behavior. Selected local signature,
filename and overlap checks remain. Unsupported patch/strong-encryption bits
are checked on selected central entries, while differing local flags retain
Python's behavior. Equal local offsets retain Python's reversed central order
and accepted warning-only overlap case. Logical ZIP64 extents use checked u128
arithmetic; streamed reads end at the original physical ZIP boundary and stay
within the unchanged ZIP/output bounds. These guards remain alongside decoded
count/CRC and exact retained-file hashes. Reads stop at the declared uncompressed length as the
reference does, without a new trailing-compressed-byte policy. Offsets and sizes
use checked native arithmetic. Parsed JSON uses the existing byte-aware
128-container/100,000-value, Unicode-scalar, 4300-digit integer and finite-float
profile; original encodings and successful verifier stdout hashes are preserved.
These native resource/encoding boundaries are not a full Python-domain claim.

Only the inherited `PATH`, `HOME`, `USER`, `LOGNAME`, `GH_TOKEN`, `GITHUB_TOKEN`
and `XDG_CONFIG_HOME` reach the verifier, with `LC_ALL=C`. Use the controller's
trusted maintained `gh` installation and configuration. Proxy/tracing/SSH
variables are omitted, credentials never enter arguments or public output, and
no candidate value selects a command or flag. The tool and original file
witnesses are checked around every call. GH consumes paths; these checks do not
prove which inode it opened or lock a filesystem against the same UID.

Each process has empty, explicitly closed stdin and independent 8 MiB stdout /
64 KiB stderr bounds. Both streams drain simultaneously. Bounded nonempty stderr
is permitted. Natural EOF, exact group retirement, consuming reap and checked
pipe closes are required; a leader exit alone is insufficient. Child exits
124/130/143 remain subordinate failures, distinct from the original owner's
signal/deadline. Every admitted peer receives settlement after a refusal, while
the first observed cause remains primary. Blocked kernel I/O/reap is outside a
hard cooperative wall bound; late completion remains negative.

Exit 0 returns one bounded newline-terminated final ten-field passive envelope
only after all four cryptographic results, exact certificate/claim/subject checks
and local finalization. Its four verification hashes cover complete original
stdout bytes. A previous receipt, zero child status or parsed claim alone cannot
establish this result. Callers must wait for exit 0 and complete bounded stdout;
any nonzero result invalidates all output, including a complete passive envelope.
For a failure known before output, the command attempts to write fixed negative
JSON. Errors during output or final signal/deadline checks can leave a partial
line or an already emitted complete envelope. The final exit status remains
authoritative. Operational failures return 1, and invalid usage returns 2 with
fixed usage. Failure text never includes private values. The dedicated synchronous
CLI must run outside an entered Tokio runtime; help and notices need no runtime,
environment or files.

`release attestation verify --licenses` prints the exact authenticated 1022-byte
rawzip MIT text through a checked writer; `release build --licenses` retains the
existing runtime notices. This is an affected-dependency notice set, not a full
workspace audit. The new direct edge uses rawzip's default `std`/`alloc` features
and no normal dependency, build script or native/TLS graph; root must record the
actual target/default dependency inventory. Embedding the notice adds no signed
payload file, server subject or Ops subject. The server ten-file/four-subject and
Ops three-subject contracts remain unchanged.


## Fresh local release verification

`simplestchat-ops release verify` implements the complete local operation from
`build/verify-release.py`: select an explicitly pinned release through fresh
GitHub API observations, download and extract it or reverify a supplied cache,
then run the four maintained cryptographic verifiers. It returns the local
directory and two verified file digests before contacting a deployment host. The Python
reference and existing deployment/Ansible callers retain their operational
roles; this command introduces no installer, signing or host callsite switch.

```sh
target/release/simplestchat-ops release verify \
  --repository swiftraccoon/simplestChat \
  --revision "$REVISION" \
  --artifact-id "$ARTIFACT_ID" \
  --ci-run "$CI_RUN_ID" \
  --output-parent /controlled/release-evidence \
  --timeout-seconds "$VERIFY_SECONDS" \
  --finalization-seconds "$VERIFY_FINALIZATION_SECONDS"
```

Add `--artifact-dir /controlled/cached-release` to reverify retained bytes.
`--output-parent` remains a required argument but is unused in that branch.
Cached mode still fetches current artifact/workflow metadata and runs all four
verifiers; it requests no token or download grant and does not resolve curl.
Neither mode accepts a stored success receipt or caller-supplied API envelope
as evidence of a fresh selection.

The public selectors are the repository, lowercase 40-character revision and
positive artifact/CI identifiers below `2^53`. Every full flag occurs once;
split and equals forms and ASCII decimal numbers are supported. Fresh API checks
bind the unexpired production artifact's exact name, identifier, ZIP size/digest
and associated main-push CI run. That run must have completed successfully in
the selected repository at the selected revision and workflow. Its actual
attempt, certificate identity and all signed subjects remain bound by the
cached verifier's checks. Provider JSON is bounded and rejects duplicate keys;
extra provider fields do not become authority.
The returned directory must be representable as UTF-8. A non-UTF-8 active Unix
path is an ordinary failure with the same finalizers; Python's surrogate-path
encoding is outside this native output profile. An unused cached output parent
is still ignored.

Both budgets are explicit, with `0 < M < N`. The original entry clock covers
parsing, API/tool admission, download, extraction, signature verification,
revalidation, output and all finalization. Body work ends at `entry + N - M`.
Metadata, token and API roles have 20-second body caps; blob transfer has a
240-second cap. Each is clipped to the original remaining body window while
reserving its process settlement. The four verifier roles retain their
180-second caps and settlement rules. No nested CLI, retry or fresh deadline
extends the original budget. The existing callers' 660/1080-second guards also
cover other phases; this source-level operation does not increase them.

Fresh acquisition creates private, exclusive evidence beneath a canonical
absolute output parent. It checks available disk against the selected ZIP,
maximum extracted bytes and the reference's 256 MiB margin. Partial ZIPs and
extracted prefixes remain private evidence after failure. The original ZIP and
ten selected files are retained, streamed, hashed and checked through the same
directory/file custody used by cached verification. Fresh acquisition preserves
the reference's extraction pass followed by the cached comparison pass; cached
mode keeps its single comparison pass. These checks detect observed changes;
they do not lock a filesystem against another process with the same UID.
Download commits follow the reference's 64 KiB logical reads. Extraction uses
one bounded 1 MiB logical buffer over decoder reads of at most 64 KiB, matching
the reference's failed-write/CRC prefixes. All ten required members are admitted
before the first extraction output, and the final logical chunk is checked
before it is written.

Download transport requires a trusted curl 8.4 or newer with HTTPS support, in
addition to the maintained authenticated `gh` used for metadata, token retrieval
and the four verifiers. Curl is an external platform TLS/runtime prerequisite.
It adds no Rust TLS dependency and does not imply identical Python and curl CA
stores, authenticated executable distribution or macOS Ops bootstrap.

Curl's fixed profile disables user configuration, proxies, netrc, redirects,
retries and decompression. It uses HTTP/1.1 and preserves the validated request
target, including signed query bytes and dot segments. Tokens and signed
download targets enter only a bounded escaped stdin configuration; they never
enter argv, the curl environment, public errors or retained evidence. Only the
authenticated fixed GitHub endpoint receives the token. The approved Azure blob
request is credentialless, sends the reference's Accept/identity headers and
suppresses curl's default User-Agent. No candidate selects a program, flag,
arbitrary destination or output pathname.

The API grant operation completes after a bounded final 302 header with exactly
one validated Location, matching the reference that closes without consuming
the response body. Planned retirement of that owned curl role is distinct from
owner cancellation or transfer success. Blob transfer instead requires final
200, exact identity encoding and optional Content-Length, natural stream EOF,
normal child exit, exact size/SHA and checked output finalization. Bytes stream
into the parent's original exclusive descriptor. Cleanup-induced EOF, a parsed
header or a leader exit alone cannot establish complete download success.

The locked httparse parser handles response framing and header names. Selected
values are projected from the bounded original header bytes: only leading
whitespace is removed, while trailing whitespace and folded selected fields
retain the reference's refusal. Duplicate Location is refused; duplicate length
or encoding fields retain the reference's comma-join semantics. Only interim
100 responses are skipped. Curl's HTTP framing and platform TLS acceptance form
an explicit native transport profile, rather than a claim of equivalence over
every malformed HTTP response or Python platform trust store.

The reference uses per-socket read timeouts; curl exposes a different transport
interface. The native profile uses fixed connection caps of 20 seconds for the
API and 15 seconds for the blob, plus the original 20/240-second role windows.
It imposes no minimum throughput or pipe-progress watchdog. A quiet blob gap
longer than 15 seconds can therefore remain valid here if the complete transfer
fits its whole window, while the reference would refuse it. This is a scoped
timing difference that must be reviewed before a caller switch. A pipe timer
would measure curl buffering rather than socket activity; a different HTTP
adapter would require its own TLS, cancellation and resource-lifetime evidence.

Exit 0 returns one bounded newline-terminated JSON object with exactly
`revision`, `directory`, `image.tar` and `release.json`; the latter two values
are verified SHA-256 digests. Wait for the final exit and complete stdout.
Operational failures return 1 and invalid usage returns 2. Any nonzero exit
invalidates earlier output, including a complete object emitted before a late
signal, close or deadline refusal. Known peer finalizers are attempted while
the first failure remains primary. Blocked kernel I/O or reaping remains outside
a hard wall-time guarantee. The synchronous entry point must run outside an
entered Tokio runtime.

`release verify --licenses` includes the scoped httparse 1.10.1 MIT notice,
preserving its copyright and exact 1063 bytes, including the final newline.
The checksum-pinned crate's notice SHA-256 is
`391a5396cec6230bfabd4ef4eb2350eb895bc5efce377a2218f5702ed020d3e3`.
The existing rawzip notice remains available through
`release attestation verify --licenses`, with runtime notices through
`release build --licenses`. These are affected-dependency notices, not a full
workspace license inventory. Httparse is an already locked, dependency-free
crate; its new direct edge changes neither the signed payload filenames nor
the server/Ops subject sets.

The measured default musl normal/build dependency graph grows from 58 to 59
packages, adding only httparse. The direct getrandom edge below reuses the
already reachable version; no package version or checksum changes.

The command also retains the exact getrandom 0.4.3 MIT notice: 1130 bytes with a
final newline and SHA-256
`523a42c25d245dde9c015f882cec7f4555aad883382a6cf19b4b7d9b2cd5419b`.
Its direct dependency reuses a crate already reachable through the existing
default graph. The safe OS-random call borrows a fixed 16-byte buffer until it
returns; the original owner is checked before directory creation. The native
call can block, with the same kernel-stall qualification as file operations.

Private sharing covers the two local verification operations and the
prepared-receiver fetch controller under the same operation owner. Tests and actual
authenticated transport measurements are separate evidence in the
[testing guide](../docs/testing.md); source checks and inert oracles do not
establish installed-binary or host readiness.

## Prepared-receiver release fetch controller

`simplestchat-ops release fetch` implements the complete local transaction in
`build/release_fetch_controller.py`: fresh explicit API selection, local signed
bundle verification, then the prepared Python receiver's SSH protocol. The
Python reference, receiver and deployment/Ansible callers remain operational.
This standalone route does not install a target executable, change bootstrap,
retry an uncertain fetch or stage/deploy an image.

```sh
target/release/simplestchat-ops release fetch \
  --repository swiftraccoon/simplestChat \
  --revision "$REVISION" --artifact-id "$ARTIFACT_ID" --ci-run "$CI_RUN_ID" \
  --host "$HOST" --user "$SSH_USER" --identity /controlled/ssh-identity \
  --output-parent /controlled/release-evidence \
  --timeout-seconds "$FETCH_SECONDS" \
  --finalization-seconds "$FETCH_FINALIZATION_SECONDS"
```

Optional `--port` defaults to 22. `--verified-directory /controlled/candidate`
selects the cached local branch; it still performs fresh metadata and all four
maintained cryptographic verifiers. Omitting that flag downloads and extracts
local evidence first. An explicitly empty directory remains a supplied cached
selector and is refused by this native canonical-path profile; it never selects
fresh acquisition. Both branches create an exclusive private `release-fetch.*`
attempt before metadata. Downloaded evidence uses its fixed `verified/` child.

The original private verification result supplies the full ten-field envelope
directly under the same owner. A saved report, public four-key result or
caller-supplied success JSON cannot substitute for it. `artifact.json` is saved
exclusively before SSH begins. After sending the envelope, only the receiver's
exact two-field Ready message permits a new API token/download grant. The URL
used for a local download is never reused for the remote fetch. GitHub tokens
stay in their maintained local roles; the SSH environment contains only the
reference PATH/HOME/USER/LOGNAME/SSH_AUTH_SOCK subset and LC_ALL=C. Grants enter
SSH stdin only, and never public output, arguments or retained journals.

The fixed OpenSSH argv preserves BatchMode, strict host keys, disabled multiplex,
forwarding/proxy/local-command controls and the original receiver command.
OpenSSH still uses the operator's trusted user/system configuration and known
hosts; no `-F` override or new host-key bootstrap is introduced. A non-root user
uses the existing `sudo -n --` receiver command. The identity must be a real-UID
owned regular file with exact mode 0400 or 0600. Native active paths are canonical
absolute nofollow paths with UTF-8 spelling; this is narrower than Python's
relative cache and surrogate-path domain. Retained descriptors/hashes detect
observed identity/tool changes, but OpenSSH opens its own identity pathname.
These checks are neither an atomic descriptor handoff nor a lock against a
same-UID writer. Existing SSH configuration and agent semantics remain trusted.

N and M are required positive budgets with `0 < M < N`. One original entry clock
covers parsing, tool/filesystem admission, two metadata calls, local acquisition
or cache verification, four cryptographic verifiers, SSH, the post-Ready grant,
reporting, output and runtime/umask restoration. Body ends at `entry + N - M`.
The SSH protocol retains 30 seconds inside that body for its existing 15-second
exit, five-second natural EOF, five-second TERM and five-second reap windows.
Ready/receipt waits retain their 45/315-second caps, clipped to the original
protocol end. Every post-admission witness/hash/read body checkpoint inherits
that same protocol ceiling, including tool checks before a new grant's child
window is calculated. The ceiling is fenced before transport settlement; the
original body and whole ends remain unchanged. Local GitHub/curl/verifier phase
caps and the documented native idle/CA profile above remain in force. No subprocess CLI or fresh owner restarts
a clock. The existing callers' 660/1080-second guards are unchanged; a source
port does not choose or increase their limits.

Stdin remains open after the URL until the final receipt arrives. The cumulative
Ready/final receive counter and the final trailing buffer retain the reference's
separate 64 KiB bounds; physical output may therefore exceed one 64 KiB total.
Only natural EOF followed by permitted ASCII whitespace and the current child
status can complete transport. Forced EOF or local group retirement cannot
establish receipt delivery or a remote durable outcome. The original leader is
reserved until all exact-group retirement signals finish, then reaped once.
Known pipe/file/tool/directory finalizers are attempted after failure, while the
first cause stays primary. Blocked kernel I/O, close and reap remain outside a
hard wall-time guarantee.

The validated sixteen-field remote projection is saved before transport
completion. `remoteSettled` is adopted only after successful transport finish;
it is still a receiver claim, distinct from a current zero SSH exit and complete
positive receipt. Missing/malformed receipts, timeouts or a locally killed SSH
process leave the remote outcome unknown. Inspect retained evidence; the
controller performs no automatic retry or remote rollback. `outcome.json` is an
exclusive bounded private journal written/fsynced under the original finalization
window, including negative outcomes when time/signals permit. Its observation
precedes later close/output/runtime fences. Exit 0 and complete final stdout
are both required; any nonzero status invalidates earlier complete or partial
reports. Syntax/unknown-flag failures return 2. Parsed selector/key/path
validation returns 1 with the original fixed three-field `invalid_options`
report; invalid native N/M budgets remain usage 2. Other operational failure
returns 1 with fixed redacted classes. Native private typed causes preserve the first failure, while
later cleanup/journal uncertainty may update the legacy report projection.

`release fetch --help` and `--licenses` require no runtime/environment/files or
remote operations. Notices reuse the exact scoped local-verifier texts; no
new dependency or complete workspace license inventory is introduced. The
synchronous entry point must run outside an entered Tokio runtime. Review the
[validation evidence](../docs/testing.md) before considering any caller switch.


## Explicit native host receiver

The complete source route is:

```sh
simplestchat-ops release receive --timeout-seconds 300 --finalization-seconds 30
```

The numbers are an explicit caller choice, not defaults or a measured operational
recommendation. Both selectors are required positive ASCII seconds,
`0 < M < N <= 300`. One original N includes setup, the two physical stdin frames,
transfer, extraction, verification, publication, receipt, standard descriptor
restoration, runtime, signal completion and umask restoration. The body ends at
N−M; curl's five-second TERM plus five-second reap allowance remains inside that
body. M is reserved for local finalization. Kernel filesystem/random/close/reap
stalls remain outside a hard wall guarantee.

The root-only dedicated CLI takes one at-most-16-KiB LF envelope object, checks
the fixed private layout and workload lock, persists a unique `download.*`
attempt, then emits `{"schemaVersion":1,"status":"ready"}`. Only then does it read
one at-most-16-KiB LF URL object. It does not require stdin EOF and ignores later
frames. Positive signed 64-bit host identities are distinct from the local API's
narrower selector profile. The passive envelope does not establish fresh API,
signature, SSH actor, installation or deployment authority.

The fixed roots remain `/srv/simplestchat-public`, `/etc/simplestchat-public` and
`/run/simplestchat-bench`. This operation retains partial ZIP/member/evidence files
and manifest-first partial publication after failure. It checks both existing
pair targets before either link, accepts exact repeat and verified racing
same-inode aliases, and never replaces or deletes a foreign target. It does not
run Docker, touch a service, select a deployment or recover an unknown outcome.
A final `fetch-outcome.json` and attempt sync are attempted while the lock is
held. Lock close precedes the final sixteen-field report. Report claims and
public committed-byte progress remain separate from a lower partial-write prefix.

Curl 8.4+ with Linux/OpenSSL is an explicit platform prerequisite. The process
uses the retained fixed `/etc/ssl/certs/ca-certificates.crt`, an owned empty capath
and disabled native CA discovery. It runs HTTP/1.1 with TLS 1.2 minimum, fixed
`simplestchat-release-receive` User-Agent, no ambient config/proxy/netrc/redirect/retry or
decompression role, and carries the opaque URL only in bounded stdin config.
Version/backend/CA exclusivity still require actual free-VM evidence. Curl's
fifteen-second connect limit and the original native whole/body clock do not
recreate urllib's fifteen-second per-socket idle timer. No low-speed floor is
used. Raw first CE/CL values are checked, but malformed/conflicting HTTP framing
may be rejected differently by curl; full wire/temporal parity is unclaimed.

The shared byte-JSON profile bounds depth, value/string growth, nonfinite and
surrogate handling.
`--help` and the limited-scope `--licenses` route run before host/runtime effects.
Syntax errors return2; operational refusals return1. Exit0 includes all known
peer finalization and the original end fence. A nonzero exit invalidates earlier
Ready, partial output or a complete earlier report, including a late failure.

The existing Python receiver, prepared-helper preflight, controller command and
operational callers remain unchanged. Source compilation and inert comparison do
not authenticate a Linux musl executable on the target. The current authenticated
Ops installer supports first install and exact repeat, not an upgrade. Installing
or switching this receiver requires separately complete bootstrap and runtime
proof; this source route makes no such switch.
