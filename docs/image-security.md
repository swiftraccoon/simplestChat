# Production image security evidence

Image evidence applies to the exported production artifact. Source dependency
checks alone cannot establish which native code or operating-system packages
were included in that image. Each check must retain the exact image/platform,
archive and file hashes it examined; a rebuilt image is a different artifact.

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
- Only the declared glibc/math/compiler-support dynamic libraries. OpenSSL and
  the C++ runtime are intended to be statically linked in this build.

The interpreter and transitive shared libraries must resolve within the same
image filesystem. Absolute symlinks are interpreted relative to that image;
host libraries cannot satisfy a missing dependency. The report records resolved
file paths, byte counts and SHA-256 hashes. Native/package provenance must
associate these files with their build or RPM identities separately.

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

Rust license declarations come from the actual Cargo build graph. Registry
archive bytes must match `Cargo.lock`; local patched packages must match the
vendor receipt. A referenced license file retains its hash, and missing
expressions remain missing. The image checker joins these declarations to exact
name/version/source identities observed in `.dep-v0`. The application has no
asserted license: its SPDX value is `NOASSERTION`, with an explicit policy for the
unpublished first-party package and verified `publish = false`. This grants no
license and does not waive requirements for shipped dependencies.

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
suppress the scan. Public findings contain rule, original path, projected line
and original-file hash, never candidate secret values. A changed file cannot
inherit an old exact-finding exception. Detection covers printable strings and
scanner-supported encodings; it does not decode UTF-16, compressed/encrypted
content or strings split by nonprintable bytes. A passing result is not proof
that arbitrary hidden data contains no secrets.

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

## Evidence handling

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
`secret-paths.json`, `sbom/sbom.syft.json` and `spdx/sbom.spdx.json`. Trusted
release attestation must bind those successful results to the original archive
bytes; a broad upload of the working directory is unsafe.

The trusted `ci.yml` main-push signing job runs
`build/release_attestation.py prepare` against those exact directories after all
required gates pass. It emits `release-predicate.json` and copies only the SPDX
SBOM and image-security outcome into the release artifact. The signed statement
has `image.tar` and `sbom.spdx.json` as subjects and binds the other release files
through the predicate's SHA-256 map. `release-attestation.jsonl` contains the
signer's bundle. Signing a rebuilt image, a PR artifact, or a failed security
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
