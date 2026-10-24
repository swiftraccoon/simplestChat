# Fresh local release verification: affected API review

This prospective record covers the complete new `release verify` operation and
private sharing with cached verification. The local [coding standard](coding-standard.md),
[commit standard](commit-standard.md) and [54-entry inventory](rust-api-checklist.md)
apply. This does not retrospectively audit the workspace or switch a caller.

- **C-CUSTOM-TYPE, C-NEWTYPE, C-STRUCT-PRIVATE:** explicit artifact/run selectors
  use distinct private types and positive identities below `2^53`. Fresh API
  metadata is validated before becoming the existing passive nine-field value.
  No JSON value creates an original File/Dir/tool/process owner or freshness.
- **C-SERDE, C-VALIDATE:** two maintained GH API calls bind exact artifact, digest,
  same-repository successful main-push CI and attempt. Both cached and download
  branches then perform the four existing fresh cryptographic verifiers. The
  exact four-key result contains revision, directory and image/manifest digests;
  it supplies no installed-binary, remote-host or deployment authority.
- **C-RW-VALUE:** the limited license writer takes a generic writer by value and
  writes through a checked borrowed stdout descriptor. Download and ZIP readers
  keep original nofollow File owners; no raw-FD adoption or unsafe parser is
  introduced. Fixed heap buffers and cursors survive pending read cancellation.
- **C-FAILURE, C-GOOD-ERR, C-QUESTION-MARK:** usage2 and operational1 preserve fixed
  public errors. Raw statuses, GH/curl bytes, tokens and grants remain private.
  First cause precedes later cleanup; recoverable EINTR resumes the original
  stream/cursor so the next original owner check can classify actual signals.
  The final exit status invalidates all earlier complete or partial output.
- **C-DTOR-FAIL, C-DTOR-BLOCK:** every admitted known peer receives its explicit
  settlement/close attempt before positive completion. API header retirement is
  a distinct private role: an actual complete302 with one validated Location
  and fully written/closed stdin permits planned unread-body retirement. It
  does not infer success from cleanup EOF or a subordinate exit. Blob success
  requires actual0, natural streams, exact bytes/digest and original-FD sealing.
- **C-SEND-SYNC, C-DEBUG, C-COMMON-TRAITS:** a dedicated two-worker runtime drives
  the same original signal owner. Private Rc custody stays on its operation
  thread. Useful traits are derived for passive data only; no speculative
  public Clone/Default/serialization makes live authority.
- **C-EXAMPLE, C-CRATE-DOC:** the public facade documents its dedicated synchronous
  runtime precondition, required positive N/M with `0 < M < N`, statuses and
  cooperative limits. A harmless help example exercises routing without tools.
  Entry time includes setup; one final fence follows Owner/Signals/runtime
  teardown, checked output and umask restoration. Blocked kernel I/O/reap/random
  latency is outside a hard wall guarantee.
- **C-PERMISSIVE:** direct `httparse =1.10.1` and `getrandom =0.4.3` use the existing
  locked versions. The measured normal Linux musl graph grew from 58 to 59: only
  httparse is newly reachable; getrandom was already reachable. No package
  version/checksum changes accompany these direct edges. The authenticated
  httparse crate SHA-256 is
  `6dbf3de79e51f3d586ab4cb9d5c3e2c14aa28ed23d180cf89b4df0454a69cc87`;
  its exact MIT notice is 1063 bytes, SHA-256
  `391a5396cec6230bfabd4ef4eb2350eb895bc5efce377a2218f5702ed020d3e3`.
  The getrandom crate SHA-256 is
  `300e883d756b2e4ec94e02791f39b04b522276138852cfc41d9fb7e904106099`;
  its MIT notice is 1130 bytes, SHA-256
  `523a42c25d245dde9c015f882cec7f4555aad883382a6cf19b4b7d9b2cd5419b`.
  Exact embedded notices are delivered by `release verify --licenses` alongside
  the unchanged rawzip notice. This is a limited operation notice, not a complete
  inventory. External curl is a maintained platform prerequisite, not a linked
  TLS package, copied dependency or claim of identical Python CA configuration.
- **C-STABLE:** private implementation details carry no public representation
  promise. Server ten-file/four-subject and Ops three-subject contracts stay
  unchanged. Existing Python production references and callers remain available.

The native profile requires full flags once, ASCII decimal selectors and explicit
canonical absolute nofollow active directory paths with UTF-8 output spelling.
Non-UTF-8 active paths are refused through ordinary checked cleanup rather than
serialized with Python surrogate escapes. Cached output-parent is required
but ignored, matching the reference branch, and curl/token/URL roles are never
resolved there. Fresh extraction intentionally precedes the proven cached ZIP
comparison, so fresh has two ZIP passes while standalone cached has one.
Original extraction logical chunks are 1MiB, cached comparison 64KiB, and actual
stream reads stay at most 64KiB; allocations are bounded heap buffers.

Curl 8.4+ is retained and version/HTTPS checked only in the fresh branch. Raw
header spans preserve selected trailing/folded values that httparse normalizes;
only 100 is skipped as interim. API and blob fixed connection limits are 20/15s,
with whole role caps 20/240s clipped by the original N/M owner. No low-speed floor
is imposed. Curl does not expose Python's exact per-socket 15s read-idle clock: a
quiet blob gap beyond 15s can be accepted within 240s. This explicit native timing
profile must be revisited before any operational caller switch. Wire parser/CA
profiles are not claimed equivalent for every malformed HTTP response.

Validation on 2026-10-24 includes all 362 ordinary Ops checks (302 library,
54 CLI and six documentation tests), all three attestation comparisons against
the unchanged Python references (103 fresh, 111 cached and 72 preparation
scenarios), the workspace Rust quality gate, all
575 helper checks and native/Linux GNU all-target Clippy. The final test-recorder
repair distinguishes original handle lifetimes when a descriptor number and
inode are reused; an independent regression also refuses to credit a dropped
original with a later handle's checked close. Production close behavior is
unchanged.

A source-bound Darwin release build performed actual fresh acquisition of signed
artifact `12559686976`, from CI run `40091336563` at
`1e88240f388384fecce80ec620d970b781bc3d03`. The 304,559,886-byte ZIP matched
SHA-256 `9a5d1d97ccf54b9c59b1c42fdd63058aa44e7a460f120f7d824d2f5b70a49278`.
Fresh verification took 139.731 seconds and cached repetition 20.304 seconds;
both returned exit 0, empty stderr and the same exact four-key result. All
11 original files remained unchanged. Fixed role observers counted two API
calls and four distinct verifiers in each mode; only fresh mode resolved curl,
requested a token and performed two HTTP roles. Cached repetition succeeded
with curl removed from its search path. These functional timings include the
role observer's logging overhead, using GH 2.103.0 and platform curl 8.7.1.
A private copy with one changed image byte retained the same length and was
refused with exit 1 after two API calls, before any cryptographic tool ran;
the original verified inventory remained unchanged.
They establish neither capacity nor authenticated Darwin distribution, Linux
musl execution, host deployment or a switched operational caller.

Earlier failed trials remain separate from the passing executions. They exposed
Darwin OS-random/statvfs assumptions, zombie-only process-group signaling,
truncated download tails, late stream observations, and negative drain loops.
The corrections have focused regressions. A cancelled slow oracle run was not
a pass. The final comparator also records the native ASCII selector restriction
instead of silently accepting Python's leading-plus numeric syntax. Clippy
found a fixture loaded twice; one private shared test module resolves that
without a lint allowance. The older custody recorder then conflated two real
descriptor lifetimes; its independent fix retains exact-once checked closure.

The non-UTF-8 output regression exercises a real raw filesystem candidate on
filesystems that support it. Darwin's tested filesystem refuses such a rename
with EILSEQ before the operation; there the same actual metadata/verifier/cleanup
path uses a private test-only late output-spelling observation to exercise the
fallible formatter. That is not a claim of Darwin raw-name filesystem support.
Natural download stdout EOF commits its pending reference logical-read tail
before classifying curl's exit, so a truncated transfer retains those observed
bytes while remaining negative. EOF caused by retirement never commits a tail.
Every successful normal post-await stream event is fenced against its original
role end before bytes or EOF can authorize a write, independently of the larger
whole-operation end.
