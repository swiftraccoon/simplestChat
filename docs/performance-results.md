# Local regression measurements — 2026-09-09

This compares the pre-dependency-update revision `b386532` with current `6459846`.
It is a bounded local comparison, **not a production capacity guarantee** or a
validation of every recent feature at scale. See [methodology](performance.md).

## Provenance and scope

- Apple M5 Max, 18 logical CPUs, 128 GiB RAM; macOS 27.0, Darwin 27.0.0 arm64.
- Both production servers: locked release builds, Rust 1.98.1 / LLVM 22.1.8,
  static OpenSSL 3.5.8, default target CPU, no custom RUSTFLAGS. The baseline's
  toolchain file pins 1.98.0, but both used 1.98.1 to isolate dependency changes.
- Baseline: `b3865329042bdee439bcbebad62d6d4879e68c21`; candidate:
  `64598461361e39086fd6a45d22f6c6af941ae739`. No runtime server-source changes
  were present in the candidate worktree during these measurements.
- One corrected, uncommitted release generator was used unchanged for both.
  Its source and executable hashes are recorded alongside every run. The exact
  measured source and binary are archived; subsequent watchdog and unexpected-
  producer-closure hardening is verified separately below, not retroactively
  attributed to the measured binary.
- Browser: Node 26.8.1, Playwright 1.63.0, Chromium 153.0.8010.12 and the same
  harness; fresh disposable PostgreSQL 15.19 databases for each browser run.
  Separate production-container checks used PostgreSQL 18.6 on Linux/arm64.
- Server/generator were co-located. No builds or other test workloads were run
  during the measured intervals; unrelated laptop activity was not controlled.

| Executable | SHA-256 |
| --- | --- |
| Baseline server | `dd8f94e2734b0207411ba3be79ada1552c395f9347fba427d4c6eebce584a8c1` |
| Candidate server | `cacb2849ada1bc793a474ab443471800f5c37083f336b7069fc78f2b2ce503c4` |
| Measured synthetic generator | `3ba404d304ada043f046c89aa9be881110d318843acc3a4d0c7396f99928080f` |

## Browser, API and real decoded media

Three alternating pairs ran in A/B, B/A, A/B order. Each run used two fresh
browser contexts, two registered users, one persisted room, ten paced public
messages and five seconds of decoded fake-camera media. OS caches were not
flushed. Values below are sample medians, not service-level guarantees.

| Metric | Baseline | Candidate | Sample count per revision |
| --- | ---: | ---: | ---: |
| First contentful paint | 44 ms | 44 ms | 6 contexts |
| Startup main-thread task time | 33.14 ms | 32.46 ms | 6 contexts |
| Startup JS heap | 2,604,130 B | 2,587,140 B | 6 contexts |
| Registration request | 15.31 ms | 15.09 ms | 6 registrations |
| Chat delivery, including automation overhead | 28.84 ms | 29.34 ms | 30 messages |
| Camera publish to first decoded frame | 146.55 ms | 147.04 ms | 3 publishes |
| Main JS bundle, uncompressed | 391,284 B | 386,797 B | 1 locked build |
| Same JS compressed with Node's default gzip | 83,726 B | 77,585 B | 1 build |

All six runs passed sustained decoded-frame progress, reported zero received
packet loss/dropped frames in their short samples, and left zero open peer
connections or attached media streams after leaving. These observations do not
establish behavior on real devices, other browsers or lossy networks.

No obvious browser/API regression appeared at this small local scale. Chat
medians differed by about 0.5 ms, with overlapping observed ranges (baseline
24.38–39.29 ms; candidate 24.32–40.51 ms). Camera readiness varied much more
between individual runs than the median difference. No statistical performance
budget is inferred from three repetitions.

## Synthetic media and process resources

Three alternating pairs completed with 10 clients, one worker/room, 480p/30 fps,
4 audio + 4 video subscription caps, a 5-second ramp, 10-second warmup and
60-second shared measurement. Medians across the three runs per revision:

| Metric | Baseline | Candidate |
| --- | ---: | ---: |
| Admission P99 | 1 ms | 0 ms |
| Send ICE/DTLS-ready P99 | 408 ms | 407 ms |
| Receive ICE/DTLS-ready P99 | 409 ms | 407 ms |
| Received packets/second | 6,920 | 6,920 |
| Server CPU, percent of one core | 10.26% | 10.16% |
| Server sampled peak RSS | 89.63 MiB | 89.78 MiB |
| Generator CPU, percent of one core | 18.49% | 18.23% |
| Generator sampled peak RSS | 34.58 MiB | 34.63 MiB |

Every run validated all 80 consumers across the full 60 seconds, with no empty
one-second delivery buckets. Connection/room/participant gauges returned to zero
after approximately 30 seconds of reconnect grace. Millisecond quantization means
0 ms is sub-millisecond, not instantaneous; the descriptive admission difference
is not a meaningful performance improvement. No material regression was observed
in this small single-room workload.

The attempted 50-client single-room run was rejected after ten joins by the fixed
room/IP admission limit, independently of WebSocket upgrade/connection limits.
That series remains marked failed. Only its six completed 10-client rows are
included in the separate 10-client summary; rejected attempts are not capacity
measurements. The runner now rejects insufficient initial ramps before launching.

A subsequent 30-client/four-room baseline run passed admission and created all
240 consumers, but one client received no media on its eight streams for the
entire minute despite ICE/DTLS readiness. The other 232 streams passed. This run
failed and cannot support a larger-scale performance conclusion. Investigation
is separate from the passing small-workload comparison; failed artifacts remain
retained rather than being silently replaced by successful retries.

One predetermined 15-second diagnostic per revision subsequently passed all
240 streams, using the same frozen generator and workload with more setup logs.
This establishes neither a fix nor which component failed. The original
whole-client receive failure remains intermittent and unresolved; its error-only
logs cannot distinguish a generator receive-path problem from server forwarding.
That evidence gap led to the instrumentation below; the original failed run
cannot be retrospectively localized with its retained logs alone.

## Follow-up receive-path investigation

The optional generator diagnostics now retain bounded pre-close transport/RTP
statistics, candidate ports, sanitized SDP/SSRC mappings, connection histories,
and consumer resume acknowledgments. The local runner can correlate these with
64-byte loopback packet captures, capped at 500,000 packets. Diagnostic runs skip
resource sampling and cannot produce a performance comparison.

The instrumented release generator is
`608286ce6843f1406c5fa0e8addb6c7eb980716135b82d772ce25a826d98b240`;
both server binaries remain exactly those in the provenance table above. No
server behavior or production admission safeguard was changed. Generator media
negotiation and measurement algorithms are unchanged; diagnostics add logging
and post-window snapshot work.
The added diagnostic tests bring the focused generator suite to 21 passing tests
and the helper/orchestrator suite to 13. An instrumented four-client smoke passed
all 24 consumers on each revision, with complete snapshots, successful capture
finalization and no kernel-reported capture drops.

The follow-up artifacts are in `/tmp/simplestchat-receive-diagnostics.cpCFzp/`,
including the frozen generator, manifests, reports, state logs and packet prefixes.
The fixed three alternating 30-client/four-room pairs completed in A/B, B/A, A/B
order, with the original 5-second ramp, 10-second warmup and 60-second shared
interval. All six passed: 180 client sessions and **1,440 validated streams**, each
with all 60 eligible seconds receiving packets and no empty-second gaps. All
180 snapshots completed without diagnostic failures; capture exits were clean
with no kernel-reported drops. Connection/room/participant gauges returned to
zero after each run, and all owned servers, generators and capture processes
were stopped.

Independent packet analysis of the first pair matched all 240 expected SSRCs per
run to the correct receive ports, resume acknowledgments and track callbacks.
Captured expected-media sequences had no interior gaps, duplicates or reordering.
Only expected probation traffic was additional. The packet cap ends around
25.76 seconds after startup—roughly the first 10.76 of 60 measured seconds—so
captures do **not** establish full-minute wire delivery. The full-minute checks
come from the generator. Final RTC snapshots and application counters are not
atomic; small intervening packet-count differences are not proof of loss.
Server warnings in this pair began about 28 seconds after generator completion,
during reconnect grace, rather than during the workload.

The original failure did not recur. This is additional passing correctness
evidence, **not a fix or a larger-scale performance comparison**. Logging and
packet capture can perturb timing; a recurrence with correlated evidence is
still needed to attribute the original failure before proposing a server change.

### Quiet-generator follow-up

A fixed five alternating pairs then used the **original frozen measured
generator** from the provenance table, with no `--diagnostics` flag and
`RUST_LOG=error` for both processes. The runner's capture-only mode retained
64-byte loopback prefixes with a larger 2,000,000-packet cap and no resource
sampling. Server binaries and the 30-client/four-room workload were unchanged.
The exact runner was archived before subsequent capture-shutdown hardening.

All ten runs passed: **300 client sessions and 2,400 validated streams**, each
with packets in all 60 eligible seconds and no empty-second gaps. Every capture
exited cleanly with zero kernel-reported drops; connection/room/participant
gauges returned to zero after every run. Both server hashes still matched after
the series, and all owned processes were stopped. Raw artifacts and the archived
runner remain private under `/tmp/simplestchat-quiet-capture.BcJqdV/`.

Independent analysis of the first baseline capture accounted for all 240
expected SSRCs, 30 publisher ports and 30 receiver ports, with no unexpected
media streams or interior sequence gaps, duplicates or reordering. However,
stopping capture immediately after generation left approximately 592 ms of
buffered tail unread: its measured-window coverage was about **59.408 of 60
seconds**, despite remaining below the packet cap and reporting no kernel drops.
Application coverage and captured wire coverage remain distinct claims. The
runner now allows a bounded two-second post-generator drain; this change is not
retroactively attributed to the ten archived captures.

The original zero-media failure again did not recur. Error-only logs cannot
exclude upstream receive errors because the RTC pipeline can log them at WARN.
Static review did not establish a renegotiation/key-installation race or another
justified server fix. These are additional correctness observations, not proof
that the intermittent fault is resolved, nor a larger-scale performance result.

A new two-peer loopback regression test exercises the generator's real receive
path while consumers grow from two to eight, followed by unchanged-mapping
replay. Each phase requires received packets beyond its post-update queued-write
watermarks on every SSRC; old buffered packets cannot satisfy the check. This
covers SSRC registration and reader survival, not mediasoup MID rewriting or
the original 30-client failure. The focused suite passed **22/22 tests**, and
the new test passed a fixed **10/10 repetitions**. The helper/orchestrator suite
passed **16/16**, including capture draining/late failures and server/generator
hash checks. No runtime server behavior was changed.

A separate four-client pair checked the drain change with the same frozen
generator, a two-second ramp, ten-second warmup and eight-second measurement.
Both runs passed all 24 consumers. Captures extended beyond the nominal window
end and retained each stream's final packets within normal packet cadence; the
earlier buffered-tail gap was absent. Captured expected-media lifetime totals
matched every client's application count exactly: 28,757 packets on baseline,
28,756 on candidate. No interior sequence gaps, duplicates, reordering or kernel
drops were found. Wall-clock reconstruction of the baseline measurement window
differed from the application's monotonic window by eight boundary packets;
candidate window counts matched. This is not evidence of packet loss.

The small pair validates capture shutdown in this workload, not capacity or a
general zero-loss guarantee. Its reports, captures and exact runner snapshot are
under `drain-smoke/` and `benchmark-local-drain.mjs` in the quiet-capture artifact
directory. All owned processes were stopped and cleanup gauges returned to zero.

## Excluded pilots and correctness checks

- Browser pilots initially inspected an idle Chromium probation SSRC instead of
  the active video stream. The harness was corrected to aggregate frame deltas
  across streams, with a regression test. Failed pilots remain excluded and
  retained; all six measured runs use the same corrected measurement code.
- A synthetic compatibility pilot used only 5 seconds of warmup. Two late video
  subscriptions waited for the next synthetic keyframe and failed sustained
  delivery. The unchanged binaries passed with the standard 10-second warmup;
  assertions were not relaxed. That failed pilot is retained and excluded.
- The frozen generator passed conference and single-publisher webinar smokes,
  invalid-argument rejection, and a deliberately stalled local connection
  (exit 124 with an incomplete/failed report).
- Final harness hardening keeps the watchdog independent of report/collector
  locks and treats an unexpected active publisher closure as a failure. The
  measurement counters and percentile algorithms are unchanged. Final debug
  smokes passed conference (24 consumers), webinar (6), audio-only (12), and
  churn (36 validated consumers, 10 short-lived exclusions, two reconnects).
  These four-client checks are functional tests, not resource comparisons.
  A deliberately stalled local connection also exited 124 with an incomplete,
  failed summary and a timeout marker. The final release generator was rebuilt
  separately from the archived measured binary.
- Checks passed: 136 Rust tests in each library/server target, 17 generator
  tests, all three PostgreSQL test definitions included, 119 web tests, 17 real-
  browser community checks, 11 helper/orchestrator tests, formatting, production
  web build, shellcheck and actionlint. The CI-style debug media smoke also passed.
- The new production container smoke passed on the existing local Podman VM:
  non-root Linux/arm64 image, PostgreSQL 18.6, all 14 migrations, static UI,
  health/API readiness, a real registration write and restart. Test containers
  and their temporary data were removed. Docker Desktop was unavailable; the
  existing Podman Docker-compatible socket ran the unchanged smoke. These
  expanded CI checks have not yet run on GitHub Actions.

## Artifacts and remaining work

Raw local artifacts were retained under `/tmp/simplestchat-performance.2hVM2P/`:
`build-provenance.json`, the browser comparison and exact measured-harness
snapshots, compatibility pilots, and the conference/multi-room run manifests,
per-client reports, process samples and server cleanup snapshots. Verification
logs are in `/tmp/simplestchat-final-tests.3tSvUQ/`. These are temporary local
paths, not committed fixtures; archive them privately before the OS removes them.
The two short 30-client diagnostic probes and executable-hash manifest are in
`/tmp/simplestchat-30-client-diagnostic.vCFc1T/`; the original failed minute remains
under `multi-room-measured/` in the main performance artifact directory.

Container build/runtime logs are in `/tmp/simplestchat-container-build.DU1uaK/`.
The new image is retained as
`localhost/simplestchat-docs-ci-20260909:20260910t022006z`
(`e0144d11be289e7a6dcc926203eb23f1bdea34462e017a6bbd8d520fcd88b148`),
with UID/GID 10001:10001. The previous image and unrelated containers were not
modified. Rootless/rootful local stores required copying only this new image.

Next: resolve the larger synthetic receive failure before claiming scale
validation, then use a dedicated authorized runner, longer churn/soak and
authenticated workloads, multiple worker counts/network conditions, real
Safari/Firefox/mobile devices, and performance budgets chosen after measuring
repeatability. The earlier
feature-only comparison (`e5b6373` → `b386532`) has not been run here. Do not
generalize this dependency comparison into a claim about all feature changes.
