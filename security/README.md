# Security tool inputs

The repository owns its scanner configuration and installation policy. The shared
entry point is `build/check-security.sh`; operating instructions and enforcement
are described in [the security guide](../docs/security.md).

## Authenticated native executables

`build/security-tools.lock.json` records exact versions, HTTPS distribution URLs,
archive SHA-256 values and explicit archive-member-to-install-path mappings.
`build/security_tools.py` installs only those mapped regular files. It rejects
links, device entries, path traversal, duplicate entries, missing files, excessive
archive expansion and unsupported platforms. It never invokes an archive's
installer or extracts arbitrary paths onto the host.

Install the default source scanners or resolve one verified executable:

```sh
python3 build/security_tools.py install
python3 build/security_tools.py path semgrep-core
```

Tools are installed in a private directory below
`target/security-tools/<lock-digest>/<platform>`. A receipt binds every installed
executable and native companion library to the selected lock and archive.
Resolution checks ownership, permissions and file hashes before returning a
path. Interrupted installation does not publish a usable prefix. Installation
does not merge new tools into an unrelated or incomplete prefix; an explicit
`--directory` selects a separate installation.

The host account, interpreter and checkout remain trusted. A writable local
receipt is an integrity check for accidental changes and unexpected replacement,
not a signature that protects against compromise of that same account. Review
the upstream release and independently authenticate every changed digest before
accepting a lock update. A checksum obtained from the same compromised upstream
release would not establish publisher identity.

The standalone Semgrep engine and its native libraries come from authenticated
official wheels. The Python CLI, authentication integration and MCP dependencies
are not installed. Every selected companion library participates in receipt
verification. No engine version is borrowed from the developer's global PATH.

## Python scanner environment

`requirements.in` declares the small scanner dependency set; `requirements.txt`
contains its complete hashed wheel lock. The shared shell entry point creates a
separate environment keyed by that lock's digest. It installs with
`--require-hashes --only-binary=:all:` and checks dependency consistency before
publishing the environment receipt. Deployment and build-tool environments are
independent and are audited separately.

To refresh the lock in a reviewed tool-update change:

```sh
uv pip compile --universal --python-version 3.12 --generate-hashes \
  --only-binary :all: --no-emit-index-url security/requirements.in \
  --output-file security/requirements.txt
build/check-security.sh fast
```

Review both the direct version change and all transitive changes. The gate audits
its own scanner environment; a vulnerable scanner dependency is not automatically
exempt because it is used only in CI.

## Source secret coverage

The fast gate snapshots tracked and nonignored working-tree files before
scanning. Each original file is bounded to 16 MiB, the complete snapshot to
128 MiB, and the file count to 20,000. The identity snapshot includes lockfiles,
vendored inputs and binary files; ignored build output and local credentials are
outside it. Source scanners omit `vendor/` by default, including current-tree and
Git-diff secret scanning. An explicit local
`build/check-security.sh fast --include-vendor` includes vendor sources; this
opt-in is rejected in CI environments. The full identity snapshot continues to
bind dependency/provenance checks. The image tier scans shipped image contents
separately, including bundled dependency bytes.

Gitleaks normally excludes some filenames and content types. The shared
`security_secret_projection.py` helper creates a neutral-name, printable-ASCII
view of every selected file. Its fixed text prefix prevents binary magic from
triggering the pinned scanner's content exclusions. Contiguous ASCII strings,
tabs and line endings are preserved; other bytes become newline delimiters.
The projection remains bounded by the original file limit plus its fixed prefix.
Before writing projections, the gate requires enough disk space for all source
bytes, every prefix and a 64 MiB reserve. The file-count ceiling bounds prefix
overhead as well as total work; a budget failure rejects the entire scan.
Gitleaks' separate size cutoff is disabled because it must not silently omit a
file already accepted by these repository bounds.

Every projection records the original path, original SHA-256 and byte count,
alongside the projected SHA-256, byte count and format. Private
`secret-coverage.json` evidence binds the full set to the source manifest;
`summary.json` retains only file/byte counts and the coverage digest. Findings
refer to projected line numbers, so the original path and hash identify the
source to inspect. Scanner output remains private and redacted.

Each fast run first requires the actual pinned detector to find exactly one
redacted, never-issued credential-shaped canary inside projected binary input.
An inline `gitleaks:allow` comment must not suppress it. The current-tree scan
and the selected Git diff scans must then return both a successful exit status
and an explicit empty findings array. Fixture allowances retain their exact
review fingerprints and original paths; translating a fixture to its neutral
filename never exempts the same value elsewhere.

This is printable-string detection, with up to three scanner decoding passes
for supported encodings. It does not decrypt or unpack arbitrary content, decode
UTF-16 credentials, or reconstruct strings separated by nonprintable bytes.
Git history checks cover textual diffs; the complete projection applies to the
selected current tree. Passing the gate does not establish that arbitrary
binary or encoded content contains no secrets.

After installing the pinned tools, run the focused real-engine checks with:

```sh
SIMPLESTCHAT_GITLEAKS_ENGINE_TESTS=1 python3 -m unittest discover \
  -s ops/ansible/tests -p test_security_secrets.py
```

## Review records and test data

`exceptions.json` is the shared, exact-match, expiring review ledger. Its schema
requires a scanner, finding fingerprint, scope, owner, rationale, reachability
assessment, expiry and review link. Malformed records fail the gate; expiry
applies to the selected source scope. Vendor source reviews are enforced only
for explicit local vendor analysis, while dependency and image reviews always
apply. Wildcard scopes, broad query suppression and indefinite exemptions are not
supported. [Review notes](reviews.md) explain the initial records.

`semgrep/` contains reviewed local rules and positive/negative engine fixtures.
`native/` contains authenticated synthetic parser inputs and native toolchain
pins. These are test data, not recordings from real users. Rule and corpus
changes need their own focused review and the corresponding real-engine checks.

The migration baseline authenticates SQL that was already deployed when this
gate was introduced. It does not allow editing those migrations. New migrations
are checked by Squawk and remain outside that fixed baseline.
