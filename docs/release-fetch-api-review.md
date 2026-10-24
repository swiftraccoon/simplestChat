# Prepared-receiver release fetch: affected API review

This prospective record covers the complete new `release fetch` controller and
private reuse of the actual local verification owner. The local
[coding standard](coding-standard.md), [commit standard](commit-standard.md) and
[54-entry checklist inventory](rust-api-checklist.md) apply. It does not audit
unaffected code or authorize an installed receiver/caller/bootstrap switch.

The reusable local verifier retains the applicability decisions recorded in
[fresh verification](release-fresh-verification-api-review.md) and
[cached verification](release-verification-api-review.md). This change extends
those decisions only to the concrete second controller operation:

- **C-CUSTOM-TYPE, C-NEWTYPE, C-STRUCT-PRIVATE, C-VALIDATE:** explicit artifact and
  run identities retain distinct validated types below `2^53`. The controller's
  numeric selectors use the reference's first-nonzero ASCII syntax, at most
  sixteen digits. Host, user, port and identity/path selectors are validated
  before use. Private `VerifiedRelease` is constructed only after the actual
  fresh metadata and four verifier results; its internal full envelope supplies
  the SSH frame. Saved JSON and the standalone four-key result do not create it.
- **C-SERDE:** passive reports and the established sixteen-field receipt
  projection serialize useful contract data. The live owner, group, File/Dir,
  credentials and grant are not serializable/debuggable authority. Unknown
  receipt fields remain private; output includes only the established projection.
  Frame Unicode escaping preserves original ASCII/numeric lexemes rather than
  parsing arbitrary extended integers through a lossy generic JSON number.
- **C-RW-VALUE:** notice output retains the checked by-value generic writer and
  borrowed stdout-FD adapter. The SSH adapter owns heap read buffers and stable
  write cursors across pending reads/writes; a cancelled individual poll does
  not discard a frame or close another peer. There is no raw-FD adoption, unsafe
  native wrapper, generic process framework or arbitrary command/flag API.
- **C-GOOD-ERR, C-FAILURE, C-QUESTION-MARK:** ordinary input/transport/policy
  failures are typed private causes. Public errors are fixed and omit tokens,
  grants, raw remote output and native diagnostics. The first cause remains
  recorded through all peer finalizers. Legacy cleanup/journal report classes
  can describe later uncertainty without clearing that primary. Exit status is
  authoritative over an earlier complete or partial report. Syntax failures are
  usage2; parsed selector/key/path validation returns the original fixed
  three-field `invalid_options` report1 after known peer closes. Invalid new
  native N/M budgets remain usage2. Other native operational causes are
  deliberately coarser than every Python FetchError string.
- **C-DTOR-FAIL, C-DTOR-BLOCK:** exact group retirement precedes one consuming
  reap. Natural protocol EOF/current SSH status and forced group cleanup remain
  distinct. Final negative journals use the same original namespace/end/signal
  owner after a primary refusal, without clearing it or granting body authority.
  Writes, fsync, readback, original namespace and checked close are explicit;
  all known peers receive finalization even if one fails. Drop is fallback.
- **C-SEND-SYNC, C-COMMON-TRAITS, C-DEBUG:** one dedicated two-worker runtime
  drives the original signal registration. Private Rc custody stays on the
  calling operation thread; passive selectors may clone, live resources do not.
  No speculative Send/Sync/Clone/Default or Debug exposes private credentials.
- **C-EXAMPLE, C-CRATE-DOC:** the facade documents dedicated synchronous runtime
  use, required explicit N/M, statuses, host/bootstrap prerequisites and unknown
  remote outcome. A harmless help example requires no tools or services. The
  original clock includes setup and actual runtime/umask teardown; blocked
  kernel read/write/close/reap/random calls remain outside a hard wall bound.
- **C-PERMISSIVE:** no new dependency edge/package/native TLS dependency is
  introduced. The existing scoped rawzip/httparse/getrandom/runtime notices
  remain available. `release fetch --licenses` delegates the same exact local
  verifier notices. External maintained OpenSSH is a platform prerequisite,
  not a copied/linked dependency or authenticated target Ops distribution.
- **C-STABLE:** the public Rust surface is the dedicated `run` entry point;
  private helper factoring gives no representation promise. The server's ten
  files/four subjects, Ops three subjects, unchanged Python receiver and default
  operational callers retain their established contracts.

The native accepted-path profile requires canonical absolute nofollow active
paths with UTF-8 spelling. Python's relative cache, explicitly empty cache as
CWD, ancestor-symlink identity and surrogate-path domains are not silently
claimed equivalent. Explicit empty is refused as supplied, never treated as an
omitted fresh selector. OpenSSH still consumes its identity pathname and trusted
configuration/known-hosts/agent state. Retained checks do not eliminate a
same-UID check/read race or authenticate a distributed executable.

Transport preserves exact fixed OpenSSH flags and the one remote command,
including existing sudo behavior. It sends no GitHub token through the SSH
process environment. A fresh short-lived grant is acquired only after actual
Ready and carried in bounded stdin. Stdin remains open until the final receipt.
The two reference response/trailing counters are independent; no additional
64 KiB aggregate policy is inferred. All six ASCII `bytes.strip` whitespace
bytes, including vertical tab, are accepted for the final suffix; non-ASCII
whitespace is refused. A receipt's settled/finalized claims alone establish
neither current transport success nor observed durable target writes.

One original whole N/finalization M budget is required. The fixed thirty-second
SSH tail remains inside the body; post-Ready API and receipt work cannot spend
it. The same owner carries an active protocol ceiling through every existing
body witness/hash/read checkpoint after SSH admission, including pre-window tool
checks. A final phase fence precedes tail settlement; no owner/end is replaced
and no primary error is cleared. Local final journals, output, signals/runtime
teardown and umask restoration remain inside the original whole end. No default 660/1080-second caller budget
is added or changed. Local curl timing/CA/wire qualifications remain those in
the fresh verification review and must be reconsidered before a caller switch.

The native/CLI/oracle fixtures below are inert and cannot prove real SSH,
crypto, TLS, authenticated musl distribution, actual host publication or
operational readiness. The separately executed free-KVM proof supplies runtime
evidence after publication; its result must be recorded independently. Existing
Python references remain unchanged while this evidence is collected.

Validation on 2026-10-24 includes all 382 ordinary Ops checks (316 library,
59 CLI and seven documentation tests), the strengthened 73-scenario comparison
against the unchanged Python controller and receiver, and all 286 preserved
attestation reference scenarios. The whole comparison checks saved local
envelopes and final journals as well as stdout, remote publication and process
settlement. Workspace formatting, all-target/all-feature Clippy and private
Rustdoc passed; Linux GNU all-target Clippy, three CI automation tests, 33
CI-policy tests and ten container-input checks also passed.

Earlier failed trials remain separate from these results. They exposed a
consuming-wait retry, two invalid fixture assumptions, a missing test import and
a reference census that included concurrently generated output files. Ownership
and fixture repairs have explicit regressions; the census still verifies every
original source in its closed roots. Review also found cooperative witness reads
that could consume the SSH reserve. The same owner's protocol ceiling now
reaches those reads, and an actual-read virtual-clock regression verifies the
30-second tail and independent peer settlement. No earlier failure is relabeled
as a passing trial.

The companion manual KVM proof is separately reviewed and tested. Its source-built
Linux GNU, actual SSH and receiver results must be recorded after execution;
local fixtures and successful compilation do not establish those results or
authenticated Ops distribution. Existing deployment callers remain unchanged.
