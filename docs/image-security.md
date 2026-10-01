# Production image security evidence

Image evidence applies to the exported production artifact. Source dependency
checks alone cannot establish which native code or operating-system packages
were included in that image. Each check must retain the exact image/platform,
archive and file hashes it examined; a rebuilt image is a different artifact.

## Fedora package refresh and review caches

Both Fedora package-install instructions in `Dockerfile` mount the current
`security/image-policy.json` and `security/exceptions.json` read-only.
Their bytes participate in the
[RUN bind-mount cache checksum](https://docs.docker.com/build/cache/invalidation/),
so changing either file invalidates both the builder and runtime package layers.
The files are admitted individually by `.dockerignore` and are not copied into
image layers. `FEDORA_REFRESH_EPOCH` remains the explicit refresh control for
package/security updates that do not change policy.

The production and load-generator Actions cache keys include these same policy
files. Their existing trusted-main/untrusted-PR and architecture separation
remains intact. Prefix fallback can recover reusable build layers, but the
Dockerfile's policy-dependent inputs prevent an obsolete package-install layer
from matching. Updating only an outer Actions cache key would not provide that
guarantee, because a restored BuildKit cache independently matches instructions.

This dependency prevents a new package review from silently reusing packages
cached under the superseded policy. It does not pin mutable Fedora repositories:
two uncached builds can still resolve different package versions. Every exported
image must pass the exact RPM/license/secret/runtime checks and retain its own
evidence. A reviewed PR image neither broadens an exception nor substitutes for
the main image's scan and signing requirements.

## Rust dependency metadata

Both production Cargo builds in `Dockerfile` use the same checksum-pinned
`cargo-auditable` executable, installed through `build/security_tools.py` from
`build/security-tools.lock.json`. The final build verifies its installed receipt
and executable hash again. There is no unverified executable lookup fallback.

`cargo-auditable` embeds a compressed dependency graph in the executable's
`.dep-v0` section. The maintained checker requires the format emitted by the
pinned stable-toolchain build, exactly one local `simplestChat` root, valid
dependency indexes and an acyclic graph. Missing, duplicate, corrupt, oversized
or unsupported metadata fails; dependency guesses from panic strings do not
substitute for the section. Decompression has an eight-MiB limit.

The embedded graph distinguishes build and runtime dependencies, but stable
Cargo metadata can still include more dependencies than the final machine code
uses. It does not identify every C/C++ library embedded by build scripts.
Independent native provenance remains necessary. See the upstream
[cargo-auditable contract](https://github.com/rust-secure-code/cargo-auditable)
and [bounded parser guidance](https://github.com/rust-secure-code/cargo-auditable/blob/master/PARSING.md).

## Static executable checks

`build/security_elf.py` reads ELF bytes without loading or executing the
application or its libraries. For an already materialized image filesystem:

```sh
python3 build/security_elf.py --rootfs /path/to/image-rootfs \
  --platform linux/amd64 --output /path/to/new-elf-report.json
```

The input must come from the selected image's validated archive. This standalone
command checks that tree; it does not itself authenticate an archive or source
revision. The complete image checker must bind its result to those identities.
It accepts the current little-endian ELF64 image platforms, checks every table
and file range before reading, and limits input and metadata sizes.

The application must encode all of these properties:

- Position-independent executable identity, an executable entry point and the
  platform's expected dynamic interpreter.
- A GNU RELRO segment and immediate symbol binding.
- A present, non-executable GNU stack declaration.
- No writable executable load segment or text relocations.
- No runtime library search-path override through `RPATH` or `RUNPATH`.
- Only the declared glibc/math/compiler-support dynamic libraries and the
  selected platform's loader SONAME. OpenSSL and the C++ runtime are intended
  to be statically linked in this build.

The expected loader may appear as both `PT_INTERP` and a direct `DT_NEEDED`
dependency: `ld-linux-x86-64.so.2` on amd64 or `ld-linux-aarch64.so.1` on arm64.
glibc's [linker-script design](https://sourceware.org/legacy-ml/libc-alpha/2013-05/msg00231.html)
includes the loader through `AS_NEEDED`, so direct linkage is legitimate.
The executable and transitive dependency checks share this platform-specific
allowlist; a loader from another architecture remains forbidden. The loader
still has to resolve inside the selected image and retain its hashed RPM
provenance, whether reached through one or both metadata entries.

The interpreter and transitive shared libraries must resolve within the same
image filesystem. Absolute symlinks are interpreted relative to that image;
host libraries cannot satisfy a missing dependency. The report records resolved
file paths, byte counts and SHA-256 hashes. Native/package provenance must
associate these files with their build or RPM identities separately.

Runtime RPM ownership resolves the recorded file path through that same image
filesystem, including Fedora's `/lib64` directory alias. The evidence preserves
both the RPM path and resolved ELF path. Exactly one regular RPM file record must
match the rehashed library's SHA-256 and size; duplicate claims, missing candidates
and altered bytes fail. Equal bytes at a different path do not establish ownership.

A hardening failure also retains bounded dynamic-dependency evidence in
`elf.json`. Up to 32 identities are reported, prioritizing unapproved dependencies;
the complete count, ordered-name digest and omission count remain explicit.
Only short conventional lowercase library SONAMEs and the supported loader
SONAMEs are displayed. Unusual names retain only their SHA-256 and approval state;
malformed names are rejected before this diagnostic is produced. This evidence
does not expand the runtime allowlist or turn a failed check into a passing one.

These checks establish encoded artifact properties. They do not prove deployed
kernel ASLR/NX policy, every function's compiler protections, decoded media
quality, or absence of vulnerabilities. The dynamic-linker rationale is covered
in the [glibc hardening guide](https://sourceware.org/glibc/manual/2.40/html_node/Dynamic-Linker-Hardening.html).

The scanner binds the raw Docker config digest, layer diff IDs and architecture
to the exact selected image ID. Matching layers alone do not establish image
identity because configuration bytes can differ. Signer preparation independently
rehashes the selected archive config before issuing its predicate.

## Conditional OpenSSL runtime disposition

The Fedora OpenSSL RPM remains affected by CVE-2026-84782 until its upstream
version is fixed. The image gate preserves that observation and every Grype
finding. It may separately classify the exact shipped server as not affected
only after `build/security_runtime.py` proves the reviewed managed execution
profile. This is not a package-wide exception and does not suppress other CVEs
or findings for another package.

The proof binds the archive and image configuration, source revision, application
binary, actual ELF/native evidence files and SPDX SBOM. It requires authenticated
static OpenSSL 3.5.9 with shared libraries, dynamic loading and engines disabled
while retaining built-in providers, including legacy algorithms;
no loader cache, preload or hwcaps paths; no ELF audit/filter or RPATH/RUNPATH
hooks; identical bytes for every present standard-directory candidate of each
approved dependency; and the exact files/DNS NSS configuration. Image defaults
and managed deployment settings use the same `runtime_profile.py` validator.
The source review in `security/runtime-source-review.json` pins complete source
trees, dependency/native manifests and profile bytes, expires on a fixed date,
and requires explicit re-review after changes. The gate does not regenerate or
renew it.

`runtime-proof.json` records these prerequisites. `vex.openvex.json` identifies
the exact managed-server product, archive hash and affected RPM PURLs using
OpenVEX's `vulnerable_code_not_in_execute_path` justification. The product identity
binds the proof, advisory policy and package identities; it does not mislabel a
Docker image configuration hash as an OCI manifest digest. `affectedFindings`
and `reviewedAdvisories` retain the original affected observations, while
`notAffectedForManagedServer` records the separate disposition. Missing or stale
proof fails the gate and leaves pre-disposition findings in the outcome. When
no affected reviewed RPM is installed, both evidence files still exist, the
proof says `required: false`, and the VEX contains no statements.

This assessment covers the reviewed server's DTLS execution path under the
shipped command and managed profile. It does not claim that unused libc/libuv
APIs cannot load code, that every installed program is safe, or that arbitrary
operator-selected commands and mounts satisfy the profile. The container
engine's existing init process is part of the trusted deployment runtime, not
an executable attested by the image ELF closure. No old ARM preview qualifies:
a fresh fixed, hardened image must satisfy every prerequisite. The conditional
statement and its proof are release evidence; signature and expiry enforcement
are described below.

## Managed runtime profile

The runtime layer removes `/etc/ld.so.cache` after its last package installation
and replaces the base image's NSS symlink with the reviewed
`security/runtime/nsswitch.conf` as a regular file. Removing the symlink before
copying prevents Docker from writing through it into authselect's configuration.
Name-service lookups
use glibc's built-in `files` implementation and ordinary container DNS for hosts;
the image no longer selects optional systemd NSS modules. This does not remove
their RPM inventory or claim that every installed program has the same behavior.

Managed deployment validates the resolved application and migration services
before startup or replacement. It retains the audited image command, user and
working directory, rejects healthchecks and lifecycle hooks, and allows only the
read-only database socket mount and bounded `noexec` temporary directory. Loader,
OpenSSL provider and locale-module environment overrides are rejected, including
empty values. The same helper checks image defaults and is bound into runtime
evidence. These controls define the supported application profile; arbitrary
administrator-selected programs and altered containers require separate review.

## Validation

The ELF fixtures are inert byte arrays, never executed. They cover independent
hardening failures, truncated and oversized tables, virtual-address bounds,
missing and malformed Rust metadata, dependency graph failures, image-relative
symlinks and missing libraries. Run them with the maintained Python environment:

```sh
ops/ansible/.venv/bin/python -m unittest discover \
  -s ops/ansible/tests -p test_security_elf.py -v
```

A real exported image remains a required integration check before treating a
new compiler, base image or checker version as release-ready.

## Authenticated native build inputs

The builder runs `security_vendor.py verify` before dependency warmup. It checks
all reviewed upstream archives and maintained patches, rather than accepting a
vendored directory because one familiar version string appears in it.
`security_native.py` then consumes the final successful production Cargo JSON
stream. It rejects test, debug and load-generator artifacts and selects the
actual build-script outputs, including cached outputs. It does not sweep an old
`target/` directory to guess which libraries were used.

The runtime image contains `/usr/share/simplestchat/native-components.json`.
Its source inventory distinguishes production, native-test, Windows-only and
header-only components. The receipt binds reviewed source and patch identities,
static archive hashes, OpenSSL headers and installer, the authenticated AWS-LC
crate, compiler identity, builder RPM/source-RPM identities, and the final
executable's SHA-256. Fedora signing-key pseudo-packages are recorded separately;
they are not misrepresented as source-backed runtime packages.

OpenSSL's current build receipt requires the reviewed `no-shared`, `no-dso`,
`no-module` and `no-engine` configuration. It binds the installed version and
configuration header hashes, the build-system disabled-options record, and the
actual `ssl`/`crypto` archive identities referenced by both native link providers.
Distinct Cargo build instances of a provider remain separate records: SSL and
crypto must cover the same provider/output-directory pairs, and every record
must identify the configured archive's exact path, size and hash.
The image consumer checks those records against the current source/installer
manifest and link inputs; a version-only receipt is insufficient. These settings
disable dynamic loading and engines while retaining the default, base and legacy
providers as built-in code. They do not remove legacy algorithms or establish
that every other component in the process is incapable of dynamic loading.

Rust license evidence covers both the successful Cargo compiler-artifact stream
and the dependency graph decoded from the exact final executable's `.dep-v0`
section. These inventories can differ: embedded metadata can include optional
resolved packages that emitted no compiler-artifact event. Every observed package
remains in policy evaluation. Each receipt record distinguishes compiler-event
evidence from embedded-metadata evidence; neither establishes that every package
contributed machine code after linking.

Registry archive bytes must match the exact `Cargo.lock` checksum; local patched
packages must match the vendor receipt. An embedded-only registry package uses
the single registry cache already selected by actual compiled manifests. Missing
archives, ambiguous identities, unsupported sources and unrecorded local packages
fail the build; the producer has no network fallback and invents no declaration.
A referenced license file retains its hash, and missing expressions remain
missing. The receipt binds the embedded graph's compressed and decoded hashes,
format and package count, as well as the executable hash. The image checker
requires the same binding and joins exact name/version/source identities without
accepting an older receipt shape. The application has no
asserted license: its SPDX value is `NOASSERTION`, with an explicit policy for the
unpublished first-party package and verified `publish = false`. This grants no
license and does not waive requirements for shipped dependencies.

Some currently locked crates publish slash-separated Cargo license declarations.
At the authenticated crates.io join, the image adapter recognizes exactly the
four reviewed values: `MIT/Apache-2.0`, `Apache-2.0/MIT`, `Apache-2.0 / MIT`, and
`Unlicense/MIT`. It derives the corresponding SPDX `OR` expression while retaining
the original declaration unchanged in the native receipt and SBOM license `value`;
the separate `spdxExpression` field identifies the derived representation. This
implements the meaning of these upstream inputs, not an alternate project
receipt format. Local and RPM declarations do not use this adapter. Unrecognized
slash forms, mixed expressions and path-like strings remain invalid under the
unchanged strict SPDX parser.

The [Cargo manifest documentation](https://doc.rust-lang.org/cargo/reference/manifest.html#the-license-and-license-file-fields)
describes the deprecated slash notation and license choices. The retained
[declaration review](../security/license-evidence/cargo-declarations-2026-09-30.json)
binds all 35 observed declarations to `Cargo.lock` archive and manifest hashes,
plus the packaged license-text hashes where present. Twelve occur in the observed
runtime SBOM. For `asn1-rs-impl`, the crate manifest declares the terms but omits
the license text; its recorded exact upstream revision explicitly offers either
Apache-2.0 or MIT. This metadata interpretation does not waive notice obligations
or establish that every package contributed linked machine code.

Static archive identity establishes build inputs, not that every archive member
survives linker garbage collection. Adapted libwebrtc sources are not silently
treated as the entire upstream library for vulnerability applicability.

## Scan the canonical exported image

The release producer builds and tests one immutable image, then exports it once.
Use that same private artifact directory, containing `image.tar`, `release.json`
and a successful `outcome.json` bound to the selected image ID:

```sh
python3 build/security_tools.py install \
  --tools syft grype gitleaks --platform linux-x86_64 \
  --directory /private/new-image-tools
python3 build/security_image.py sha256:FULL_IMAGE_ID \
  --artifact-dir /private/canonical-release \
  --tools-directory /private/new-image-tools \
  --output /private/new-image-security
```

The public release contract is Linux amd64 through Docker. Podman is available
for isolated development probes, but its native ARM export is not a substitute
for the canonical release artifact. In particular, a Podman Docker-save archive
can include unreferenced outer symlinks that the release verifier intentionally
rejects. Do not rewrite an archive after scanning it or claim a development
snapshot is a signed release.

Materialization also requires a case-sensitive filesystem. A macOS development
volume that aliases distinct Linux paths (for example, terminfo `Eterm` and
`eterm`) fails rather than merging those files. Local ARM probes establish
individual tool and ELF behavior; the complete canonical archive check remains
a required Linux integration gate.

The checker requires the exact `sha256:` ID, checks the export receipt, verifies
the archive hash and every uncompressed layer digest, and compares the selected
local image's layer identities with that archive. It never runs the application
image. Parser failures, missing evidence, scanner errors, policy findings,
timeouts and cleanup failures all return a nonzero status.

The archive parser has explicit limits: eight GiB input, two GiB per expanded
layer, four GiB total expanded layer data, 256 MiB per regular file, 200,000
members and 128 layers. Extended tar metadata has both per-header and cumulative
limits. Whiteouts and opaque directories apply to the previous layer before new
files. Hardlinks capture their original file bytes. Paths, link traversal,
devices, sparse entries and conflicting metadata are checked before filesystem
materialization; archive modes, owners and devices never control host resources.

## Scanner isolation and database freshness

Only checksummed, receipt-verified Syft, Grype and Gitleaks executables enter the
scanner mount. The tools run on the separately pinned Fedora scanner base,
never on the application image's executable, loader or shell. Each scanner runs
as a non-root identity with a read-only root, no capabilities, no-new-privileges,
no engine socket or home/source/auth mounts, two CPUs, two GiB memory with no
additional swap, a 128-process limit and a bounded temporary filesystem. Artifact
scans have no network. Writable outputs have file-count and byte limits in the
controller as well as a 900-second process deadline.

Syft's archive catalog operation has a one-GiB `/tmp` tmpfs because its image
reader decompresses and caches layer data there. Other scanner operations,
including Syft format conversion, retain the 256-MiB tmpfs. Both remain inside
the same two-GiB container memory limit with no additional swap: temporary
storage is a ceiling, not reserved memory. The archive parser's four-GiB
expanded-input limit is independent and does not promise that every accepted
archive fits Syft's smaller working storage. Exhaustion, OOM, timeout or partial
scanner output fails the check; there is no automatic larger or unbounded retry.
This follows Syft's documented [archive source caching](https://github.com/anchore/syft/wiki/supported-sources).

Each of at most eight scanner invocations retains `scanner-result-NN.json`.
These small public-safe receipts contain the fixed tool/failure classification,
command and container exit statuses, OOM/time/output-limit flags, cleanup
verification, declared resource limits, and the byte count/SHA-256 of the bounded
private command log. They contain no log excerpts, environment, command arguments
or artifact-derived paths. A diagnostic identifies the observed failure; it does
not replace a successful complete scanner report. Full command logs remain private.

Grype database preparation is a separate online container. It mounts only the
verified tools and a new empty database directory. The expanded database budget
is four GiB; the current database is approximately three GiB. The subsequent
artifact scan uses the database read-only with updates disabled. The checker
requires Grype's valid current v6 status, the expected HTTPS provider/checksum,
Fedora/GitHub/NVD provider coverage and a build age of at most 120 hours. Database
and import-receipt hashes bind the recorded status to the bytes used. Missing or
stale databases fail; they are never treated as zero vulnerabilities.

Cleanup rechecks each randomly named container's exact immutable ID, image and
ownership label before removal, and confirms its ID is absent afterward. A name
prefix is never permission to delete a resource. Identity ambiguity leaves a
failure and retained evidence rather than removing an unrelated container.

## What each result establishes

Syft catalogs the image's RPM and embedded Rust inventories. Authenticated native
inputs and source-derived licenses supplement the SBOM. The static C++ runtime
retains its actual Fedora RPM identity for advisory matching. The ELF checker
also requires every resolved runtime library to have one owning image RPM whose
recorded SHA-256 matches the library bytes.

Grype findings include unfixed vulnerabilities. High, Critical and unknown
severity findings block the result unless an exact reviewed exception applies;
ignored matches or suppressed fix-state classes do not produce a passing
assessment. Native inputs without reviewed advisory applicability are explicitly
identified as an inventory limitation. No guessed CPE makes an adapted source
subset equivalent to an entire upstream product. Bundled/minified JavaScript may
not be independently discoverable in the image; the locked npm source audit is
an additional required gate, not a claim that the image SBOM reconstructs every
Vite module.

License expressions use bounded SPDX `AND`, `OR`, parentheses and `WITH`
semantics. Standard license and exception identifiers match without case
sensitivity, while user-defined references retain their identity. Every required
conjunct must be approved; an approved alternative may satisfy an `OR` choice.
Missing and unreviewed expressions fail. The maintained allowlist is an
operational policy, not a substitute for preserving required notices or
satisfying distribution obligations. The exact Fedora static-runtime aggregate
license and RPM scopes are documented in the [libstdc++ review](../security/fedora-libstdcxx-review.md).

Unparsed license exceptions bind the exact package PURL and every ordered raw
declaration field, including evidence paths. A verified location layer digest
is represented by an explicit marker in the review fingerprint, so an otherwise
equivalent layer rebuild can reuse the review. The original records and their
separate SHA-256 remain in the image evidence. Missing or malformed layer IDs,
changed declarations and changed package scopes cannot inherit an exception.
See the [raw RPM metadata review](../security/fedora-rpm-license-review.md#unparsed-declared-metadata)
for the current reviewed records and fingerprint contract.

Gitleaks receives a printable-text projection of every retained regular file
from every image layer, including files deleted later, plus the complete image
configuration/history. The shared projector retains contiguous ASCII printable
bytes, tabs and line endings; other bytes become newline delimiters. A fixed
plain-text prefix covers every application-format sniff offset in the pinned
scanner, including ISO signatures. Neutral numbered filenames prevent built-in
path exclusions from skipping lockfiles, vendored files or detector settings.

Before writing projections, the controller checks every file's size, a maximum
of 200,001 regular files (layer members plus config), a four GiB aggregate
projection budget, and enough free space for the complete projected bytes plus
256 MiB reserve. Each projection adds the recorded fixed prefix to its bounded
source size. The scanner's decimal-MB skip threshold is disabled because these
byte limits already reject oversized input. No file is silently omitted to fit
a budget. Original and projected SHA-256 hashes, sizes and format bind every
neutral path to its complete original file.

Image-provided configuration, ignore files and inline allow comments cannot
suppress the scan. Public findings retain rule, original path, all scanner
coordinates, and complete original-file and projection hashes, never candidate
secret values. Exact public-data reviews identify the complete detected region
at that path; they do not approve the containing file or package. Detection
covers printable strings and
scanner-supported encodings; it does not decode UTF-16, compressed/encrypted
content or strings split by nonprintable bytes. A passing result is not proof
that arbitrary hidden data contains no secrets.

The public secret verdict retains digest-only `projectionSpans` evidence.
Each record binds the original whole-file hash, full projection hash, and all
four scanner coordinates. The helper mirrors Gitleaks 8.30.1's bounded file
framing and byte-column convention: columns include the preceding LF except at
a fragment's first line. A uniquely resolved match region receives its byte
length and SHA-256. Decoded findings identify the original encoded region;
the region is not necessarily the rule's captured `Secret` group. Ambiguous
coordinates on fragmented long lines, or coordinates without an exact region,
remain explicitly unresolved and never receive a guessed digest or a review
fingerprint. Every span must correspond one-to-one with its current scanner row,
path map, file/projection hashes and coordinates before policy matching.

An image-secret fingerprint is `image-match:` followed by the SHA-256 of the
UTF-8 bytes `simplestchat-image-secret-match` plus a NUL separator and canonical
JSON (sorted keys, compact separators, ASCII escaping). The JSON binds `format`,
`projectionFormat`, `rule`, `path`, `spanBytes` and `spanSha256`. Only a uniquely
resolved complete match can receive this identity. The pinned scanner/projection
format, exact original layer path, rule, region length and digest must all agree
with an unexpired reviewed exception. Changed surrounding file bytes or line
positions retain new evidence without changing a reviewed public region's
identity; changed regions, paths, rules or formats remain blocking. Every other
finding is evaluated independently, including findings in the same file.
Source-repository Gitleaks fixture policy is separate and unchanged.

The helper authenticates complete projection bytes using no-follow regular-file
reads, with 256 MiB plus prefix per file, four GiB aggregate reads, 20,000
findings, 100,000 candidate checks and a 90-second deadline. `--redact=100`
remains enabled; candidate text, context, scanner `Match` and `Secret` fields
are never published. Hashes reveal equality and permit offline guessing of
low-entropy values; they are content identifiers, not encrypted secrets.

Before scanning the artifact, the identical pinned executable and arguments
must detect a never-issued inert credential-shaped canary in each text, ELF,
PDF and ISO-shaped input, through the same projector. Inline allow comments
must not suppress them. Empty, missing, partial or unrelated canary reports fail
before the artifact can be declared clean.

All per-finding waivers use `security/exceptions.json`: an exact scanner,
fingerprint and scope, named owner, rationale, reachability assessment, review
link and expiry. There is no second image-specific waiver mechanism. Expired or
malformed records fail the whole policy check.

The scanner behaviors are documented upstream in the
[Syft target guide](https://oss.anchore.com/docs/guides/sbom/scan-targets/),
[Grype configuration reference](https://oss.anchore.com/docs/reference/grype/configuration/),
[pinned Gitleaks configuration](https://github.com/gitleaks/gitleaks/blob/v8.30.1/config/gitleaks.toml)
and [SPDX expression specification](https://spdx.github.io/spdx-spec/v2.2.2/SPDX-license-expressions/).

## Native advisory coverage

Authenticated source and license records establish which native inputs were
compiled. They do not establish advisory coverage. The supplemental binary
entries currently have no PURL or CPE, and Grype's automatic CPE inference is
disabled. A zero-match result therefore does not establish NVD coverage for
OpenSSL, AWS-LC, Abseil, FlatBuffers, libuv, unordered_dense, libsrtp or libwebrtc.
The separately identified Fedora static C++ runtime retains RPM matching.

Reviewed identities must preserve component boundaries. NVD has verified product
families for OpenSSL, libuv, Abseil C++ and non-FIPS AWS-LC; that does not mean every
pinned version has an exact dictionary entry. The current nondeprecated
FlatBuffers dictionary records found in the September 2026 review describe the
Rust crate, not the native C++ entry. No confirmed identity was found for
unordered_dense. The Versatica libsrtp fork and adapted libwebrtc subset require
source-specific applicability review; neither inherits the whole upstream
product's CPE or version ranges. A Rust wrapper's advisory coverage also does not
establish complete coverage for its bundled native library.

Fresh databases can lack affected-product mappings for a newly published CVE.
On 2026-09-30, the retained Grype database contained CVE-2026-84782 in NVD with
status `analyzing`, no affected-CPE/package mappings and no Fedora record. The
[official OpenSSL CNA record](https://openssl-library.org/news/secjson/cve-2026-84782.json)
already identified the High-severity issue and the affected 3.5.0–3.5.8 range,
fixed in 3.5.9. Adding an OpenSSL CPE alone would not close that ingestion gap.

The fast source gate separately requires the pinned OpenSSL patch to match the
single current stable archive in its supported major/minor series on the
[official source page](https://openssl-library.org/source/). Missing, ambiguous,
unavailable or stale evidence fails; the check never updates the pin or changes
the supported series. Its receipt binds the observed page and native manifest
hashes. This release-freshness check complements advisory matching; it is not a
vulnerability-free or application-reachability verdict. Empty advisory API
responses likewise do not establish indexing or complete native coverage.

### Reviewed OpenSSL RPM safeguard

The image gate additionally applies
`security/advisories/openssl-cve-2026-84782.json` to every observed Fedora OpenSSL
source-RPM subpackage. The reviewed policy records the official CNA and advisory
URLs and SHA-256 hashes. The current scope is Fedora 44's OpenSSL 3.5 series:
3.5.0 through 3.5.8 are affected; 3.5.9 and later patches pass this particular
check. Different series require a new review. Package name, upstream version,
release, epoch, architecture, source RPM and qualified PURL must agree; missing
or inconsistent identities fail. A higher RPM release suffix does not prove a
distribution backport and cannot bypass the affected upstream version.

When policy evaluation completes, `checks.json` retains these results under
`vulnerabilities.reviewedAdvisories`, including the policy digest, source evidence,
assessed package identities and blocked findings. Grype's match count and findings
remain unchanged; independent advisory findings carry their own source marker
and are included in the overall
blocked list. Existing Grype exceptions cannot waive this safeguard. If the
required runtime proof fails, `outcome.json.vulnerabilityFindings` retains the
pre-disposition findings and the gate fails before later policy checks complete.
This is a known-advisory check, not a claim of exhaustive native coverage or
application reachability, and it does not assert that Fedora has published a
fixed RPM. A zero-match scanner result cannot override it.

## Evidence handling

The image gate retains `runtime-license-evidence.json` before evaluating license
policy. It binds the exact archive, image, revision, platform and SPDX SBOM to
each runtime RPM's package URL, source RPM and ordered license declarations.
For package-owned license files and named documentation notices it records paths,
sizes and SHA-256 hashes from the authenticated image filesystem, including the
resolved path of permitted notice symlinks. File contents are never included.
Package, metadata, file-count and byte limits are enforced; a regular notice
whose bytes disagree with RPM metadata fails the image gate. Empty notice sets
remain explicit, and static build inputs are identified separately.

This report grants no license approval or automatic exception. A reviewer can
compare its exact hashes with reviewed upstream notice evidence even when the
license gate fails and the release archive is not published. The image outcome
binds the report hash, and signing verifies those bytes before attesting the
outcome. The report is published with the production-security CI evidence,
separately from the ten-file release ZIP; its hash remains authenticated through
the signed `image-security.json`. Missing or unfamiliar notices still require
review; the report is not a claim that every applicable notice obligation has
been fulfilled.

A passing `outcome.json` binds the archive, selected image, revision, platform,
SPDX SBOM, native and ELF reports, database evidence, secret-path map, tool lock,
policy and exceptions by SHA-256. `checks.json` separates vulnerabilities,
licenses and secrets; a scanner exit of zero alone never establishes coverage.
Raw image configuration and manifest blobs are removed from the publishable
SBOM while their image identities remain.

Keep the entire output directory private. It contains original layer bytes,
raw configuration, raw scanner reports, writable-mount evidence and database
files. Publish only the explicit reviewed evidence set: `outcome.json`,
`checks.json`, `elf.json`, `native.json`, `database-status.json`,
`secret-paths.json`, `runtime-proof.json`, `vex.openvex.json`,
`runtime-license-evidence.json`,
`sbom/sbom.syft.json` and `spdx/sbom.spdx.json`. Trusted
release attestation must bind those successful results to the original archive
bytes; a broad upload of the working directory is unsafe.

The trusted `ci.yml` main-push signing job runs
`build/release_attestation.py prepare` against those exact directories after all
required gates pass. It emits `release-predicate.json` and copies the SPDX SBOM,
image-security outcome, runtime proof and conditional OpenVEX disposition into
the release artifact. The signed statement has `image.tar`, `sbom.spdx.json`,
`runtime-proof.json` and `vex.openvex.json` as subjects and binds the other release
files through the predicate's SHA-256 map. Signing and subsequent deployment
verification require matching proof/artifact identities, exact evidence hashes
and a consistent conditional disposition. Required source reviews expire at
the start of their stated UTC date; a previously signed release cannot bypass
that expiry. When no reviewed affected RPM is present, the authenticated proof
records `required: false` and the VEX statement list is empty.
`release-attestation.jsonl` contains the signer's bundle. Signing a rebuilt image, a PR artifact, or a failed security
outcome is outside this contract.

Deployment verifies the bundle with GitHub's maintained cryptographic verifier
before any remote action, then checks its exact source, workflow, run, attempt
and file relationships. The receiver rehashes the downloaded bytes against that
verified claim. The separate manual build workflow and unsigned local builds
are not deployable. See [release verification](../ops/ansible/RELEASES.md#trusted-artifact-requirements).

Run the archive, ELF, policy, lifecycle and evidence-join fixtures with the
maintained environment:

```sh
ops/ansible/.venv/bin/python -m unittest discover \
  -s ops/ansible/tests -p 'test_security_image*.py' -v
ops/ansible/.venv/bin/python -m unittest discover \
  -s ops/ansible/tests -p test_security_archive.py -v
ops/ansible/.venv/bin/python -m unittest discover \
  -s ops/ansible/tests -p test_security_elf.py -v
```
