# Fedora RPM license metadata review

Reviewed 2026-09-30. The [evidence record](license-evidence/fedora-rpm-review-2026-09-30.json)
identifies the earlier ARM scanner probe by image ID and report SHA-256. That
probe contains 147 RPM entries; it is not the final release image, does not
establish Rust/native inventory completeness, and is not a canonical image pass.
The final Linux release inventory remains authoritative.

## Standard license terms

The review adds 55 explicit software license or license-with-exception terms to
`image-policy.json`. Each is identified by [SPDX License List 3.29.0](https://spdx.org/licenses/)
and has an `allowed` record in
[Fedora's maintained legal data at the recorded commit](https://forge.fedoraproject.org/legal/fedora-license-data/src/commit/cbd8b74cd481a598668f05d1878fcd53a8ef78e4).
The evidence lists every term, exact Fedora expression, status, source URL and
source-file SHA-256. The SPDX license and exception data file hashes are also
retained. SPDX identity alone does not establish policy approval.

These additions cover specifically named BSD/MIT/HPND variants, runtime and
build-tool exceptions, and other reviewed software terms already used by the
Fedora runtime. They do not permit arbitrary variants or unknown exceptions.
The original complete package expression remains in the SBOM; `AND` still
requires every term, while `OR` permits an independently approved alternative.

Six GFDL terms remain outside the general allowlist because Fedora classifies
them as documentation-only. `OPUBL-1.0` also remains blocked: Fedora marks it
not allowed generally, with a specific exception for Vim documentation when its
optional restrictions are absent. Neither context can be inferred from a bare
license name. The evidence records these seven unresolved terms separately.
Custom `LicenseRef-*` terms, including Fedora's public-domain and Callaway
references, still require exact package/aggregate review. No general custom
reference rule was added.

## Unparsed declared metadata

The probe exposes two declared records whose `spdxExpression` is empty:

- `libtool-ltdl` declares `LGPLv2+`. The pinned Fedora legal record maps that
  abbreviation to `LGPL-2.0-or-later`, which this policy already permits. The
  observed RPM records its `COPYING.LIB` file and digest. The
  [upstream Libtool manual](https://www.gnu.org/s/libtool/manual/html_node/Using-libltdl.html)
  describes LGPL terms and the Libtool exception. The review preserves the
  observed declaration rather than asserting that Syft observed a normalized
  SPDX expression. Current package-page metadata is not substituted for the
  recorded version's actual declaration.
- `gpg-pubkey` declares `pubkey`. RPM's
  [tag documentation](https://rpm.org/docs/latest/manual/tags) identifies these
  special records as public-key storage. The review is bound to the exact
  recorded key fingerprint, version and PURL. It is not a general software
  exemption based on the package name.

Both reviews expire on 2026-11-29. Their `license-raw-v2:` fingerprint hashes the
ordered raw license-record array, using compact JSON with sorted object keys and
ASCII escapes. Values, parsed expressions, declaration type, URLs, evidence
paths, annotations, unknown fields and every array's order participate. Only a
license location's `layerID` changes to the fixed `sha256:<image-layer>` marker,
after its original value passes strict SHA-256 syntax validation. Missing or
malformed layer fields cannot inherit that identity. This prevents a rebuild of
the same package declaration from requiring a fresh legal review merely because
the RPM database was written into a different image layer.

The exact package PURL remains the exception scope. The checker retains the
original records, including every layer ID, in `rawLicenseRecords`, and their
independent full-record digest in `rawLicenseRecordsSha256`. Image and SBOM
hashes continue binding that evidence to the scanned artifact. The expression
remains `UNKNOWN` when no parsed expression exists; approvals appear under
`waived`. This does not rename a license, remove a package or permit all unknowns.
The earlier `license-raw:` identity is replaced without a compatibility path.

A missing record, changed declaration, additional unparsed record or different
evidence location cannot inherit the review. A recognized declaration alongside
an unparsed declaration remains blocking until the complete set is reviewed.
The reviews originate from the retained ARM observation. The `libtool-ltdl`
PURL remains specific to its aarch64 version and source RPM. The public-key
record remains specific to its exact key fingerprint and architecture-neutral
PURL. Equivalent declarations at those exact scopes can reuse review after a
layer rebuild; a different package architecture, version, source or key cannot.
The canonical release SBOM must still establish the actual observed package
identity and complete declarations before any exception applies.

This policy controls review of declared metadata for this application. It does
not establish fulfillment of license notices, source availability or other
distribution obligations. Package-context reviews must inspect the actual
canonical inventory and relevant upstream terms before adding or renewing an
exception.
