# Operations policy tools

`simplestchat-ops` validates local release manifests, receiver receipts and
Docker/OCI image archives, and inspects local executable identities. It has no
network, engine, service, SSH or deployment command. Local unsigned artifact
preparation can write a new private directory, as described below. Python remains
the release runtime and the reference implementation. A separate read-only
release preflight checks the fixed host paths without executing installed helpers.

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
| `src/release/archive/generator*.rs` | Separate generator profile with streamed config/layer graph identity |
| `src/release/preflight*.rs` | Fixed read-only host admission and exact installed-helper byte checks |

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
`workspace-ops-v1` inventory. Its SHA input begins with the profile plus NUL, followed
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
