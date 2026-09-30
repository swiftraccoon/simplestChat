# Canonical runtime RPM license context review

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

## Evidence and identity

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

## Package-specific findings

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

## Assessment and continuing conditions

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
