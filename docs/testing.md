# Testing

Run commands from the repository root after [development setup](development.md).
Use disposable local services, never a shared or production database. Direct Cargo
commands require the development setup's static OpenSSL and AWS-LC environment,
including the matching AWS-LC Rust bindings. `cargo run --locked -q -p xtask -- check --rust` checks both
native prefixes before invoking Cargo.

## Fast checks

```sh
cargo run --locked -q -p xtask -- check
cargo test --locked --workspace --all-features -- --test-threads=1
```

`cargo run --locked -q -p xtask -- check` shares the CI quality gates: typed web lint, formatting,
source/helper tests, TypeScript/build, Rust Clippy, checked documentation, and
strict Python lint/format/types. Use `--web`, `--rust`, `--helpers`, or `--python`
for a focused group. It requires installed development dependencies and never
starts application services. Native tests
are deliberately separate from the source-quality gate.

The Rust workspace tests include the operations manifest policy checks and the
migrated quality-gate and local-launcher regressions. For tooling-only changes,
run the relevant crate without the server's native test graph:

```sh
cargo test --locked -p xtask -- --test-threads=1
cargo test --locked -p simplestchat-ops
cargo test --locked -p xtask ci::ops_artifact -- --test-threads=1
cargo test --locked -p xtask --features public-smoke ci::public_smoke -- --test-threads=1
```

The [local Ops producer](../ops/README.md#controlled-local-unsigned-producer) has
inert selected-tool and owned-filesystem regressions. Those tests do not run
Cargo as a real builder, link or execute a candidate, acquire a toolchain,
sign or install anything. Its production command is separately authorized by
an original-request pin and complete caller-selected source/curated-kit pins;
those hashes are not authentication or proof of complete compiler I/O closure.

The same focused group includes the [selected local Git source exporter](../ops/README.md#selected-local-git-source-export).
It uses bounded fake commands and tiny temporary Git repositories with isolated
configuration/templates, including a corrupted nested-tree/OID fixture. An
installed regular Git supporting the fixed no-lazy-fetch options is
required for those local plumbing fixtures. No clone, fetch, build, candidate,
engine or host operation runs. Export observations do not authenticate acquisition
or replace eventual trusted CI and Debian runtime evidence.

The [offline registry hydrator](../ops/README.md#offline-lock-bound-registry-hydration)
uses tiny inert gzip/tar archives and owned filesystem faults. Focused tests
check original lock/archive SHA binding before parsing, mature TOML depth and
duplicate refusal, complete gzip/tar framing, safe paths and extension budgets,
actual stored-byte Cargo checksums, normalized modes and independent inventory.
They never invoke Cargo, a compiler or imported crate/build-script code. Tests
also fence known cancellation/deadline outcomes and descriptor-budget unwind;
an explicitly authorized real cache extraction is separate from those fixtures.

The [retained Ops verifier](../ops/README.md#flat-retained-ops-verification) has
complete owned-file/fake-verifier transaction tests, selected inert Native `gh`
fixtures, same-length seal-mutation refusals, descriptor-budget checks and
virtual-clock/cancellation cases. These run no real attestation verifier, GitHub
API, trust-root acquisition, candidate or installer. Successful synthetic JSON is
explicitly a supplied observation, never cryptographic evidence. Actual online
interoperability, trusted Ops signing and Debian runtime evidence remain separate.

The same focused Ops group covers the
[hosted producer and signed-artifact acquisition](../ops/README.md#hosted-ops-production-and-signing).
Owned inert ZIP fixtures exercise full framing, CRC/stream termination, member
limits and rejected aliases. Fake and actual inert Native gh processes exercise
exact API identity, paired capture seals, cancellation, deadlines, cleanup and
original file witnesses through replay. They perform no live GitHub request,
signature verification, compiler build, candidate execution or installation.
Complete coordinator fixtures substitute inert source, registry and producer
stages and a tiny private catalog; the real coordinator still owns download
capture admission, archive/kit processing, copying, evidence assembly and final
replay. Production fixes those stages and the compiled catalog, with no runtime
override. These fixtures establish orchestration behavior rather than a cold
hosted build or the truth of synthetic producer observations.

Failure-only transport fixtures cover actual inert curl exit 28 with original
paired captures, closed timeout labels and refusal of diagnostic emission after
capture, close or deadline failures. Unknown text and neighboring private data
must not be published or promoted to a specific cause. These checks preserve
the primary child failure, successful-download validation and original limits;
they do not diagnose a live hosted network or establish archive availability.
The catalog regression retains the original musl archive hash and size while
rejecting the former locator, another mirror owner and another commit.

Rust installer fixtures include file-only manifests with explicit structural
parent directories. Those directories admit no undeclared files, links or empty
siblings; declaration-kind mismatches and missing declared entries still fail.
The pinned GNU `rust-std` empty packaging-directory exception is tested against
other components, siblings, payloads, child files and nonempty directory bodies.
The workflow policy tests separately check main-only signing, its required-job
dependencies and its exact three subjects. A successful hosted build and an
actual post-CI acquisition are separate interoperability evidence; Debian host
runtime and installation evidence are separate again.

The [authenticated Ops Debian runtime and installer](../ops/README.md#authenticated-debian-runtime-and-first-installation)
add delegated acquisition/verifier traces, original USTAR/member/source witnesses,
closed keeper receipts, final replay faults and the fixed first-install lock and
namespace tests. The source-projection tests reject extra imports, namespace
changes and identical-byte inode substitutions while retaining the original
checkout witnesses. Actual isolated Python import regressions place legacy
bytecode and native-extension siblings in the caller checkout and execute only
the closed projected keeper. Python keeper/guest fixtures use inert process and
VM adapters;
they do not execute the signed candidate or boot a guest. Borrowed regular-file
stdin tests preserve the original inode after pathname replacement and retain
caller ownership through process cancellation. Linux signal handling and the
actual Debian executable/first-install behavior require their respective Linux
tests and successful manual `ops-runtime.yml` run. The existing full application
VM workflow remains a separate suite.

Keeper help fixtures cover legacy/current/future global-help layouts while
retaining strict route, status, stderr and output-size checks. The compiled
`xtask ops-artifact runtime-diagnostics` leaf has separate pure/bounded-file tests:

```sh
cargo test --locked -p xtask --bin xtask \
  ci::ops_artifact::runtime::diagnostics::tests:: -- --test-threads=1
```

The reporter checks source identity, malformed/oversized/nonregular reports,
fresh output/readback, clamped signal-status compatibility and actual read-only
stdout refusal. It retains only one sanitized JSON after checked finalization;
`lastObservedCase` is not a claim about the first failed case. Original runtime
failure stays negative. The manual signed Debian run remains the actual guest
execution and first-install gate.

Operations automation runs the standalone Ops library suite with its default
dependency profile before the reference comparisons. On Linux this includes the
host, process and archive cases. This exercises the distributed capacity runtime
profile early; the workspace suite also checks the combined feature profile.
Both jobs select CPython 3.14.8 before fingerprinting or creating the controller
environment, keeping the manifest oracle's Unicode database consistent.
Ignored subprocess workers run only through their owning tests, and Python
comparisons have explicit invocations in the pinned controller environment.

The private VPS operation adds owned local transaction fixtures, actual inert
paired preparation processes, and bounded gzip/tar extraction. The paired tests
exercise lease-stream EOF, quiet-child cancellation, release ordering and the
first interrupt surviving a later signal during joined cleanup. The source
inventory CLI also runs with 196 selected files and a child-only 96-descriptor
limit. These checks do not execute a remote capacity workload.

Whole-controller parity requires the pinned Python environment. Operations
automation and its local-CI route explicitly run the otherwise ignored test;
missing dependencies fail instead of skipping it:

```sh
PYTHON_CHECK_ENV="$PWD/ops/ansible/.venv" \
  cargo test --locked -p xtask --bin xtask \
  ci::ops_artifact::capacity_vps::controller_tests::all_eight_whole_reference_scenarios_preserve_local_policy_and_explicit_transport_deltas \
  -- --ignored --exact
```

The unchanged Python controller runs with inert external adapters in all eight
scenarios. This checks local policy and documented transport differences; a
signed musl binary, Debian kernel/process behavior and a measured host run need
their own evidence. The Python controller remains available as the reference.

Linux operations automation also explicitly runs the host policy comparison:

```sh
PYTHON_CHECK_ENV="$PWD/ops/ansible/.venv" \
  cargo test --locked -p simplestchat-ops --lib \
  capacity_vps::host_tests::whole_host_policy_matches_original_python_oracle \
  -- --ignored --exact
```

That host test is compiled only on Linux. Direct automation on other platforms
reports its absence; `ci-local` runs the Linux workflow. The ignored inert child
used by the real process regression is launched only by its exact parent test.
Do not replace either oracle command with a broad `--ignored` invocation.

### Production release builder

The native [production build/export command](../ops/README.md#production-release-build-and-export)
has a closed whole-reference driver at
`ops/ansible/tests/release_build_oracle.py`. Its 26 scenario families contain
112 finite cases, using unchanged actual `build_release`, real source/image tar
bytes, extraction, hashes, lock/state policy, archive validation, publication and
failure finalizers. Only tool, clock and privileged-state observations are inert.
The driver starts no Docker, Git, network or host workload. Its bounded JSON keeps
fixture inputs separate from observed verdicts, commands and retained records.

Run the driver tests with the pinned operations Python environment:

```sh
ops/ansible/.venv/bin/python -B -m unittest discover \
  -s ops/ansible/tests -p test_release_build_oracle.py -v
ops/ansible/.venv/bin/python -B ops/ansible/tests/release_build_oracle.py \
  --scenario export-missing-tag
```

Its optional `--fixture-directory PATH` creates a fresh private directory of exact
source/image input archives, with relative names, SHA-256 and byte counts in the
result. These inputs are generated independently of the candidate's output.
Fixed test fixtures are limited to 2 MiB each and 64 MiB per export; these test-only
limits do not narrow the production builder's source/archive acceptance domain.

Operations automation and its local-CI route explicitly run the otherwise ignored
Rust comparison with that same environment. Missing dependencies fail; the test
does not silently skip:

```sh
PYTHON_CHECK_ENV="$PWD/ops/ansible/.venv" \
  cargo test --locked -p simplestchat-ops --lib \
  release::build::tests::whole_reference_scenarios_match_unchanged_python \
  -- --ignored --exact
```

Broad database `--include-ignored` runs must skip this exact test and run it through
the explicit pinned-Python automation route instead. They must also skip
`release::build::process::tests::signal_worker`, the ignored child fixture launched
only by its two owning native signal regressions. Run those ordinary parent tests
to exercise the child; a broad ignored run supplies no owning selector.
Offline comparison establishes
policy across build/export, preflight refusal, original selectors and tags, malformed
source/image archives, migrations, deadline exhaustion and retained failure records.
The comparison admits one explicit Docker-save transport difference: Rust omits
the reference's exact `--output` and known archive-path pair and requires the
original writer loan. Other argv, environment, capture and selector checks remain
exact; the Python reference is unchanged.
It does not prove process ownership, physical Docker effects or signing provenance.
Separate actual inert child tests cover concurrent pipe pressure, per-stream and
inspection budgets, signed status, descendants, cancellation, escalation, original
first failure and all-peer closes on Linux and macOS. Custody tests separately
exercise original descriptors, alias/inode changes and post-publication failures.
Archive-stream tests retain more than the diagnostic output limit through the
original inode, keep binary stdout out of logs, refuse overflow before writing
beyond the 8 GiB archive bound, and retain partial bytes after a failed child.
The bound-edge fixture starts its observation near the limit rather than writing
8 GiB. Write, metadata and close failures remain negative while peer finalizers
run; whole-operation tests independently compare the archive's seal and close
observation before publication. Non-archive command journals use null archive
fields rather than reporting a file close that did not occur.
Phase/deadline tests use the original virtual clock; chunk checkpoints cannot bound
a kernel read or filesystem operation that itself blocks indefinitely.

Source custody fixtures also inject a real extra context file. The direct test
refreshes all directory metadata observations first, then requires exact membership
to refuse it independently of those seals. A separate extraction fixture inserts
an unrecorded file before initial sealing; fresh directory metadata cannot admit
it either. The whole-operation case before sealing requires no build or save;
the case injected during the inert build requires no save. Neither may publish
the release manifest.
Cleanup retains the unknown inode while closing every admitted original tree
directory handle. The source ledger's explicit limits are 100,000 logical entries
and 16 MiB of summed normalized relative-name bytes. It adds no source depth limit
or 8 GiB source-byte cap; the image archive keeps its separate existing profile.

Close observations include a test-only admission generation as well as the live
descriptor, device and inode. A later open can reuse all three numeric values;
it cannot satisfy an earlier handle's checked-close requirement. An owned
single-test subprocess exercises actual descriptor reuse and confirms that a
dropped original receives no credit from a later explicit close.

Directory enumeration borrows a newly opened original cursor `File` on Linux.
The portable macOS iterator has an additional opaque libc cursor whose automatic
`closedir` return status is unavailable. Tests of explicit successful closes cover
the admitted original `File` handles, including their cursor, rather than asserting
an unobservable library-cursor close result. Neither membership enumeration nor
directory metadata seals remove the same-UID check-to-read race during Docker's
pathname reads. These mutation fences are not an immutable-context or complete
compiler-input-closure proof.

Metadata fixtures exercise the existing strict Ops JSON profile through the
builder's actual decoder and private guard policy. Valid image/state shapes remain
accepted with ignored integers beyond `u64` and finite `f64`, preserving raw
4300-digit integer and finite float spelling. Cases include 4301-digit refusal,
underflow exponents, nonfinite/overflow values, Unicode scalar and paired-surrogate
strings, lone-surrogate refusal, decoded duplicate keys and distinct bool/null.
Depth 128 and 100,000 total values include ignored containers and fields; the next
container or value is refused. The image fixture includes an ignored string just
over 1 MiB, which must pass the caller's 2 MiB inspection cap, plus exact-cap and
cap-plus-one checks. Actual protected-file cases retain the separate 64 KiB
benchmark and 16 KiB persistent-state encoded bounds.

The metadata adapter accepts UTF-8 strings and rejects raw NUL or a leading BOM,
matching the reference's already decoded string boundary rather than generic
JSON byte encoding detection. Escaped `\u0000` remains valid. These tests document
the explicit Rust profile; they do not establish full Python acceptance parity
for deeper or otherwise out-of-profile ignored metadata. The Python reference
remains unchanged.

The focused budget fixtures cover export selections 60, 139, 140 and 300 seconds.
The whole export end remains `N + 60`, verification reserve and cap are both
`v = min(140, N)`, and the complete 40-second tail remains unchanged. At virtual
entry zero, endpoints are absolute seconds since that original entry:

| N | Whole end | v | Setup end | Setup body end | Active end | Active body end |
| --- | --- | --- | --- | --- | --- | --- |
| 60 | 120 | 60 | 46 | 20 | 46 | 20 |
| 139 | 199 | 139 | 46 | 20 | 46 | 20 |
| 140 | 200 | 140 | 46 | 20 | 46 | 20 |
| 300 | 360 | 140 | 120 | 30 | 206 | 180 |

Setup and active columns are independent admission probes at entry, not budgets
to add together. The setup body includes its 30-second query cap. Repeated queries
reuse the saved setup end; elapsed setup consumes the same absolute active body
ceiling. After the initial active body ceiling plus its 26-second child/pipe tail,
the verification matrix is:

| N | Verification start | Verification end | First query end |
| --- | --- | --- | --- |
| 60 | 46 | 106 | 76 |
| 139 | 46 | 185 | 76 |
| 140 | 46 | 186 | 76 |
| 300 | 206 | 346 | 236 |

Each verification query has a 30-second cap and reuses its original phase end,
retaining 26 seconds for child/pipe settlement and 14 for file finalization.
The fixtures also refuse late setup and exhausted-body admissions, clamp cleanup
to the original whole end, and preserve an already observed failure through later
cleanup exhaustion. Interrupt/terminate cases record typed observations in the
virtual owner; they do not deliver OS signals. Actual SIGINT/SIGTERM behavior is
covered by the separate native subprocess regressions.

Actual Ubuntu build and exact-image export must also fit the documented workflow
budgets, preserve admitted output inodes and pass the existing server archive
profile. Unsigned output is insufficient receiver authorization. The separate
signer must still publish exactly the existing ten server files. The manual
workflow remains dispatch-only/read-only and retains failed evidence without
turning failure into success; its policy tests also pin separate native preparation,
the compiler bootstrap and the unchanged 75-minute job limit.

Rust tests include native media checks; database tests are opt-in. Serial
execution reduces UDP port-allocation races. Web tests use mocked browser APIs;
the web build runs TypeScript checking and produces the production UI.

Run the browser unit suite with `npm --prefix web test`. It uses Node 26's native
TypeScript support and `--experimental-test-module-mocks`; the package script
supplies that flag. A focused invocation from the repository root is:

```sh
node --experimental-test-module-mocks --test web/tests/chat-store.test.mjs
```

`web/tests/native-fixtures.mjs` imports actual source modules. Its small resolver
handles the browser's extensionless imports and unit-test stylesheet no-ops;
Node owns module linking, evaluation and dependency mocks. Each fixture captures
its injected browser globals in its own module instances, so concurrent sessions
remain independent. Tests may import unmocked dependencies normally. The compiler
separately checks the strict browser project and positive/negative type-contract
fixtures; native type stripping performs no type checking. Module mocking remains
an experimental Node API, so use the pinned Node release when reproducing CI.

Credential-continuity tests use bounded validation futures to distinguish
database unavailability from revocation and exercise expiry, drain, notification
loss and grace-registration races. Browser unit tests cover scheduled HTTP refresh
and same-socket renewal retries, deadlines and retirement by newer authentication
actions. These do not constitute a database-outage load test; the ignored
database-backed renewal test exercises successful real validation.

Room-control tests cover write ordering, sender/permission changes and caller
cancellation. The ignored persistence test uses a row lock in the disposable
database to check chat/state access during blocked SQL and publication after
the requesting task is cancelled. It is a concurrency regression test, not a
database-outage or throughput benchmark.

### Runtime secret exposure regressions

The `runtime_canary` Rust tests submit unique inert values through the real
in-process Axum router and signaling dispatcher, then inspect application
observations. They complement the Gitleaks detector self-test: a working secret
scanner does not prove the running application keeps credentials out of logs.

```sh
cargo run -q -p xtask -- test-postgres \
  cargo test --locked --lib runtime_canary \
  -- --include-ignored --test-threads=1
```

The database case registers an owned account, checks accepted and rejected
password login, reads its profile, issues a WebSocket ticket, rotates its refresh
cookie, rejects malformed credentials and an absent invitation, then checks
logout revocation. Submitted canaries and issued access, refresh and WebSocket
credentials must remain absent from real tracing JSON, the authenticated metrics
response, the enabled media snapshot, request URIs and URL-bearing response
headers. Intended authentication bodies, refresh cookies and profile data remain
observable only to their test client; they are not mistaken for logging leaks.
Cleanup names only the account created by that fixture.

The dispatcher case admits an owned participant, delivers a chat message and
denies a subsequent message after membership removal. It checks the actual reply
channel, membership event and successful/failed diagnostic operation records,
then verifies that names, messages, supplied passwords and reconnect credentials
are absent from tracing, metrics and the real diagnostic JSONL recorder. Recorder
shutdown must flush successfully. Both capture buffers are capped at one MiB;
overflow remains a test failure even if a logging layer swallows its write error.
HTTP response collection is capped at 512 KiB. Positive event assertions prevent
an empty capture from passing.

The subscriber follows request-future polling across awaits without changing the
process-global subscriber. Capture uses the default production info-level JSON
filter, and the guards check complete values plus standalone URL-form and Base64
encodings. These tests cover the selected request futures and exports; they do
not cover uninstrumented detached tasks, arbitrary partial/nested encodings,
browser storage, PostgreSQL/proxy logs or external artifact upload pipelines.
No network client, deployed target or existing database is used. Ordinary Rust
tests run the dispatcher and capture checks; CI's disposable database suite also
runs the ignored authentication case.

### Python automation

The automation job checks the actual branch-tip subject, including a pull
request's head commit, with `cargo run --locked -q -p xtask -- ci commit-subject`.
It accepts only a complete 40-hex `COMMIT_REVISION` and captures the fixed
`git log -1 --format=%B REVISION` command privately, with a 30-second deadline,
1 MiB output limit and 64 KiB first-line limit. The first LF ends the subject;
NUL removal and C-locale byte whitespace rules match the unchanged POSIX
`.githooks/commit-msg` hook, including non-UTF-8 input. Diagnostics never echo
the message. Known Git failures and interrupts retain their status; grammar or
bound failures return 1, and invalid invocation/revision returns 2. Owned
command cleanup covers descendants remaining in its group.

The same pinned compiler step now precedes this check, before automation cache
lookup. Rejected CI subjects therefore pay toolchain setup and any cold xtask
compilation first. The subject step took 50.6 and 59.4 seconds in hosted runs
`40243312977` and `40248975462`; these include cold bootstrap rather than only
grammar validation. Local Git commits continue using the bootstrap-free POSIX
hook. Byte-oracle and inert Git-process fixtures check these boundaries without
installing tools or contacting a host.

Use Python 3.12 or newer and the pinned controller/checking environment:

```sh
python3 -m venv ops/ansible/.venv
ops/ansible/.venv/bin/pip install -r build/python-requirements.txt
cargo run --locked -q -p xtask -- check --python
PATH="$PWD/ops/ansible/.venv/bin:$PATH" \
  cargo run --locked -q -p xtask -- ci automation --checks
```

The shared `ci automation --checks` command preserves quality, syntax and
standalone Ops checks. Its complete suite uses two current discovery children
plus one frozen maintenance-class child. Disjoint module selectors come from
the sorted, alternating flat inventory. The unchanged release-playbook module
has twelve tests: its ten release cases and one public-selection case run against
current YAML; its one legacy maintenance case runs unchanged against explicitly
byte-pinned old YAML in a private runtime copy. No Python source or loader is
added. Current native caller policy is checked by Rust YAML contract and mutation
tests. The module's full content pin prevents future classes from being silently
omitted by this explicit class split.

With inherited `DISPOSABLE_TEST_DATABASE=1`, there is one current discovery
child with explicit module/class selectors plus the read-only legacy child.
Database-mutating cases still share one current process. Unsupported names,
packages, symlinks, byte-pin changes and observed inventory changes refuse.
The separate workflow database checks keep their existing owners. Direct raw
`python -m unittest discover` is no longer the complete current suite: it would
apply the frozen old-launcher assertion to the new native caller. Focused
unchanged modules may still be run directly; use the Rust driver for the complete
current/reference contract.

An ordinary test failure is retained while the peer finishes naturally. Spawn,
I/O, cancellation and deadline failures settle every admitted child before the
operation returns. One 600-second unit-suite interval starts before preparation,
reserves 30 seconds for finalization, and spans all active children, runtime teardown,
replay, closes and the final signal check. The outer ten-minute job limit stays
independent and can expire earlier. Combined stdout/stderr is capped at eight
MiB per current child and one MiB for the legacy child, then replayed in fixed
role order after all settle. Success requires every
selected child and finalizer; group cleanup does not certify escaped sessions,
all SDK descriptors or a hard bound on blocked kernel I/O.

The cancelled `c8c04c4` job passed validation but expired at receipt creation; its
2,108 Python cases took 332.757 seconds versus 266.512 in the previous successful
run. On this Mac, unchanged sequential discovery passed the same 2,108 cases in
227.871 seconds. The Rust runner's 997- and 1,111-case processes passed in 110.052
and 131.246 seconds, with the exact same case-ID multiset and 17 existing skips.
The complete automation command, including quality, Ansible and native checks,
took 230.516 seconds locally. These measurements use the same pinned Python
environment with no database; they do not establish hosted timing or the
five-minute CI ceiling and one-minute target. Those measurements precede the
native caller/class split; direct discovery was that historical sequential
baseline and is not advertised as today's complete suite. Run focused inert ownership tests with:

```sh
cargo test --locked -p xtask --bin xtask \
  ci::automation::python_shards::tests:: -- --test-threads=1
```

The source gate runs Ruff's complete stable rule set, its formatter, and
basedpyright's `all` mode over every maintained Python file and type stub,
including tests. It rejects a new source directory until explicitly included.
The offline release-HTTP oracle package under `xtask/src/ci/release_http_policy`
is a maintained checked root; its exact historical `reference/` copies retain
the existing reference exclusion and are checked by byte pins and oracle tests.
Third-party `vendor/` sources and ignored private operator scripts are outside
this policy. ShellCheck, `jq`, and the Ansible dependencies are required for the
offline tests; the optional Compose renderer needs the Docker CLI, not a daemon.

`pyproject.toml` defines the policy. Mutually exclusive formatting/docstring
rules and standalone-module/unittest conventions have documented exceptions;
reviewed subprocess, CLI-output and transaction-complexity exceptions stay next
to the affected code. `Any`, unknown types, untyped arguments, unchecked casts
from decoded data, and test-wide typing exemptions are not used to pass the gate.
JSON boundaries validate decoded values before constructing typed records;
options and fixture state use dataclasses where appropriate. The narrow Ansible
stubs in `typings/` describe only the pinned APIs used here and retain `object`
for values that callers must validate.

CI uses this same gate. Set `PYTHON_CHECK_ENV` to another virtual environment
directory to select both its pinned tools and its Python import environment.
The checks do not install packages, contact deployment hosts, or start services.
Updating a host helper requires `build/deploy.py --install-helpers` once so that
the complete wrapper/module set is reconciled; subsequent releases retain the
prepared-helper hash checks and app-only replacement flow.

### Readiness and shutdown

After building the server, run the same process-lifecycle smoke used by CI:

```sh
cargo build --locked --workspace --all-targets --profile test
./target/debug/load_test bench shutdown-smoke --binary "$PWD/target/debug/simplestChat"
```

The smoke starts its own guest-only server on unused ports, checks `/ready`,
and verifies that a client-initiated close finishes cleanly with code 1000.
It then opens a separate connection, joins a room, and retires its reserved
process group while concurrently draining WebSocket messages. It requires the
temporary `serverRestarting` notice with reason `Server shutting down` (and rejects
terminal `roomClosed`), clean WebSocket close code
1001 and exit status zero within 20 seconds, including observed group retirement
and both output pipes. This is stricter than the retained JavaScript reference's
leader-exit timestamp. The smoke connects only to its own allocated loopback
ports and supplies a fixed guest environment without database or auth credentials.
Native tests separately cover drain races, stalled cleanup and readiness probe
failures. See [shutdown guarantees and limits](configuration.md#shutdown).

The Rust filter `boundary_properties` runs finite property checks without services:
2,048 seeded Unicode labels, 1,024 decorated ASCII identities, every signaling
operation's canonical round trip, 512 escaped-text examples, integer/envelope
boundaries, all 27 nullable-settings combinations, bounded limiter counters and
448 combinations of ticket lifetime and independent clocks. Inputs and case
budgets are fixed in the tests; ticket secrets still come from the real secure
issuer and are never printed. These tests check wire types separately from
handler authorization and message-size admission. They do not run a continuous
fuzzer, publish media, or replace the database and real-browser tests.

Mocked client checks cover restart versus permanent deletion, retained room/lobby
intent, public-draft isolation, bounded jittered retries, explicit retry and leave,
and stale or interrupted rejoins. With the UI built and
[browser tooling installed](../web/e2e/README.md), run the real restart check:

```sh
./target/debug/load_test bench restart-browser-smoke --binary "$PWD/target/debug/simplestChat" \
  --browser chromium --output "$PWD/results/restart-browser"
```

Use a fresh output directory each time. This check owns its local servers and
browser, verifies two guests rejoin without reloading, preserves a public draft,
and delivers messages before and after restart. Capture calls are intercepted
and must remain zero. CI runs Chromium; `--browser firefox` and `--browser webkit`
use those installed Playwright engines. Retained results include recovery timing
and browser/server exit status. They do not prove physical-device media recovery.

The Rust owner retains two sequential guest-server lifetimes and one Node browser
owner. Its inherited anonymous control stream admits exactly `stop-first`,
`start-second`, then `stop-second`, with one bounded canonical request and ACK at
a time. First-server exit 0, reserved-group retirement and owned-pipe completion
precede the start ACK. Node must independently observe both restart notices
before requesting the second server. It reuses the same selected binary, random
loopback ports and fixed guest-only environment; database/auth settings are not
inherited. The same captured signal owner covers every phase and final close.

Both installed Node and the repository-fixed Playwright module/runtime remain
caller-trusted. `--node /absolute/executable` selects an installed Node explicitly;
otherwise the captured PATH selects its first executable candidate, resolving
that selection once. There is no installation stage. The controller retains
stamps and SHA-256 measurements for five fixed first-party files: the restart
phase/channel, browser owner, lifecycle cleanup and WebKit owner. Final rehashes
are optimistic snapshots, without authenticated origin or protection from later
same-UID writes. They do not describe all Playwright/tool/runtime I/O.

`--output` is required, absolute, UTF-8 and fresh. The 0700 leaf retains its
`.creator` witness and 0600 artifacts: `outcome.json`, admitted `server-1.log` and
`server-2.log`, `browser.log`, and a validated `browser-receipt.json` when present.
Each server stream and Node stderr are bounded to 1 MiB; server logs concatenate
stdout then stderr after retirement, without an interleaving claim. The strict
Node receipt is at most 32 KiB and contains counters and selected observations,
not chat text, URLs, raw frames or dependency error messages. Native server
records explicitly separate exit, reserved-group signals, leader reap and owned
stream observations from the browser cleanup verdict. Duplex Control EOF and
explicit close are separately required; escaped descendants are outside these
observations. Unknown or failed cleanup cannot pass. Failed journeys can still
report successful browser cleanup.

The fixed scheduling budget is 75 active seconds plus 100 for finalization, with
an internal 175-second fence inside the unchanged three-minute CI step. Success
also retains the original strict elapsed time below 120 seconds. Readiness uses
20-second phase windows with 750 ms bounded requests; fragmented control frames,
ACKs and terminal EOF each have a two-second slot. An admitted transition keeps
its returned server owner even after cancellation. Node retirement starts
concurrently with that transition settling and any returned server retirement.
The conservative Node tail is 35 seconds grace, up to three Darwin inspection
paths of 2 seconds plus 5 seconds settlement, a 5-second terminal wait, two
5-second owned stream tasks and a possible 5-second guard fallback: 76 seconds.
The server tail is 20 + 21 + 5 + three 5-second tasks + 5: 66 seconds. At most one
server remains unretired; the tails overlap rather than adding, leaving room for
EOF and file/control final checks within the reserve. These are scheduling and
refusal bounds, not guarantees against OS, filesystem, reap or cold-build stalls.

Presentation is artifact-only. Exit codes are 0 for complete success, 1 for
failed/incomplete observations or unproven cleanup, 2 for invalid arguments,
124 for an observed deadline and 130/143 for observed INT/TERM. The first known
failure survives later cancellation. The saved outcome describes observations
before its own final publication/namespace closes and final clock/signal fences;
only CLI exit 0 confirms those final checks. It may retain a positive observation
when a later finalizer fails. This is not a durable cleanup receipt. The native
signal fences deliberately correct the original JS's generic cancellation exit,
and continued cleanup observation refuses late page errors or external requests
that the original could report after its last success check. Node checks elapsed
time before and after accepting a positive join/restart observation; the retained
reference tested its condition first and could accept an observation after its
phase window. Native Control EOF also has a post-read two-second fence, so an
already-ready read cannot bypass the timer when scheduling resumes late.
Byte-retained JS
oracles, virtual-time fake hosts, and actual inherited-FD integration with inert
guest scripts/fake Playwright cover source parity and ownership. They do not run
the real server or browser. The existing guarded seven-day `restart-browser`
upload keeps the artifacts; no hosted timing or runtime parity is claimed here.

Manually check leaving during recovery and a prolonged outage: after two minutes,
automatic retries stop and explicit retry remains available. Do not interrupt a
shared server without authorization.

To validate the server's actual diagnostic JSONL schema with the same owned
guest-only workflow, also run by CI:

```sh
./target/debug/load_test bench diagnostics-smoke --binary "$PWD/target/debug/simplestChat"
```

This additionally requires complete recorder coverage, successful `join_room`
and `room_lock_wait` records, and clean server exit. It retains a new private
`results/diagnostics-smoke.*` directory, including failure artifacts. The recorder
must already exist at readiness; its original private inode is retained through
positive writer retirement, then the typed Rust reader consumes a duplicate FD.
It does not use a browser, capture media or upload anything. See
[diagnostic interpretation](diagnostics.md#read-the-results).

Both commands accept `--output /absolute/fresh-directory` with a UTF-8 path; an existing output
is refused. The directory is 0700 and regular artifacts are 0600. The diagnostic
files remain `server-diagnostics.jsonl`, `server-diagnostics-report.json` and
`diagnostics-smoke.json`; bounded `server.log` appears on failure. Both modes
also retain `shutdown-retirement.json`; shutdown mode writes `shutdown-smoke.json`.
The reused directory owner retains its `.creator` witness. Core shutdown and
diagnostic report fields retain their reference meanings, with the named timing
correction above. Retirement facts cover reserved-group signals, leader reap and
owned streams; they do not prove absence of escaped descendants. Reports are
observations before final file/namespace closes; only command exit 0 confirms all
required final checks. They are neither durable receipts nor locks against later
same-UID changes.

Runtime presentation is artifact-only, with no stdout parity claim. Exit codes
are 0 for complete success, 1 for failed/incomplete observations or unproven
cleanup, 2 for invalid arguments, 124 for observed deadlines, and 130/143 for
observed INT/TERM. An earlier failure survives later cancellation. Native final
signal fences deliberately correct the references' generic cancellation exit
and shutdown's late-cancellation success. The original JS files remain references;
inert tests execute their byte-pinned copies and compare full core results.
Hosted CI retains both smoke directories alongside media evidence in the
existing guarded seven-day `media-smoke` artifact upload; ACT and cancelled
jobs keep the existing upload exclusion.

The entry scheduling budget is 240 seconds: 150 active plus 90 for finalization.
Stage slots are readiness 20, each connection/join 20, peer close 5, drain 20 and
stopped-recorder reading 30 seconds, all capped by remaining active time. Final
retirement includes a 20-second leader grace, up to three Darwin inspection
paths of 2 seconds plus 5 seconds settlement each, a 5-second terminal wait,
three 5-second stream tasks and a possible 5-second guard fallback (66 seconds),
plus 2 seconds per retained socket and final file/signal checks. The synchronous
reader runs in an awaited blocking worker with sticky abort checks; cancellation
never detaches it or skips its file closes. Readiness bodies are capped at 4096
bytes with redirects/proxies disabled; typed WebSocket messages/frames are 64 KiB
with 128 frames per socket. A clean close requires the complete close handshake
and protocol termination, rather than trusting a close frame alone. HTTP and
pending-connect library handles use their normal RAII cleanup; returned sockets
are explicitly closed. These are source scheduling/refusal bounds, not hard OS,
filesystem, reap or cold-build guarantees. The real server/network path remains
unmeasured by the inert oracle and fake-host checks.

### Opt-in macOS mDNS diagnostics

On an explicitly selected Mac, `MDNS_DIAGNOSTIC_E2E=1`, `TEST_ANNOUNCE_IP`
(an assigned IPv4 address) and a private `E2E_ARTIFACTS` directory enable:

```sh
cargo run --locked -q -p xtask -- ci mdns preflight --artifact-only
cargo run --locked -q -p xtask -- ci mdns snapshot --artifact-only
```

The complete controller retains the original C registration probe and JavaScript
as references. It reads `/sbin/ifconfig -a`, the fixed system mDNS plist, only an
allowlisted launchctl label, and the original narrowly scoped unified-log query.
Interface counts follow Node 26's BSD adapter: IPv4 records on interfaces with
both `UP` and `RUNNING`, including aliases; `LOOPBACK` determines whether a record
is internal, matching the pinned Node 26.10.0
[BSD adapter](https://github.com/nodejs/node/blob/v26.10.0/deps/uv/src/unix/bsd-ifaddrs.c).
Only bounded names/flags determine eligibility; optional metric/MTU/tail text is
not treated as address ownership. No address-range heuristic substitutes for
those flags. Preflight
then invokes the selected `/usr/bin/cc` with the original literal flags, and the
fresh probe with an assigned address and random UUID. The probe keeps its own
five-second registration deadline and explicit deallocation observation. C source
bytes are bounded, retained and rehashed around compilation/finalization; selected
tool metadata and paths are revalidated. These are optimistic local observations,
not authentication of a compiler, SDK or installed runtime.

Both modes preserve system-before-registration ordering and write only fixed
stages, numeric errors, interface counts and timestamps. The shared artifact
directory may contain browser reports; fixed `mdns-*-results.json` files use
exclusive anchored creation with mode 0600, without replacing prior evidence.
Raw command stdout/stderr each have a 512 KiB limit in a fresh private temporary
directory outside the upload directory. All admitted CLI groups, captures, source
and executable descriptors settle, and exact temporary cleanup is attempted,
before `passed` is computed. A selected temporary parent equal to or beneath the
physical artifact directory is refused, including symlink aliases.
Unknown entries or replacements prevent success;
foreign names are never recursively removed. The report is a prepublication
observation: its own final write/close, a later signal or the final deadline can
still fail. CLI zero is authoritative, not a durable successful-close receipt.

Artifact-only mode has a 55-second internal policy beneath the unchanged
one-minute CI steps. Only one CLI is active at a time. Each admission deducts
25 seconds for that child's settlement and five for files/final fences, then
clips the original 5/15/8-second command caps to remaining body time. The nominal
Darwin child tail is grace 1, inventory/settlement 7, terminal observation 5,
armed fallback 5 and parallel pipe completion 3 seconds. Original full preflight
caps including interface inspection sum 43 seconds; the 25-second active window
does not promise they all fit. Earlier completed commands release their slots;
short remaining time refuses with 124. Local mode without `--artifact-only`
keeps JSON stdout through the existing owned replay, reserves its 25-second slot
and uses a 90-second entry policy. These are scheduling/admission allowances,
not hard bounds on the OS, filesystem, reap, SDK compilation or cold CI startup.
The existing pinned Rust bootstrap precedes both workflow calls.
The caller directory is captured once: relative artifact/TMPDIR paths and PATH
entries use it, while the retained C source stays anchored in the workspace.

Ordinary command failures remain diagnostic observations and do not skip later
reads/probes when time and cleanup proof permit. Actual exit 124 does not imply
`timedOut`; a private closed-role Native record distinguishes observed timeout,
interruption, settled exit, group cleanup and pipe finalization. Native timeout,
INT and TERM return 124/130/143 as explicit corrections to the reference's
generic failure status. Known earlier failures remain primary. Supported input
adds bounded finite UTF-8 numeric JSON, finite ifconfig grammar/counts and safe
integer counters; malformed encodings, numeric overflow and surrogate-only
strings may refuse even if JavaScript could parse them. Unknown private fields
are omitted. C source is at most 64 KiB, an interface listing has at most 4096
headers/IPv4 records and 4096 bytes per line, names at most 64 bytes, log input
at most 32769 split lines and 4096 distinct codes per category, and the sanitized
record at most 128 KiB. The byte-retained actual JS/tests, fake compiler/system adapters,
virtual clocks and inert Native children cover source behavior; real mDNS, OS
snapshot, C compilation, registration and hosted runtime are unmeasured here.

## Disposable PostgreSQL and browser tests

The owned test environment is a Rust command: `cargo run -q -p xtask -- test-env`
wraps `test-postgres` (a disposable loopback cluster) around `test-server` (an owned
server with a clean configuration), and each runs alone too; `xtask/src/test_env/`
holds them. `cargo run -q -p xtask -- test-server` runs `target/debug/simplestChat` as it finds it, so
build first: a stale binary answers a new message type with "Invalid message
format". The disposable server also opens registration and raises two
per-address budgets for the five browsers a suite drives from one address
(`REGISTRATIONS_PER_IP_PER_HOUR=100`, `AUTH_REQUESTS_PER_MINUTE=300`); a
deployment keeps the defaults in [configuration](configuration.md).

CI's three controller database steps use a fixed typed selection:

```sh
cargo run --locked -q -p xtask -- ci database-check monitoring
RESTORE_CONTAINER_E2E=1 cargo run --locked -q -p xtask -- ci database-check restore
cargo run --locked -q -p xtask -- ci database-check migration
```

These retain the existing Python test patterns. The restore step first pulls the
same digest-pinned PostgreSQL 18.6 image and retains `RESTORE_CONTAINER_E2E=1` in
the workflow. Discovery runs literal `pg_config --bindir` with a 30-second outer
bound and 4096-byte stdout ceiling, admits one existing absolute directory, and
prepends it using `join_paths`. The optional pull has a 600-second outer bound;
the existing ten-minute automation-job deadline still applies. The environment
is captured once and threaded into preparation, cluster configuration and the
test child; no process-global environment is changed. Preparation transfers its
registered signals only after CLI group cleanup, then awaits the same cluster
owner directly without another Cargo build or nested harness. These checks do
not replace the Python suites' container ownership. Fake tools and inert children
verify the command/environment/cleanup boundary; hosted Linux PostgreSQL and
container runtime cost remain unmeasured until that integration runs.

With PostgreSQL tools on `PATH`, use the owned bootstrap:

```sh
cargo build --locked --bin simplestChat
cargo run -q -p xtask -- test-env \
  cargo test --locked -p simplestchat -- --include-ignored --test-threads=1
```

It creates a fresh loopback-only PostgreSQL cluster on port 15434, starts and
migrates an owned application server, then stops both after the command. It
refuses an occupied port and retains its private temporary directory for
diagnostics. Set `TEST_POSTGRES_PORT` when another local service owns that port.

The database commands select the server package. Run ordinary workspace tests
separately using the command at the start of this guide. The workspace also has
ignored tooling fixtures that require an owning parent or the pinned Python
automation environment; a blanket `--include-ignored` would launch them outside
that owner. CI includes database tests while skipping those three named fixtures;
operations automation explicitly runs both complete Python reference comparisons.
The trust-authenticated database is disposable, never a shared development DB.

Each harness test command owns a fresh process group. It retains the leader's
identity through final group cleanup, including when that leader exits normally
and leaves descendants, then reaps it. INT/TERM keeps the existing ten-second
test-command cleanup grace and returns 130/143; unproven group cleanup cannot
pass or hide an existing test failure. These signals use the shared numeric
process owner, without a `kill` command lookup. Groups do not cover children
that start a separate session or external resources owned by the selected test.

Startup signal handling is active before any setup or readiness helper starts.
The application, optional HTTPS proxy and TURN relay each retain a separate
owned group through bounded shutdown and reaping; `groupCleanupPassed` is
recorded separately from leader TERM/KILL attempts in server/relay receipts.
A zero leader status alone cannot pass shutdown. Readiness/version CLIs also
own groups and have outer deadlines; captured metadata is limited to 4096 bytes.
A normally exited helper's group is retired before waiting for final pipe EOF.
Cleanup continues through repeated interruptions; a pending INT/TERM still
prevents an otherwise successful harness from returning zero.

PostgreSQL daemon ownership uses `pg_ctl` and the newly created data directory,
separately from these CLI groups. Status 3 is a not-running observation; status
4 or a failed probe is uncertain, triggers an exact-data-directory stop attempt,
and cannot pass. A successful startup requires a final not-running observation
after shutdown. PostgreSQL detaches its startup child before writing its pidfile:
an unsuccessful, timed-out or interrupted dispatched start therefore stays
**unproven**, even if later probes all return 3. Cleanup makes bounded best-effort
status/stop retries over a five-second observation window, checked between
individually bounded helper calls; this is not a five-second total cleanup limit
or proof that a delayed detached child cannot remain. The original failure or
130/143 is preserved, the private diagnostics remain, and this path never claims
successful daemon closure. No executable-name or discovered-PID kill is used.


For browser tests, replace the Cargo test command with
`npm --prefix web/e2e test` after the [browser installation steps](../web/e2e/README.md).
Build the UI first; do not rebuild assets during a browser run.

### Browser runner

Maintained browser suites use the pinned `@playwright/test` runner. The Rust
`test-env` command owns PostgreSQL and the application; a runner fixture owns
each browser process and requires a clean observed exit. Existing `node
web/e2e/<suite>.cjs` commands remain compatible and select the same runner tests.
Importing a suite exposes its in-process `run` function without launching it.

```sh
E2E_BROWSER=firefox cargo run --locked -q -p xtask -- test-env \
  npm --prefix web/e2e test
E2E_BROWSER=chromium npm --prefix web/e2e run test:layout
E2E_BROWSER=firefox npm --prefix web/e2e run test:resilience
npm --prefix web/e2e run test:runner -- --list
AUTH_LIFECYCLE_E2E=1 cargo run --locked -q -p xtask -- test-env \
  npm --prefix web/e2e run test:runner -- --project=accounts-chromium --grep=auth-lifecycle
```

Projects group account suites under `accounts-<engine>`, media suites under
`media-<engine>`, and the bounded UI workload under `stress-chromium`.
Community, layout and resilience have their own engine projects. Each suite
keeps its explicit opt-in environment variable; selecting a group does not
enable its other scenarios. Passkey fixtures require Chromium. Layout and
resilience use intercepted local browser fixtures and own their frontend listener.
Real-backend Firefox media also requires an interface-owned `TEST_ANNOUNCE_IP`,
as described in the [browser guide](../web/e2e/README.md).

`cargo run --locked -q -p xtask -- media-ip` prints that address through a bounded
owned output process. `media-ip --github-env` instead appends only
`TEST_ANNOUNCE_IP=<IPv4>` to the captured existing `GITHUB_ENV` file. The file must
be absolute, regular, singly linked, owned by the current user, at most 1 MiB
including the new line, and not writable by group or other users. Existing
contents must be UTF-8 with LF termination and no NUL or CR. Symlinks, changed
identity and malformed files refuse publication. Retained descriptor checks
assume the caller exclusively owns that runner file; they are optimistic checks,
not a lock. A line can already be visible when a later close, signal or identity
check fails, so its presence alone does not certify successful completion.

Both modes capture the environment once and use one INT/TERM registration.
The unchanged Linux and Darwin parsers read only the default interface's route
and IPv4 listing. Each query has a three-second body timeout and a 64 KiB stdout
limit plus one overflow sentinel byte. Every acquired CLI group receives a
retirement attempt before its captured pipe's final EOF drain and explicit close,
including normal leader exit with inherited pipe writers. Successful output
requires positive group-retirement proof. The detector schedules two 25-second query slots
within 60 seconds; the standalone command schedules 90 seconds, including a
30-second output slot and final checks. These are admission/watchdog policies,
not hard OS, reap or filesystem timing guarantees. Known INT/TERM and timeout
outcomes retain 130/143/124. They do not prove closure of privileged or detached
processes outside the owned group. Cold compilation and hosted route/runtime
behavior have not been measured by the inert regression fixtures.

The five two-browser CI steps use
`cargo run --locked -q -p xtask -- ci browser-suite SUITE`, where `SUITE` is
`auth-lifecycle`, `auth-cross-tab`, `room-control-confirmation`, `media-continuity`
or `presentation`. This runs Chromium then Firefox through the existing owned
`test-server` harness, stopping at the first failure and preserving its status.
Use the same explicit suite opt-in, installed browser tooling, built application
and disposable database required by the individual suite. `RUNNER_TEMP` must name
an existing absolute directory; each browser retains the workflow's existing
artifact subdirectory there. The two media suites resolve one interface-owned
announcement before starting either browser. INT/TERM stops the active harness
and allows up to 45 seconds for its normal cleanup before terminating remaining
members of that owned process group; an interrupted plan never starts the next
browser or publishes successful coverage.

Projects have bounded timeouts: accounts and layout three minutes, resilience
two minutes, and community, media and stress ten minutes. `E2E_RETRIES=0` is the
default; values `1` or `2` retry the complete test with fresh fixture ownership.
The community membership journey remains serial, with its individual checks
reported as runner steps.

`E2E_ARTIFACTS` selects the private report directory; the Rust harness sets it
for owned-service runs, and standalone runner commands create a fresh temporary
directory. Reports include `runner-results.json`, `runner-results.html`, a safe
`step-trace.json` timeline, each suite's bounded reports and redacted screenshots,
and `browser-lifecycle.json` shutdown evidence beneath `test-results/`. Directories
are private and report files use mode `0600`. Raw Playwright traces, video,
automatic screenshots and action error logs are disabled because account and
media API parameters can contain credentials. The step timeline retains fixed
scenario names, timing and outcomes. Existing diagnostic, canary, soak and
performance measurement commands retain their standalone execution paths.

UI stress checks its final report verdict inside the bounded-workload runner
step. A completed negative report fails that step even when cleanup succeeds.
The first failing traffic task and otherwise opaque sampling operations retain
fixed diagnostic codes; raw exception messages and call logs are omitted.
Report completion does not mean every workload assertion ran: use its retained
phases to distinguish completed checks from work abandoned after a failure.

The WebKit follow-speaker reload issue remains open as of 2026-10-24 with
Playwright 1.64.0 / WebKit 27.2 (r2370). A fresh run of the complete original
community journey passed 44 checks with the existing Chromium-only measurement
skip, including the follow-speaker reload assertions. The single attempt took
188.335 seconds including harness setup and cleanup. Browser and server exits
were clean, and the disposable database was stopped. Application sources matched
release `1e88240`; the retained assets identified `6a3acc8`, and copied binaries
were bound by their source/build record and hashes. No assertions, error filters,
retries or scenario steps were changed.
This observation did not reproduce the intermittent failure or establish a fix.

Earlier bounded reload probes reproduced two fetch access-control `pageerror`
events, including with the push fixture removed. A fresh credential-free control
loaded no application code: three native fetches returned 200 before navigation;
during a held document replacement, three explicitly caught rejections produced
three access-control `pageerror` events and zero DOM error/unhandled-rejection
events. Browser and server exits were clean. This supports the mechanism described in
[Playwright issue 43120](https://github.com/microsoft/playwright/issues/43120);
the relationship to the application's intermittent reload failure remains an
inference. Zero-page-error assertions remain enforced, with no ignored category
or application workaround.

### Passkey verification

Run native WebAuthn ceremonies with Chromium's owned virtual authenticator:

```sh
PASSKEY_E2E=1 cargo run -q -p xtask -- test-env \
  node web/e2e/passkey.cjs
```

The opt-in helper uses `http://localhost` as the browser origin and fixes the RP
to `localhost`; the HTTP bind and database remain on loopback. The suite exercises
resident-key registration and usernameless login against the actual verifier,
including rejected email selectors, wrong handles, changed signatures, replay,
stale/equal counters and a natively signed assertion from a different origin.
It also verifies that an otherwise functional nonresident credential cannot enter
the discoverable flow. A separate finite scenario registers three accounts through
the actual UI in one persistent virtual authenticator. It checks distinct UUID
handles, matching backend identities and three coexisting resident credentials,
then signs in through the UI with all three available. To verify each account
deterministically, the fixture temporarily removes the other two credentials from
that owned authenticator and restores them from memory, preserving current
signature counters. Browser request options and assertions are not rewritten;
the test checks modal mediation and an empty credential allowlist at the native
API boundary, plus the resulting backend and displayed account identities.

Credentials remain in memory and the disposable database; reports contain only
outcomes. The virtual authenticator selects credentials automatically, so this
proves coexistence and account routing, not a visible multi-account chooser.
Native Bitwarden/Keychain selection and physical authenticator behavior still
require device testing.

The companion management suite exercises fresh proof, backup-key enrollment,
last-sign-in-method protection, session revocation and recovery using native
credentials and owned disposable accounts. Its removal journey verifies that
password proof stays available in the confirmation and deletes the selected key
without opening a passkey prompt. A passkey-only replacement journey keeps one
authenticator throughout, requires fresh proof and a saved-recovery-key
acknowledgment before a separate registration gesture, then verifies the new
credential signs into the same account without enabling a password. It also
checks that the old server record and sessions are retired. Its session-management
checks create two additional sign-ins for that owned account, remove one through
Account and then sign out all others, and verify HTTP rejection and live WebSocket
closure while the current session survives. Finally it signs out the current
session and verifies that refresh no longer restores it. Passkey-only Account
controls must retain invitations, session management and forget-device actions:

```sh
PASSKEY_E2E=1 cargo run -q -p xtask -- test-env \
  node web/e2e/passkey-management.cjs
```

Run `cargo run --locked -q -p xtask -- ci native-dtls` after configuring the pinned OpenSSL installation
to exercise the worker's DTLS closure reasons, media warning context and native
bandwidth constraint handling. It builds
a separate temporary worker test target, leaving Cargo's production archive untouched. Paired native
transports cover orderly shutdown, fatal errors, fingerprint/SRTP failures and
handshake timeout; the orderly-close case also checks warning-level output.
Fake-clock media cases verify inactivity/activity timing, bounded private
identities, tuple-history expiry/reuse and warning coalescing, including timer
cleanup and a finite burst's delayed summary. They do not send network probes.
The bandwidth cases exercise returned target/pacer/probe work, learned estimate
preservation, bound changes, synchronous observer reentry and bounded discovery
for unmet video demand. They cover offline/idle admission, repeated allocation
changes, probe spacing, estimate preservation without feedback and bitrate caps.
Real impaired
presentation recovery remains a separate hosted browser check.
The controller uses a fresh 0700 temporary directory and copies only bounded,
regular, nofollow source entries: at most 4096 entries, 32 nested levels, 16 MiB per
file and 64 MiB total. It refuses links, special files, invalid UTF-8 names/text
and changing selected bytes or directory membership. Source files are stamped
when read and revalidated after copying. These are optimistic identity/stamp
checks, not an atomic source snapshot or an authenticated Cargo/runtime closure.
Retained namespace walks permit at most 24 absolute path components. The original
`build/check-native-dtls.sh` remains the unchanged reference for inert selection
and command-policy tests; ordinary checks do not compile the worker.

Python, CC, CXX and pkg-config selection uses captured PATH/caller-directory
semantics, including literal paths with spaces. Python executes through its
validated selected absolute path, preserving ordinary symlink/virtualenv lookup;
the owned worker executes through its validated canonical path. Literal PYTHON,
CC and CXX values still pass to Invoke. Executable metadata/path checks do not
authenticate pyvenv.cfg, Python modules, compiler binaries or the complete build
runtime, and Invoke's own working-directory changes can affect relative compiler
lookup. The clean environment preserves the reference's pinned wheel, pip,
Meson-output and test-tag settings without ambient Python/pip configuration.

Every phase has a fresh reserved process group. Combined build, DTLS and orderly
logs each stop at 64 MiB of actual file bytes; group retirement precedes bounded
EOF/explicit close. Failure retains the private logs and removes disposable
source/tool/build trees where their binding remains proven. Success removes
the prefix. Removal is limited to 100000 entries and 64 levels; failure is not
silently ignored. Existing 60-line failure and 3-line success tails, including long
lines, share a 64 MiB forwarding ceiling and use an owned bounded replay child.
Output, cleanup, late interruption and entry-relative deadline failures cannot
return 0. No process owner claims detached/session/privileged descendants.

CI keeps its lookup-only `native-dtls-v2` namespace and existing 12-minute check
and 25-minute job budgets. The expanded key includes vendored/reference/install
inputs plus xtask/shared providers, workspace manifests, Cargo.lock, compiler
configuration and the workflow itself. Unchanged UI bytes do not invalidate it.
A cache miss installs the existing pinned Rust 1.99.0 toolchain and runs the
controller with `--record-success`; ordinary local invocation never publishes a
marker. Cargo cold compilation inside that budget remains unmeasured. The old
roughly nine-minute native build timing is not evidence for this port.

The authorized CI option publishes the fixed unsigned `native-dtls-verified`
revision marker after successful assertions, output and owned-prefix cleanup.
The publisher explicitly closes, then checks pending cancellation/deadline.
Failures attempt rollback only through the original complete namespace witness
and exact staged inode/stamp, including uncertain rename/sync outcomes. If close
or re-admission fails, a marker may remain; its presence alone is not final
process/close proof or a release signature. Late failure logs are retained under
a fresh private `.failed` prefix only if that original parent can be re-admitted.
The existing artifact glob covers both prefixes. Inert fixtures and Python oracle
comparisons do not establish native compilation, wheel installation or hosted
runtime parity.

Exercise call outcomes with real browser decoding on owned loopback services:

```sh
CALL_OUTCOME_E2E=1 IMPAIR_SCRIPT=none IMPAIRED_PROFILES=baseline \
  cargo run -q -p xtask -- test-env \
  node web/e2e/impaired-network.cjs
```

This mode checks one terminal observation for an empty room, a video-only join,
an audio-only join and an audio reconnect using the diagnostic preview. It uses
synthetic devices and cannot establish physical output audibility.

### Chat removal and saved history

The community browser suite checks public removal confirmation/cancellation,
member/PM control visibility, quoted-text scrubbing on all peers and tombstones
after a real signaling reconnect and snapshot. It also exercises public-history
retention On/Off, guest reads, search and removal, plus reading and sending account
PMs through Messages with live delivery and recipient isolation. Run against the
freshly rebuilt server and production UI with an owned disposable database:

```sh
COMMUNITY_E2E=1 cargo run -q -p xtask -- test-env \
  node web/e2e/community.cjs
```

The notification-policy check waits for the sender's committed history row and
cleared pending-send recovery before closing its conversation. Recovery displays
the original message text before confirmation; matching that text alone does not
establish a completed send. Intentional response-loss cases keep their separate
close/reopen and idempotent-retry checks.

The focused Rust database case
`database_public_message_removal_scrubs_durable_history_without_runtime_copy`
persists messages, removes the runtime copy, and verifies old-history deletion,
quote scrubbing, PM/cross-room exclusion and one durable moderation event across
repeated requests. Run it with the migrated disposable database and
`--include-ignored`; ordinary Rust tests skip it. Source tests cover late replay,
retry acknowledgements, removed reactions and reply-draft cancellation.

`database_history_retention_pagination_private_isolation_and_monotonic_reads`
covers saved pagination, expiry, isolation and forward-only read positions;
`authorization_database_inbox_offline_delivery_read_sync_and_consent` covers the
real inbox HTTP routes, offline sends, retries and preference enforcement.
Browser source tests cover hidden/covered conversations staying unread, failed
searches, out-of-order loads, account changes and idempotent send retries.

Manually verify two physical devices and a server restart, newcomer access to
opted-in room history, retention shortening/extension and guest PM continuity.
These device/restart journeys and backup retention are separate from the
automated browser and database checks above.

### Authenticated chat continuity

Run the opt-in 20-minute session soak against a fresh local database and server:

```sh
SESSION_SOAK_E2E=1 cargo run -q -p xtask -- test-env \
  npm --prefix web/e2e run test:session-soak
```

Two separately registered accounts send through the real chat UI across scheduled
token refresh and original token expiry. The test checks delivery, drafts, room
membership and cleanup; an automatic full rejoin does not count as uninterrupted
service. Production token lifetimes and rate limits stay unchanged. Reports retain
timings and outcomes, never tokens, passwords or cookies.

Add `SESSION_SOAK_PROFILE=smoke` for a 30-second harness check; it does **not** test
refresh or expiry. Both profiles use desktop and 375px Chromium viewports, with
capture denied. They do not establish physical mobile or media continuity.

### Frontend lifecycle and degraded-browser regression checks

After building the production UI, run `npm --prefix web/e2e run test:resilience`.
This isolated Chromium suite routes HTTP and signaling fixtures into the actual
production bundle. It checks native authentication/creation dialog focus and
background isolation, blocked or full preference storage, stale room-creation
responses, capability-aware onboarding, roster focus preservation and composer
containment at short mobile viewport sizes. It uses no database or live backend
and does not capture media. A separate temporary artifact directory records the
checks and browser cleanup; `E2E_ARTIFACTS` selects an explicit output directory.

Run `npm --prefix web/e2e run test:layout` for the broader responsive-layout matrix
and the real-backend community/accessibility suites for integration behavior.
Mocked UI success does not establish native-device, mobile-browser or backend
correctness. CI runs both the layout and lifecycle suites against built assets.

For a silent UI review, set `E2E_ARTIFACTS` to a new directory and run
`node web/e2e/silent-ui.cjs` from the repository root with
`E2E_BROWSER=chromium` or `firefox`.
It checks auth-error visibility, keyboard activation, dialog focus return and
short-window reachability. Capture, peer creation, playback, Web Audio and speech
APIs are blocked before application code runs; any attempted use fails the review.
See the [browser guide](../web/e2e/README.md#silent-ui-review) for scope and artifacts.

### Automated accessibility smoke

After installing the browser tooling and building the UI:

```sh
ACCESSIBILITY_E2E=1 cargo run -q -p xtask -- test-env \
  node web/e2e/accessibility.cjs
```

This separate Chromium gate scans join/authentication, room creation, chat,
account and every settings tab, plus 320px layouts and light/dark Help. It also
checks keyboard activation of the newest-message button after real chat overflow,
native dialog Escape/focus restoration and Help opening without leaving the room.
See [browser accessibility coverage](../web/e2e/README.md#accessibility) for report
details. Automated rules and selected keyboard paths do not establish full WCAG
conformance; screen-reader, zoom, device and manual checks remain necessary.

### Advanced: manage your own disposable cluster

Create a fresh database ending in `_test`. The server helper requires
`DISPOSABLE_TEST_DATABASE=1` and a loopback `DATABASE_URL`; it rejects remote hosts
and connection-query overrides other than `sslmode=disable`. It does not create
or delete the database.

With PostgreSQL tools on `PATH`, first check that port 15434 is unused, then:

```sh
test_cluster="$(mktemp -d "${TMPDIR:-/tmp}/simplestchat-pg.XXXXXX")"
initdb -D "$test_cluster/db" -U test_owner --auth=trust --encoding=UTF8 --no-locale
pg_ctl -D "$test_cluster/db" -l "$test_cluster/postgres.log" \
  -o "-h 127.0.0.1 -p 15434 -k $test_cluster" start
createdb -h 127.0.0.1 -p 15434 -U test_owner simplestchat_test
export DATABASE_URL='postgres://test_owner@127.0.0.1:15434/simplestchat_test?sslmode=disable'
export DISPOSABLE_TEST_DATABASE=1

cargo build --locked --bin simplestChat
cargo run -q -p xtask -- test-server \
  cargo test --locked -p simplestchat -- --include-ignored --test-threads=1
```

Trust authentication is only for this disposable loopback cluster. The helper
starts its own server, applies SQLx migrations, waits for database-backed API
readiness and exports `TEST_DATABASE_URL`. It stops the server when finished.
The additional tests
cover recovery/revocation, refresh-token rotation and persisted social/moderation
behavior. A previously migrated disposable `TEST_DATABASE_URL` can also be used
directly with `cargo test`.

Keep the cluster running for [browser tests](../web/e2e/README.md), which document
installation, port overrides and Firefox/macOS WebKit setup. Do not rebuild UI
assets during a browser run.

When finished, stop only the cluster created above, in the same shell:

```sh
pg_ctl -D "$test_cluster/db" stop -m fast
```

Keep its logs if needed, then remove the temporary directory after confirming
its path.

## Container and CI checks

Hosted Ubuntu 24.04 CI prepares its existing APT mirror list through the fixed,
no-argument `xtask ci apt-mirror` entry before browser or KVM dependencies. The
root operation requires Linux/X64, the hosted GitHub context and the actual
Ubuntu/Noble release; ACT is excluded. The workflows invoke the already compiled
absolute binary through narrowly scoped sudo environment preservation. Local
disposable CI skips that preparation, and Cargo never runs as root.

Only the known 144-byte runner list or its exact preferred form is accepted.
Priorities select the existing official archive first, security second and Azure
third; all three URI bytes remain unchanged. Source definitions, pockets, keys,
APT authentication, package requests and existing install timeouts are unchanged.
Root-owned retained directories and regular-file checks precede a checked atomic
replacement, readback and explicit closes. An already preferred list is read-only.
Late failures remain failures even when the preferred file is visible; atomic
visibility is not a lock against another root writer. Focused policy and owned
filesystem tests do not establish faster hosted delivery or successful package
installation; those require the actual CI logs.

With Docker available:

```sh
docker build --target production -t simplestchat-ci:production .
cargo run --locked -q -p xtask -- container-smoke
```

The smoke creates its own disposable containers, applies every source migration
and checks health, HTML, all four public PWA files' MIME types and bytes, empty
rooms, one account write and restart of the same app container/image. HTTP is
published only on a random loopback port; PostgreSQL and the app share a fresh
network namespace and temporary data. Set `PRODUCTION_IMAGE` to another built
image. The local facade defaults to `simplestchat-ci:production`; CI uses
`cargo run --locked -q -p xtask -- ci container-smoke`, requiring an immutable
image ID and the explicit hosted/disposable Linux context.

The captured Docker selection must resolve to a local Unix endpoint. An explicit
`DOCKER_CONTEXT` retains Docker's precedence over `DOCKER_HOST`; unsupported
remote/TLS transports are refused. Every later call uses that frozen endpoint and
rechecks its bounded opaque engine ID. This is an equality observation, not
daemon authentication. Both images resolve to immutable IDs before the journal
and separate create/start phases. A settled inspection failure may trigger a
pull of the one fixed PostgreSQL digest; it is not evidence that an image is
absent. The pull shares the remaining active budget rather than a new 30-second
cold-pull limit.

Container ownership requires the generated name/nonce label, full ID and image
to match before removal. Successful scoped listing must confirm absence afterward;
an inspect failure alone cannot. Both retirements are attempted after failure or
cancellation. A lost create response without a positive witness remains unproven.
The five-minute watchdog reserves one minute for cleanup. Each readiness loop
allows at most 120 probes with 500ms delays, subject to that shared deadline;
their worst-case command times are not claimed to fit. Before every dispatch,
the body budget also reserves that command's grace plus 20 seconds for scheduled
terminal observation, group fallback and combined-log settlement, including
Darwin's zombie-only inventory proof. This is scheduling allowance, not a bound
on OS/filesystem stalls or reap scheduling. The outer CI step also includes
compilation. Cold hosted execution and real-image behavior remain unmeasured
for this port.

Artifacts stay private in a fresh 0700 directory, with exclusive 0600 fixed-name
files; `CONTAINER_TEST_ARTIFACTS` must select a new directory beneath an existing
parent. Temporary-parent aliases are canonicalized before anchoring. Sources
require no symlink ancestors/finals, four assets at most 16MiB each, and at most
4096 migration-directory entries; SQL is counted without reading its contents.
The bounded HTTP subset rejects folded/control-bearing headers and duplicate
Content-Type. Curl ignores inherited configuration, uses literal loopback URLs,
and never saves registration JWT/cookie output. Each container log retains both
channels privately, capped at 1MiB; raw guest logs are not echoed on failure.

The summary records individual assertion, engine, CLI-group and source-close
observations, without an overall success claim before its own publication/close.
CLI zero additionally requires final namespace/descriptor closes, pending-signal
and deadline checks. CLI groups cover only descendants remaining in their groups;
engine resource cleanup is a separate inspected-ID proof. Owned fake engine/HTTP/
clock tests and the unchanged `build/test-container.sh` oracle cover the policy.
The oracle records the env-shebang Bash choice/version. Bash3.2's `errexit` can
ignore the old failed `[[ ]]` port check; Rust always refuses non-loopback ports.

### Manual generator image ownership

The manually dispatched `load-generator-image.yml` keeps its Linux/amd64
`loadtest` target, fixed tag, restore-only layer cache, 45-minute job and two
uploaded filenames. After the pinned Rust 1.99 bootstrap it runs
`cargo run --locked -q -p xtask -- ci load-generator-image`. Bootstrap and cold
compile timing have not been measured; they consume the same external job
budget. This command requires an explicit hosted GitHub Linux runner or an
ACT runner marked `LOCAL_CI_DISPOSABLE=1`. It captures one environment and signal
registration, clears Git context overrides, and observes the selected checkout's
clean HEAD before and after the recipe. Selected Git, Docker and Zstd paths and
metadata remain bound through their final closes. These observations do not
authenticate executable bytes, Git configuration, source closure or the runtime.
Checkout and temporary paths must be UTF-8 and comma-free for the fixed cache
CSV argument; literal spaces and shell text remain ordinary path bytes.

The owner freezes one local Unix Docker endpoint and opaque daemon identity,
creates private Docker/Buildx configurations and records nonce builder/node,
container and labeled-volume intent before mutation. It validates the BuildKit
container's immutable ID, name, owner environment marker and exact state-volume
mount. Retirement inspects references, removes only the admitted exact container
and matching unreferenced volume, independently observes their absence, and
removes the private builder record with `--keep-daemon --keep-state`. An empty
lookup after an uncertain creation is unproved. Foreign replacements and changed
labels, mounts, references or daemon identity refuse success. These are
optimistic observations under exclusive caller-trusted engine authority, not
atomic ownership or whole-engine cleanup. Formatter shapes are source-checked
against Docker CLI 28.5.1 and Buildx 0.29.1/0.30.1; this is not a claim of
observed hosted-engine compatibility. The nonce builder deliberately replaces
the old shell's unconditional deletion of a fixed builder name.

The scheduling policy admits at most 2,700 seconds from command entry. The
40-minute build remains an upper cap; its actual body is clipped after reserving
120 seconds of grace, 20 seconds of settlement and a 1,500-second tail. Thus an
immediate build has at most 1,060 seconds, less preparation time. The tail accounts
for 27 actual cleanup CLIs (including daemon checks) at 26 seconds each, four
image observation CLIs at 104 seconds total, the save command's additional
26-second daemon check, three binary phases at 141 seconds each, two final Git
CLIs at 52 seconds, a shared 120-second hashing/graph budget, 25-second bounded
output replay and 30 seconds of closing/fences: 1,482 seconds with 18 seconds of
slack. Every dispatch rechecks remaining admission. Groups retire before final
EOF drain and explicit owned-file closes, including normal-exit descendants.
Hash workers retain the same async signal owner, cooperatively stop at chunk/
header/graph checkpoints and are always awaited before return. These are
watchdogs and scheduling allowances; OS, filesystem and reap stalls have no
hard wall-clock guarantee.

Export uses separately bounded binary stdout (8 GiB raw, 9 GiB compressed) and
private stderr (2 MiB), with metadata capped at 64 KiB and build logs at 64 MiB.
The closed Ops generator profile checks the original tar hash, runtime/revision,
fixed tag and every selected authoritative graph payload. Classic image IDs bind
the config hash and ordered raw layer DiffIDs; containerd IDs bind a verified
root descriptor and complete one-platform stored graph. Optional classic OCI
extensions must also be self-contained; a classic export retaining compressed
registration descriptors without their compressed payloads is refused. Unused
Docker `LayerSources` registration hints are outside that authority graph. No
layer is unpacked or loaded. General multi-platform/store/version compatibility
and hosted export acceptance remain unrun. The server archive verifier is
unchanged.

Compression and decompression use the selected Zstd executable, retained read-only
inputs and independent bounded outputs. The roundtrip raw hash/count must match
the verified export. Only the sealed compressed archive and exact observed
`image-id.txt` line enter the fresh final directory. Known child/guest failures,
130/143 cancellation and proven 124 timeouts remain primary over later cleanup
failures. Zero additionally requires group proof, joined hash workers, readback,
all owned closes and final signal/deadline checks. Final artifact visibility can
precede a late directory-close failure; presence alone is not CLI completion.
Private nonce directories/configurations/logs remain for failure evidence; no
raw tool or daemon output is publicly replayed. Engine daemon log storage is not
bounded by these CLI stream limits. Tests use synthetic tar bytes, owned fake
executables and the unchanged retained shell literal, without builds, downloads,
compression, engine or image workloads.

### Offline configuration validators

CI uses `cargo run --locked -q -p xtask -- ci validate-config promtool` and
`ci validate-config caddy` in the existing automation and deployment steps. Both
require the captured hosted or explicitly disposable Linux context and
`RUNNER_TEMP`. Caddy also requires the workflow's exact `CADDY_IMAGE` and literal
CI-only proxy secret. Image pins, guest argv, mounts, network isolation, capabilities,
tmpfs and Promtool resource limits match the retained shell command fixtures;
Caddy gains no new CPU, memory or PID limit. The commands do not build images.

Docker context selection resolves to one local Unix endpoint, then every
operation checks its opaque engine identity. This is an equality observation,
not daemon authentication. A successful bounded repository catalog permits a
pull of only the fixed selector when its digest is absent. Distinct tag aliases
may share one immutable ID; malformed rows, repeated tag/digest identities or
contradictory target IDs refuse. Failed commands, log completion or close errors
never authorize a pull. The selector is inspected again before immutable-ID
creation. Intent precedes create; exact name, nonce label, ID and image must
agree before removal. Empty lookup after an uncertain create is not absence
proof. Normal completion requires an independently inspected terminal guest
status, exact-resource removal and a successful subsequent absence observation.

The 570-second scheduling policy gives active work 120 seconds and reserves
450 seconds for settlement. The complete failure path admits at most 15 cleanup
CLIs with five-second bodies, one-second grace and 20-second settlement allowances,
plus bounded output replay and final fences. Every dispatch checks its remaining
allowance. This is a watchdog and refusal policy, not a hard bound on OS, reap or
filesystem stalls. Existing ten-minute automation and 95-minute deployment job
budgets are unchanged; earlier job setup and cold compilation/pulls can consume
them. Hosted cold timing and actual image execution remain unmeasured.

Selected inputs are the two current Promtool YAML files or `Caddyfile`, each at
most 1MiB, opened without symlink ancestors/finals and rechecked before explicit
close. Their framed original-byte hash is an optimistic selected-input observation,
not an authenticated revision or include/I/O closure. Private artifacts remain
under `RUNNER_TEMP/simplestchat-config-validation-KIND-TOKEN`, with a fresh 0700
leaf and exclusive 0600 fixed-name files. Metadata is capped at 64KiB (context
discovery at 4KiB), guest logs at 2MiB and admitted command logs at 96 files.
Both channels use one bounded combined pipe, and admitted CLI groups retire
before final EOF drain, explicit log close and anchored readback. This does not
bound the engine's own log storage or prove closure of detached/privileged groups.
Raw daemon/configuration output stays private; console output is a fixed bounded
guest-status projection. `config-validation.json` records prepublication facts;
its presence is not final CLI success. The two CI jobs retain only this exact
JSON basename even after failure; they never upload its private parent, command
logs, configurations or ownership records. The report adds a fixed command role,
zero-based private-log ordinal and adapter stage for its first observed adapter
failure. Its numeric refusal code is distinct from an optional observed child exit;
unknown child status stays null. Later cleanup cannot replace this first anchor.
Errors outside the adapter can leave that field null, and failures before report
publication can leave no artifact. Existing selected-input, engine and endpoint
hashes remain passive equality observations.

The 13e8065 hosted Promtool attempt returned 124 after about five seconds, with
guest status unavailable. Endpoint discovery completed, but no command metadata
was retained; the failed role and cause remain unknown. This diagnostic change
preserves that trial and all existing command caps, watchdogs and cleanup rules.
It does not establish a successful validator run or justify increasing a timeout.

Zero additionally requires all input and output closes, CLI/resource proof,
bounded replay, pending-signal and deadline fences. Owned fake processes, files,
engines and the actual retained shell commands provide inert coverage. This
diagnostic overlay adds no new real-engine or validator workload; the failed
hosted trial above does not establish guest execution or success.

### App-only releases and rollback

The workflow's 20-minute release step runs an explicitly prebuilt feature binary:

```sh
"$RUNNER_TEMP/release-http-policy-build/debug/xtask" ci release-checks \
  --http-policy-binary "$RUNNER_TEMP/release-http-policy-build/debug/xtask"
```

The existing Compose validation step first installs `libssl-dev` and `pkg-config`,
then builds with `--features release-http-policy` into that separate target directory.
It unsets the workflow's pinned OpenSSL/AWS-LC build-prefix variables for this host
client build. Later default-feature Cargo commands cannot overwrite the selected
binary. Compilation and package provisioning are outside the runtime step; the
95-minute job allowance covers them. Default xtask commands remain TLS-free.

In an explicit hosted or disposable Linux CI context, the wrapper resolves the
first executable Node in its captured `PATH` and runs the literal pinned
Playwright Chromium-path expression. Ordinary Node/Chromium symlinks resolve
once to canonical nonempty regular executables. Metadata snapshots impose no
executable-size ceiling and do not claim descriptor execution or exclusion of a
same-UID writer. Discovery has a 30-second body, 5-second TERM grace, 20-second
settlement allowance and 4096-byte stdout cap. Node, Chromium, selected Python
and the feature checker are retained and checked through coordinator settlement,
then explicitly closed. A coordinator-only capability checks the original
remaining-tail deadline again at Native's final pre-spawn boundary.

The Python coordinator retains its parallel scanner-download and release-fixture
policy, original image/output inputs, private output and status. Both caller forms
require `--http-policy-binary ABSOLUTE_FEATURE_BINARY`; there is no Python checker
fallback, implicit unfeatured binary or runtime Cargo build. For manual use, build
`cargo build --locked -p xtask --features release-http-policy --bin xtask`, then
pass the resulting absolute path to `build/test-release-container.py` alongside
its existing required flags. A caller retains and rechecks the original checker
file/path before and after its child settles, with explicit all-attempt close.

The wrapper admits one 1140-second entry: discovery 55 + coordinator body 850 +
TERM 200 + Native settlement 20 + final fences 10 =1135, leaving five seconds of
scheduling slack. The coordinator gives only its fixture task an 850-second body
and 180-second TERM grace; scanner defaults remain unchanged. The fixture has a
660-second work alarm, checker TERM 60 plus its existing five-second reap,
100-second cleanup window and ten-second publication allowance: nominal 835,
within the fixture's 850-second body. Repeated external signals latch during
awaited child finalizers. Cleanup commands reserve their own body/TERM/reap inside
one original 100-second monotonic deadline; known failures survive late clocks.
These are scheduling allowances, not OS/reaping or cold-build guarantees.

The opt-in `xtask ci release-http-policy` owns the complete original passive
response check. It requires Linux/root and only the original five flags:
`--fixture-token`, `--caddy-id`, `--app-id`, `--ca` and `--ca-sha256`. There is no
origin or arbitrary path option. Four fixed `/usr/bin/docker --host
unix:///var/run/docker.sock inspect --format ... FULL_ID` calls prove the exact
running Caddy/app identities, fixture/project/service labels and sole loopback 443
publication before and after five requests. No daemon-info discovery, inherited
Docker context, credentials, full inspection or raw response enters the receipt.

The five GETs are `/`, `/api/capabilities`, `/api/auth/profile`, `/metrics` and
`/diagnostics/media`. A mature reqwest HTTP/1 client connects only to 127.0.0.1:443,
verifies `localhost` with TLS 1.2 minimum, and trusts only the retained CA bundle.
The CA is a nofollow regular file of 1 to 65536 bytes, bound to the original SHA256,
namespace and full descriptor stamp, rehashed before/after HTTP and explicitly
closed. Requests set Host localhost, exact Accept, Accept-Encoding identity and
Connection close, with proxies, redirects, retries, decompression, cookies and
authentication disabled. The original exact status/type/security-header/CSP,
no-cookie/server, no-store, capabilities/auth omission and private 404 predicates
remain the policy. Bodies retain at most 128KiB for HTML or 4096 bytes for each
other endpoint, plus one refusal byte. Parsed headers admit at most 64 fields and
16KiB of projected Latin1 names/values; this is a post-parser accepted limit,
not a bound on the HTTP library's earlier parser allocations.

One shared absolute 25-second HTTP window covers all five requests. Three seconds
is connect/read inactivity, not a per-GET total. The checker uses one 180-second
entry: four calls at5 body +1 TERM +20 settlement =104, HTTP 25, CA/tool/files 10
and receipt 25 total 164, with 16 seconds slack. Each phase reserves unfinished
phases and the final 35-second tail against that same clock; cancellation replaces
unfinished phases and cannot reset or extend it. The checker parent allows a
210-second body then60 TERM and its existing five-second reap. Known native
statuses124/130/143 and other child failures remain failures through all closes.
The optional owner and its Native dispatch arm compile only with
`release-http-policy`; the disabled command prints the fixed build instruction.

Docker stdout/stderr use distinct private0600 unlinked bounded captures (64KiB
and 4KiB). Original Finished metadata/count/SHA seals gate readback, so a later
same-inode rewrite cannot become a new observation. Retained CA/capture ancestor
chains admit at most16 normal components each; acquisition is serial with only
two capture peers and a single selected tool live. These bounds cover this
owner's named files/pipe owners, not all descriptors internal to TLS, runtime or
standard-library child setup. No raw CLI/header/body/CA payload is published.
The fixed ten-key success receipt describes prepublication observations; final
CLI 0 additionally requires receipt replay, pending-signal, deadline and all
explicit-close gates. Client/runtime Drop is not an observed internal socket,
TLS-task or descriptor close receipt. No remote, browser-Origin, application,
live TLS or privileged Docker interoperability is proved by this source port.

The original Python checker, fifteen tests, harness and complete local import
closure are retained byte-for-byte, with compiled and current-file byte/stamp
pins checked before every reference subprocess. An offline oracle executes the actual fifteen
tests and original selected predicates with an audit hook denying process and
socket creation; unused YAML/template import APIs are fail-on-use sentinels.
Rust comparisons preserve recursive duplicate/nonfinite refusal, integer/float
identity and raw-byte bounds with Python's BOM and UTF16/32 JSON detection.
The bounded JSON domain requires Unicode scalar strings/keys and depth below 128;
Python's lone-surrogate and deeper irrelevant-label inputs are named refusals.
Unicode decimal/header-whitespace fixtures use the pinned Python 3.14/Unicode 16
profile. Owned fake HTTP/clock/file/controller cases and actual inert selected
processes cover cancellation, timeout, overflow, original-seal mutation and
all-attempt retirement. They never contact a Docker daemon or TLS endpoint.

CI also runs `build/test-release-container.py` against that existing production
image, using the real release helper and public deployment templates. It checks
a successful replacement, failed candidate startup and rollback while requiring
PostgreSQL and Caddy to stay running.

The same hosted fixture holds two Chromium documents across upgrade and rollback.
Each first imports its edition-specific deferred chunk only after the selected
backend no longer has it. The check requires direct backend 404s, successful
proxy delivery, uncached HTML and revision metadata after every image change,
unchanged document identities and zero capture calls. The fixture uses synthetic
public chunks with the production binary and pinned proxy; it does not run a
historical application or proxy binary.

After fixture readiness, the harness also checks five fixed anonymous HTTPS
responses for proxy headers/CSP, public JSON cache policy, absent cookies,
missing-authorization rejection, private endpoint omission and bounded content.
It verifies fixture ownership and the local CA and sends no account or chat
mutations. See the [passive response-policy check](response-policy.md) for scope,
limits and existing complementary coverage.

Push CI retains that same immutable production image for deployment. Offline
operations tests cover export without rebuilding, commit/run/artifact identity,
bounded CI waiting, single-host selection and stopping after a failed deployment
or verification. Deploying a retained image does not rerun the full suite; it
checks artifact integrity, runtime compatibility and readiness, then runs the
bounded anonymous HTTP checks of `/health`, `/ready` and `/`. The separate
`--public-chat-smoke` option explicitly opts into creating guests and sending a
public chat message; it is disabled by default. The supported `--force` path
accepts an exact-revision unsigned build without waiting for CI, while retaining
the runtime safeguards and recording that override. See the
[release workflow](../ops/ansible/RELEASES.md).

Run it only as root on a fresh, disposable Linux AMD64 or ARM64 host with a local
Docker Engine (API 1.48+), Compose, OpenSSL, curl, `nsenter`,
`update-ca-certificates`, and the
[controller Python environment](../ops/ansible/README.md). The production image
remains Linux/AMD64; an ARM64 fixture host needs AMD64 emulation. Use the CI-pinned
Node/npm installation, install `web/e2e` with `npm ci --ignore-scripts`, and install
its pinned Chromium with `npm --prefix web/e2e exec -- playwright install --with-deps chromium`.
Pass both executable paths explicitly across `sudo`:

```sh
release_node="$(command -v node)"
release_chromium="$(node -e 'process.stdout.write(require("./web/e2e/node_modules/playwright").chromium.executablePath())')"
sudo ops/ansible/.venv/bin/python -B build/test-release-container.py \
  --disposable-host --image simplestchat-ci:production \
  --node "$release_node" --chromium "$release_chromium" \
  --output /tmp/simplestchat-release-check
```

The output directory must be new. The helper refuses existing public deployment
paths/containers and occupied fixture ports. It creates private deployment
fixtures and installs a local test certificate authority; do not use a workstation
or deployed VPS. Verified owned containers are removed on normal completion;
an interrupted command leaves uncertain resources untouched and reports failed
cleanup. Private fixture files and derived test images remain until the disposable
host is destroyed. CI uploads only sanitized `report.json`,
never private logs, credentials or database dumps. This does not reboot a host,
exercise real users/media or establish production outage duration.

Rust linting, native/PostgreSQL tests, the memoized native DTLS check and three
browser suite groups (accounts, media, stress) run as parallel CI jobs. Their
native toolchain caches bind the vendored sources, whose files receive a fixed
mtime so reusable worker artifacts stay valid across checkouts. Rust lint,
native/PostgreSQL tests and browser backends use separate `rust-check`,
`rust-test` and `browser-backend-v1` dependency-cache namespaces. Browser builds
select only `simplestchat` and `simplestchat-load-test`, with their current default
features (including the full load generator). Their Rust-only `browser-backend`
success cache retains the server, load generator and both dependency files. Its
fresh namespace binds product/native source, manifests and lockfile, compiler and
runner identity, build environment, workflow build prefix and the current
key/save/verify implementation. Unrelated Ops implementation and xtask modules
do not invalidate these two artifacts; workspace and tool manifests remain
conservatively bound. A restored receipt must pass all four hashes and the
role-specific dependency-file checks before either executable is installed.
Unexpected tracked or generated dependencies fail closed. The older `backend`
scope remains covered by its unchanged frozen-reference comparisons. The
retention owner recognizes the fixed new compiled-cache role under its existing
main-ref, freshness, retained-copy and deletion limits. Verified-success receipts
keep their existing protected treatment.

The affected API review applies `C-CUSTOM-TYPE`, `C-VALIDATE` and `C-SERDE` to
the private scope enum, exact receipt identity and dependency admission. A
matching artifact hash cannot admit an input outside its role, and receipt
serialization grants no release or execution authority. The existing file,
process, error and explicit-finalization contracts retain their prior scope.

Three historical CI-shape methods run against a byte-pinned workflow reference
without changing their Python source. The other 42 methods in those two modules
continue to inspect current inputs. Current Rust tests preserve the complete
restart ownership/evidence checks, workflow projection changes and refusals, and
locked workspace test graph with its exact skip list. The existing reference
child shares the original 600/30 owner and captures; no child or timer is added.
Its unchanged test/support copies and original helper paths retain checked file
and directory custody. Copies do not isolate imports from privileged same-UID
mutation. These reference results cannot substitute for current browser suites
or release provenance.

The test job separately primes all targets with
`cargo build --locked --workspace --all-targets --all-features --profile test`,
including development dependencies and test harnesses; it then runs the complete
test command with ignored tests included and one test thread. Browser artifact
reuse cannot substitute for that test graph, current required checks or release
provenance and signing. The production image caches
completed application layers as well as warmed dependencies, with a new cache
key whenever its build inputs change. BuildKit validates fallback layers against
the current inputs. The native image policy commands are:

```sh
cargo run --locked -q -p xtask -- ci production-image build
cargo run --locked -q -p xtask -- ci production-image verify
```

They require Linux and either a GitHub-hosted runner or explicit
`ACT=true LOCAL_CI_DISPOSABLE=1`. Ordinary local invocation refuses engine work.
The pinned Rust bootstrap runs on both CI paths. Build checks the exact
`GITHUB_SHA` and a clean checkout before any engine operation, journals a fresh
unique builder before creation, and always attempts its cleanup. It preserves
the 50-minute build timeout and two-minute forced-stop grace; the ARM64 native
BuildKit bootstrap retains its three-minute timeout and ten-second grace.
The source guard runs again after cleanup. Only a canonical immutable image ID
with a matching revision label and `SOURCE_REVISION` image configuration reaches
`GITHUB_ENV`, after builder cleanup. Existing runner
output files are append-only and must be regular files. Cache hits import layers
without exporting; misses promote a completed export only after success.
Verification checks the same source revision and immutable image, user
`10001:10001`, root-owned application/assets/migrations, the complete migration
inventory and absence of the load generator. Its fixed guest shell runs with
network disabled, read-only root, all capabilities dropped and no new privileges.
The verification container has a journaled unique name and owner label; start
and cleanup use its inspected immutable ID. Cleanup refuses a changed identity.
SIGINT/SIGTERM return 130/143, timeouts return 124, invalid context returns 2,
and command failures preserve their exit status. Owned fake process tests check
these boundaries without contacting an engine; they do not establish real
container-build parity. The release fixture's controller preparation overlaps
the image build through two commands:

```sh
cargo run --locked -q -p xtask -- ci release-controller start
cargo run --locked -q -p xtask -- ci release-controller wait
```

The launcher creates an origin-bound private namespace and hands off to a
detached broker before publishing its commit. The worker dispatches no installer
before that commit. A ten-second precommit lease bounds an incomplete handoff;
publication uncertainty cannot authorize the launcher to signal a detached
worker by PID. The broker retains the original four apt/venv/pip commands,
inherited tracking/PIP environment and pinned requirements, whose descriptor
and namespace binding remain checked through installation. The venv and later
Python invocation keep their existing paths.

Startup diagnostics distinguish executable admission and revalidation, namespace
preparation, worker launch, and worker exit before readiness. Other errors retain
the coarse preparation failure. Requirements are limited to 1 MiB and the running
controller executable to 320 MiB. An oversized source reports its original
descriptor byte count and bound after the file and directory close attempts.
Public diagnostics contain only fixed phase labels and these numeric sizes.

Private records are capped at 16 KiB, commit/cancel requests at 4 KiB, and actual
combined log bytes at 64 MiB. Descriptor walks admit at most 24 path components.
The worker has a 90-minute fallback watchdog, including a 20-second cleanup
reserve; this does not prove the remaining time in the 95-minute job. The
five-minute collector verifies closed log/status hashes before replaying the
original bytes through an owned bounded replay process. Failed installers still
fail collection with status 1; observed cancellation/deadline results remain
130/143/124. Malformed settlement cannot erase an already chosen interruption.

Prepared evidence covers settled installer CLI groups and the closed
requirements/log/status facts. It does not certify worker exit or the later
publisher-directory closes. The collector checks its own namespace/source
closes and final signal/deadline fences. No numeric PID is signaled across
steps. Sudo/root/sessioned descendants and engine resources remain outside the
CLI-group claim; a missing terminal record is unproven preparation. If later
steps skip collection, the worker watchdog and runner disposal remain separate
boundaries. Owned inert-command tests exercise the protocol without installing
packages or claiming hosted-runner cancellation parity.
The initial cold `xtask` compilation precedes the overlap; no hosted Linux
startup cost or installer timing has been measured for this port.

[CI](../.github/workflows/ci.yml) also runs formatting, locked Rust builds/tests,
web tests/build, readiness/shutdown and pinned Chromium integration tests, dependency audits, native
dependency guards and production-image non-root/loader checks.
The [browser compatibility workflow](../web/e2e/README.md#ci) adds weekly and
manual Firefox/Linux and WebKit/macOS runs.

The preceding Compose installation uses
`cargo run --locked -q -p xtask -- ci install-compose` in the same explicit
hosted/disposable Linux contexts. It authenticates the existing Compose 5.6.0
AMD64/ARM64 digests before either fixed `sudo install` command. Curl retains its
HTTPS/TLS policy, 10-second connection and 60-second transfer bounds, plus a
256 MiB artifact cap. Output goes directly to a retained descriptor in a fresh
0700 directory. Both the download and the private install copy receive bounded
SHA-256 checks with stable metadata/bindings; input files close before privilege.
The caller's version check retains its 10-second limit and two-second kill grace.
The root check retains its clean environment, fixed daemon socket and inner
GNU timeout (10 seconds plus two-second kill grace), with an outer 15-second
owned-command bound. Both must report exactly 5.6.0.

All exits attempt owned staging cleanup. A fixed private
`compose-install-<random>.json` records stage, status, verified/download state,
installation attempts/completion, version checks and separate process/staging
cleanup proofs. The process proof covers the owned CLI group; sudo can separate
root process groups. The retained inner timeout bounds the root version check.
Its parent
namespace stays descriptor-bound through the final exclusive 0600 report write;
received command text and paths never enter that evidence. An interrupted
privileged install can already have changed the plugin; cleanup retires staging
and does not undo that installation. The step keeps its three-minute budget,
including its first cold xtask compile; hosted cold-start timing remains
unmeasured. Owned fake commands verify policy and cleanup, without downloading,
installing or contacting an engine.

The npm setup steps keep `node build/install-npm.mjs` as the authenticated
installer, followed by `cargo run --locked -q -p xtask -- ci npm-path` to
publish its fixed 12.2.0 `bin` directory. Publication requires an existing
absolute `GITHUB_PATH` regular file and owned runner/prefix/bin directories
that are not writable by other users. It refuses final symlinks, special files,
relative paths and control characters in the published path; it preserves the
runner file and closes retained descriptors explicitly. The command starts no
child or download and does not reauthenticate the installation or prevent later
changes by the same filesystem owner. The web job installs its pinned Rust
toolchain before this step. Filesystem fixtures cover the publication boundary;
the Node installer's existing tests remain the reference for installation.

The automation controller's apt, venv and pip commands also remain direct. After
successful installation, `cargo run --locked -q -p xtask -- ci controller-path`
publishes only `RUNNER_TEMP/ansible-controller/bin` through the same protected
PATH-file writer. It accepts no alternate prefix, starts no installer and cannot
substitute npm's directory. The existing compiler setup precedes controller
installation; no new toolchain bootstrap or token-bearing step is introduced.
Both roles attempt every retained descriptor close, and a write or close failure
cannot report success. This verifies publication policy, not controller-package
authenticity or immunity to later changes by the same filesystem owner.

The unconditional Rust dependency audit uses
`cargo run --locked -q -p xtask -- ci audit-rust` in the same explicit
hosted/disposable Linux contexts. It retains the reference
`python3 build/security_tools.py install --tools cargo-audit --directory PATH`
and `path cargo-audit --directory PATH` commands. That installer authenticates
the scanner and its receipt. The native command accepts only the selected
`tools/bin/cargo-audit` beneath its fresh 0700 prefix, retains its descriptors,
and rechecks file and directory identities before execution and after audit.
Files must be executable, regular, single-link and at most 256 MiB; directories
and the scanner cannot permit group/world writes. This is a namespace/identity
snapshot, not descriptor execution or a lock against a compromised same-UID
runner.

The scanner retains inherited audit output and exactly `audit --deny unsound`,
with only `RUSTSEC-2021-0145` and `RUSTSEC-2026-0097` excluded for mediasoup-sys
build tools. The removed optional SQLx/rsa advisory remains unsuppressed.
Installer, path resolution and audit share one 30-minute command deadline; the
job remains unconditional with its existing 30-minute budget, including any
cold compilation. Every exit attempts descriptor closes and bounded,
descriptor-relative prefix cleanup without following links. A fixed private
`rust-audit-<random>.json` records stages, status and separate executable,
CLI-process-group and namespace cleanup evidence through the retained parent
descriptor. Received output and paths never enter that report. SIGINT/SIGTERM
return 130/143, timeout returns 124, invalid context returns 2, and subprocess
failures preserve their status. Owned fake installers/scanners verify these
boundaries without an install, advisory database download or live audit;
authenticated Linux installation and cold-start runtime remain unmeasured.

### Run CI locally

Commit the candidate locally, then run the complete required workflow without a
push or pull request:

```sh
cargo run --locked -q -p xtask -- ci-local all
```

The launcher uses [act](https://github.com/nektos/act) to execute
[CI](../.github/workflows/ci.yml)'s `required` job and its actual dependency graph,
including the reusable security and CodeQL workflows. There is no separate list
of local checks to drift from the workflow. Install Go 1.25.0 or newer and Podman on
macOS (`brew install go podman`). Git, Python 3, curl and `patch` are also
prerequisites. The launcher builds its checksum-pinned act 0.2.89 with the
repository's shared concurrency limit, then verifies and reuses that executable
under `.cache/ci-act`. It does not use an ambient act installation.
The launcher enables act's shallow action cache so pinned actions fetch their
exact revision without downloading full repository history.

On macOS, the launcher creates or reuses only its rootful `simplestchat-ci`
Podman VM (12 CPUs, 80 GiB RAM, 80 GiB disk). It refuses to stop another running
VM or select Docker Desktop implicitly. A VM started by the launcher is stopped
when it exits; an already running VM stays running. Existing VMs keep their
configured resources. To resize an older owned VM, first end its CI invocation
and verify that `podman --connection simplestchat-ci-root ps --all` lists no
containers, then run:

```sh
podman machine stop simplestchat-ci
podman machine set --cpus 12 --memory 81920 simplestchat-ci
podman machine start simplestchat-ci
```

On Linux, use an empty engine
inside a dedicated disposable VM and identify its local socket explicitly:

```sh
DOCKER_HOST=unix:///path/to/disposable/docker.sock \
  cargo run --locked -q -p xtask -- ci-local all --disposable-engine
```

AMD64 Linux uses the pinned Ubuntu 24.04 AMD64 runner and provides the same ISA
coverage as GitHub. Apple silicon uses its pinned ARM64 counterpart for the same
suites, including first-party CodeQL and functional native tests. Optional vendor
source analysis and native sanitizer/replay checks are outside this CI graph.
Production images remain
explicitly AMD64 on both paths. After checking that the owned VM has no containers,
the Mac launcher enables its registered Rosetta interpreter for AMD64 production
commands and disables the competing QEMU handler. The packaged QEMU crashes when
querying the pinned AMD64 Rust compiler. Local ARM64 production builds derive a
digest-pinned native BuildKit image with only its bundled AMD64 QEMU removed,
so build commands use the kernel's Rosetta registration. The container driver and
exported layer cache remain in use, with a fresh journaled builder name for each
invocation; no pre-existing fixed-name builder is deleted. Authenticated OpenSSL
extraction uses Python's filtered tar reader, which works with Rosetta. These ARM64 test runs
exercise the same checks but do not claim identical ISA coverage to GitHub.
The default signed deployment requires the hosted AMD64 workflow. An explicit
owner-requested [`--force` deployment](../ops/ansible/RELEASES.md) is a separate,
recorded override and does not report skipped CI as passed.
At most four actual jobs run concurrently across the entire graph, including
matrix entries and reusable workflows. Each is limited to 3 CPUs and 16 GiB RAM,
leaving 16 GiB of the owned VM's memory for services and the engine. The maintained
act patch shares one limit across nested workflows; changing the unpatched
`--concurrent-jobs` alone would multiply independent matrix pools. The canonical
workflow matrices and their coverage remain unchanged.
The privileged job containers are confined to the disposable engine. Each
Docker-dependent job authenticates the official Docker 29.8.2 static archive for
its architecture and starts that pinned daemon with a fresh storage volume, private
cgroup namespace and private socket. The installer replaces the runner image's
older embedded daemon only inside the guarded disposable job container; it checks
both the executable and running server version. Authenticated containerd 2.4.1
and Buildx 0.37.2 releases override the older bundled components; the running
containerd and runc versions must match the maintained host selection. The outer engine socket is
never mounted, and the production release fixture keeps its empty-engine and
disposable-host checks.

PostgreSQL services use distinct declared ports for the Rust and three browser
groups. A local-only relay connects each job's loopback address to its published
service; the existing loopback-only database checks remain active. The launcher
refuses pre-existing containers. It requests act cleanup and checks for leftover
containers after execution; an interrupted setup can require inspection of the
owned VM before another run.

The full gate requires a clean committed checkout and checks its identity again
after execution. Completion receipts bind every required matrix entry and the
successful aggregate to the current run and revision; dry runs or filtered
matrices cannot report a complete local gate. Workflow jobs publish those records
with `cargo run --locked -q -p xtask -- ci-evidence record --id CHECK`; the final
local aggregate runs `ci-evidence complete`. The Rust producer preserves the
Python reference's JSON schema and bytes, reads the real workflow matrices, and
needs no Python YAML environment. YAML parsing accepts aliases and merge keys
with explicit document, depth and replay limits; includes and custom tags are
rejected. The local runner uses the same receipt identity and matrix validation.

`--base REF` selects the ancestor
used for changed-source checks; by default it uses the merge base with local
`origin/main`, or `HEAD^`
when already at that base. Update the tracking ref before validation when needed.
Local `.env`, secret, input and variable files are not passed to the workflow.
CodeQL installs the checksum-pinned bundle for each job's actual architecture.
If `.cache/codeql-tools` already contains the pinned bundle and receipt for the
actual runner architecture, the launcher mounts that cache read-only; every scan
still verifies its CLI and query pins. An incomplete cache is left unused.
The analyzer cache stays outside Cargo's `target/`, which Rust caching restores
and cleans recursively. Existing authenticated bundles can be moved from
`target/codeql-tools` to `.cache/codeql-tools` while no local CI run is active;
the same receipts and analyzer/query checks apply at the new location.

The shared `cargo run --locked -q -p xtask -- ci required` policy reads
`GATE_RESULTS` once and requires successful results from exactly the eleven canonical
workflow jobs. It rejects missing, skipped, cancelled or failed jobs, duplicate
job/result fields, malformed JSON and input above 64 KiB. Other job metadata and
outputs cannot establish success and are never printed. Both hosted and local
aggregate jobs install pinned Rust before this policy; hosted CodeQL health and
local execution evidence still run afterward. The added hosted compilation cost
has not been measured on GitHub.

Local diagnostic preservation uses `ci copy-evidence image-database`,
`ci copy-evidence release-container` and `ci copy-evidence release-checks` through
the same xtask entry point. These commands read the owned `RUNNER_TEMP` and
`LOCAL_CI_EVIDENCE` roots and copy only the workflow's fixed report/log allowlists.
An absent, non-directory or symlink source succeeds without requiring an evidence
root; a valid source creates a new directory even when optional files are absent.
Destination directories use `0700`, files use `0600`, and existing destinations
fail rather than overwrite earlier evidence. Symlink and special-file entries
are skipped; descriptor-relative access prevents replacement paths from escaping
the admitted directories. Each command bounds inventories to 4,096 entries per
directory, 1,024 copied files, 16 MiB per file and 256 MiB total. Exceeded limits,
changed sources and I/O failures fail quietly with a fixed message; private
paths and file contents never enter the console. The affected jobs bootstrap
the same pinned Rust toolchain only in the isolated local runner.

The four local CodeQL scans enforce the same first-party source scope, security
query suites, exact finding reviews and completed-query health. `vendor/` is
excluded from automated source analysis. Native C/C++ source analysis and
ASan/UBSan/replay suites are optional local checks, absent from push, scheduled,
manually dispatched and complete local CI. Their `--include-vendor` opt-in is
rejected in CI environments. Dependency and production-image audits, source
provenance, build integrity, application native tests and the functional DTLS
regression gate remain active. GitHub's
stored-analysis ingestion check and OIDC release signing/publication remain
hosted operations; a successful local gate does not claim a GitHub signature.
Rust CodeQL uses the same pinned CLI runner locally and on GitHub.
Query RAM follows the pinned official action's allocation: cgroup-limited memory
minus one GiB and five percent of memory above eight GiB. It can reuse a complete
evaluated database only for identical source, analysis scope, suite,
analyzer and query pins, runner generation/architecture/trust, and absolute
source paths. Rust inputs include every Rust source and Cargo manifest, the
`src/`, `tests/`, `migrations/`, `vendor/` and `.cargo/` trees, authorization data,
shared `web/tests/` JSON fixtures and the analysis helpers and pins. Vendor bytes
remain compiler/dependency inputs even when vendor source diagnostics are outside
the selected analysis scope. Documentation,
deployment files and unrelated frontend packages do not invalidate Rust analysis;
the source archive guard rejects any omitted tracked file.
The restored bundle hash, extraction metadata and archived source bytes must
match. A miss performs real extraction and query evaluation; a corrupt entry
fails. Complete healthy query results are saved before current policy runs, so
a finding that needs review does not discard successful analysis. Policy failure
still fails the job and blocks signing. The five exact review data/document files
are outside the analysis-only cache key; archived source must not reference them.
Successful-check caches keep their separate input rules. On a hit, CodeQL reuses
its own BQRS results and regenerates original SARIF; raw SARIF is never cached
or relabeled. Rust bundles discard intermediate query caches while
retaining all final results and diagnostics with `--cache-cleanup=clear
--include-results --include-diagnostics`. Rust policy artifacts retain the
original SARIF and query execution log for complete cold/warm comparisons.
Evidence records the original evaluation
revision and explicitly enables `queryReuse`. The CLI also writes
`queryReuseEnabled` and `originalEvaluationRevision` into each generated SARIF,
including reports whose current policy fails. Every automated run applies current
first-party finding reviews. Valid regenerated SARIF still
uploads when findings fail policy; that failure continues to block signing.

Optional vendor CodeQL runs use the same CLI with an explicit local flag:

```sh
python3 build/security_codeql_local.py --revision "$(git rev-parse HEAD)" \
  --include-vendor --language c-cpp --suite all \
  --openssl-prefix "$PWD/target/openssl-4.1.0" \
  --output "$PWD/results/codeql-native-local"
```

The output directory must be new. Native analysis needs Linux and the pinned
OpenSSL installation. Omitting `--language` with `--include-vendor` selects all
five supported languages; without the flag, the default is the four first-party
languages. Optional native creation records initialization, traced compilation
and database import separately and requires actual DTLS, STUN, SCTP and RTP
coverage. Its cache additionally binds compiler and installed package identity,
OpenSSL headers/libraries/settings, the complete vendor tree and native analysis
helpers. Compact bundles preserve final query results and diagnostics, and
current policy still runs. This command is never a CI or deployment prerequisite.

Rust can separately reuse Cargo build outputs when fresh extraction is needed.
This cache binds the actual pinned compiler, runner, trust, instruction set,
dependency bytes and fixed source path. Only keyed vendor and pip constraint
modification times are stabilized; changed Rust source still requires fresh
extraction, full queries and current policy. Cache publication is capped at one
GiB of distinct file data, counting hardlinks once. Direct local runs accept
`--rust-cargo-cache`; the official
workflow installs the pinned Rust toolchain before the compiled Rust database-key
lookup. A database hit still skips fresh extraction and dependency-output reuse;
it now requires the small tooling build as well as the compiler bootstrap.

The key command is
`cargo run --locked -q -p xtask -- ci codeql-cache key --language rust --source-root /absolute/source`.
It requires the same declared hosted image or pinned local runner identity as
`ci key`; run it inside that runner environment.
The same entrypoint offers `validate-bundle`, `validate-source`, `save` and
`restore`; use `ci codeql-cache --help` for their explicit paths and arguments.
Keys reuse the CI source inventory and bind the analyzer/query pin, source path,
suite, trust/platform namespace, and native package/compiler/OpenSSL identities
where applicable. The Python module remains the scanner driver's import API and
parity reference. Reusing a database still regenerates reports and applies
current finding policy.

Native source readback supports the Stored and Deflate methods used by genuine
CodeQL archives. Other compression methods can be inspected as bounded bundle
metadata but are rejected when source bytes must be hashed. The validator never
extracts members: it rejects unsafe paths, decoded name aliases, links,
encryption, cached SARIF and missing evaluated queries. Matching UnicodePath
metadata is validated along with UTF-8/CP437 names; ambiguous NUL names are
rejected even where an older Python ZIP reader truncated them. Archives are
limited to 100,000 members and four GiB of expanded data, with sixteen MiB per
keyed source file. Save uses private staging and the existing exact receipt
format; restore verifies the bytes before the installed CodeQL CLI can unpack
them. Tests use owned files and fake analyzer commands, including cross-language
receipt interchange with the Python reference.
On 2026-10-21, a fresh debug tooling target built in 11.33 seconds on the local
macOS ARM64 workstation with Rust 1.99.0 and registry sources already downloaded.
That measurement excludes toolchain installation and downloads; hosted cold
timing has not been measured.

After successful push CI on main, `cache-retention.yml` removes obsolete caches
from known families. It verifies the triggering run and current main revision,
protects the newest cache in each trust/platform namespace and every cache
created or accessed within two hours, and targets eight GiB of total storage.
Unknown cache families and other refs remain untouched. Run
`cargo run --locked -q -p xtask -- ci-retention` for a read-only plan; applying it requires
`--apply --run-id <successful-CI-run> --revision <current-main-SHA>`. Deletions
are limited to cache IDs, with refreshed inventory before each deletion and
limits of 100 deletions and 240 seconds.
The hosted maintenance workflow builds the tool before exposing its scoped
token. The Python implementation remains a reference for planner parity; unit
tests exercise the Rust client with a fake `gh`, including incomplete inventory,
changed cache identity, stale main revisions, output limits and cancellation.
Canonical UTC GitHub timestamps are checked across supported Python versions;
the separate oracle for noncanonical ISO extensions follows the maintained
Python 3.14 reference and runs only on compatible interpreters.

Scheduled mutation, performance/soak and macOS WebKit compatibility workflows
are additional tiers, not part of the required push gate.

Verified successes for Rust checks are reused
only when their exact input key and private receipt match. Keys bind file modes,
workflow/tool/security policy, runner image identity, architecture and the
main-versus-untrusted cache namespace. Source, executable mode, runner and trust
changes still invalidate them. Rust
keys include every tracked non-web input and web JSON configuration and shared fixtures such as
`web/tests/layer-cap-cases.json`. Operations checks conservatively include every
tracked file because they inspect frontend configuration and test helpers.
A missing cache runs the original checks; an invalid restored receipt fails.
Logs identify the original checked revision rather than claiming reexecution.
Dependency/advisory audits and JavaScript helper regressions still run each time.

The optional local [native sanitizer/replay runner](native-security.md) can
reuse prepared images and verified compiled executables. Those artifacts are
not CI success receipts and do not add a native-analysis prerequisite to the
required graph. Every selected local runtime suite still executes against its
bound executable; compilation and runtime evidence remain separate.

CI fingerprints and success receipts run through `xtask ci key|save|verify`.
The pinned toolchain is available before a cache lookup. Keys retain the existing
SHA-256 framing, scope/trust/runner identities and receipt schema; the tracked
compiled tool and its Cargo/toolchain inputs are included in every source closure.
`--root <checkout>` selects an owned canonical checkout for local verification;
it defaults to this repository. `save` and `verify` require an absolute receipt
directory. Python remains the parity reference and supplies the current security
helpers' source-closure API; scanner drivers retain their existing implementation.

The four standard workflow fingerprint steps use
`cargo run --locked -q -p xtask -- ci key --scope "$CI_CACHE_SCOPE" --github-output`.
This computes the same scoped key and appends exactly one `key=...` line to the
runner-provided `GITHUB_OUTPUT`, without printing the key on stdout. The file must
already exist at an absolute path and be regular; the command appends without
creating, truncating or following its final symlink. Missing output configuration,
unsafe/open failures, failed fingerprints and write/close errors fail the step
with fixed labels. The flag applies only to `key`; ordinary `ci key` still prints
its key for local use. Cache namespaces, restore/save guards and success receipts
retain their existing policy. Database-byte selectors use the separate image-cache
publisher described below.

`cargo run --locked -q -p xtask -- ci automation` runs the installed Python
quality gate first, then offline Ansible lint, every maintained controller
playbook's syntax check, ShellCheck and operations unit tests. Each command's
arguments are explicit and the first failure stops the gate. It installs no
tools and applies no playbook to a host.

Hosted and local CI split that complete plan between `ci automation --checks`
and `ci automation --references`. The first retains quality, syntax, ordinary
Ops/CLI tests and full Python test discovery. The second owns every exact ignored
whole-operation comparison: VPS controller/host, image builder and the eight
release operations. With no selector the local command retains their original
complete order. The checks job and reference matrix run in parallel under the
same ten-minute ceiling per job; every entry must succeed before signing. Fixed
reference groups are `foundation` (VPS controller/host, builder, prepare and cached
verify), `fetch` (fresh acquisition and fetch), `host` (receive, stage and standalone
restore verification),
and `deploy` (the complete host deployment comparison). For example,
`cargo run --locked -q -p xtask -- ci automation --references deploy` selects
the deployment comparison; replace `deploy` with another fixed group as needed.
The reference matrix's pinned setup action explicitly requests every component
in `rust-toolchain.toml` before the first Cargo invocation: `rustfmt`, `clippy`
and `rust-analyzer`. The action otherwise uses the minimal install profile;
Cargo's rustup proxy can provision additive toolchain-file components later.
Actual run `40150788623` installed the minimal compiler successfully, completed
its pinned Python installation, then failed at that later provisioning step on
an existing `bin/cargo-clippy` conflict. The host-reference job ran no operation
comparisons. The conflicting file's origin remains unproved. Completing the
existing request during setup closes the implicit provisioning gap; it does not
delete toolchains, repair arbitrary component corruption or establish that a
future hosted run passes.

Bare `--references` still runs every comparison. Matrix failure does not cancel the
other groups. Reference comparisons always run and use no result cache.
Both Python setup steps cache pip download bytes, keyed by the checker and
controller requirement files. Each job still creates its own environment and
installs the complete hash-pinned wheel requirements; a download-cache hit never
skips installation or checks. This uses the pinned setup action's
[dependency cache](https://github.com/actions/setup-python/blob/ece7cb06caefa5fff74198d8649806c4678c61a1/docs/advanced-usage.md#caching-packages).
In run `40803995276`, downloading the 61.1 MB Node wheel took 199–217 seconds,
versus 0.607 seconds in run `40799322008`. Setup consumed most of the ten-minute
automation and host-reference limits, and both jobs were cancelled during their
checks. Those cancellations are failures to complete, not passing comparisons.
Download reuse reduces repeated network work; a cold cache remains dependent on
the package service and does not establish a runtime guarantee. Job and workload
limits are unchanged.

Local CI also requires a successful execution receipt for each of the four
reference matrix groups. Missing, duplicate, unknown or filtered groups fail
completion even if an aggregate job result says success.
The existing automation receipt proves only the checks
job, with workflow/xtask changes invalidating prior keys.

Standalone restore verification is one additional exact process in `host`, after
receive and stage. A saved hosted host-comparison step took 136 seconds; the new
90-scenario restore comparison took 95.343 seconds wall locally. Their approximate
231-second sum is comparison-only, combines different environments and excludes
bootstrap, compilation and contention. It is a placement estimate, not a hosted
capacity guarantee. The full job still has ten minutes; a timeout fails its
required matrix entry. Deploy remains separate, and no comparison is dropped,
filtered or replaced by a receipt. The ordinary Ops gate also runs
`release_backup_cli`. Tracked workflow, `xtask`, native and external Python oracle
changes invalidate the existing checks/Rust-test keys; no cache namespace or
fingerprint selection policy is broadened.

The production-image job completes Compose and Caddy validation, including the
always-retained closed validation metadata, before starting the release-fixture
controller's fixed apt/venv/pip bootstrap. That bootstrap still starts before the
image build and is collected afterward through the same checked handoff. The
`8c6b451` trial timed out at the first Docker daemon-identity query, before any
validator container was created. Its underlying cause and whether the earlier
bootstrap was still active are unknown. The ordering change prevents this
bootstrap's package work from overlapping validation; it changes no command,
retry, deadline, required gate or cleanup policy and is not a proven cause fix.

The preceding receiver head's hosted automation took 537 seconds of its 600-second
job limit, including 435 seconds in the complete automation command. Local staging
adds about 52 seconds of native/reference comparison and 74 seconds of Python
reference tests. Those measured costs motivated this split; they do not establish
new hosted runtimes or the aspirational five-minute CI target.

`cargo run --locked -q -p xtask -- ci native-security` verifies the native
dependency versions, source pins and overlay hashes used by the Rust test job,
then scans the built server for the required and forbidden OpenSSL versions and
checks Linux linkage with `ldd`. It reads local inputs and runs the fixed Cargo
inventory/linkage commands; it performs no installation or native compilation.
The Linux CI step runs it after building the server.

Browser jobs reuse backend executables only for the same bound inputs and after
checking both executable and compiler dependency-file hashes. Browser backend
keys bind the app/full-load packages and their product/native source trees, Cargo
manifests/configuration, the fixed native-toolchain action/installers, current
Rust key/save/verify policy, compiler environment overrides and runner/trust
identities. Unrelated Ops implementation and other xtask modules are outside
this artifact role. The CI workflow's global configuration and browser job
through backend publication are bound;
independent image-job, operations-test and documentation edits do not rebuild
unchanged binaries. Unsupported workflow layouts fail closed.
Before publication, both Cargo `.d` files must name the expected binary and only
tracked, keyed sources or the known generated FlatBuffers/current pinned OpenSSL
inputs. External files, untracked embedded files and unknown generated inputs
are rejected. Restores recheck this dependency boundary before installing either
binary. Every browser suite still runs against the current frontend. Entries are
saved only after the corresponding build or check succeeds, without prefix
fallback. Cold caches and changed compiler inputs still require complete builds.

Production CI downloads and authenticates its pinned image-scanner tools while
the disposable release/rollback fixture runs. Both bounded results must pass;
their private logs stay separate. The later image scan rechecks tool hashes and
runs every image policy against the same exported image. Scanner containers run
only after the fixture has cleaned its engine. Within the image gate, fresh
database preparation, offline SBOM generation/conversion, and secret scanning
run in three independent branches. Two shared scanner slots retain the existing
maximum of four container CPUs and four GiB of container memory; a waiting
branch observes cancellation before starting a scanner. Each branch has separate
logs, timing receipts, and invocation limits within the unchanged eight-scanner
ceiling. Vulnerability matching waits for all three branches. Neither overlap
establishes a measured five-minute job budget.

The image gate also caches Grype database download bytes under a scanner-specific,
archive-generation key with separate main and untrusted namespaces. Every
invocation still runs the online updater, verifies the database hash and allowed age, and performs
the full vulnerability scan and current policy checks. Restored bytes have a
bounded file inventory and checked hashes; a malformed cache fails validation.
No image scan verdict is cached. Local image checks can use the same path with
`--image-database-cache <absolute-directory>`.

The two CI jobs use `ci image-cache hint` to publish the same Python reference's
prefix and optional metadata hint. Prefix failure remains fatal. A failed hint
falls back to that exact prefix with the existing warning only after its owned
CLI group is proven stopped/reaped; cancellation or uncertain cleanup fails.
Malformed successful replies fail before publication. Trust must match the
current main/pull-request boundary, and every scanner/generation digest must be
64 lowercase hexadecimal characters. These keys select download bytes; the
Python online updater and scanner remain unchanged.

After successful preparation or production scanning, respectively,
`ci image-cache status preparation` and `ci image-cache status production`
resolve only the fixed status paths beneath `RUNNER_TEMP`, using the unchanged
Python `--status` reference. Both key and prefix are validated before one fixed
record is appended to the runner's existing absolute regular `GITHUB_OUTPUT`
file. It never creates/truncates that file or follows a final symlink; its
retained descriptor closes on success or failure. Received command output and
paths are not printed. Every Python selector has a 30-second bound and
five-second forced-stop grace, and only owned CLI-process-group cleanup is
proven. A timed-out hint may fall back after successful cleanup; status failures
remain fatal. SIGINT/SIGTERM return 130/143 and cannot become a cache miss.

The image-database compiler bootstrap now also runs on hosted CI, at the same
reviewed action location and immutable Rust pin. Its 25-minute job and the
20-minute preparation / 25-minute production scan steps retain their guards and
budgets, including cold compilation. Hosted cold-start cost is unmeasured.
Owned fake selectors and pure Python-oracle fixtures verify output/fallback
policy without registry, updater, scanner or engine work; they do not establish
online freshness, cache integrity or Linux runtime parity.

Action and build caches persist under `target/act`. Each invocation writes a
private directory under `results/` containing its event, workflow log, source
revision/base, actual runner platform, excluded vendor-analysis scope, AMD64 production target,
exit status and retained compact check summaries. `--output`
accepts a new directory. A single-job run is available for focused fixes and
always reports that it is partial:

```sh
cargo run --locked -q -p xtask -- ci-local web
cargo run --locked -q -p xtask -- ci-local rust-lint
cargo run --locked -q -p xtask -- ci-local browser --matrix group:accounts
```

#### Recover inconsistent ARM CPU features

Some Apple Silicon guests advertise SVE2 without the SVE instructions it requires.
The launcher reads the owned VM's actual Linux auxiliary-vector flags before
configuring its emulator or starting CI, and refuses that inconsistent combination.
This caused the pinned `cryptography` wheel to crash during OpenSSL initialization,
before `ansible-lint --version` could run: the failing instruction was `cntb` in
`_armv8_sve_get_vl_bytes`. The [upstream cryptography report](https://github.com/pyca/cryptography/issues/14733)
and [Parallels diagnosis](https://kb.parallels.com/en/131179) describe this guest
CPU-feature problem. The kernel documents
[`arm64.nosve`](https://www.kernel.org/doc/html/latest/admin-guide/kernel-parameters.html)
to disable the unsupported feature exposure.

For the owned Fedora CoreOS VM only, end its CI invocation and verify that
`podman --connection simplestchat-ci-root ps --all` lists no containers. Then
apply the kernel argument and restart that VM:

```sh
podman machine ssh simplestchat-ci 'sudo rpm-ostree kargs --append-if-missing=arm64.nosve'
podman machine stop simplestchat-ci
podman machine start simplestchat-ci
```

[Fedora CoreOS documents kernel-argument changes through rpm-ostree](https://docs.fedoraproject.org/en-US/fedora-coreos/kernel-args/#_modifying_kernel_arguments_on_existing_systems).
The launcher never changes kernel arguments or restarts an already running VM
to repair this condition. Rerun `cargo run --locked -q -p xtask -- ci-local all` after reboot; its preflight
checks the resulting feature flags, and the unchanged dependencies and lint rules
must pass normally. These commands apply only to `simplestchat-ci`, not another
project's VM or a shared engine.

### Weekly performance and soak

The [Performance workflow](../.github/workflows/performance.yml) (weekly and
manual) compares a baseline build with the candidate on one hosted runner under
coarse regression budgets, runs the session soak with an eight-second
PostgreSQL outage at its midpoint, and runs the impaired-downlink browser and
generator checks under netem. See [continuous comparison](performance.md#continuous-comparison)
for the budgets and their limits, and
[production shape and impaired networks](performance.md#production-shape-and-impaired-networks)
for the Podman capacity runs and the local impairment helpers.

The hosted Linux comparison uses
`cargo run --locked -q -p xtask -- ci performance-compare`. It captures the
environment once, resolves the baseline before recording worktree creation
intent, and preserves the baseline Cargo working directory, inherited native
toolchain settings and shared candidate `target`. The saved baseline binary is
copied, read back and closed before the exact shared-target source is unlinked.
The candidate server and generator then build in their original order. The
10/30/100-client profiles, one/two-worker choices, three alternating repetitions,
ramp/warmup/measurement intervals and ring graph stay fixed. The selected Cargo
path is invoked as selected, including a rustup shim's basename; this is not a
compiler, build-script or source-origin authentication boundary.

Each CLI has a reserved process group and a bounded private combined log. Git
metadata is read back through its original anchored file binding (4 MiB); build
and comparison logs each allow 64 MiB. After any dispatched worktree add, cleanup
reconciles a complete successful Git inventory and only removes this invocation's
exact detached revision and retained directory. Missing, foreign or ambiguous
ownership cannot authorize deletion. Positive Linux removal also requires the
retained directory's link count to reach zero. Failed cleanup cannot return zero.

The comparison receives a generated `BENCH_CI_RUN_ID`; ordinary `bench local`
commands omit it. Its fixed `comparison-owner.json` is an exclusive private
completion observation bound to that ID, all six finished runs and actual owned
native group/stream retirement, including auxiliary commands. The outer owner
checks it after every admitted comparison settlement, independently of CLI status.
A missing or invalid receipt leaves inner closure unknown; it supplies no PID
authority or proof about escaped sessions, privileged resources or the host.
The outer CLI group alone cannot close the benchmark's separate service groups.

The owner admits commands within 90 minutes with a five-minute finalization
reserve, including three bounded Git cleanup slots. It reserves 90 seconds for
cooperative benchmark cleanup and 20 seconds of CLI settlement scheduling space;
before launch the six measurement intervals must still fit (450/810/1632 seconds
for 10/30/100 clients). These are admission policies, not bounds on OS scheduling,
filesystem operations, reap or cold compilation. The unchanged 90-minute job also
includes earlier setup and unmeasured compilation. Fixed profile, inert process
and retained Bash-reference fixtures do not establish hosted build or workload
timing. Threshold evaluation and artifact retention retain their existing guards.

For the separate fixed thirty-account, twenty-minute media/chat measurement,
dispatch `gh workflow run performance.yml --ref main -f only=account-room`.
Its measured command is `./target/release/load_test bench account-room`; the
Rust owner starts and retires both selected CLI groups without nested Cargo.
Unit tests use fake HTTP/database/process observations and actual JavaScript
policy oracles; they do not establish hosted performance or a live database run.
Run the focused, serialized fixtures with
`cargo test --locked -p simplestchat-load-test --lib account_room -- --test-threads=1`.
This job runs only on demand on an isolated GitHub-hosted Linux runner, with a
fresh PostgreSQL service, and exercises real refresh on every media connection.
It retains safe resource, delivery and database-inventory evidence, including
completed failures. See [the measurement contract](performance.md#continuous-comparison)
for its receive policy, validity gates and scope. It is outside the weekly jobs
and the required CI suite.

## Media correctness and performance

See [performance](performance.md) for the optional RTP generator and comparison
workflow, or [browser/API measurements](../web/e2e/README.md#informational-browserapi-performance)
for startup, chat delivery and decoded media. The short CI RTP smoke checks
delivery correctness; it does not set a performance budget.

### Repeated-session lifecycle check

With the server/UI built and Chromium installed:

```sh
LIFECYCLE_E2E=1 cargo run -q -p xtask -- test-env \
  node web/e2e/lifecycle.cjs
```

This opt-in check reuses two pages across six media sessions, including reconnect
and real grace-period expiry. It checks sampled media progress and cleanup while
retaining bounded diagnostics and informational memory trends. It is not part of
default CI or a substitute for physical-device checks. See
[configuration and evidence limits](../web/e2e/README.md#repeated-session-lifecycle-check).

### Session soak with a database interruption

The scheduled performance job uses `cargo run --locked -q -p xtask -- ci
session-soak` for the fixed full twenty-minute profile. The workflow passes its
PostgreSQL service ID, network and published port through the documented
[Actions job context](https://docs.github.com/en/actions/reference/workflows-and-actions/contexts#job-context).
The supervisor binds the fixed disposable database URLs to port 5432, pins the
local Unix Docker transport and rechecks the observed opaque daemon identity,
immutable image, full container ID, service network and published port before
mutation. Daemon ID equality is an observation, not authentication or a global
uniqueness proof. Ordinary host and mismatched-database contexts are refused.

One captured environment and registered signal owner pass directly through
the existing server harness, workload and Docker helpers. No nested Cargo,
background restarter or detached task owns cancellation. The 600-second delay
begins at supervisor entry; eight seconds begin only after a positively stopped
inspection. The same service must subsequently be observed running and healthy.
Failed or interrupted stop attempts receive bounded restoration attempts,
including polling transitional/starting states, before server cleanup. An
observed healthy state after an uncertain engine request does not prove that
no delayed daemon action can occur. The runner still owns service disposal.

The active fallback watchdog is 37 minutes beneath the unchanged 40-minute step;
the first workload-terminal observation must precede it. Cleanup may use the
reserve: restoration is scheduled against a 90-second deadline ending 40 seconds
before the entry-relative step boundary. Each 5-second inspection or 20-second
stop/start body is capped by remaining time minus 14 seconds for CLI settlement.
Workload/server retirement receive separate scheduling allowances. Calls that
cannot fit are refused, and uncertain restoration or cleanup cannot pass. The
last step-relative deadline and pending signals are checked after harness
cleanup. These allowances do not bound OS/filesystem stalls, cold compilation
or external job cancellation; hosted runtime remains unmeasured.

The fixed JSON supervision projection goes to command output after harness
cleanup. It contains only observed booleans and the harness status, and is not
a durable receipt or runner-disposal proof. `dockerCliGroupsSettled` covers
Docker helper groups; the overall status separately gates harness cleanup. It contains no container
ID, daemon identity, database URL or child output. Existing browser and server
artifacts retain their original locations. Known nonzero outcomes survive
restoration failures; success requires the scheduled outage, positive observed
restoration and successful CLI/harness settlement. Inert child and fake
clock/engine tests do not establish real soak or container behavior.

## Manual release checklist

### Real-browser targets

Record the OS and browser version, result and any reproduction steps for each:

| Device | Browser                                            |
| ------ | -------------------------------------------------- |
| macOS  | Chrome and Firefox, tested separately and together |
| iPhone | Kagi                                               |
| iPad   | Safari                                             |

On the Mac, run `cargo run --locked -q -p xtask -- run`, open `http://localhost:3000` in Chrome
and Firefox, and join the same room with different names. Guest-only local mode
is enough for media checks; account and room-management checks need the
[disposable database setup](#disposable-postgresql-and-browser-tests).

Phone/tablet camera tests need an HTTPS test address reachable from those
devices. `localhost` refers to the device opening the page, not the Mac, and
the local launcher intentionally keeps HTTP on the Mac. Until a mobile test
address is available, record mobile checks as **not run**, not passed.

### Hands-on checks

Use an owner and a guest in separate browser contexts:

- Preview and cancel without publishing; leave during a permission prompt. Test
  denied permissions, device changes, unplugging a live mic/camera and explicit
  restart. Confirm the other capture kind stays active.
- Exercise screen sharing, fullscreen and picture-in-picture where supported.
  Verify viewer mute, volume and hide controls do not affect another viewer.
- Reconnect during public/private conversations; check drafts, unread counts and
  account isolation after logout. Verify a redeemed recovery key cannot be reused.
- Check keyboard-only dialogs, screen-reader labels, push-to-talk, participant
  menus, desktop resizing and real mobile navigation/capture.
- On touch devices, open the keyboard, rotate the device, switch apps or lock
  the screen, then return. Check that controls remain reachable and media either
  works or gives clear recovery guidance. Record what happened; background media
  behavior is not assumed to match desktop browsers.
- On physical iOS Safari and Android Chrome, repeat those lifecycle checks with
  the camera on and microphone muted, then with push-to-talk. Confirm a resume
  does not turn on a device the user switched off or reopen a private preview.
  Switch Wi-Fi/cellular and reconnect Bluetooth headphones; retain per-person
  mute/hide/volume and the selected speaker, or show a clear speaker-selection
  action if that output disappeared. Switch front/back cameras and unplug/replug
  an active input. After leaving or fully rejoining, capture must stay off until
  the user explicitly enables it. These hardware checks remain manual; the
  media-continuity suite uses synthetic lifecycle hints and fake device lists.
- Install on a physical iPhone/iPad Home Screen and an Android browser that
  supports Web Push. Enable notifications from Account, send an unread account
  PM with the app closed, and verify the generic notification opens Messages.
  Check denial, disabling, logout, another account on the same browser and revoked
  sessions. Browser fixtures validate the UI and service worker logic without
  contacting a real push provider or proving OS lock-screen delivery.

Headless tests use fake devices and resized desktop viewports. Complete the
manual checklist on real browsers and devices before a release.


## Manual public protocol smoke

From a checkout with the pinned Rust toolchain, the opt-in client is:

```sh
cargo run --locked -q -p xtask --features public-smoke -- public-smoke --origin https://chat.example.com --room lobby
```

The explicit `public-smoke` Cargo feature enables the native TLS client. It needs
the platform's native TLS development prerequisites (the pinned OpenSSL prefix
on Linux). Default `xtask` builds omit that client so CI setup and input keys can
run before native-library installation. CI tests and Rust quality checks enable
all features, including the inert client tests.

Use only a deployment you own or are authorized to test. This operation joins
**two guests and writes exactly one public test message** in the selected room.
It performs the ten established checks: health, readiness, homepage, security
headers, same-origin static assets, private-endpoint denial, guest joins, matching
chat acknowledgment and delivery, first-leave observation, and clean WebSocket
shutdown. Both guests leave. The protocol has no leave acknowledgment; only the
first leave is independently observed by the second guest. No account, camera,
microphone, media, browser rendering, passkey, TURN or capacity workload runs.
Native WebSockets do not establish browser Origin-policy enforcement.

The origin must be an explicit HTTPS origin without credentials, path, query or
fragment. Room names contain 1–128 ASCII letters, digits, underscores or hyphens.
There are no retries, redirects, proxy discovery, cookies, authentication headers,
WebSocket Origin headers or subprotocols. TLS uses the platform native verifier
and its normal trust configuration; disabling certificate or hostname verification
is unavailable. `NODE_TLS_REJECT_UNAUTHORIZED=0` is refused. The mature HTTP and
WebSocket libraries own their internal DNS/TLS/HTTP resources through their normal
Drop boundaries; the client does not claim independently observed closure of every
internal descriptor, task or remote operation.

The HTTP representation profile explicitly requests `Accept-Encoding: identity`
and refuses other content encodings. The old Node client could decode additional
encodings. Transfer framing is decoded by the mature HTTP implementation. Health,
readiness and each of three denied endpoints accept at most 4 KiB, the homepage
128 KiB, and each of at most six JS/CSS assets 2 MiB. At most twelve requests accept
12 MiB + 148 KiB of body data, with only one body retained at a time. Accepted
headers are capped at 64 fields and 64 KiB after the mature parser; these are not
bounds on every allocation inside that parser. Duplicate headers are joined as
Node Headers.get joins them. HTTP text uses UTF-8 replacement, JSON duplicate
keys use the last value, and unknown private fields/numeric spellings are omitted.
Selected JSON strings must be representable as Unicode scalar strings; selected
lone UTF-16 surrogates are refused where Node can retain them.

Each socket accepts 256 KiB frames and complete messages, at most 4096 yielded
messages including controls, and 8 MiB of yielded payload per guest. Hidden wire
continuation frames are not separately counted. The client retains both socket
owners through all failures, queues the public message at most once, and counts
queue acceptance separately from flush/delivery. A positive close requires the
1000 close exchange, flush and protocol EOF. A forced transport drop is negative.
Late terminal protocol errors are refused even during cleanup; this is stricter
than the old event waiter after its last work observation.

The scheduling window is 35 seconds of work, one shared 5-second cleanup window
for both socket finalizers, and a 45-second entry limit including artifact and
final fences. Each HTTP request has an 8-second local limit. Same-clock checks
before and after positive observations refuse exact or late boundaries. Already
admitted socket finalizers are always joined, including after cancellation. This
is a scheduling/watchdog policy, not a hard bound on OS stalls, filesystem calls,
TLS/DNS internals or executor descheduling. No additional cleanup timeout is
added per guest. The old loose timer acceptance, local timeout status, and forced
250 ms stdout-exit behavior are not retained.

Presentation is artifact-only. A fresh private 0700 directory defaults to
`results/public-smoke.<UUID>` under the captured caller directory; a new default
`results` parent is also 0700. `--output FRESH_ABSOLUTE_DIRECTORY` selects another
fresh leaf. Its existing ancestors must be physical paths without symlinks, with
at most 24 path components; on macOS use the physical temporary path rather than
`/tmp` or `/var` aliases. Existing parents retain their permissions. The successful
payload is a 0600 `summary.json` (a bounded private `summary.json.part` may remain
after failed publication), containing the original schema-1 core fields,
fixed check/counter/failure facts and public origin/room/run attribution. No chat
content, participant IDs, raw frames, response bodies or arbitrary errors are
retained. Publication keeps the original file through rename, validates bounded
readback, then checks original file/namespace witnesses through explicit closure.
These checks are optimistic observations, not an atomic namespace lock.

Exit codes are 0 for final success, 1 for an assertion/transport/artifact refusal,
2 for arguments, 124 for an observed deadline, and 130/143 for observed INT/TERM.
The old JS CLI flattened failures to 1. `summary.json` is a prepublication
observation: a later publication, file/directory close, signal or deadline can
return nonzero after a positive saved observation. Final CLI 0 is authoritative.
The unchanged `build/public-smoke.mjs` and its seven tests remain pinned
executable references. Inert VM oracles exercise their complete operation and CLI;
owned fake HTTP/sockets and mature duplex WebSocket fixtures test the native
controller. These source tests make no claim of a real deployment/TLS/browser run.

## Standalone private container benchmark

The source-level Rust owner provides `xtask container-benchmark run` and
`xtask container-benchmark cleanup` (also through `xtask ci container-benchmark`).
It requires an explicitly authorized root/Linux host, root-owned installed
Docker/nsenter/curl tools, cgroup v2, and a trusted rootful Docker daemon on the
**same Linux host** through a frozen local Unix endpoint. A Unix socket does not
prove that the daemon shares the controller's kernel. Reported Desktop, rootless,
non-Linux and non-v2 profiles are refused. This source port has not established
real host, namespace, engine, media, capacity or installed-service interoperability.
The existing two Bash scripts, Ansible installation, Python callers, service
launcher and ExecStopPost are unchanged; no installed path selects this owner.

`run` takes the same nine flag pairs: required `--server-image`,
`--generator-image`, `--output`; optional `--workers` (1–4, default 1), `--clients`
(2–30, default 10), `--rooms` (1–4, default 1), `--duration` (3–180, default 60),
`--ramp-up` (1–120, default 5), and `--warmup` (10–60, default 10). Numeric inputs
are canonical positive decimal integers of at most three digits; clients must
fit rooms×10 and rooms cannot exceed clients. Both images are exact lowercase
`sha256:` IDs. Their IDs are opaque Docker identities, not an assertion that an
index digest equals a configuration digest. No pull or build occurs. `--output`
is a fresh absolute UTF-8, control-free path without commas, with at most 20
components; its existing parent is canonicalized once and retained. Existing
output, symlink leaves and physical overlap with the control namespace refuse.

One root 0700 `/run/simplestchat-bench` namespace and root 0600 workload lock cover
the complete run; recovery always takes that same lock with a 30-second wait.
There is no public `--normal`, root bypass or configurable host/proc/sys path.
The normal path checks the optional protected public release record and refuses
any running public Compose project before making its output or container intent.
Nonempty captured `DOCKER_HOST` has the original script's precedence over context;
otherwise only captured Docker context discovery selects the Unix endpoint.
Later commands use the frozen endpoint, a fresh private empty Docker config, and
closed selected-tool/environment profiles. Tools are caller-trusted, not signed
or authenticated by these metadata checks. The original command recipe and
immutable ID/name/run/role/network observations must match before mutation.

A protected journal precedes create attempts. Each role has an exact nonce name,
image, label set and full CID. Containers retain the original loopback/no-network
server and generator-shared-network protections: UID 10001, read-only root,
no capabilities/new privileges, bounded tmpfs/memory/CPU/PIDs/nofile/core and
local log rotation. Guest environment is fixed; the fresh metrics token is
supplied only to the server-create child and an unlinked 0600 read-only curl
configuration. It never enters argv, the journal or public failure text.
Namespace requests pin original process-start/cgroup/netns witnesses and keep
`/proc/<controller PID>/fd/<retained FD>` alive until the selected CLI's reserved
group and owned I/O settle. These are optimistic same-host observations, not
atomic PID/namespace locking or proof about escaped processes.
An initially absent journal stays an absence witness until this owner publishes
its own record; a later entry is not adopted. Existing control readers and the
workload lock admit ordinary type, owner, group, mode and link count before open,
then compare the acquired descriptor with that admitted entry.

Readiness has a 60-second phase, generator completion retains ramp+warmup+duration
+60 seconds (at most 420), room zero-count polling has 40 seconds, and server stop
uses daemon grace 30 with a 33-second CLI body. The new controller serializes
terminal inspection, cgroup observations and Docker stats under one Native and
one Signals registration, then cross-checks the generator's terminal exit with
`docker wait`. This replaces the shell background sampler and long concurrent
wait. No separate sampler process or supervisor is introduced. Original retained
cgroup/process witnesses close before container removal; disappearance is accepted
only after that role's exact terminal observation. Both containers require clean
zero exits for workload success. Cleanup may still succeed after workload failure.
Before generator intent or create, the remaining active window must cover its
selected ramp, warmup and duration plus the fixed 110-second create/start CLI and
settlement allowance. Readiness and zero-count loops retry only an unavailable
curl result backed by actual process settlement, original output seals/readback,
and successful token/namespace finalization. Authority, I/O or finalization
uncertainty is fatal; a later successful response cannot erase it.

The standalone scheduling profile is **2400 seconds with a 1260-second retirement
reserve**; external recovery has its own 1260-second entry including lock wait.
This deliberately does not fit the unchanged 900-second RuntimeMax/90-second
TimeoutStop service. The conservative retirement call graph reserves ten domain
calls per role: name inventory, known-CID inventory, ownership inspection, stop,
possible kill, logs, terminal inspection, plain removal, CID absence and name
absence. Each domain call is preceded by the existing Core daemon-ID check.
Metadata bodies 6s + grace 1 + settlement 20 cost 27s; the Core check costs 26s. Twenty
pairs cost 1060s; generator/server stop bodies 8/33 add 29s, totaling 1089s. A fresh
bind 26 gives 1115s. Internal in-flight 55 + 1115 + replay 25 + files/fences 20 =1215s;
external lock 30 + 1115 +25 +20 =1190s. The 1260 reserve leaves 45/70s scheduling
slack. Remaining-tail admission uses the original clock and releases each settled
slot once; it does not reset after acquiring the lock. Slow setup can refuse.
These are scheduling checks, not hard bounds on OS/reap/filesystem stalls.

Metadata stdout and stderr use separate private unlinked bounded logs (64KiB,
context discovery 4KiB stdout). Original Finished byte counts, SHA-256 and complete
file stamps seal both channels after all native finalizers, before readback or
parsing. Raw metadata stderr and arbitrary CLI errors are not artifacts. Combined
role logs are bounded to 32MiB per role and redacted before publication; recovery
also removes every lowercase-hex run of 64 or more bytes because the plaintext
secret is deliberately not retained. This can hide ordinary hashes in log text.
Encoded secrets are outside this literal-redaction claim. Namespace, cgroup and
source files have finite anchored/read bounds; at most 512 samples/64MiB accepted
sampling data are retained. A 128-slot reservation covers this owner's known file,
probe and bounded-log allowances before acquisition; deep namespaces can refuse.
This is not a whole-process or daemon descriptor limit. Standard-library child
stdio transfers and directory iterator/drop boundaries are not independently
observable close receipts.

The guest workload directory is 0700 UID/GID 10001; its summary is admitted separately
as a nofollow ordinary UID 10001 file of at most 8MiB, mode 0600 or 0644. Timeout-file
absence, exact selected schema 2 settings/provenance/coverage/consumer counts and
zero failures are required. Unknown private JSON stays omitted; duplicate selected
keys use the last value. Exact decimal identity follows the retained jq 1.7.1
profile, preserving numeric/boolean distinctions. Three active Prometheus counters
require singleton digit-zero values; the snapshot singleton uses the observed
AWK decimal strnum-equals-one profile (including decimal exponent/leading sign,
near-one decimal rounding and CR), without claiming provider-specific hexadecimal
coercion. The native profile accepts leading CR/VT/FF and trailing CR, while
refusing trailing VT/FF. The actual-reference oracle separately checks all six
values against three complete observed result patterns: One True AWK 20200816
matches this native pattern, Mawk 1.3.4 20200120 rejects all six, and GNU Awk 5.3.0
accepts all six. Each actual invocation must match one whole six-row status and
pass/fail pattern; mixed or unknown patterns fail. Provider/version metadata is
retained for diagnostics, rather than treated as authority for coercion behavior.
The other 183 shared-domain vectors retain exact parity and cardinality checks.
Selected JSON strings and keys must be Unicode scalar strings.

Cleanup cross-checks exact names and known CIDs and never uses `rm --force` or
`--volumes`. A lost create response plus current absence remains unproved.
Emergency recovery byte-retains the previous private result before writing a
negative result; a failed retention copy leaves the prior result unchanged and
cannot finalize the journal. Other proved roles still get retirement attempts.
New intents include a daemon identity hash. New cleanup refuses an unfinished
legacy shell journal without that witness; finalized legacy records remain
admissible. The installed legacy cleanup remains available unchanged.
`finalized=true` certifies verified retirement, independently of workload success;
it never substitutes for `passed=true`. Saved result/cleanup reports are
prepublication observations: later file/namespace close, signal or deadline failure
can still return nonzero. Final CLI 0 is authoritative. Failure codes preserve the
first known status, including 124/130/143 rather than the shell's final flattening 1.
No raw token, private error or full container inspection is a receipt.

The two unchanged scripts and their unchanged 47-test donor are byte-pinned.
The executable oracle runs those actual tests and extracted JQ/AWK/Bash blocks
with installed tools and inert inputs. It does not rewrite paths or claim to run
the whole Bash launcher: its fixed root/proc/namespace operations remain held.
Complete fake-controller run/recovery/fault tests, owned filesystem tests, and
inert selected-tool Native fixtures cover the new owner. Real Linux/static
shipping, engine/kernel behavior and service-budget approval remain prerequisites
for any later installed-callsite switch.


### Offline server attestation preparation

The closed `ops/ansible/tests/release_prepare_oracle.py` executes unchanged
Python preparation with real inert archives, raw hashes, ordered file copies,
late runtime/UTC checks and exact retained outputs. Its 72 named scenarios cover
success, all eight evidence hashes, booleans/check sets, destination conflicts,
partial copy I/O, runtime identity/VEX/expiry, opaque source/SBOM, empty/raw
hash-only evidence and byte-JSON encodings. Only the fixed UTC date and named
real-file copy faults are adapted. The exported pre-prepare inputs have a
2 MiB/32-file test-fixture bound; production hash-only inputs gain no such cap.
The four explicit nofollow/Unicode-scalar/depth/value profile differences remain
separate from matching observations.

```sh
ops/ansible/.venv/bin/python -B -m unittest discover \
  -s ops/ansible/tests -p test_release_prepare_oracle.py -v
PYTHON_CHECK_ENV="$(pwd -P)/ops/ansible/.venv" \
  cargo test --locked -p simplestchat-ops --lib \
  release::attestation::prepare::oracle_tests::whole_prepare_scenarios_match_unchanged_python \
  -- --ignored --exact --test-threads=1
cargo test --locked -p simplestchat-ops --test release_attestation_cli
```

The ignored comparison runs the actual pinned Python driver and complete Rust
operation on independently exported original files. It compares file sets,
bytes/SHA-256/modes, original-four retention and exact predicate serialization;
it does not turn decoded metadata into signing authority. Native tests also
exercise explicit closes, source exchanges, first-error ordering, original
budget/reserve exhaustion, copy/readback effects and real registered INT/TERM
while retaining a hash input. The real CLI suite uses empty environment and
private inert files, including a complete successful offline preparation.
No test starts Git, Docker, network or a host workload.

`release::attestation::prepare::signal_tests::retained_hash_signal_child` is an
ignored owned worker. Its ordinary parent alone supplies its private fixture
paths and observes registered-signal readiness, then settles the exact child
group, reaps it and closes both capture readers before checking results. The
broad database `--include-ignored` route must skip that worker and the pinned
whole-oracle test by their exact names. Automation invokes the oracle explicitly
in its maintained Python environment; neither test silently skips missing
prerequisites. These are separate test contracts from attested installation or
production signing/deployment proof.


## Cached verification checks

The native cached `release attestation verify` operation has separate policy,
actual local process, empty-environment CLI and whole-reference coverage. Its
closed Python oracle executes the unchanged `verify_directory` against real inert
ZIP/tar/files with only verifier observations and UTC adapted. It never contacts
GitHub or a host. All 111 named scenarios are available through:

```sh
ops/ansible/.venv/bin/python3 -B ops/ansible/tests/release_verify_oracle.py \
  --scenario success
```

The exact ignored Rust comparison is
`release::attestation::verify::oracle_tests::whole_verify_scenarios_match_unchanged_python`.
Run it explicitly under the pinned controller Python environment in automation:

```sh
PYTHON_CHECK_ENV=/absolute/pinned-controller-env \
  cargo test --locked -p simplestchat-ops --lib \
  release::attestation::verify::oracle_tests::whole_verify_scenarios_match_unchanged_python \
  -- --ignored --exact
```

The broad workspace `--include-ignored` command skips the unique basename
`whole_verify_scenarios_match_unchanged_python`; the owning automation still
invokes its qualified name with `--ignored --exact`. The whole test owns the
bounded Python child, both captures, group retirement/reap,
original caller deadline and subsequent Rust verification. Repeated use requires
eight fresh verifier observations, while each ordinary call requires four.
The oracle compares passive envelopes, exact argv/environment/caps, refusal
prefixes, raw stdout hashes and retained bytes. Three explicit native profile
cases distinguish witnessed ZIP/inode changes and Unicode-scalar refusal from
reference-positive behavior. Actual helper tests use owned inert subprocesses,
including cap sentinels, backpressure, descendant pipes, signal/deadline and
close/read/conversion faults; the same exact ordinary test owns its child role,
so no independently invoked ignored child route is added.

The actual `release_verification_cli` target checks route/help/notices/usage,
selector limits and fixed errors in an empty environment/private cwd. Its
successful case uses a controlled local `gh` fixture, proving native dispatch and
policy together without claiming cryptography. The shared preparation owner,
file custody and late bindings keep their existing 17-native/4-CLI/72-oracle
regression contract and must be rerun on the final combined source.

Local Python oracle/quality results, default-stack process/CLI results, target
checks and actual maintained-GH verification are separate evidence. Before any
caller switch, record four fresh authenticated verifier results on exact original
signed production inputs, repeat the operation, reject modified/certificate/run
mismatches, measure the existing caller budget, and prove verification still
finishes before Receiver/SSH construction. Copied prior API metadata proves no
new API selection. No current source-only or inert fixture result satisfies that
runtime/authentication gate.


## Fresh release verification checks

The complete native `release verify` operation has separate native fault,
actual CLI and whole-reference checks. Run its ordinary tests with the normal
parallel test runner:

```sh
cargo test --locked -p simplestchat-ops --lib release::attestation::acquire::
cargo test --locked -p simplestchat-ops --test release_fresh_verify_cli
```

These checks own inert local GH/curl fixtures, actual pipes and process groups,
signals, retained files, output faults and cleanup. Lifecycle/umask assertions
run in an owned exact subprocess rather than changing the unit-test process's
global mask. Their fixtures establish neither real GitHub selection nor TLS or
cryptographic verification.

The finite Python oracle executes the unchanged `build/verify-release.py` entry
point and its real selection/download/extraction/cached-verification policy on
103 named scenarios, adapting HTTP and cryptographic observations locally:

```sh
ops/ansible/.venv/bin/python3 -B \
  ops/ansible/tests/release_fresh_verify_oracle.py --scenario fresh-success
```

Its genuine Rust comparison owns the reference process and then exercises the
complete native operation with inert tools and real fixture bytes. Invoke the
ignored test explicitly under the pinned controller Python environment:

```sh
PYTHON_CHECK_ENV=/absolute/pinned-controller-env \
  cargo test --locked -p simplestchat-ops --lib \
  release::attestation::acquire::oracle_tests::whole_fresh_release_matches_unchanged_python \
  -- --ignored --exact
```

Operations automation owns this exact comparison in hosted and local CI. The
broad database `--include-ignored` route skips only its unique basename
`whole_fresh_release_matches_unchanged_python`; the oracle remains an explicit
required automation step. The ordinary CLI target runs in the same Ops gate.
No separately ignored native signal worker is introduced.

The comparison covers both branches, repeat calls, current API relationships,
token/grant roles, raw headers/targets, body limits and retained failure prefixes,
late file/run/certificate refusals and the final public result. Native boundary
differences are explicit in the
[affected API review](release-fresh-verification-api-review.md), including curl
framing/platform trust, read-idle timing and UTF-8 output paths. Cached operation
must still acquire fresh API metadata and perform four verifications while curl
and token retrieval are unavailable.

Keep this fixture evidence separate from actual authenticated acquisition.
Before switching an operational caller, use current explicit signed artifact
selectors with a source-bound native binary: fetch and extract real bytes, run
all four maintained verifiers, repeat through the cached branch, check original
bytes and resource finalization, and measure the full caller's remaining budget.
Prove completion before constructing Receiver/SSH or contacting the deployment
host. An API-shaped fixture, copied metadata, source-built binary or successful
compiler check alone does not establish authenticated distribution or bootstrap
on a different target. Existing Python callers remain available until that
separate migration is complete.

On 2026-10-24, a source-built Darwin executable freshly acquired and verified
signed artifact `12559686976` from successful CI run `40091336563` at
`1e88240f388384fecce80ec620d970b781bc3d03`, then repeated cached verification.
Both invocations returned exit 0, empty stderr and identical four-key results;
all 11 files retained their original bytes. Fresh mode took 139.731 seconds and
cached mode 20.304 seconds, including fixed role-observer logging. Each performed
two current API calls and four distinct signature verifications; cached mode
succeeded with curl unavailable and requested no token or grant. This is real
platform transport and authentication evidence, separate from authenticated
binary distribution, Linux musl execution and deployment-host integration.
See the [affected API review](release-fresh-verification-api-review.md) for the
artifact digest, tested tool versions and earlier failed trials.

## SSH release fetch checks

The native `release fetch` command verifies locally before starting its single
OpenSSH session with the existing Python receiver. Run ordinary native and CLI
checks separately from the whole-reference comparison:

```sh
cargo test --locked -p simplestchat-ops --lib release::attestation::fetch::
cargo test --locked -p simplestchat-ops --test release_fetch_cli
PYTHON_CHECK_ENV=/absolute/pinned-controller-env \
  cargo test --locked -p simplestchat-ops --lib \
  release::attestation::fetch::oracle_tests::whole_fetch_controller_matches_unchanged_python \
  -- --ignored --exact
```

The finite comparison executes the unchanged Python controller and receiver on
73 scenarios. Its inert SSH bridge runs the original receiver as an owned
child; it supplies external GitHub, signature and HTTP observations locally.
It checks both local acquisition branches, exact preflight/Ready/grant ordering,
private evidence and failure prefixes, current exit and natural EOF, projected
receipts and resource settlement. Its real subprocesses and filesystem checks
establish neither real SSH authentication nor production remote effects.
Operations automation runs the exact ignored comparison explicitly; the broad
database ignored-test route skips its unique basename instead.

Native regressions exercise both frames, stdin ownership through the final
receipt, cancellation, subordinate status, missing EOF and descendants. Receive
and trailing-output budgets remain separate: a valid whitespace fixture exceeds
120 KiB overall while each original counter stays within 64 KiB. Raw read-ahead
also counts toward the receive budget. A consuming-wait failure cannot cause a
second reap or a later signal through a released process identifier; independent
pipe finalizers still run and the unavailable reap observation remains negative.
Other faults cover first failure with final report write/close, original deadlines,
key admission and explicit-empty cached paths. Actual CLI signal tests and the
runtime/umask fence run in owned subprocesses.

Syntax errors and unknown flags return usage status 2. Parsed selector or key
refusals return the reference's fixed three-field `invalid_options` result and
status 1. Invalid native-only whole/finalization budgets are usage errors.
The native path profile rejects an explicit-empty cached directory; it never
turns that supplied value into a fresh download. See the
[affected API review](release-fetch-api-review.md) for the complete native profile.

Keep these fixtures separate from the actual disposable-VM SSH proof. That
proof must bind the selected source-built executable, signed server artifact,
generated guest/client identities and pinned known-host entry; execute both
local-cache and local-download branches through real OpenSSH and the unchanged
receiver; compare original published hashes and inodes; and finish all known
VM, process, pipe and file owners before reporting success. Source-built Linux
GNU execution is separate evidence from authenticated musl distribution or an
installed caller migration. An already acquired archive alone does not exercise
SSH or the receiver's download. Existing operational callers remain available.

## Native host receiver checks

The explicit native `release receive` operation has its own host contract and
CLI, separate from the controller's default prepared Python receiver route. Run
its ordinary regressions and actual CLI checks before the complete comparison:

```sh
cargo test --locked -p simplestchat-ops --lib release::attestation::receive::
cargo test --locked -p simplestchat-ops --test release_receive_cli
PYTHON_CHECK_ENV=/absolute/pinned-controller-env \
  cargo test --locked -p simplestchat-ops --lib \
  release::attestation::receive::oracle_tests::whole_host_receiver_matches_unchanged_python \
  -- --ignored --exact
```

The comparison runs 104 cases through the unchanged Python host receiver with
real private files, ZIP extraction, locks, syncs and publication. It supplies
inert HTTP/process observations and compares retained effects, report classes
and progress. Before/after censuses include the original reference sources and
the maintained Python set. They establish source stability during each run,
not that every unrelated policy test is unchanged since an older commit.
Operations automation owns this exact ignored comparison; the broad database
route skips its unique basename.

Native cases cover original publication aliases, empty CA-directory custody,
close and sync failures, final report ordering, descriptor flags and the retained
whole-operation deadline. The actual subprocess signal cases cover INT/TERM
while waiting on standard input and after the complete report. Thread-directed
late injections complete their kernel signal handler before teardown continues;
a process-directed signal still pending at handler removal is outside that
observation. The parent separately requires natural pipe EOF, terminal leader
observation, group retirement and one consuming reap under the original deadline.

The command keeps fixed host paths and root admission, requires explicit
`0 < finalization-seconds < timeout-seconds <= 300`, and does not change prepared
callers or bootstrap installation. Its curl/OpenSSL/CA prerequisites, fixed
User-Agent, parser bounds and socket-idle differences are documented in the
[affected API review](release-receive-api-review.md). A finite comparison does
not establish unrestricted Python compatibility or actual TLS behavior.

The separate manual [signed musl runtime proof](release-receive-runtime-proof.md)
authenticates both artifacts, installs the retained Ops executable in a free
Debian KVM guest, and exercises actual SSH fetch with the native host receiver.
It requires positive repeat-publication identity plus lock, journal, conflicting
target and wrong-checksum negatives. Its one 1800-second keeper includes both
acquisitions and reserves 120 seconds for settlement. Source/inert checks alone
do not establish a successful workflow run, signed musl execution or deployment.

## Native host staging checks

The explicit `release stage` command keeps the prepared Python callers available.
Run ordinary, actual CLI and whole-reference checks with the pinned environment:

```sh
cargo test --locked -p simplestchat-ops --lib release::attestation::stage::
cargo test --locked -p simplestchat-ops --test release_stage_cli
PYTHON_CHECK_ENV=/absolute/pinned-controller-env \
  cargo test --locked -p simplestchat-ops --lib \
  release::attestation::stage::oracle_tests::whole_stage_matches_unchanged_python \
  -- --ignored --exact
```

The 81-case comparison executes the complete unchanged Python staging main
with real private files, TARs, flock, atomic records and actual inert Docker
children. The native operation uses independent lower observations; its process
adapter has separate actual owned-child tests. The comparison checks both
config and selected image-manifest IDs, received hardlinks, repeat identity,
command/effect order, journals and retained failures. It contacts no Docker daemon.

Native cases distinguish natural response completion from EOF caused by cleanup,
retain known validator IDs after other stream-close failures, refuse unknown
creation authority, and check the post-lock outcome and runtime/umask/signal
fences. The 40-second validator tail stays inside the original body, leaving
finalization for local records and runtime retirement. Small accepted budgets
can refuse before container creation. See the [API review](release-stage-api-review.md)
for the native limits and Python whitespace, line and 4300-digit version rules.

The separate manual [signed musl staging proof](release-stage-runtime-proof.md)
authenticates both artifacts and exercises actual Docker first stage and repeat
in one free Debian KVM guest. Native actor history and the separately inspected
checksum probe have distinct claims; the probe establishes its own isolation.
Source/inert checks alone do not establish signed Stage execution, production
application deployment, a database migration, rollback or caller cutover.


## Native standalone restore verification

The complete `release backup verify` operation compares 90 finite scenarios with
unchanged `restore_verify.py` main, using actual private files, locking, retained
snapshots and owned inert child processes with Docker/database replies. The exact local comparison
passed in 92.25 test seconds (95.343 seconds wall). It retains raw original/native
outcomes and effects, including the named stricter readiness and admission profiles
in the [affected API review](restore-verification-api-review.md). It does not
establish authenticated musl distribution, a prepared-host public CLI run, actual
PostgreSQL restore/amcheck or an operational caller switch.

Run its sole ignored process in the pinned controller environment:

```sh
PYTHON_CHECK_ENV="$PWD/ops/ansible/.venv" \
  cargo test --locked -p simplestchat-ops --lib \
  release::attestation::restore::oracle_tests::whole_restore_matches_unchanged_python \
  -- --ignored --exact --test-threads=1
```

Operations automation configures this exact command in the existing `host`
reference group for hosted and local CI. The full local plan also retains it;
`ci automation --references host` selects receive, stage and restore together.
The broad all-features database test explicitly skips only the unique basename
`whole_restore_matches_unchanged_python`. It must not launch this fixture under
`--include-ignored`: its sole-process guard precedes the actual umask and Owner,
and a broad or shortened selector is refused. The separate ordinary Ops gate runs
`release_backup_cli`; full Python discovery retains the producer regressions.
No job timeout, group/receipt roster, dependency or installed caller changes.

The optional [signed backup-verification runtime fixture](release-backup-runtime-proof.md)
adds three independent free Debian KVM cases: current data, a valid historical
subset from a failed release, and a restorable privacy-invalid backup. It reuses
the authenticated Ops installation and exercises the same installed musl binary
with actual PostgreSQL. Each case retains its original 1800/120 fixture budget,
360-second setup ceiling and one explicit native 1200/120 invocation. The
[three-case signed run at `1f5098d`](https://github.com/swiftraccoon/simplestChat/actions/runs/40232524502)
passed with actual installed-musl restore/amcheck, the expected privacy SQL
refusal, preserved source state and complete fixture/VM cleanup. The proof guide
records its exact artifacts, earlier failed trial and process-lifecycle limits.
Existing operational Python and nightly/offsite callers are unchanged.


## Optional maintenance prerequisite experiments

The four-case launcher workflow selects the Rust outer entry
`xtask ops-artifact maintain-runtime`; the previous Python controller remains an
unchanged reference. Its [API review](maintain-controller-api-review.md) names
the retained bootstrap roles and independent child versus outer clocks. The
authenticated installed Ops binary now owns the explicit native guest role,
with exactly 52 project source pins and 15 operational helper pins. Frozen Python
still supplies Ops33, Ansible and operational helpers. The same 1,800-second
outer limit and 120-second reserve apply,
with a 510-second setup cap and 1,170 seconds required before GO.

The guest's render failure checkpoint distinguishes pure rendering (`render`),
the exact inert-input handoff (`render-inert-handoff`), configuration publication
(`render-files`), helper custody (`render-helpers`), data-directory creation
(`render-directories`), the actual assets child (`render-assets`), its decoded
receipt (`render-assets-receipt`) and final tool admission (`render-tools`).
Only those fixed values cross the outer decoder; no raw file or child details
are exported. Decoder regressions preserve pre-native failures and null native
results, reject those labels as success checkpoints, and refuse unknown labels,
failure categories and extra fields. The first failed checkpoint survives guest
cleanup. A label identifies its operation group; `render-assets` also covers
pre-spawn checks and does not prove the child started. The earlier four-case
`3982fd5d` run failed at the broad `render` label
with all eight QEMU epochs fully closed; that report does not identify which
render operation refused. These diagnostics still require fresh signed runtime
qualification and establish no native launcher or 900-second result.

The `0d8e7594` trial also failed all four before native execution. Its
`render-tools` label remained active through the following network operation;
it did not isolate a tool-admission failure. Successful collector finalization,
known network-create adoption and clean guest recovery place the source-derived
failure region before successful guard-install adoption. The exact hosted branch
was not exported. An owned Docker 29.8.2 control produced the expected bridge
profile; the exact firewall output also passed an Ubuntu `nft --check` control.
Neither local control identifies the hosted refusal or proves guest containment.

Network diagnostics now enter `network-collector` before the first effect and
distinguish creation, marker join, profile, route, firewall preparation, guard
installation, capture startup/readiness and the guard probe through fixed
`network-*` checkpoints. The first guest Rust refusal kind also retains the
closed `input`, `changed`, `command` or `bounds` category instead of generic
`refused`. No raw diagnostics or extra wire fields are added. Decoder regressions
preserve truthful negative metadata while refusing unknown values, injected text,
extra fields and passing receipts with a failure category or setup checkpoint.
These changes need their own fresh signed qualification; no underlying setup
correction or successful four-case result is claimed.

The subsequent `df7314d3` trial reached `network-route` and refused before native
execution. Pinned [libslirp DHCP handling][maintain-restricted-dhcp] omits the
default router in restricted mode. Setup now uses the bounded passive lookup
`ip -json -4 route get 10.0.2.2`, admitting the exact direct peer/source and safe
physical device into a private validated type. Wrong, routed, multipath,
non-unicast, unsafe and ambiguous replies remain refused. The firewall, capture
filter, packet allowance, setup deadline and remaining native debt are unchanged.
No default route, DNS service or external packet probe is added.

The existing single guard datagram now targets the unused on-link sentinel
`10.0.2.254:53`, so it can exercise the output DROP rule without a default route.
It retains the payload, one-attempt count, exact completed-send/EPERM rule,
checked close and fresh counter increment. Route-unreachable errors remain
negative, and the unchanged capture must still contain no packet records.

Pure receiver tests include canonical direct routes, parsed ancillary metadata
and malformed/unsafe neighbors, plus the sentinel's fixed subnet/address bounds.
A separate owned Linux control removed its
default route and captured an empty default list plus a direct lookup with the
expected destination/source. That synthetic Ubuntu/iproute control and the
primary-source mismatch do not expose the historical guest's route contents or
prove fresh Debian/QEMU containment. Signed four-case qualification, including
the genuine 900-second scenario, is still required.

[maintain-restricted-dhcp]: https://github.com/qemu/libslirp/blob/3ad1710a96678fe79066b1469cead4058713a1d9/src/bootp.c#L273-L288

Repeated CI attempts can retain server artifacts with the same commit-based
name. Discovery selects the unique newest matching creation instant, refusing
malformed matching timestamps or a tied newest instant. That timestamp only
locates a candidate: the signed predicate and current metadata must still bind
both Ops and server to the requested successful run and exact attempt. Failed
authentication never falls back to an older artifact.

The fixed three-member transfer writer is tested through its production path at
all twenty possible member offsets modulo a 10,240-byte TAR record. It keeps the
library's 1,024-byte zero terminator and adds no whole-record padding. The former
padding generated a 10,752-byte tail at offset 9,728, which the unchanged Rust and
frozen Python readers reject. Tests retain the header/order/member-hash checks
and separately reject wrong roles, short or nonzero terminators and over-limit
padding; generated output must have the exact minimal tail.
These are archive/custody checks, not fresh signature or VM execution proof.

Controller setup uses `--no-install-recommends` with explicit firmware and CA
packages. An owned minimal Ubuntu 24.04 APT simulation preserves the required
QEMU/ISO/network/trust closure and common package versions while omitting unused
GUI, UEFI and remote-storage recommendations. It installed no packages and is
not boot or hosted-performance proof. QEMU boot, cloud-init and all four cases
still require the actual signed workflow under its unchanged 45-minute limit.

```sh
cargo test --locked -p xtask ci::ops_artifact::maintain_runtime::
cargo test --locked -p xtask ci::ops_artifact::hosted::
cargo test --locked -p simplestchat-ops --features maintain-guest --lib \
  release::attestation::maintain_prerequisite::guest:: -- --test-threads=1
cargo test --locked -p simplestchat-ops \
  owned_process::tests::completion_signals_survive_runtime_teardown_in_actual_owned_children
node --test build/tests/container-inputs.test.mjs
```

Native tests cover closed guest/native status and phase joins, complete Ops33
receipts, real input/namespace mutations, full recap refusal, fixed work cleanup
and actual isolated-child INT/TERM after runtime teardown. Exact reference
catalog/source assertions and the existing unmodified Python suites remain
separate evidence. They do not prove full cross-language controller equivalence
or actual signed four-case PostgreSQL/Caddy/900-launcher runtime success.

The source-import regression uses real local Git operations to cover its mutable
administrative namespace: the shallow input closes before fetch can create a
reflog directory. Immutable project-source checks and the one-commit revision,
tree and clean-worktree checks still apply. This local ownership evidence is
separate from the signed VM experiment.

The native launcher's cleared environment also suppresses Python bytecode.
A Linux regression runs the unchanged seed helper's argument-refusal path through
the production command builder and checks that its retained parent directory
and source bytes stay unchanged. This import check performs no application
seeding; the signed VM trial must still establish that operation.

A Linux seqpacket regression queues the settled and final result frames before
an owned inert wrapper exits naturally. The receiver must consume the queued
result before classifying terminal status as a missing result, retaining the
original result deadline and all packet checks. Missing, invalid or late frames
remain refusals. This exercises result delivery and ownership without executing
the application launcher or proving the complete guest transaction.

The Caddy profile uses the pinned Docker daemon's canonical inspection value,
`CAP_NET_BIND_SERVICE`. A local Docker 29.8.2 daemon/client experiment with the
exact pinned Caddy image and normal create arguments reproduced that value;
the earlier bare-name predicate refused it before Caddy start. The regression
also refuses the bare alias, extra or wrong capabilities, an empty list and null.
The create argument remains `--cap-add NET_BIND_SERVICE`. This corrects an
inherited frozen-reference defect; neither the Docker profile observation nor
the unit tests establish fresh signed VM or TLS success.

Fresh TLS endpoint admission accepts exactly one enabled no-new-privileges
option: the raw-create form `no-new-privileges` or the Compose-preserved form
`no-new-privileges:true`. The regression derives both forms from their actual
project constructors. False, malformed, missing, duplicate, contradictory and
extra options refuse. Image, endpoint, mount, capability, certificate, key and
fresh handshake checks remain separate. A `tls-observe` refusal can occur at
this profile check before any handshake; that checkpoint alone does not identify
a cryptographic failure.

Daemon failure metadata distinguishes ten closed operation checkpoints without
exporting event values, IDs, names or command text. The outer decoder admits
these labels only on failed receipts; successful guests still require `complete`.
The built-in `none` network is inspected before the launcher and rechecked during
observation. Only the joined validator and migration actors may supply its one
ordered connect/disconnect pair each, with their complete actor traces still
required. Unknown networks, actors, actions, duplicate or missing pairs and
changed network profiles refuse. The built-in network is never retired by the
fixture. These checks do not establish the exact branch in earlier hosted failures.

Daemon observation tests require the one pinned-Compose application replacement
rename, joined to the selected app, retired old app, exact names and labels and
the complete ordered lifecycle. They retain refusals for missing, duplicate,
late, foreign, wrong-service and malformed-identity events. A separate private
Rust probe uses a real pinned-Docker create/rename event with a synthetic retired
context and an inert image; it does not start a container or establish complete
Compose, hosted-branch or VM equivalence.

Stop observation also accounts for the pinned daemon's separate state and event
notifications. Moby can wake a stopped-container waiter before publishing `die`,
so `stop` may follow `create,start` for an admitted retired app or migration.
This early stop additionally requires that actor's already admitted signal-15
`kill`. The complete final history still requires `die,destroy`; signal checks,
roles, current-app exclusion, duplicate rejection and unknown-action refusals
remain. An early kill-error recovery without the joined kill remains outside this
narrow profile. Trace fixtures cover both stop/die orders and incomplete or
foreign histories. This source-proven ordering does not identify the precise
event refused by an earlier hosted `daemon-action-history` report.

Capture diagnostic tests cover classic-PCAP endian/magic variants, bounded first
header classes and truncated or malformed headers. IPv4 cases include options,
fragments, incomplete headers, snapped payloads and ambiguous port combinations.
The seven narrower IPv4 hints do not establish application-protocol validity,
direction, origin or the contents of earlier generic IPv4 failures.
Every nonempty capture still refuses, and the prior exact 24-byte/zero-status
rule remains. First-failure tests
retain earlier body/cleanup causes instead of replacing them during local capture
retirement. The outer decoder accepts only the closed capture checkpoints
on failed receipts and rejects them on passing receipts, together with unknown
labels and added fields. These metadata tests do not identify the previous
timeout capture's contents or establish a successful fresh containment proof.

Live monitor checks also refuse a capture as soon as they observe more than
the 24-byte header. Such data cannot later become an empty capture, so an
already failed monitor cannot authorize GO or continued native work. Partial
startup headers remain admissible until finalization. Recovery continues to
pump and record the watch while attempting known actor and network retirement
under the original clock; final capture refusal still prevents success. An
explicit first-capture witness preserves its checkpoint, including a first
capture during cleanup, without equating unrelated errors with the same code.
These checks locate observation, not packet transmission, ownership or direction.

The native maintenance VM's user-network backend explicitly selects
`ipv4=on,ipv6=off` before boot. SSH, its host forward, controller inventory and
fixture services use IPv4. This disables an unused provider capability whose
reviewed network stack can generate periodic IPv6 router advertisements; it
does not identify the sender of a captured advertisement. The argument regression
joins the backend, network device and SSH client to the same private forward.
Disks, QMP, sandbox and resource limits are unchanged. Guest firewall,
capture rules and the empty-capture requirement stay unchanged, including
refusal of every other packet. This profile needs fresh hosted bootstrap and
full-timeout evidence. Earlier generic IPv4 captures remain unexplained, and
the frozen Python fixture retains its existing network profile.

The Rust controller prepares one guest disk in two serial VM instances. The first
instance performs the existing authenticated installation and Ansible bootstrap,
then pulls the exact pinned PostgreSQL and Caddy images. A checked guest
`sudo -n /usr/bin/sync -f /` must finish before retirement. The controller reaps
and closes the first QEMU, readers and QMP observer, seals the prepared overlay
while its original inputs remain held, closes those inputs, and re-admits the
same disk, base, seed, SSH identities and
forward. One retained directory witness spans that handoff and closes before the
second spawn. The second backend additionally selects `restrict=on`; it retains
the explicit SSH host forward and receives fresh SSH/cloud-init admission. Guest
setup inspects the local images and their exact digest/platform identities. Fresh
natural TLS material is generated after this restart. Neither a successful image
pull nor the restart alone proves an empty subsequent capture.

Both instances share the original 510/1680/1800-second setup/body/whole ends and
the original 1170-second admission debt. Stdout and stderr each retain their
aggregate 2 MiB cap; QMP retains its aggregate 65536-byte/128-message cap. A failed
first retirement, changed file or namespace, exhausted budget or failed second
admission prevents the native invocation. Outer summaries now use schema version
2 and add `preparationQemu`, with the same complete 14-field cleanup contract as
`qemu`. Consumers must require both records and the aggregate caps; guest request
and response schemas remain unchanged. Local handoff and mutation tests establish
custody and bounded policy, while fresh signed four-case execution remains the
runtime requirement.

Unexpected VM stream EOF remains an immediate proof failure, but its existing
reader continues draining the still-bounded peer under the same clock and
settlement windows. Later EOF or successful closes cannot restore the VM's
cleanup verdict. A settled nonzero startup before SSH admission may add the
failure-only `vmStartupDiagnostic`: the preparation/restricted epoch, actual
signed `rawStatus`, closed `stderrClass` and complete retained `stderrSha256`.
Read, write, capacity, deadline or close uncertainty in the evidence owner leaves
this diagnostic unavailable. Complete native Ubuntu QEMU fixtures bind the finite
classes `host-forward-setup`, `backing-file-missing`, `qcow2-format` and
`image-write-lock` to their entire expected records and admitted arguments.
The `kvm-access` class additionally matches the exact two-line permission-denied
record observed in a signed hosted trial, with its admitted QMP prefix.
A complete unknown message is `unclassified`; no raw streams,
paths or arguments enter the receipt. The optional diagnostic supplies no
execution authority and does not change schema 2's positive 29-field shape.

The workflow grants `/dev/kvm` read/write access to the runner's existing primary
group. Linux KVM emits device-change events, and Ubuntu's systemd uaccess policy
can flush named-user ACL entries on those events while preserving named groups.
A one-time user ACL can therefore admit the first VM and fail a later open.
The group grant retains the nonroot controller and leaves world permissions and
device ownership unchanged. The hosted failure's complete stderr identifies a
KVM permission denial; its actual host ACL transition was not exported. The
uaccess explanation is source-backed, and fresh signed two-boot runtime remains
the required integration evidence.

The native guest's fixed bridge profile enables host-loopback publication with
outbound masquerading disabled, under the unchanged three default-drop firewall
hooks. Network regressions require this exact profile and its canonical IPAM
gateway; internal networks, missing/enabled masquerading, extra routing options
and an empty or changed endpoint gateway refuse. The frozen Python fixture keeps
its old internal bridge. A local exact-Docker comparison stopped at a nested
cgroup startup refusal, so it establishes neither HTTP publication nor firewall
enforcement. The fresh signed VM must prove the unchanged launcher's actual
loopback readiness, natural TLS, containment and complete finalization.

Guest feature checks require the project's pinned OpenSSL headers/libraries;
default Ops/xtask bootstrap builds leave native crypto disabled. Signed-material,
actual local TLS, inert-file handoff and maintenance-table firewall tests cover
the new guest's finite contracts. The distributed producer selects the optional
feature and its authenticated 70-archive patched-musl/OpenSSL 4.1.0 static kit.
Inert producer tests do not establish its cold build, final signed static ELF or
fresh guest execution. All Python remains frozen.

The dev/test profile optimizes the pinned `sha2` dependency so full retained-input
checks do not run unoptimized compression. The [runtime proof guide](maintenance-launcher-runtime-proof.md)
records the failed first setup trial, unchanged integrity checks and clocks, and
the measured local before/after workload. Hosted runtime success remains required.

The manual [maintenance prerequisites KVM experiment](maintenance-prerequisites.md)
has two independent free-runner jobs. One observes the exact pinned Caddy image,
naturally generated local TLS material, guest-only public-root trust, fresh-process
reuse and the existing network-none validator with separate 8 MiB `/data` and
`/config` tmpfs mounts. Its guest OUTPUT/FORWARD and
physical egress guards remain until the exact owned VM exits. The other runs the
fixed [PID-namespace topology catalog](maintain-namespace-prototype.md), with 11
named cases and 14 fresh namespaces, original kernel handles and actual adopted
reaps. Neither job proves the other's behavior or complete maintenance success.
The optional workflow has no artifact selector, API token, production host,
required-CI role or installed caller change.

The Caddy 97-case suite and namespace 14-case boundary suite use the maintained strict
Python configuration. These local tests execute no actual helper role, namespace,
KVM, Caddy or TLS. The separately recorded namespace run passed its finite catalog.
The [Caddy run at `4322717`](https://github.com/swiftraccoon/simplestChat/actions/runs/40226216828)
passed its finite TLS/storage prerequisite with cleanup and all VM lifecycle
checks. The operational 900-second launcher and Rust fd0 transport remain unrun.
No application deployment, caller cutover or authenticated Ops distribution is
established by that Caddy-only run.

The separate manual [fixed-launcher runtime fixture](maintenance-launcher-runtime-proof.md)
adds four independent signed-musl guests: positive, dirty source, controlled
cancellation and the actual unchanged 900-second timeout. It joins the exact
installed Ops inode, shallow source tree, native PID-namespace settlement,
PostgreSQL state, naturally generated Caddy materials and known daemon actors.
Its native [API and ownership review](maintain-native-prerequisite-api-review.md)
records the fixed clock profile and compiler/test evidence. Earlier Linux CI ran
the fixed run/init FD/process methods; the latest frozen-guest trial passed Ops33
then failed at rendering. All four fresh signed trials remain required for the
new guest route. This fixture does not replace operational Python or change a
host caller. Run its 30 unchanged reference keeper tests with:

```sh
ops/ansible/.venv/bin/python -B -m unittest discover \
  -s ops/ansible/tests -p 'test_security_vm_maintain_prerequisite.py' -v
```

Run the two earlier local prerequisite suites independently:

```sh
ops/ansible/.venv/bin/python -B -m unittest discover \
  -s ops/ansible/tests -p 'test_caddy*.py' -q
ops/ansible/.venv/bin/python -B -m unittest discover \
  -s ops/ansible/tests -p 'test_maintain_namespace_probe.py' -q
```

The optional jobs keep positive summaries success-only and separately attempt an
exact closed failure-summary artifact under `always()`. Its fixed negative
checkpoint/case/status/error metadata contains no raw streams, keys, work or
configuration. Missing or partial metadata is unavailable evidence; final nonzero
exit invalidates passive bytes and never becomes success through retention.

The e271a8c optional run passed the finite namespace experiment (11 cases / 14
trials) and failed Caddy at a hash-bound `tool_missing` prerequisite refusal.
No Caddy TLS or complete maintenance proof is established. The guest bootstrap
now explicitly installs the separate Docker CLI with the daemon under the same
no-recommends policy. A fixed workflow choice selects `both` (default), `caddy`
or `namespace`; the corrected Caddy-only trial remains pending.

The first corrected Caddy-only run reached containment after tool/image/network
setup and failed with an I/O exception; cleanup passed, but no exact errno/site
was retained. A source-backed Linux DROP `EPERM` marker edge is now handled only
with a fresh exact named counter delta and live capture. Unrelated I/O, stale
counters, close failures and original deadlines remain negative. Fixed local
checkpoints and symbolic errno categories retain future failures without raw
messages or paths. The actual recorded I/O cause and Caddy TLS proof remain
unproven pending a further Caddy-only trial.


The Caddy transport correction retains the internal bridge and all
containment/capture/resource caps. It selects only the fresh owned container's
joined internal IPv4 endpoint, rechecks the same container/image/network/address
around TLS phases, and observes actual port mappings separately from requested
ones. SNI, Host, trust, Caddyfile and natural seven-file material policy remain
unchanged. Direct bridge TLS does not establish the production loopback-published
path. Closed connection/handshake diagnostics contain no raw address or error
text. Source fixtures cover wrong identities, special/foreign addresses, stale
observations and late phase completion, including the old helper's admitted
31-second success under a 30-second phase. No actual engine, TLS or KVM result is
established by this correction; the `f54d8ed` trial remains `tls_not_ready` with
its exact connection cause unknown. The suite also exercises a full
real-file negative-copy loop over exact-owned terminal daemon observations,
refuses ambiguous/foreign terminal identities, and pairs actual local descriptor
closes with SSL refusal. A close error retains the initial SSL exception and
cannot become successful negative proof or a retry. Further timing fixtures
join an initially running copy to a later exact-owned nonzero exit only after
an actual clean phase-live refusal. They reject foreign identity/image/network,
global expiry/cancellation, successful-TLS downgrade and a successful close that
finishes after the TLS phase. Final observed-state metadata uses only remaining guest
body time and the existing command caps, never another TLS window. Versioned
Moby-shaped fixtures distinguish nonzero restart backoff (`Running` and
`Restarting` true, PID0) from live unpaused serving state; backoff is only an
observed refusal and never terminal proof. The fixed `unless-stopped` policy is
unchanged. Negative retry exhaustion performs one fresh joined state query;
positive/fresh TLS paths remain strict.

The pinned Caddy base OCI configuration exposes `2019/tcp` in addition to the
three requested ports. A fixture accepts that admin key only when unbound
(null or empty); every actual admin host binding and every other unknown port
remain refused. Unbound EXPOSE metadata never proves operational publication.

The `8c6b451` Caddy trial reached endpoint admission and refused `endpoint_ipv4`
before TLS, with cleanup/restoration verified. No exact address predicate or
successful TLS is established by that summary. The versioned Moby
[bridge join][moby-gateway-join], [endpoint accessor][moby-gateway-endpoint] and
[inspect update][moby-gateway-inspect] permit an empty inspected container
`Gateway` while IPAM retains its gateway. The fixture accepts only that empty string or the exact
canonical
IPAM gateway, with a distinct closed `endpoint_gateway` refusal for null,
missing, foreign or nonstring values. IPAM canonical/subnet checks and gateway
exclusion from target selection remain unchanged; all live validation is pending.

[moby-gateway-join]:
  https://github.com/moby/moby/blob/v28.2.2/libnetwork/drivers/bridge/bridge_linux.go
[moby-gateway-endpoint]:
  https://github.com/moby/moby/blob/v28.2.2/libnetwork/endpoint_info.go
[moby-gateway-inspect]:
  https://github.com/moby/moby/blob/v28.2.2/daemon/container_operations.go

The post-TLS diagnostic fixtures distinguish a failed endpoint metadata recheck
after a completed TLS exchange, a later original-file open, and the unchanged
material analyzer. They retain actual local originals through admission failure
and checked cleanup, exercise the real metadata-schema refusal, and reject
unknown codes, non-scalar arguments and error subclasses from public projection.
A secondary close refusal cannot replace the first admission error. The three
new checkpoint literals and analyzer-code catalog are checked against the
controller's bounded projection; no arbitrary guest value enters its summary.
The `1381912` actual `unexpected` failure remains unexplained by its historical
TLS `close` checkpoint, and no complete Caddy material/TLS result is claimed.

A real synthetic certificate chain exercises successful analyzer completion and
clears its checkpoint before the next phase. Separate trusted-TLS fixtures
refuse the initial metadata check without stale material context and mark a
post-success endpoint refusal distinctly. These tests add no live authority.

The material fixture constructor also guards each current parent/file descriptor
before metadata or reads. Actual admitted-FD child-open, metadata and read
refusals paired with checked-close errors preserve the first exception and
close every current descriptor before assertions.

The source-bound leaf profile tests use real cryptography-generated signatures
and certificates without BasicConstraints. The retained old-analyzer trial ran
one positive regression and refused `leaf_constraints`; the corrected profile
permits that extension to be absent only on the leaf. Both two- and three-cert
chains pass, while present `ca=true`, missing/unsuitable KU, missing or
client-only EKU, and either authority missing BasicConstraints remain negative.
All other analyzer checks remain exercised unchanged. The actual `f3559e6`
summary reports the grouped code rather than private extension facts, so these
source/inert results do not identify its exact branch or prove a corrected
engine/TLS/KVM trial. See the [versioned source chain](maintenance-prerequisites.md).

## Prepared-host native maintenance

Run the focused local transaction checks with the pinned native dependencies:

```sh
cargo test --locked -p simplestchat-ops --features maintain-guest \
  maintenance_effect_tests -- --test-threads=1
cargo test --locked -p simplestchat-ops stage::reentry -- --test-threads=1
cargo test --locked -p simplestchat-ops distribution::install::tests -- --test-threads=1
cargo test --locked -p xtask ci::ops_artifact::export -- --test-threads=1
```

These checks use real private files, atomic publications, dump/list ownership
and actual flock handoffs with finite synthetic daemon/launcher responses. They
cover reviewed settings and candidate consumption, migration checksum holes
with exact final equality, backup failures before interruption, selected-state
retention after launcher refusal, second-lock contention, changed selected files,
changed database CID and final-ledger mismatch. They do not prove live Docker,
SQL migration, trusted HTTPS or Linux native stream/group behavior.

The separate manual `maintenance-host-runtime.yml` workflow accepts exact
same-head successful CI attempt and signed Ops artifact coordinates:

```sh
gh workflow run maintenance-host-runtime.yml --ref main \
  -f run_id="$CI_RUN" -f run_attempt="$CI_ATTEMPT" -f artifact_id="$OPS_ARTIFACT"
```

Its four fixed cases each own a fresh disposable Debian guest: `host-success`,
`host-launcher-refusal`, `host-migration-upgrade` and `host-proxy-upgrade`.
The host profile has its own fixed 3,780/120-second owner, setup
ceiling 1,560, complete-maintain 1800/60 and remaining post-GO debt 2,040. Setup
includes the existing general preparation plus an actual normal-launcher
bootstrap. The longer test owner is specific to this two-operation fixture;
it does not change the existing prerequisite 1800/120 profile, its 510/1170 split,
the public prerequisite 1050/60 interval, or the public maintain 1800 maximum.
The new job has a 90-minute outer runner limit. Cold acquisition can still refuse
inside its setup ceiling; no worst-case acquisition schedule is claimed to fit.

Whole-operation acceptance requires actual signed-input verification, baseline
live services and trusted TLS, backup publication before stop dispatch, actual
first unlock and fresh guarded acquisition, expected configuration and database
state, and complete native/guest/VM finalization. The refusal case must observe
the real launcher reject the dirty source after selection and keep that selected
state. An expected native refusal does not excuse cleanup or packet observations.
Every required observation, process status and actual invocation/job/workflow
result remains separate from source implementation or saved result JSON.
The previously completed four-case launcher prerequisite is a different proof.

Cold-baseline PostgreSQL refusals can retain the first bounded `hostDiagnostic`
at checkpoint `daemon-pg-history`. The closed reason identifies source/query
derivation, event shape or identity, create/start/die ordering, nonzero exit,
incomplete triplet or command-count failure. It exposes command hashes and
bounded counts only; row indices refer to the original retained event array.
These reasons are valid only at that baseline stage and never on a passed
receipt. An earlier failure retains its checkpoint and diagnostic. The older
launcher-prerequisite output, clocks and acceptance criteria remain unchanged.
Controlled incomplete-health fixtures at both snapshot boundaries continue to
fail through the strict launcher-prerequisite wrapper. The complete host fixture
instead permits a cut health execution only with a unique retained create for
the same container and exact inspected health command. It reconciles the gap and
requires every distinct cold-boundary obligation to end with string exit code
`"0"` before whole-operation success. Tests cover both edges, shared-ID counting,
gap and final completion, and rejected unknown origins, other commands, reused
IDs, nonzero exits and incomplete bootstrap commands. This reconciliation does
not clear other uncertain mutations or renew an owner or observation timer.
Known read-only health completion debt is tracked separately so it does not
bypass or block the existing mutation-admission guard. Terminal daemon uncertainty
includes unresolved cold debt even when a preparation audit has cleared its own
mutation latch; completing health work cannot clear an uncertain mutation.

The host proof now requires the nineteenth field `baselinePgEvidence`, containing
only `openingHealthSeeded`, `trailingHealthDeferred`, `coldHealthObligations` and
`coldHealthCompletions`. The first two are 0–1; the latter two are 0–2, with the
obligations counting distinct IDs and completions no greater than obligations.
The union is bounded by the maximum and sum of the two boundary counts.
Evidence is null before PostgreSQL admission and may be partial after admission
if another baseline check fails. A passed baseline requires nonnull evidence,
and a passed whole operation requires all obligations completed. Producer and
controller use this same closed shape; eighteen-field historical receipts are
retained evidence of their original revision, not accepted new receipts.
Zero counts do not prove the boundary path ran. A fresh signed trial and nonzero
boundary evidence remain separate from controlled tests and diagnostic hashes.

Controlled README dirtiness, launcher status 1 and retained selection establish
the refusal case by comparison with the clean baseline. Those observations do
not identify a unique failing Bash branch; the frozen launcher has silent checks
that can share that exit status.

The host fixture's guest DATA roster remains the 52 rendered-input source files
and 15 helper files. The new Rust host/controller implementation executes from the
current authenticated Ops binary and is additionally bound by the expanded
outer source catalogue; it is not interpreted from those 52 guest DATA files.
The migration case reconstructs a source-bound pre-035 generation: it seeds
member and invitation rows, reverses both real role columns and validated
constraints, and removes only the verified 035/036 ledger rows in one transaction.
Independent reads require the exact source prefix, actual old data and checks
before maintenance, then transformed data, both newly validated checks and the
full source ledger afterward. The redemption role stays unchanged. This tests
actual current migration execution against controlled older schema and data;
it does not run a historical server. Ledger-only deletion is insufficient.

The proxy case starts an actual legacy proxy using the exact historical
Caddyfile from `51e70de6` and a count-checked removal of the asset-store mount.
It independently verifies three legacy binds and no installed receipt before
maintenance. No environment candidate is supplied. The actual upgrade must
validate through the old proxy, publish the current source Caddyfile and receipt,
replace the proxy with the same pinned image and four exact binds, and retain
the complete observed replacement history. Historical configuration bytes do
not establish a historical executable signature.

`upgradeEvidence` preserves nullable pre/post SQL observations and the proxy
generation record; the applicable successful case requires its complete evidence.
Added SQL work caps (30 seconds in setup and 15 in observation) remain within the
existing original setup and 60-second observation ceilings, including settlement.
No timer restarts or worst-case fit guarantee are introduced. Source checks and
synthetic neighbors are separate from signed runtime evidence. Preserve failed
trials and report the exact successful source and case set.
