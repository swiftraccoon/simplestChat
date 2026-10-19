# Fedora RPM database secret findings

## Two regions after the vim 9.2.1202 refresh — 2026-10-19

When the runtime package layer was rebuilt from the Fedora 44 repositories
(`vim` 2:9.2.1129 became 2:9.2.1202-1.fc44), rpm rewrote its SQLite
`Basenames` index: two of the four reviewed regions recur byte-for-byte, two no
longer occur, and two new complete `generic-api-key` regions appeared at the
same path `001/usr/lib/sysimage/rpm/rpmdb.sqlite`, observed in
[CI run 39212373766](https://github.com/swiftraccoon/simplestChat/actions/runs/39212373766)
(revision `353c5e7755adb64e13251321cff77a098d0a6edb`, image
`sha256:eab407f8ef7ef96b03db3d8170463cce9b3d86c168f3abc08bbf9dda2ff7d2ed`,
artifact `12168437967` rehashed against GitHub's digest). The
[new evidence](secret-evidence/fedora-rpm-2026-10-19.json) covers the two new
regions; the two dormant fingerprints
(`image-match:0e1f4907…f648c`, `image-match:ae0325c1…0d999`) are removed from the
active ledger and the two recurring reviews keep their 2026-09-30 evidence.
Owner `swiftraccoon` and expiry **2026-11-29** are unchanged.

A new bounded local replay (two CPU cores, 2 GiB, 900-second deadline, the pinned
Fedora base and the canonical `dnf upgrade` plus `dnf install` transaction with
the same refresh epoch; no application built or executed) installed the same 148
runtime package identities as the canonical image. Its complete database and
projection differ from the canonical ones, as transaction timing shifts the
projection by 33 and 51 lines, but the pinned detector (gitleaks 8.30.1 on the
same ASCII projection) reports exactly four regions whose complete match bytes,
lengths and SHA-256 digests equal the canonical findings: the two recurring
reviews and the two new regions. Read-only `PRAGMA quick_check` passed.

The 58-byte region is one live `Basenames_key_idx` record for a public
`fedora-gpg-keys` 44-2 signing-key filename (26 filename bytes and two serialized
row-identifier bytes) followed by a 32-byte freeblock whose four-byte header and
surviving content are a freed copy of the same public filename; that filename is
also a live record in the authenticated immutable Fedora base database. The
detector's candidate is exactly the public filename. The 57-byte region spans two
live records of the same package (cell size, record header, two public filenames
and their row-identifier bytes), with the candidate being one filename plus one
row-identifier byte, exactly the structure of the three 57-byte regions reviewed
on 2026-09-30. Every detected byte is accounted for by index-cell structures or
freeblock metadata; no byte is an issued credential or a signing private key.

Each new ledger entry binds the exact rule, original layer path, pinned
scanner/projection format, complete match length and SHA-256. A changed region,
path, rule or format requires a fresh review; every additional finding remains
independently blocking. There is still no blanket path or package exception and
no automatic expiry renewal. A fresh canonical image gate is required before
signing or deployment, and the Fedora repositories may move again before it.

## Original review — 2026-09-30

Reviewed **2026-09-30** under the owner-authorized security rollout.
Owner: `swiftraccoon`. Review expiry: **2026-11-29**, unchanged.

The [current evidence](secret-evidence/fedora-rpm-2026-09-30-pr6.json) supports
four exact `generic-api-key` findings in
`001/usr/lib/sysimage/rpm/rpmdb.sqlite`. They contain public package filenames
and adjacent SQLite index metadata. This agent technical assessment approves
only those complete detected regions at that exact path; it neither exempts RPM
databases nor approves arbitrary package content.

## Canonical evidence and comparison

[CI run 36804977075](https://github.com/swiftraccoon/simplestChat/actions/runs/36804977075)
scanned revision `4839ebcc194e2a03b5653a720a6e9c39b64565ca`, image
`sha256:8694dee5fb069746c2e9618c2a324f54e53c2d3903158606aa3f37317faf51fe`.
Artifact `11137861810` was independently rehashed and all 18 members compared
before review; its ZIP SHA-256 is
`b4797374b470c1e22d3c7afc122c476d22f5038cd7daca75ba093dabeba140d8`.
Its four blocking secret findings are distinct from the prior reviewed spans.
The image gate failed; this review does not claim a subsequent passing build.

A new bounded local replay built only the canonical runtime package-install
instruction from the exact pinned Fedora base. It used two CPU cores, 2 GiB of
memory and a 900-second deadline; no application was built or executed. All 148
installed runtime package identities match the canonical SBOM, including the
updated `glibc` package. The separate authenticated static RPM input is outside
that installed inventory. Public signing-key filenames belong to the exact
`fedora-gpg-keys` version `44-2` package in the canonical SBOM.

The replay's entire 9,412,718-byte maintained ASCII projection equals the
canonical projection SHA-256
`c4fa9f2d789064aebe4a05c6d696b150196eceb855a9ae72b60808fc9cd465dd`.
All four complete scanner `Match` values also agree in length, SHA-256 and
coordinates: three 57-byte regions and one 65-byte region. This comparison uses
the complete detected bytes, not just the candidate token or a normalized SQL
value. The raw database hashes differ. The full canonical image archive was not
retained, so source-layout analysis is explicitly a replay observation, bound to
the canonical scanner bytes through the complete projection and span digests.

## Source assessment and limits

All four captured candidates occupy live `Basenames_key_idx` records. For three,
one printable serialized row-identifier byte directly follows a public filename.
Their complete 57-byte detected regions span two live filename/index records
apiece. The fourth candidate is exactly a public filename; its full 65-byte
region spans a live record and an adjacent 34-byte deleted-record freeblock.
The latter's four-byte freeblock header is accounted for, and its surviving
29-byte filename plus one-byte row identifier match a live record in the
independently authenticated immutable Fedora base database.

Read-only SQLite integrity checks, live row/package joins, explicit index-cell
and freeblock traversal, and canonical public filename hashes account for every
byte in all four regions. The interpretation follows SQLite's documented
[record format](https://www.sqlite.org/fileformat.html#record_format),
[index representation](https://www.sqlite.org/fileformat.html#representation_of_sql_indices)
and [B-tree freeblocks](https://www.sqlite.org/fileformat.html#b_tree_pages).
The filenames identify the public
[Fedora signing-key package](https://packages.fedoraproject.org/pkgs/fedora-repos/fedora-gpg-keys/);
they are not signing private keys or issued bearer tokens.

Each expiring ledger entry binds the exact rule, original layer path, pinned
scanner/projection format, complete match length and SHA-256. All serialized
row-identifier bytes remain included. A changed region, path, rule or format
requires a fresh review; every additional finding remains independently
blocking. There is no blanket path or package exception and no automatic expiry
renewal.

The [previous observation](secret-evidence/fedora-rpm-2026-09-30.json), from
run `36749727744`, remains historical evidence. Its four fingerprints are removed
from the active ledger and do not authorize either current or future findings.
Regression tests require each current review individually and reject superseded,
changed and additional spans.

Only hashes, coordinates and structural metadata are retained publicly. Candidate
values and raw scanner matches remain private. A fresh canonical image gate is
still required before signing or deployment.
