# Operations policy tools

`simplestchat-ops` validates local release manifests, receiver receipts and
Docker/OCI image archives. It has no network, engine, service, SSH or deployment
command. Python remains the release runtime and the reference implementation.

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
