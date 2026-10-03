# Initial security review records

These notes support the exact records in `exceptions.json`. They are not a
blanket acceptance of a scanner, package, directory or severity. The initial
review uses the supplied security assessment and the checked-out sources at
`2bd6d38215ddc4217b3f6cde0bd8fa30d93ce1cb`, with new scanner results reviewed on
2026-09-30. Records expire on 2026-11-29 and remain visible in gate summaries.
The repository owner is responsible for replacing the dependency or renewing a
specific assessment with current evidence.

## Public source fixtures

The initial Gitleaks findings were exact public values in isolated JWT/connection
tests, the RFC 6455 example handshake nonce, upstream mediasoup documentation and
tests, and two scalability-mode enum strings. They are not credentials enrolled
in any service. Each exception binds both the complete value and its original
file path. A different value in the same test, or that value in another file,
does not inherit the exemption.

The primary source scan uses neutral filenames so upstream default filename
allowlists cannot omit lockfiles, minified sources or binary extensions. The
private evidence map retains the original path and source digest. The reviewed
fixture path is translated to one exact neutral filename; a repository-wide
value exemption is never generated. The supplementary Git range scan retains
the original paths and includes redaction as well.

## GitHub Actions portability findings

Ten initial Zizmor findings have narrow records:

- Two informational `superfluous-actions` findings refer to the pinned Rust
  toolchain installer. The maintained local `act` image must bootstrap the
  repository toolchain; it does not have the same preinstalled tools as a hosted
  GitHub runner.
- Eight low-severity `self-repository` findings refer to six specific invocations
  of the maintained native-toolchain composite action and two same-revision
  reusable workflow calls. GitHub supports the newer
  `$/.github/actions/...` syntax, but the supported `act` 0.2.89 parser recognizes
  only `./` as a local action. The exact action source is obtained by checkout
  without persisted credentials. No step replaces the checkout with another
  repository before invoking the composite.

Each record binds the audit ID, hash of the actual action feature and complete
symbolic workflow location. An added use, changed action reference or moved
step requires review. The two high-severity `GITHUB_ENV` findings were corrected
by declaring the OpenSSL paths in workflow environment mappings. Missing
concurrency controls were also corrected; neither category is exempted.

The scheduled mutation job adds one exact `self-repository` review for its
native-toolchain invocation. It uses the same checked-out action and has no
intervening source replacement. Its separate mutation cache namespace and
private compiler outputs do not supply production release artifacts. The
existing local-action syntax rationale applies to this additional call site.

On 2026-10-03, two additional informational `superfluous-actions` findings were
reviewed for step 2 of `security-fast` and `security-deep` in `security.yml`.
The actual local fast job at `984c4d8` failed before its shared checks because
`rustup` was absent; both jobs previously invoked it directly. The existing
`dtolnay/rust-toolchain` action at
`6bed0761d98439e5a578e2877258200ad565ba87` bootstraps missing rustup, exports its
binary path and installs the explicitly selected Rust 1.98.1 toolchain. Its
bootstrap uses the official HTTPS rustup installer; this review does not claim
that the installer bytes are independently checksum-pinned. The two records
bind the action reference and each exact job location, retain the 2026-11-29
expiry and leave the shared checks blocking.

## Rust dependency health

The audit distinguishes these health warnings from vulnerability-class results:

| Package | Advisory | Review basis |
| --- | --- | --- |
| `atty 0.2.14` | RUSTSEC-2024-0375 | Unmaintained terminal detection in native build dependencies |
| `atty 0.2.14` | RUSTSEC-2021-0145 | Windows handle-alignment issue; supported production target is Linux |
| `instant 0.1.13` | RUSTSEC-2024-0384 | Unmaintained target-specific timing dependency |
| `paste 0.1.18` | RUSTSEC-2024-0436 | Unmaintained build-time procedural macro dependency |
| `rand 0.7.3` | RUSTSEC-2026-0097 | Narrow custom-logger/reseed interaction; no affected production path was identified in the supplied assessment |

The gate runs against the current advisory database and permits only the exact
advisory/package/version triples. Another advisory for the same crate or another
version fails. Warning-class findings remain counted, rather than being hidden
with a global command-line ignore list. These reviews do not establish that an
unmaintained dependency is safe indefinitely.

The duplicate budget is an explicit inventory of the reviewed Cargo graph.
`cargo deny` also checks licenses, allowed source origins, wildcard dependencies
and the forbidden bundled-OpenSSL crate. Duplicate reductions pass; new groups
or versions require a focused budget review. No dependency subtree is ignored.
