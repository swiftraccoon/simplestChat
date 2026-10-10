# Canonical runtime RPM license context review

## Current tzdata 2026e assessment — 2026-10-10

The canonical noarch package `tzdata` changed from `2026c-2.fc44` to
`2026e-1.fc44` with the 2026-10-10 Fedora package refresh
(`FEDORA_REFRESH_EPOCH`), observed in
[CI run 38072015598](https://github.com/swiftraccoon/simplestChat/actions/runs/38072015598).
Its **exact 2026e-1 PURL replaces the 2026c-2 scope** in the active ledger. The
25 other assessments (22 original entries and the three glibc release-9 entries
retained separately) remain unchanged. The
[new evidence](license-evidence/fedora-tzdata-2026-10-10.json) records this
separate source review; the original evidence remains byte-for-byte historical.
Owner `swiftraccoon` and expiry **2026-11-29** remain unchanged.

The package retains the complete declaration
`LicenseRef-Fedora-Public-Domain AND (GPL-2.0-only WITH ClassPath-exception-2.0)`
and fingerprint
`license:adb0ac3b20b5f933623670fd873e969dbac77eadd946649c0b7683e0c87b6f82`. Only
the Syft declaration's provenance layer digest changes. No term is removed from
the declaration and no license is added to the global allowlist.

### Exact source and notice comparison

The official read-only Koji `getBuild` response identifies successful
[build 3114496](https://koji.fedoraproject.org/koji/buildinfo?buildID=3114496),
`tzdata-2026e-1.fc44`, completed 2026-10-06, and Fedora dist-git commit
`64d594762ddecc2a65d9d5b7ee635a396f0dcf26`; Bodhi update `FEDORA-2026-7d957ce87d`
reached stable on 2026-10-08. The
[immutable spec](https://src.fedoraproject.org/rpms/tzdata/raw/64d594762ddecc2a65d9d5b7ee635a396f0dcf26/f/tzdata.spec)
differs from the reviewed 2026c-2 spec (commit
`51f4bc555c69c04300c38945d13ed00657d7f150`) only in its Version, release-version
defines and Release lines plus two changelog entries; its License line, Source
entries, notice installation and subpackages are unchanged. The commit's sources
manifest binds `tzdata2026e.tar.gz`, `tzcode2026e.tar.gz` and the unchanged
`javazic-1.8-37392f2f5d59.tar.xz` by SHA-512; all three were downloaded over
HTTPS from IANA and the Fedora lookaside and match. The IANA archive's `LICENSE`
member (252 bytes, SHA-256 `0613408568889f5739e5ae252b722a2659c02002839ad970a63dc5e9174b27cf`)
is byte-identical to the installed `/usr/share/licenses/tzdata/LICENSE`, which
matches its RPM digest and the 2026c-2 review. No downloaded code was executed.

| Package | Context and retained evidence |
| --- | --- |
| tzdata | Same declaration and fingerprint as 2026c-2; exact IANA/code/javazic source hashes and the installed notice re-checked against the new commit. The Java compiler still supplies the GPL/ClassPath source context; `tzdata-java` and Java payload paths remain absent from the runtime RPM inventory. No generated-output licensing conclusion is inferred. |

### Artifact binding and limits

The evidence is [artifact 11678000632](https://github.com/swiftraccoon/simplestChat/actions/runs/38072015598/artifacts/11678000632)
of that run (1,824,381 bytes; its SHA-256 matches GitHub's artifact digest). Its
`outcome.json` binds the runtime notice report, runtime proof and SPDX document
by hash, and the independent Syft report agrees on image ID, revision and the
ordered declaration. That retained image check **failed**: its runtime proof and
vulnerability verdict passed, while its license verdict blocked exactly this one
changed scope before the review and none after it. This assessment does not
relabel the retained run as successful or supply a signed passing-release
attestation. Ledger matching remains exact **PURL plus fingerprint**; source and
notice hashes record review evidence, not additional automatic matching fields.

## Current glibc release-9 assessment — 2026-10-01

The three canonical amd64 packages `glibc`, `glibc-common` and
`glibc-minimal-langpack` changed from `2.43-8.fc44` to `2.43-9.fc44` in
[CI run 36804977075](https://github.com/swiftraccoon/simplestChat/actions/runs/36804977075).
Their **exact release-9 PURLs replace the three release-8 scopes** in the active
ledger. The 23 other assessments below remain unchanged. The
[new evidence](license-evidence/fedora-glibc-2026-10-01.json) records this separate
source review; the original evidence remains byte-for-byte historical.
Owner `swiftraccoon` and expiry **2026-11-29** remain unchanged.

All three retain the complete, ordered declaration and fingerprint
`license:30642b306924ce375ecb58acd5513b6bf1b01d94b18a6cfbde3817bc0871424c`.
Only the original Syft declaration's provenance layer digest changes. No term is
removed from the aggregate and no license is added to the global allowlist.

### Exact source and notice comparison

The official read-only Koji `getBuild` response identifies successful
[build 3110438](https://koji.fedoraproject.org/koji/buildinfo?buildID=3110438),
`glibc-2.43-9.fc44`, and Fedora dist-git commit
`8e9e7eb6e4f312296c6fc451b8e0ca1049725391`. The
[immutable spec](https://src.fedoraproject.org/rpms/glibc/raw/8e9e7eb6e4f312296c6fc451b8e0ca1049725391/f/glibc.spec)
changes only the upstream snapshot, Fedora release and changelog relative to the
reviewed release-8 spec. Its license expression, notice installation, subpackages
and exact shared-package dependencies remain unchanged. All seven auxiliary
source files and four Fedora patch files are byte-identical between those
immutable commits.

The downloaded `glibc-2.43-71-g9cda6fc96a.tar.xz` archive matches the SHA-512 in
the [exact Fedora source manifest](https://src.fedoraproject.org/rpms/glibc/raw/8e9e7eb6e4f312296c6fc451b8e0ca1049725391/f/sources).
Its four regular notices match both the current image and historical review,
and `COPYING.LIB` still links to `COPYING.LESSERv2`. The unchanged
`manual/libc.texinfo` retains its invariant sections and cover-text terms.
`glibc-doc` is absent from this image's runtime inventory; that absence does not
remove terms from the recorded aggregate.

The [exact upstream delta](https://sourceware.org/git/?p=glibc.git;a=commitdiff_plain;h=9cda6fc96abd035d9cbe68482138d4a78a51a7d5;hp=bc95068f5f9d7f57d0f01757fed0900893b122b8)
changes 40 paths without changing the notices or manual. Four added regression
test files carry the existing FSF LGPL-2.1-or-later form; no changed license grant
on runtime source files was found. The retained evidence records source-response,
archive, member, patch and delta hashes. Sources were read over primary HTTPS;
no downloaded code was executed and no detached RPM signature or reproducible
build verification is claimed.

| Exact release-9 package | Reviewed context |
| --- | --- |
| glibc | Owns four verified regular notices plus the verified `COPYING.LIB` link. Retain the entire software/documentation declaration and manual context. |
| glibc-common | Same source and declaration. The exact Fedora spec requires matching glibc, which supplies the observed shared notice set. |
| glibc-minimal-langpack | Empty metapackage in both the spec and observed RPM file inventory. Its exact glibc/common dependencies supply the verified shared notices. |

### Artifact binding and limits

[Artifact 11137861810](https://github.com/swiftraccoon/simplestChat/actions/runs/36804977075/artifacts/11137861810)
comes from source revision `4839ebcc194e2a03b5653a720a6e9c39b64565ca`.
The downloaded 1,836,792-byte artifact ZIP has SHA-256
`b4797374b470c1e22d3c7afc122c476d22f5038cd7daca75ba093dabeba140d8`,
matching GitHub's artifact metadata. All 18 regular members were compared
byte-for-byte. Its outcome binds the notice
report and SPDX document. The independent Syft report agrees on image ID,
revision, layers, exact package PURLs and ordered declarations. The evidence
retains all corresponding hashes and the CI-reported exported-image archive hash;
the image archive itself was not separately downloaded for this review.

That retained image check **failed**: its runtime proof passed, while its license
verdict blocked exactly these three changed package scopes and its independent
secret check also failed. This assessment does not relabel the retained run as
successful or supply a signed passing-release attestation. A fresh canonical
check must pass all gates. Ledger matching remains exact **PURL plus fingerprint**;
source and notice hashes record review evidence, not additional automatic
matching fields. Changed source, notice or distribution context still requires
review, and all notice/source/relinking/attribution obligations below remain.

## Original 2026-09-30 assessment — historical observation

Reviewed **2026-09-30** by an agent under the owner-authorized security rollout
for `swiftraccoon`; review expiry is **2026-11-29**. This is an explicit technical
scanner-policy assessment of the exact canonical amd64 package records in the
[retained evidence](license-evidence/fedora-runtime-2026-09-30.json).
It accepts 25 contextual declarations and the observed x86_64 `libtool-ltdl` raw
record through individual `image-license` entries. It introduces no global
license allowance or automatic exception importer.

The canonical CI image check **failed** with `image_runtime_rpm_owner`, after
collecting notice evidence and before its runtime/VEX and license verdicts.
The independent, read-only license sub-verdict has 26 blocked records before this
review and zero afterward; its 28 reviewed findings include two existing exact
public-key and static-runtime records. This does **not** make that image check,
VEX assessment, signing job or release pass.

The evidence comes from [CI run 36738045459](https://github.com/swiftraccoon/simplestChat/actions/runs/36738045459),
[artifact 11109374252](https://github.com/swiftraccoon/simplestChat/actions/runs/36738045459/artifacts/11109374252),
for source revision `629991a568d6ef6fae363d770dce15cbfe9b6f02`.
The CI outcome binds the notice report, which binds the exact SPDX document.
The retained Syft report independently agrees on image ID, source revision,
layers and the reviewed ordered declarations. Its separate digest is retained;
the outcome does not directly bind that Syft file. The archive hash below is
reported by CI; this review did not separately download and rehash the archive.

| Observed artifact | SHA-256 |
| --- | --- |
| Image configuration / image ID | `6f3b0661ae0462644633bdc8461c24a1207e31eae68c5c9c23381bb499a53da6` |
| Exported archive, as recorded by CI | `d13a30592bad946a7cb44e1d437733bd3bf6d5f73c11256934438d227ee356e1` |
| Runtime notice report | `69443c8d8ff5713c4ece90ba6deebd2c06f96707adf356130b18381e3c1d415a` |
| SPDX document | `fcbccded6fa16d071e4b6e07aafb88ecda1cdd4fe2b12853f220630e9aeccd9e` |
| Syft document | `148135ae11154d93d82aede243cdbd2eeb78e9c71dd649b9b16c0d5e4553cd90` |

This review explains package-wide license declarations that need specific
context. It does not add licenses to a global allowlist, relabel RPM metadata,
license the application, or establish complete redistribution compliance.
Unexecuted files can retain license obligations. Preserving notices is necessary
but does not, by itself, satisfy source availability, compilation, attribution,
documentation, or other applicable conditions. Changes to distribution require
separate review.

### Evidence and identity

The evidence separates package identities, original ordered Syft declarations,
required notice sets, and primary sources. Repeated declarations and shared
notices appear once and are referenced by ID. Every declaration retains its raw
record SHA-256 and current policy fingerprint. The 25 contextual declarations are complete expressions: their active
fingerprint is `license:` plus the hash of the exact ordered, parenthesized
expression. The raw `libtool-ltdl` record instead uses the maintained
`license-raw-v2:` identity, preserving every field and array order while replacing
only a validated location layer digest with the image-layer marker. Original raw
record hashes remain separate provenance; they are not alternate approvals.

Upstream/Fedora source bytes were fetched over primary HTTPS and rehashed. Exact
Koji successful-build records bind the six specifically investigated Fedora
releases to immutable dist-git commits. The associated source manifests bind
source archives by SHA-512 where recorded. No independent detached RPM or release
signature verification is claimed. Sources cited through a mutable branch or
version tag remain bound to the reviewed byte hash; the URL alone is insufficient.
No package scripts or downloaded source code were executed during this review.

The canonical notice report accounts for regular-file bytes and image-confined
notice symlinks. All 45 unique contextual notice expectations and the additional
libtool notice matched; shared owners have exact observed PURLs. The six rootfiles
payload SHA-256 values also matched the canonical SPDX file records. Packages without notices require an identified shared owner or specific
payload context. Absence of a notice is not a permission. The evidence's
`noticeSets` record exact image paths, lengths and hashes; `sources` retain exact
URLs, source revisions and byte hashes without workstation paths or raw file text.

### Package-specific findings

| Packages | Canonical version | Context and retained evidence |
| --- | --- | --- |
| coreutils, coreutils-common | 9.10-5.fc44 | Software/documentation aggregate. The official manual has no invariant sections or cover texts. The exact spec requires matching coreutils-common, which retains the release-matching COPYING notice. |
| filesystem | 3.18-52.fc44 | Koji resolves the autorelease source identity. Metadata has 1,551 directories, 15 links and six zero-byte ghost placeholders, with no regular-file payload. The source also contains layout-maintenance Lua scriptlets. Review remains specific to this package's public-domain declaration. |
| glibc, glibc-common, glibc-minimal-langpack | 2.43-8.fc44 | Exact Fedora release-8 source and shared-notice dependencies are established. Minimal-langpack is an empty metapackage. Four regular notices match upstream; COPYING.LIB resolves to the retained LGPL notice. The manual has invariant sections and cover texts, and its glibc-doc package is absent from the observed runtime RPM inventory. |
| gmp | 1:6.3.0-5.fc44 | Four notices match the exact release. The manual has required cover texts despite no invariant sections; its owner is gmp-devel, absent from the observed runtime RPM inventory. Preserve the declared software/documentation aggregate. |
| grep | 3.12-3.fc44 | Exact release manual has no invariant or cover texts; COPYING matches. Preserve the GPL/LGPL/GFDL aggregate. |
| gzip | 1.14-2.fc44 | Retain both GPL and GFDL notices. Upstream manual wording says 1.3-or-later while RPM declares 1.3-only; preserve the observed declaration. |
| libevent | 2.1.13-1.fc44 | Exact current tag and Fedora spec identify BSD, ISC and explicit public-domain DNS portions. Installed LICENSE matches. The earlier 2.1.12 package is outside this review. |
| libfsverity | 1.6-4.fc44 | Installed LICENSE matches upstream MIT text. Fedora's automatically converted Callaway label remains unchanged; this does not permit arbitrary Callaway declarations. |
| libgcc, libgomp, libstdc++ | 16.2.1-2.fc44 | Exact GCC revision and runtime-exception context retained. Require the shared libgcc notice set and complete Fedora aggregate. Runtime exception and redistribution conditions are not discharged by this review; a shared-notice requirement does not invent a package dependency. |
| libselinux | 3.11-2.fc44 | Exact Fedora source and four patches checked. LICENSE matches upstream; recorded patches leave it unchanged and new hash source retains public-domain attribution. |
| libxcrypt | 4.5.2-3.fc44 | The exact recorded Fedora patch's two LICENSING hunks reconstruct all 6,148 installed bytes without fuzz. AUTHORS and COPYING.LIB match upstream unchanged. Added test/Autoconf attributions remain intact. |
| popt | 1.19-10.fc44 | Retained MIT notice and exact upstream public-domain hashing implementation explain the two-part declaration; neither term is removed. |
| readline | 8.3-4.fc44 | Manual has no invariant or cover texts; retain software COPYING/USAGE and the full declared aggregate. |
| rootfiles | 9.0-6.fc44 | No dedicated notice. Six exact template payload hashes establish the reviewed content context for Fedora's named noncopyrightable classification. All six hashes match canonical SPDX; ghost entries are not verified content. |
| setup | 2.15.0-28.fc44 | The installed COPYING explicitly supplies the package's public-domain statement. Exact notice bytes provide the evidence; no separate upstream tag is inferred. |
| tzdata | 2026c-2.fc44 | Exact IANA/code/javazic source hashes checked. The Java compiler supplies concrete GPL/ClassPath source context; the separate Java package and Java payload paths are absent from the observed runtime RPM inventory. No generated-output licensing conclusion is inferred. |
| util-linux-core | 2.41.5-1.fc44 | Specific tagged helpers supply public-domain context. Retain all ten notices and the complete aggregate; a copied notice alone does not establish that its corresponding component is shipped. |
| vim-data, vim-minimal | 2:9.2.1129-1.fc44 | Exact manual and pinned Fedora Vim-specific review support only Open Publication documentation without section VI options. Software LICENSE is separately retained in vim-data and required for the same-source minimal package. |
| libtool-ltdl | 2.5.4-10.fc44 | Exact raw LGPLv2+ declaration, pinned Fedora abbreviation review and 26,419-byte COPYING.LIB hash match the observed x86_64 package. Preserve its unparsed scanner record; no synthetic SPDX declaration is substituted. |
| xz | 1:5.8.2-2.fc44 | Exact COPYING distinguishes 0BSD code, GPL scripts and public-domain documentation/translation portions. Preserve the package-wide aggregate rather than substituting the library-only license. |

### Assessment and continuing conditions

All 25 source-context candidates matched the actual canonical package versions,
source RPMs, epochs, raw declaration semantics, current fingerprints, required
notices and shared owners. Every canonical PURL was read from the actual
inventory and cross-checked against SPDX; none was constructed by changing an
ARM architecture string. The canonical filesystem metadata digest also matches
all 1,572 reviewed entries. The empty glibc metapackage and absence of the named
manual/Java RPMs were independently rechecked on this image.

The additional `libtool-ltdl` scope replaces the provisional ARM entry in the
[earlier raw-declaration review](fedora-rpm-license-review.md); the historical
observation remains evidence, not an active compatibility allowance. Its raw
fingerprint and exact notice bytes match that prior assessment. The existing
architecture-neutral Fedora public-key record already matches this image.
The existing x86_64 [static libstdc++ review](fedora-libstdcxx-review.md), previously
identified as a same-source policy inference, is now supported by the actual
canonical package and native-receipt observation. Its scope and fingerprint stay
unchanged; the legitimate separate ARM static-runtime review is preserved.

Each new entry is bound to one observed package PURL, one current fingerprint,
the owner, this review and the fixed expiry. The maintained ledger automatically
matches **PURL and fingerprint**; the image, source and notice hashes in this
review preserve its evidence and are not additional runtime matching fields.
A changed notice or source context therefore requires fresh review even if a
scanner emits the same identity. This review neither permits alternate historical
fingerprints nor authorizes automatic renewal.

Retain the complete notices and applicable source, relinking, attribution,
documentation and distribution conditions. This assessment does not determine
that every obligation for public redistribution is fulfilled, and application
`publish = false` is not a license grant. The image's failed runtime proof and
all other security/release gates remain independent requirements.
