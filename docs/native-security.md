# Native memory-safety checks

The optional local native-security runner tests the maintained mediasoup worker
in a disposable Linux container. It complements Rust tests, review and dependency
checks: Rust's
`unsafe_code = "forbid"` does not instrument the worker's C and C++ dependencies.
A passing result describes one workload and exact builder image; it does not
establish that the worker has no vulnerabilities.

These sanitizer and finite replay checks are not CI, scheduled-workflow,
publishing, deployment, or complete local-CI requirements. They run only when
selected explicitly on a local machine; CI environments are rejected. The
application's native runtime tests and functional DTLS regressions remain
automated, along with dependency/image audits and authenticated build inputs.

## Maintained commands

Select the vendor suite explicitly through the shared dispatcher:

```sh
build/check-security.sh deep --deep-check native --include-vendor
```

The host runner requires Python 3.12 or newer and a local Docker or
Podman engine; it uses only the Python standard library and maintained helpers.
These component commands diagnose an individual native stage:

```sh
python3 build/native_security.py verify
python3 build/native_security.py prepare --engine docker \
  --output /absolute/private/native-prepare
python3 build/native_security.py run --engine docker \
  --image sha256:THE_FULL_IMAGE_ID_FROM_PREPARE \
  --mode asan --output /absolute/private/native-asan
```

Use the full immutable image ID from the preparation `report.json`. Substitute
`ubsan` or `replay` for those independent checks. The runner accepts only the
reviewed finite workloads. It has no mutation mode, generated-corpus collection,
remote host, URL, socket address or arbitrary-command option.

Every output path must be a new absolute directory. Failures are not retried
automatically and existing evidence is never overwritten. Preserve a failed
run, fix its cause and use a new directory. `--engine podman` selects a local
Linux engine or the already-running `podman-machine-default` VM on macOS after
checking its connection is local. The runner never starts or stops a VM or an
unrelated container.

## Tool and source identity

`security/native/toolchain.json` pins official LLVM 23.1.2 Linux x86-64 and
AArch64 archives by byte count and SHA-256. One bundle supplies Clang,
compiler-rt/libFuzzer and the matching symbolizer. Preparation checks the archive
before extracting the selected compiler/runtime files. The official Zstandard
archive requires a 1 GiB decoding window, which preparation explicitly allows.
[LLVM release](https://github.com/llvm/llvm-project/releases/tag/llvmorg-23.1.2).

The Fedora 44 base uses the same immutable manifest as the release build.
OpenSSL uses the existing checksum-pinned 3.5.9 installer. Invoke, Meson and
Ninja use the worker's hash-locked, wheel-only requirements. Meson downloads
checksum-authenticated wraps during preparation; actual compile/test containers
have no network and use `--wrap-mode=nodownload`. Compiler and FlatBuffers
generator parallelism respects CPU affinity, cgroup quotas and available memory,
with at most four jobs.

The image's input label binds the maintained worker sources, runner, process
helper, Dockerfile, OpenSSL installer, Python locks/constraints, toolchain pins
and corpus. A stale builder fails before execution. Source copying follows the
worker build's allowlist and excludes generated bindings, downloaded build
trees and arbitrary checkout contents.

Preparation also records the resolved image ID. System RPM dependencies resolve
at preparation time; their repository is not frozen by the base-image digest.
The image contains their full package inventory and results record its hash.
Reuse the exact prepared image to reproduce those system dependencies. Rebuilding
a tool image is a new measurement even if the checked-in input hash is unchanged.

## Workloads and limits

| Mode | Existing target | Passing requirement |
| --- | --- | --- |
| `asan` | `test-asan-address` | The enabled worker test executable succeeds with address and leak checks enabled. |
| `ubsan` | `test-asan-undefined` | The enabled worker test executable succeeds with undefined-behavior reporting configured to stop on its first error. |
| `replay` | `fuzzer`, then individual files | Every reviewed input succeeds in its own process with exactly its declared family enabled. |

The maintained tasks remove the reference to the absent upstream
`ubsan_suppressions.txt`; no replacement suppression or blanket waiver is added.
`fuzzer-run-all` fails closed because its external corpora are absent and its
original command had no total runtime bound.

Each actual run has no network, published ports, host mounts, engine socket or
Linux capabilities. It uses a read-only root filesystem and numeric UID/GID
65532. Private tmpfs storage is limited to 5 GiB for `/work` and 256 MiB for
`/tmp`. The container selects one to four CPUs within the host/container budget,
reserving 2 GiB per compiler plus 1 GiB for other processes (9 GiB for four jobs).
It has no additional swap allowance and a 256-process limit. Engine log storage is
disabled; the owner captures independently bounded streams.

The `/work` tmpfs explicitly permits execution because Meson runs compiler
sanity-check executables there before building and running the native tests.
Leaving this implicit makes Docker's non-executable tmpfs default fail setup
with `Permission denied`, before any sanitizer coverage. `/tmp` is explicitly
non-executable; both mounts retain `nosuid` and `nodev`. This workspace permission
does not add host mounts, networking, capabilities or privileges.
[Docker tmpfs mount options](https://docs.docker.com/engine/storage/tmpfs/).

Build/test tasks have a 1,200-second deadline. Replay processes have a 10-second
outer deadline and a 5-second per-input timeout. Reviewed inputs are at most
64 KiB and libFuzzer has a 2 GiB RSS ceiling. The owner also applies a separate
total deadline. Each retained log
stream is limited to 16 MiB. ASan needs a large virtual shadow mapping, so the
runner uses container physical-memory limits instead of an incompatible
`RLIMIT_AS` ceiling. [ASan usage](https://clang.llvm.org/docs/AddressSanitizer.html),
[libFuzzer options and replay](https://llvm.org/docs/LibFuzzer.html).

Cleanup checks the exact run label, full container ID and fresh image identity;
it never deletes by a name prefix. The owned container keeps its private tmpfs
alive during bounded compilation, transfer and execution; its finite idle
lifetime removes it even if the outer client disappears. Each worker subprocess
also retains its own deadline. An uncertain cleanup is a failure, not a passing
receipt.

## Compiled artifacts

The optional local runner can reuse a prepared image and separate instrumented
executables. A compiled artifact contains only a regular ELF binary (at most
512 MiB) and a
receipt marked `built`. Its exact key binds source, tools, flags, corpus, file
modes, native architecture, trust namespace, selected mode and prepared image ID.
No prefix restore is permitted. Both the controller and the offline sandbox
check binary identity before execution. Changed, incomplete or oversized
artifacts fail validation; they never become successful test receipts.

To separate compilation from full runtime locally:

```sh
python3 build/native_security.py compile --engine docker \
  --image sha256:THE_FULL_IMAGE_ID_FROM_PREPARE --mode asan \
  --compiled-directory /absolute/private/asan-built \
  --output /absolute/private/asan-compile
python3 build/native_security.py run --engine docker \
  --image sha256:THE_FULL_IMAGE_ID_FROM_PREPARE --mode asan \
  --compiled-directory /absolute/private/asan-built \
  --output /absolute/private/asan-execution
```

Every run using compiled input still executes the complete selected sanitizer
suite or all 17 reviewed replay inputs. Compilation reports record configure,
generator, compile and install durations; runtime reports record test duration.
A successful build is reusable even if a later runtime check fails. A compiled
artifact is never a successful test verdict. Cold source/tool changes still
require compilation; report the actual selected workload and measured runtime.

## Reviewed inputs and coverage boundaries

`security/native/corpus.json` lists every hexadecimal fixture, decoded size,
SHA-256, provenance and target family: STUN, DTLS, SCTP, RTP, RTCP, codecs or
utilities. Verification rejects missing families, duplicate IDs, unlisted files,
path traversal, symlinks, malformed encoding and changed bytes. The small corpus
contains original synthetic format headers and bounded edge cases, with no
captured user traffic, private keys, bearer tokens or known exploit payloads.
New fixtures require an explicit manifest review.

The published worker crate omits the three `test/data/packet*.raw` files needed
by its RTP unit tests. Three corpus entries supply original synthetic packets
matching those tests' unchanged header, extension, payload-size and padding
expectations, with zero-filled payloads instead of captured media. The runner
materializes only the three fixed filenames and appends the terminator that the
upstream file reader omits. It neither skips these tests nor changes their
assertions. The same packet bytes also run through finite parser replay.

Individual libFuzzer filenames select finite saved-input replay. The runner
never supplies mutation directories. The upstream family selectors treat even
`MS_FUZZ_*=0` as enabled, so the runner supplies exactly one family variable and
drops ambient test tags, sanitizer options and Meson arguments.

These seeds establish input coverage, not branch coverage of every parser.
The DTLS harness creates random cryptographic values and retains state inside
a process; a fixed libFuzzer seed does not make its entire trajectory
deterministic. Replay starts a new process for each input. Listener callbacks
use in-process mocks, so this does not replace browser/media integration tests.

The selected targets instrument the worker and eligible Meson dependencies.
The separately built static OpenSSL archive is not sanitizer-instrumented, and
the receipt explicitly records that boundary. UBSan's `undefined` group does
not contain every available check. Linux is canonical because upstream ASan
initialization-order options are unsupported on macOS.
[UBSan check groups](https://clang.llvm.org/docs/UndefinedBehaviorSanitizer.html),
[ASan platform limits](https://clang.llvm.org/docs/AddressSanitizer.html).

## Evidence and validation

Preparation writes `image.id`, bounded logs and `report.json`. Runs write
`ownership.json` before container creation, separate bounded native logs, and a
final report identifying the selected mode, completed cases/families, elapsed
time, image/input hashes and cleanup conclusion. Missing results or requested
cases fail even when the container exits zero. Failed receipts never count as
completed security coverage.

Local evidence retains the two bounded native diagnostic streams, including on
failure. These streams come from the offline container:
it receives only the reviewed public source, tools and synthetic corpus, with no
host mounts, credentials or network. They can include compiler source excerpts
and sanitizer diagnostics. Keep them with the private local evidence; they are
not automatically uploaded by CI. A failed
host receipt also records the container exit status.

`ops/ansible/tests/test_native_security.py` verifies corpus, immutable-image,
environment, sandbox, timeout, exclusive-output and ownership guards offline.
These tests prove orchestration behavior only. Real Linux sanitizer and corpus
runs are required before reporting native validation.
