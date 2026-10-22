# Operations policy tools

`simplestchat-ops` validates local release manifests, receiver receipts and
Docker/OCI image archives, and inspects local executable identities. It has no
network, engine, service, SSH or deployment command. Local unsigned artifact
preparation can write a new private directory, as described below. Python remains
the release runtime and the reference implementation.

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

The crate uses existing dependency versions and does not depend on the server,
load generator, mediasoup or native TLS. The manual argument parser follows
`xtask` for this small command surface and avoids adding a CLI framework dependency.

No binary is installed by Ansible or added to the production image. Host
execution requires a separate attested operations artifact, an inspected static
`x86_64-unknown-linux-musl` build, harmless CLI checks on Debian 13, and
controller-side verification before any installed binary executes. Static CRT
selection alone does not prove the final binary's dependencies; inspect the
result as the [Rust linkage reference](https://doc.rust-lang.org/reference/linkage.html#static-and-dynamic-c-runtimes)
recommends. The existing signed server artifact and its ten-file contract stay
unchanged. Python's controller-supplied preflight remains the bootstrap trust
boundary until binary provenance, ABI and installation ownership are established.

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

The `workspace-ops-v1` inventory selects original bytes of `Cargo.toml`,
`Cargo.lock`, `rust-toolchain.toml`, `.cargo/config.toml`, the Ops/load-test/xtask
manifests, the two current mediasoup path-patch manifests, and every regular
`*.rs` under `ops/src`. This covers the current named workspace discovery
manifests, not complete Cargo/compiler I/O closure. The tool does not interpret
TOML, resolve changed workspace members/path patches, or read other workspace,
vendor or registry source. A later builder must audit the complete input closure
instead of treating this fixed selection as one. Implicit `ops/build.rs`, an
`ops/.cargo` directory and legacy `.cargo/config` are refused rather than silently
omitted from this profile. Other unselected paths are not source inputs here.

At most 64 files are selected, each at most 1 MiB except `Cargo.lock` at 4 MiB,
with a 16 MiB original-byte total. Empty selected files are recorded accurately;
the inventory does not establish that a manifest parses or source compiles.
Source traversal has at most eight descendant directory levels and 16 selected
directory owners, including the source root. Selected source names are bounded
ASCII components; links and special files are refused. Enumerations have a
combined 8,192-entry/1 MiB name budget and 255-byte entry-name limit. Source-path
ancestors have at most 32 retained owners, shared across every file. The bound
is 112 retained File handles plus transient enumeration/binding handles, rather
than one ancestor chain per file.

Retained descriptor-relative directories are enumerated with rustix's mature
`Dir` reader. Files, selected directory stamps/membership and visible bindings
are rechecked across collection. Outside ancestors bind inode/device, mode and
owner/group without freezing unrelated siblings. Every retained File close is
attempted explicitly, including after validation failure; rustix's temporary
cursor is RAII-closed internally, with no fallible close API. This detects
observed mutation/replacement; it is neither an atomic snapshot nor a lock.

The canonical SHA-256 frame is the ASCII bytes
`simplestchat-ops-source-inventory-v1` plus NUL, a big-endian u32 file count, then
entries sorted by relative ASCII path. Each entry is big-endian u16 path length,
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
