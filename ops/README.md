# Operations policy tools

`simplestchat-ops` validates local release manifests and receiver receipts.
It has no
network, engine, service, SSH or deployment command. Python remains the release
runtime and the reference implementation.

```sh
cargo run --locked -q -p simplestchat-ops -- release manifest --path /path/to/release.json
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
