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

## Review records and test data

`exceptions.json` is the shared, exact-match, expiring review ledger. Its schema
requires a scanner, finding fingerprint, scope, owner, rationale, reachability
assessment, expiry and review link. Expired or malformed records fail the whole
gate. Wildcard scopes, broad query suppression and indefinite exemptions are not
supported. [Review notes](reviews.md) explain the initial records.

`semgrep/` contains reviewed local rules and positive/negative engine fixtures.
`native/` contains authenticated synthetic parser inputs and native toolchain
pins. These are test data, not recordings from real users. Rule and corpus
changes need their own focused review and the corresponding real-engine checks.

The migration baseline authenticates SQL that was already deployed when this
gate was introduced. It does not allow editing those migrations. New migrations
are checked by Squawk and remain outside that fixed baseline.
