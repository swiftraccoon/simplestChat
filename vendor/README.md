# Maintained Cargo patches

This directory contains source for two exact crates.io releases that need
small, auditable security and interoperability fixes, plus a pinned password
selection corpus. The corpus and its MIT license/provenance are documented in
[seclists-passwords/README.md](seclists-passwords/README.md). The two Cargo packages keep their
upstream names and versions unchanged: Cargo's
`[patch.crates-io]` mechanism records that the source is local without
pretending this is a new upstream release.

Source scanning of this directory and native sanitizer/replay analysis are
optional local checks, enabled explicitly with `--include-vendor` through the
[security entry points](../docs/security.md). CI, scheduled workflows,
deployment, and the complete local CI gate do not require or run them.
Dependency advisories, shipped-image vulnerability/license/secret checks,
source provenance, build integrity, and functional native/application tests
remain enforced. The integrity verifier below authenticates build inputs; it
does not perform a vulnerability or source-pattern scan.

## Provenance

| Directory | crates.io archive | SHA-256 | Upstream VCS revision |
| --- | --- | --- | --- |
| `mediasoup-0.29.0` | `https://crates.io/api/v1/crates/mediasoup/0.29.0/download` | `a347fa0a1bd3d195233b09d41c55b5c70562b0006ab4b4c3c60c89dba2d8c5ef` | `bf70a2eceba77b5329cbe6833bab00021e3d170c` (`rust/`) |
| `mediasoup-sys-0.19.0` | `https://crates.io/api/v1/crates/mediasoup-sys/0.19.0/download` | `85eae93b38f8840f30ab93afb2d5a45b2b087237afd3464dc086a250e58e196f` | `16ec8ab2a1eb4a0dc406c6799fa5b6dd0d767d91` (`worker/`) |

The VCS revisions above are the values embedded by crates.io in each
`.cargo_vcs_info.json`.  The original license and third-party license files
from both archives are retained in place.

### Automated integrity verification

[`integrity.json`](integrity.json) is the machine-checked provenance contract.
Run the verifier before compiling downloaded or vendored native code:

```bash
vendor_check="$(mktemp -d "${TMPDIR:-/tmp}/simplestchat-vendor.XXXXXXXX")"
python3 build/security_vendor.py verify \
  --cache "$vendor_check/cache" --output "$vendor_check/evidence"
```

The helper requires Python 3.12 or newer and uses only the standard library and
the repository's bounded-process helper. Both cache and evidence directories
must belong to the caller and be private (`0700`). The evidence directory must
be new. Reuse a private cache on later runs and pass `--offline` to prohibit
network access; missing or corrupt cached bytes then fail verification. Cache
filenames are the complete source SHA-256, and every use checks the bytes again.

Each `sources` record identifies one HTTPS URL, SHA-256 and format. The verifier
authenticates every source before parsing an archive. It never extracts archives
or executes imported code. Downloads have a 120-second process deadline and a
128 MiB compressed-size limit; archive members, total expansion, file counts and
retained diffs have separate limits. Redirects must remain on HTTPS. Tar/ZIP
links, special files, duplicate names, ambiguous paths and directory collisions
are rejected. These checks authenticate previously reviewed bytes; they do not
establish publisher identity or prove that imported source is safe.

For each `trees` entry, every upstream and maintained file is compared, including
hidden files. An unchanged file must be byte-for-byte identical. Every added,
modified or deleted file must have exactly one `changes` entry containing its
upstream and maintained SHA-256; `null` means that the file is absent on that
side. A stale entry fails as well as an unlisted change. The three local WrapDB
overlays also have independent archive comparisons, preserving their provenance
alongside their inclusion in the worker's maintained patch. The SecLists data
and license use exact raw-file sources. Explicitly listed repository-authored
README files, the structurally checked `native-components.json`, and the integrity
manifest itself are the only metadata exceptions.

The complete discovered set of Meson `.wrap` files must match `wraps`. Every wrap
must use `wrap-file` with the recorded HTTPS source URL and SHA-256. Remote patches
require their own source record and hash; local overlays must exist inside the
checked vendor tree. Source fallback URLs must be explicitly recorded and remain
bound to the same hash. VCS wraps, missing hashes, additional download fields and
unlisted dependencies fail verification. This source inventory includes native
test and platform-specific dependencies; it does not claim that every entry is
linked into the production binary.

A successful run writes deterministic `report.json` and `vendor.diff`. The report
binds the manifest, upstream sources, complete per-file comparisons and diff by
SHA-256. The diff retains every textual deviation; binary deviations retain both
complete byte sequences as base64. Completed comparisons that detect file drift retain the diff and a
`failure.json` but never a success report. Keep these artifacts with the security
run for review. For an intentional update, authenticate the new upstream source,
review the complete changes, amend each affected source/deviation record and the
explanation below, then rerun verification and relevant native/application tests.
There is no command that automatically approves the current working tree.

### Native build evidence

[`native-components.json`](native-components.json) classifies the complete native
source inventory and binds its source IDs to the integrity manifest. Production
worker inputs, header-only code, native test dependencies and Windows-only inputs
have explicit roles. Adapted libwebrtc retains its branch and revision instead of
claiming to be an unmodified upstream release. AWS-LC records its locked Rust
wrapper separately from the external 5.11.0
release actually linked. The checksum-pinned installer builds a static, namespaced
`libcrypto-awslc.a` and generates matching Rust bindings with the authenticated
bindgen CLI and its locked dependencies. The wrapper's supported system mode
retains its native-version check; its older bundled source is identified as unused.
The inventory also binds the external source LICENSE digest. OpenSSL records the
exact source archive and the checksum-verifying installer. Native license
expressions are reviewed source metadata; libuv's additional BSD/ISC components
and AWS-LC's composite expression remain explicit.

The production Fedora builder retains the final successful default-feature Cargo
invocation with `--message-format=json`. After building, it runs:

```bash
python3 build/security_native.py --root /app \
  --vendor-report /app/vendor-evidence/report.json \
  --cargo-messages /app/cargo-build.json --cargo-home /root/.cargo \
  --openssl-prefix /opt/openssl-4.0.3 --aws-lc-prefix /opt/aws-lc-5.11.0 \
  --output /app/native-components.build.json
```

The helper rechecks the vendor receipt against current files, selects the actual
production server artifact, and resolves static archives only from the recorded
build-script link search paths. Missing, ambiguous, thin, unexpected or incorrectly
located native archives fail. It verifies the AWS-LC crate checksum before
comparing every unpacked source file; Cargo's exact completion marker is the only
unpacked-cache exception. It independently authenticates the external AWS-LC
release and generator archives, verifies installed headers and the native license
against source, checks configured static/no-provider/no-host-crypto-policy options,
and binds Cargo's generated bindings and linked archive to the installed receipt.
The image policy requires that same source, binding and archive identity.
The receipt binds the final executable, static archive
bytes, Cargo messages, Cargo lockfile, vendor source/patch records, compiler
identity and builder RPM/source-RPM inventory. The static C++ runtime must belong
to the recorded `libstdc++-static` package.

`rust_licenses` covers the union of packages in that actual Cargo invocation and
the dependency graph embedded in the final executable. Each record distinguishes
compiler-artifact evidence from embedded-metadata evidence; embedded-only
packages remain subject to policy without claiming they contributed machine code.
Registry
license declarations come from checksum-authenticated crate manifests; a
`license-file` retains its exact content hash. Local patched packages must belong
to verified vendor trees. No license is guessed from a filename or package name.
The unpublished first-party application intentionally has no declared distribution
license; its record is marked `first_party` and preserves that missing value.
Image license policy selects runtime packages by their exact embedded Cargo
identity, rather than treating build-time packages or the whole lockfile as
runtime dependencies. The C++ runtime license comes from its owning RPM metadata.

The image retains the receipt at
`/usr/share/simplestchat/native-components.json`. An archive hash identifies a
build input; it does not prove that every archive member survives final linking.
Header-only and adapted source records are likewise source evidence. Separate
image checks verify ELF hardening, dynamic dependencies and the exact executable
hash; runtime-loaded components and deployed protections need separate evidence.

## Maintained changes

The 2026-10-05 rebase retains the maintained worker behavior against the
0.19.0 archive while upgrading its native dependencies and Rust bindings.
Timer integrations now provide diagnostic labels and use the
upstream signed millisecond API; the manual timer fixture implements that same
contract. NACK delay and congestion constraints retain their prior behavior
with the widened upstream types. The upstream archive supplies its new RTP/RTX
encoding and SCTP reassembly fixes; those are not local patches.

`mediasoup-0.29.0` raises `lru` from 0.8.1 to 0.18.5 in `Cargo.toml` and
`Cargo.toml.orig`. The worker uses the compatible `new`, `contains`, and `put`
API surface; this removes RUSTSEC-2026-0253 from the active graph. The same
manifests upgrade Planus to 1.3.0 and the async-channel, async-lock, fastrand,
futures-lite and thiserror dependencies to their current major versions.

`mediasoup-sys-0.19.0` changes only `Cargo.toml`, `Cargo.toml.orig`,
`build.rs`, `tasks.py`, `scripts/get-dep.sh`, `meson.build`, `deps/libwebrtc/meson.build`,
`src/RTC/TransportCongestionControlClient.cpp`, `src/RTC/RTP/RtpStreamRecv.cpp`,
`include/RTC/Producer.hpp`, `src/RTC/Producer.cpp`, `src/RTC/Transport.cpp`,
`include/RTC/DtlsTransport.hpp`, `src/RTC/DtlsTransport.cpp`,
`src/RTC/WebRtcTransport.cpp`, `test/src/RTC/TestDtlsTransport.cpp`,
`include/RTC/RTP/RtpStreamRecv.hpp`, `include/RTC/MediaDiagnosticId.hpp`,
`include/RTC/MediaDiagnostics.hpp`, `include/RTC/WebRtcServer.hpp`,
`src/RTC/WebRtcServer.cpp`, `test/src/RTC/TestMediaDiagnostics.cpp`,
`deps/libwebrtc/libwebrtc/modules/congestion_controller/rtp/transport_feedback_adapter.cc`,
`subprojects/abseil-cpp.wrap`, `subprojects/flatbuffers.wrap`, `subprojects/libuv.wrap`,
`subprojects/unordered-dense.wrap`, `subprojects/catch2.wrap`, the two files under
each of `subprojects/packagefiles/abseil-cpp/` and
`subprojects/packagefiles/flatbuffers/`, the four files under
`subprojects/packagefiles/libuv/`, and
`subprojects/packagefiles/ankerl-unordered-dense/meson.build`, and removes the package-local
`Cargo.lock` and `subprojects/openssl.wrap`. It adds the two reviewed
`python-invoke-requirements.txt` and `python-tools-requirements.txt` wheel locks.

The C++ change in `TransportCongestionControlClient::SetDesiredBitrate`
bounds the congestion controller's start bitrate by the configured minimum
outgoing bitrate (`std::max(minBitrate, availableBitrate)`) instead of only
the built-in 30 kbit/s floor. libwebrtc's `GoogCcNetworkController::
ClampConstraints` already raises a start rate below the minimum, but logs an
error each time; with the application's 100 kbit/s floor that line repeated
on every bitrate update of a transport whose estimate had decayed to the
floor (941 lines in one 100-client run). Behaviour is unchanged apart from the
log line. Drop the change when upstream bounds the start bitrate itself.

libwebrtc's `TransportFeedbackAdapter` keeps one entry (about 150 bytes) per
sent packet until the receiver's transport-cc feedback covers it or the entry
ages out of `kSendTimeHistoryWindowMs`, upstream 60 s. An SFU keeps one adapter
per receive transport, so while inbound feedback is lost (an overloaded worker
dropping datagrams at its socket) the histories of a 240-publisher room grew the
process by about a gibibyte in one minute and held it (see the 2026-09-25
sections of `docs/performance-results.md`). The maintained copy uses 10 s:
feedback older than that is useless to the estimator, and the worst case
becomes a sixth of upstream's. Drop the change if upstream bounds the history
by bytes or shortens the window.

`RtpStreamRecv::ReceiveRtxPacket` updates the primary sequence state only when
RTX carries a newer original packet, using the worker's wrap-aware comparison.
Older RTX goes through the existing NACK generator, which accepts a still-missing
packet once and rejects duplicates. Applying the primary stream's 1,500-packet
misorder limit first rejected legitimate requested repairs, counted old duplicate
probes as discarded media, and allowed two consecutive old originals to trigger
a spurious sequence reset. This is a packet-processing fix; warning levels and
primary RTP, RTX-header, and large-forward-jump validation remain unchanged.
Transport congestion feedback still sees each probe before producer processing.

Browsers can send old payloads in RTX for bandwidth probing (see libwebrtc's
[`GeneratePadding`](https://webrtc.googlesource.com/src/+/main/modules/rtp_rtcp/source/rtp_sender.cc)
and [`GetPayloadPaddingPacket`](https://webrtc.googlesource.com/src/+/main/modules/rtp_rtcp/source/rtp_packet_history.cc)).
A local Firefox/Chromium call reproduced 765 paired sequence/RTX warnings with
no sender NACK requests or receiver freezes. The regressions in
`src/media/rtx_tests.rs` send actual RTP through the native worker's direct
transport: stale probes and late requested repairs fail against the original
worker, including tests of wraparound, duplicate recovery, newer RTX, and retained
primary/forward-jump rejection. Run them with
`cargo test --locked --all-features --lib media::rtx_tests -- --test-threads=1`
after the documented native setup, and verify real browser decoding separately.
Drop this patch when an upstream release handles old RTX without applying primary
restart detection.

`Producer::ReceiveRtpPacket` recognizes valid padding-only RTX on a negotiated
encoding before its primary RTP stream exists. Three owned two-Chromium startup
runs reproduced one warning each; native debug identified the repaired-RID
lookup before primary stream creation, and bounded packet metadata confirmed
RTX payload type 97, zero payload bytes and 255 padding bytes. This carries no
encoded media to recover. The transport already feeds the packet to congestion
feedback; the patch accounts for its RTX bytes without treating it as an unknown
stream. It preserves the existing cleanup of SRTP state that no media stream
owns yet, and does not create a media stream from padding. Actual repair
payloads before primary, empty non-padding packets,
unknown encodings and invalid packets retain their existing rejection paths.
The direct-transport regressions in `src/media/rtx_tests.rs` cover SSRC, repaired
RID and single-encoding lookup, padding boundaries, subsequent media/RTX and
negative cases. The accepted-padding accounting regression fails against the
unpatched producer. Drop this patch when upstream distinguishes startup RTX
padding from an unknown media stream.

`DtlsTransport` retains a fixed close reason before `SSL_clear` erases the
OpenSSL state. Established, fingerprint-verified connections ending with
`SSL_ERROR_ZERO_RETURN` are recorded as an orderly peer protocol close at debug
level. This does not imply that the user intended to leave. SSL and syscall
errors take precedence over a received-shutdown flag and remain warnings/errors,
as do alerts before the connection is established, fingerprint and SRTP
negotiation failures, and handshake/timer failures. `WebRtcTransport` includes
the fixed reason in failure logs. Existing listener signatures, FlatBuffers
notifications, CLOSED/FAILED states and receive-state cleanup are unchanged.

The isolated native tests in `test/src/RTC/TestDtlsTransport.cpp` exchange real
OpenSSL DTLS records between two in-process peers. They cover encrypted orderly
close, reset and clearing the reason on a fresh run, failure precedence with a
shutdown flag, pre-verification close, fingerprint rejection, incompatible SRTP
profiles and handshake timeout. Run `build/check-native-dtls.sh` after installing
the pinned OpenSSL build. It builds the fixed `[dtls]` test group in a private
source/output directory and separately checks that orderly close emits no
warning/error. Enabling `ms_build_tests` changes worker compile definitions and
must not reuse Cargo's production worker build.

A local two-Chromium comparison explicitly closed established peer connections
while signaling remained open: the same two application DTLS Closed callbacks
occurred before and after the patch, while six native warning lines became zero.
Both runs decoded 300 frames over 15 seconds with zero media loss or freezes.
Drop this patch when upstream preserves equivalent failure classification and
logs orderly protocol shutdown without a warning.

Native RTP inactivity warnings retain their severity and score behavior, while
adding bounded private producer/transport UUIDs, encoding index, media kind,
worker time and last accepted non-padding media/pause/resume timestamps. UUID
fields accept only the generated lowercase UUID representation; arbitrary text,
RIDs, addresses, ICE credentials and packet contents are not logged. The existing
inactivity timer is enabled only for multi-encoding simulcast producers. Its
activity observation adds one cached-clock scalar assignment when accepted media
already restarts that timer; it never changes packet acceptance or scoring.

Unknown-tuple warnings classify only the packet header family and an optional
historical UDP-tuple match. Each WebRtcServer retains at most 256 removed UDP
tuples, with a 30-second lookup window, owned address storage and bounded UUID metadata.
Registering the tuple again removes its history. A match names the previous
transport and elapsed removal time, not an authenticated sender or an expected
shutdown. TCP remains unattributed because its tuple key is a recyclable object
pointer. Live routing entries and packet acceptance are unchanged. The first
event in each of eight fixed family/history classes is logged immediately; a
10-second one-shot timer reports coalesced counts even after a finite burst ends.
Server destruction flushes pending counts and removes its timer. No packet bytes
or peer addresses enter these logs, and normal routed packets do no new lookup.

`build/check-native-dtls.sh` compiles the `[dtls]` and `[media-diagnostics]`
groups together in its private worker build. Fake-clock regressions cover timer
expiry, padding exclusion, recovery and pause/resume behavior, missing activity,
DTX timeout, owned tuple storage, expiry/reuse/capacity, explicit TCP non-attribution,
finite-burst summaries and timer destruction. The helper also checks that a real
native inactivity warning contains its expected fixture identity and timestamps.
Public authenticated media snapshots expose only salted entity references; their
version-2 stream fields add native score and a nullable producer encoding index.
Drop the native diagnostic patch when upstream provides equivalent bounded,
privacy-preserving evidence; production warning classification still requires
correlated observations, not these diagnostics alone.

Cargo ignores a dependency's
nested lockfile, so removing that generated package artifact does not change
workspace resolution. The replacement build:

- requires pkg-config to find OpenSSL 4.0.3 or newer in the reviewed 4.0 series
  (versions before 4.1.0);
- forbids Meson fallback to a bundled copy;
- requires static `libssl` and `libcrypto`; and
- makes libsrtp use the same resolved dependency.

The pinned source is OpenSSL 4.0.3, released 29 September 2026. The update follows
the [OpenSSL security advisory](https://openssl-library.org/news/secadv/20260929.txt),
including the High-severity DTLS issue CVE-2026-84782. The official source archive
SHA-256 is `325b5c806167c13b40b1ffeadfe0248197c00eccc4cf123ec1e28d2d2fd216d9`;
the installer verifies it before unpacking, and `native-components.json` binds
both the source and installer hashes. The release PGP signature was verified
against the official primary fingerprint
`B146647E45A7B33947AB226B2A2C87D161692D40`. Cargo and Meson reject earlier 4.0 releases
and other release lines. Updating the source pin requires rebuilding the worker,
Rust executables, native test image and production/load-generator images;
previous binaries and historical scan receipts do not acquire the fix.
OpenSSL 4 makes certificate subject-name access immutable. The maintained worker
now constructs an owned name, copies it into the subject and issuer, and frees it
on both success and failure. The native DTLS regression verifies both fields,
the self signature and the existing real client/server handshakes.

The maintained installer also configures `no-dso`, `no-module` and `no-engine`
alongside `no-shared`. Dynamic provider and engine loading is disabled in these
static libraries. Default and base providers remain built in; OpenSSL also builds
the legacy provider in when modules are disabled, so this does **not** remove
legacy algorithms. These settings follow the authenticated 4.0.3 source's
`Configure`, `providers/build.info` and `INSTALL.md` semantics.

The installer records `shared`, `dso`, `module` and `engine` from the completed
build's `configdata.pm` disabled map. Native evidence requires that exact installed
record, `OPENSSL_NO_DSO` and `OPENSSL_NO_ENGINE` in `configuration.h`, and the
declared version header. There is intentionally no `OPENSSL_NO_MODULE` macro.
The receipt binds those files, the reviewed source/installer/options and both
actual static libraries; mediasoup and Rust must link identical `ssl`/`crypto`
archive hashes. Image consumption requires this current receipt contract.

The build script copies the crate's allowlisted package inputs into a fresh
Cargo `OUT_DIR` snapshot and runs Meson there. It tracks the complete immutable
crate source, the relevant declared tool/compiler and pip environment, resolved
Python configuration files, and the supported `OPENSSL_DIR` headers, pkg-config
metadata, and static archives. Each build-script attempt removes only its own
prior snapshot, native build directory, Python tool directory, and worker
archive before rebuilding.
On Unix, `/dev/null` is excluded from configuration-file timestamp tracking:
`PIP_CONFIG_FILE=/dev/null` disables external pip configuration, and writes to
the device do not change configuration contents. The environment variable and
all genuine configuration/certificate file paths remain tracked.
This makes source/wrap changes and interrupted builds deterministic without
letting Meson download or extract files into the maintained vendor tree. Keep
`PACKAGE_SOURCE_PATHS` in `build.rs` synchronized with `package.include` in
`Cargo.toml` when updating the crate.

Non-doc builds fail closed unless `PIP_CONSTRAINT` names an existing file. The
repository uses `build/pip-constraints.txt`; CI, containers, checked-in VS Code
settings, deployment scripts, and the README build examples set it explicitly
so PyPI build tools cannot float between otherwise identical Cargo builds.
The maintained `tasks.py` also selects Meson 1.12.1 and Ninja 1.13.2,
matching the exact constraints. `python-invoke-requirements.txt` and
`python-tools-requirements.txt` pin the accepted PyPI wheel SHA-256 hashes for
Invoke, pip, setuptools, Meson and Ninja. `build.rs` and `tasks.py` install with
`--require-hashes --only-binary=:all:`; unsupported platforms fail instead of
running an unreviewed source build. These lock files are included in the package
manifest and immutable source snapshot. Hashes were obtained from the versioned
PyPI release metadata; review wheel changes and verify a clean native rebuild
when refreshing them. This authenticates previously reviewed bytes, not package
publisher identity or the absence of malicious code.

The optional upstream Docker tasks are disabled: this crate does not contain
their Dockerfiles, and the old helpers could pull mutable registry images with
privileged host access. `scripts/get-dep.sh` is also disabled because it imported
a mutable fuzzer branch and rewrote the checkout. Use the repository Dockerfile
for supported builds and reviewed immutable inputs for dependency refreshes.

The Abseil wrap is pinned to the official
[`20260817.0` LTS release](https://github.com/abseil/abseil-cpp/releases/tag/20260817.0),
whose archive SHA-256 is
`f7e05179df39c45434cad433f5783840bb3788ef322976f9138bc6b72b3a107d`.
The checksum matches the GitHub release asset metadata. Its maintained Meson
overlay derives from the WrapDB `20240722.0-4` patch archive (SHA-256
`e39d535c4707f6e342e84e3e616449e1cc98cb7fadda92a09820b0ae67c6d0d6`).
`LICENSE.build` remains byte-for-byte unchanged. The overlay updates the project
version, removes the 25 source/header paths removed upstream, and includes the
new production compilation units selected by the release's CMake library lists.
These cover moved exception/CPU helpers, hardening/tracing, entropy pools,
profiling, structured logging, formatting, status, clock and source-location
support. Abseil source is not patched. The local overlay avoids extracting the
older WrapDB root into a different release directory; the old source fallback
remains removed.

The FlatBuffers source and C++ schema compiler use the official
[`v25.12.19-2026-02-06-03fffb2` release](https://github.com/google/flatbuffers/releases/tag/v25.12.19-2026-02-06-03fffb2),
commit `03fffb25e2d777462b719cb4964249c30b19d58f`, archive SHA-256
`ccbce58684691de1e7d51f5e87786266b37d06ab66e9dfe2d0ec106fe50aace0`.
On 2026-10-05 the official release API marks this as latest, non-draft and
non-prerelease. Its version header remains 25.12.19; the exact source tag includes
24 subsequent upstream commits. The local Meson overlay derives from the
previous authenticated `flatbuffers_24.3.25-1` WrapDB archive (SHA-256
`9be75a2053a19e5a59175f2fbbf6e9d40f4243d2786f2661a131d3502ddfa457`), preserving
`LICENSE.build`. It replaces deleted file-writer compilation units with the
release's file/name managers and adds the Python generator implementation.
Both C++ headers and `flatc` are built from the same release, and all 28 worker
schemas are regenerated with the existing wire format and compiler options.

Both Rust crates use Planus 1.3.0. The worker build uses matching 1.3.0 translation
and code-generator crates, explicitly requesting formatted generated Rust.
The Rust and C++ generators consume the same unchanged schemas; real worker
requests, responses and notifications must pass the application media tests
when either generator is updated.

The repository's pinned OpenSSL build helper supplies the dependency in Linux
CI and container builds.  Do not relax the minimum version or restore an
OpenSSL wrap to make a build pass.

## Native dependency refresh (2026-10-05)

The following source archives are pinned to their verified upstream downloads:

| Dependency | Source archive | SHA-256 |
| --- | --- | --- |
| libuv 1.53.0 | [official distribution](https://dist.libuv.org/dist/v1.53.0/libuv-v1.53.0.tar.gz) | `cb0d6dd2128d5a95bd242c6cc982a24fe608fa93da57b6b4ec763b0018c53e64` |
| unordered_dense 5.3.1 | [official tag archive](https://github.com/martinus/unordered_dense/archive/refs/tags/v5.3.1.tar.gz) | `06c262f9d7e1ff94d92e0359f89cc5d8632f32dfffb5e176a10516c509f5e5f2` |
| Catch2 3.16.0 | [official tag archive](https://github.com/catchorg/Catch2/archive/v3.16.0.tar.gz) | `0957cae5821b17ce07f0833aaa52b5137643a8382203221f363a8303c109af34` |

libuv's local Meson overlay comes from the already-pinned
[WrapDB 1.51.0-1 patch](https://wrapdb.mesonbuild.com/v2/libuv_1.51.0-1/get_patch)
(archive SHA-256
`0fb123dee5e74621a767a8f2a29dde7219c65a01a5fe63e3c8ffeed675a2d820`).
The original `meson.build` SHA-256 is
`7c106a5a406c1ef41d12dd2208f3784814df4778fae13887ae4ba527f29b88ce`;
its project version changes to `1.53.0` and its Windows libraries add
`Synchronization`, matching the upstream CMake dependency, resulting in
`4ea34585b52222502e7cb9ffd963d9a0ff3c06a7c3251d6d09461490bd069056`.
The compiler source lists still match upstream's 1.53.0 CMake build for the
supported macOS/Linux/Windows paths. The other files are byte-for-byte imports:
`meson_options.txt` (`dc02dc5b7d5bd069782529f59012e7ac25b53325b7783609c77a3b36ca819a7c`),
`link_file_in_build_dir.py` (`cba566c9f026b7c23c2460c4262ddd420e4da63986a892653d4c15a0f9e6943d`),
and `LICENSE.build` (`7939f4c45423cec4a18236ad0a88570e33508dd7462e07b1038001f90ece65fb`).
A local overlay avoids extracting the old archive into the wrong source directory;
the old 1.51.0 source fallback is removed. No libuv C source is patched.

unordered_dense keeps its existing header-only overlay; only the declared
version changes to `5.3.1` (overlay SHA-256
`67fc3f19a4f3352e2383293cd68e36c899b7b59445d3bd7bf54f3523ac9a5166`).
Catch2 uses the published WrapDB `3.16.0-1` wrap unchanged. It is used only when
the native worker's `ms_build_tests` option is enabled, not by the production
worker. Its upstream native tests must be built separately from Cargo tests.

Remove each patch as soon as an official compatible release contains its fix.
When updating either archive, re-verify its crates.io checksum, review the
complete source diff, update this document and `Cargo.lock`, and run the full
mediasoup and application test suites.

## Maintained native security tasks

Three narrow `tasks.py` changes support the native security runner:
`MEDIASOUP_BUILD_JOBS` accepts 1–64 and bounds compiler concurrency in test
containers; UBSan stops on findings without referencing the absent upstream
suppression file; and `fuzzer-run-all` refuses its absent external corpora and
unbounded workload. Existing sanitizer and fuzzer build targets remain in use.
See the [native security guide](../docs/native-security.md) for reviewed inputs,
pinned tooling, sandbox limits and coverage boundaries.
