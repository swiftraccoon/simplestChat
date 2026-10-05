# Targeted mutation checks

`build/security_mutation.py` measures whether the existing pure-policy tests
detect deliberate changes to the actual application code. It builds the complete
Rust library in a private source copy, then runs only the tests selected by
[policy.json](policy.json). It starts no chat server, database, browser, load
generator or remote workload.

The maintained scope covers the role permission methods, Unicode name comparison
and reserved-name validation, and the common-password predicate. The selected
tests include the independently specified role authorization matrix and seeded
Unicode properties. This is a test-quality check for those functions; a passing
score does not establish application-wide security or protocol correctness.

## Prerequisites and execution

Use the pinned Rust toolchain and native prerequisites from the
[development guide](../../docs/development.md#native-and-web-build). Prepare the
checksum-pinned static OpenSSL installation and the locked Cargo dependency
cache before starting. The runner uses `--locked --offline` for Cargo and does
not install tools or dependencies on failure. Native build scripts retain their
normal reviewed dependency acquisition behavior; this helper is not a network
sandbox. The selected tests themselves perform no service or network I/O.

The shared entry point runs fast first-party source checks before the selected deep
component and installs the checksum-verified mutation tool:

```sh
build/check-security.sh deep --deep-check mutation \
  --openssl-prefix "$PWD/target/openssl-4.0.3"
```

`deep` defaults to `--deep-check all` and runs application mutations without
vendor source or sanitizer/replay analysis. Optional local `--include-vendor`
adds those vendor checks; `--deep-check native --include-vendor` selects only
the native component after the shared source gate. Vendor opt-in is rejected in
CI environments. `--deep-check mutation` selects the application mutation scope
explicitly and remains the scheduled CI command.
The shared OpenSSL argument defaults to `OPENSSL_DIR` when set, otherwise
`target/openssl-4.0.3` in the checkout.

For a focused local iteration, install the same tool and invoke the maintained
helper directly with a **new** private output directory:

```sh
python3 build/security_tools.py install --tools cargo-mutants \
  --directory "$PWD/target/mutation-tools-local"
python3 build/security_mutation.py \
  --tools-directory "$PWD/target/mutation-tools-local" \
  --openssl-prefix "$PWD/target/openssl-4.0.3" \
  --output results/mutation-policy-local
```

Use a separate tool directory from the fast or image scanners: each installed
receipt binds one exact tool set and cannot be extended in place.

The shared tool lock pins [cargo-mutants 27.1.0](https://github.com/sourcefrog/cargo-mutants/releases/tag/v27.1.0)
and its official Linux x86-64 and macOS x86-64 release archives. Upstream does not
publish an ARM executable for this version. A Mac with an explicitly configured
x86-64 execution environment can select the reviewed binary with
`--platform darwin-x86_64` during installation and
`--tool-platform darwin-x86_64` during execution. There is no automatic platform
substitution, PATH-based scanner fallback or build-from-source installer.
The shared entry point names this explicit selector
`--mutation-tool-platform darwin-x86_64`.

The command exits unsuccessfully when prerequisites, compilation, test execution,
coverage verification or cleanup fail. Read `outcome.json` before interpreting
any partial results. Reuse neither a previous output directory nor its private
target directory.

## Selection and bounds

The runner first enumerates every mutation of the exact reviewed functions.
Each selected function must produce at least one mutation. The current policy
allows at most 64 mutations and requires at least 14 completed tests for both
the baseline and each assertion-killed mutant. It rejects an empty, duplicated,
foreign or oversized inventory instead of sampling or silently skipping entries.
Changing the function list or maximum requires reviewing the policy and tests.

Mutations run serially and in a deterministic order. The private copy shares one
private Cargo target across the baseline and mutants, with two Cargo build jobs,
two mediasoup native build jobs and one test thread. The private test profile
disables debug symbols to avoid accumulating macOS split-debug objects for every
mutant; this does not change assertions or the repository's normal build profile.
Each build has a 900-second
deadline, each test invocation 30 seconds, and the complete run 3,600 seconds.
The outer process boundary bounds stdout and stderr separately to 16 MiB and
terminates the owned process group on timeout or overflow. The pinned mutation
tool handles termination of the Cargo processes it starts. Interrupted runs
remain failures; the helper does not resume or retry them.

The source copy is limited to 20,000 tracked or nonignored files, 16 MiB per
source file and 128 MiB total. Links and unsupported file types are rejected.
During compilation, the runner checks the private source/target tree against a
20 GiB ceiling and mutation reports against a 512 MiB ceiling. Each tree permits
at most 200,000 files and directories, with at least 1 GiB free space required.
An unconditional final scan catches growth since the last periodic check.
These are polled limits, not filesystem quotas or memory limits. Use a suitably
bounded CI runner for stronger host isolation.

Mutation builds cap Rust lints at warnings only inside the disposable copy:
replacing a function body can otherwise fail on intentionally unused arguments.
Type errors and other compilation failures still fail the quality check. Normal
source lint and compilation gates retain the repository's deny-warning policy.

## Verdict and evidence

A pass requires a successful unmutated baseline, the complete enumerated
inventory, and test assertions detecting every mutant. Each caught mutation
must have a successful build, Rust test failure status, and a nonzero failed
test count. Zero-test runs, ignored tests, missing results, crashes, compiler
errors, timeouts and unviable mutations cannot count as detected mutations.
The helper also checks the tool exit status independently of the report.

The output retains:

- `outcome.json`: final verdict, tool/policy/source identities and fixed failure
  code when verification fails.
- `inventory.json` and `summary.json`: the selected mutations and verified
  per-mutation results. A failure before final verification can omit the summary.
- `budget.json`: observed private workspace/report sizes and entry counts, free
  space, and the exact budget failure when a resource ceiling stops the run.
- `source-manifest.json`: the SHA-256 of every copied source file; dirty source
  is represented by its actual bytes, not claimed to be a release revision.
- Bounded command logs and `results/mutants.out/`: the pinned tool's outcomes,
  per-scenario logs and mutation diffs.

The copied source and private compilation target are removed on completion or
handled failure. Only the new output directory is owned by the runner. Reports
can contain source and failure excerpts; keep raw evidence private. CI can
upload the compact outcome, summary and inventory without promoting compiled
artifacts or caches into a release.

## CI integration

Run this optional check on a dedicated Linux x86-64 scheduled or manually invoked
job after the canonical native-toolchain setup and locked dependency fetch. Use
the shared `deep --deep-check mutation` entry point, a fresh `RUNNER_TEMP` output
directory, and a 75-minute job deadline above the helper's one-hour ceiling.
No deployment credentials,
database service or application port is required. Keep the job separate from
fast source checks and ordinary correctness tests; it measures their ability to
detect selected implementation changes rather than replacing them.

When a mutation survives, retain its exact diff and add a test that independently
describes the required behavior. If a mutation is provably equivalent, review
the selected scope explicitly; the runner has no blanket exclusion, automatic
waiver or success-by-timeout mechanism.
