# Fedora RPM database secret findings

Reviewed **2026-09-30** under the owner-authorized security rollout.
Owner: `swiftraccoon`. Review expiry: **2026-11-29**.

The [retained evidence](secret-evidence/fedora-rpm-2026-09-30.json) supports four
exact `generic-api-key` findings in `001/usr/lib/sysimage/rpm/rpmdb.sqlite`.
They contain public package filenames and adjacent SQLite index metadata.
This review approves those complete detected regions at that exact path; it
neither exempts RPM databases nor approves arbitrary package content.

## Canonical evidence and comparison

[CI run 36749727744](https://github.com/swiftraccoon/simplestChat/actions/runs/36749727744)
scanned revision `6b710c5fdd29988f608fc82f0f811ba59d9c9f19`, image
`sha256:66113924c67f0902ad6d016fccdd3daddb1643d85eb1d5ccdcf24c8af48c0041`.
Artifact `11114179205` was independently rehashed before comparison; its ZIP
SHA-256 is `6c51cb43ee5074665e2869aa72c766379fb366d461152f26ca1208ba1788f31a`.
The scan reported four blocking secret findings and 15 separately reviewed
base-image findings. Its license and vulnerability checks passed. The image
scan itself failed; this review does not claim a subsequent passing build.

All four canonical match regions resolved uniquely. Their lengths and SHA-256
values equal the private scanner's complete `Match` bytes from the retained
local replay: one 61-byte region and three 58-byte regions. The comparison uses
the entire detected region, not only a captured token or selected substring.
Canonical line positions shifted by 29 and its complete database hash differs
from the replay. Neither matching line numbers nor identical database bytes
are assumed.

The replay built only the repository's runtime package-install instruction from
the exact pinned Fedora base, within fixed CPU, memory and time limits. It did
not build or execute the application. Its 148 installed package identities
match the canonical inventory; the additional canonical RPM is an authenticated
static build input. The relevant public paths also appear under the exact
`fedora-gpg-keys` version `44-2` package in the new canonical SBOM.

## Source assessment and limits

In the inspected replay, three candidates occupy live `Basenames_key_idx`
records: printable serialized row-identifier bytes directly follow public
filenames. The fourth occupies a deleted index freeblock; its bytes also match
a live record in the authenticated immutable Fedora base database. Read-only
row/package joins and public filename hashes bind all four to the published
[Fedora signing-key package](https://packages.fedoraproject.org/pkgs/fedora-repos/fedora-gpg-keys/).
The interpretation follows SQLite's documented
[record format](https://www.sqlite.org/fileformat.html#record_format),
[index representation](https://www.sqlite.org/fileformat.html#representation_of_sql_indices)
and [B-tree freeblocks](https://www.sqlite.org/fileformat.html#b_tree_pages).
These are package metadata, not signing private keys or issued bearer tokens.
The page-layout interpretation describes the replay; only the complete detected
region digests and package context are asserted to match the canonical image.

Each expiring ledger entry uses the current image-secret identity: exact rule,
original layer path, pinned scanner/projection format, complete match length and
SHA-256. SQLite row-identifier bytes are included without normalization.
Surrounding transaction changes retain fresh file/projection hashes as evidence;
a changed region, path, rule or format requires a new review. Every additional
finding remains independently blocking. There is no blanket path or package
exception, and review expiry is not renewed automatically.

Only hashes, coordinates and structural metadata are retained publicly. Candidate
values and raw scanner matches remain private. A fresh canonical image gate is
still required before signing or deployment.
