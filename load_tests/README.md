# Media load tests

The optional Rust generator establishes real ICE/DTLS/SRTP connections and tests
SFU forwarding with synthetic Opus/VP8-shaped RTP. It does **not** exercise browser
encoding, decoding, rendering, device capture, or visual quality. Use the separate
browser checks for those paths; successful RTP forwarding is not a capacity claim.

## Build and run

Follow the [development setup](../docs/development.md) for the pinned Rust/native
dependencies, then build the optional generator:

```bash
export OPENSSL_DIR="$PWD/target/openssl-3.5.8"
export PKG_CONFIG_PATH="$OPENSSL_DIR/lib/pkgconfig"
export OPENSSL_STATIC=1
export PIP_CONSTRAINT="$PWD/build/pip-constraints.txt"
cargo build --locked --release --features load-test --bin load_test

./target/release/load_test \
  --server ws://127.0.0.1:3000/ws \
  --clients 4 --ramp-up 2 --warmup 10 --duration 8 \
  --room local-media-check --output-dir results/local-media-check
```

Run only against an owned test server. This command joins/creates the named rooms
and sends media; it does not launch, restart, stop, or reconfigure a server.
Use unique rooms or fresh server processes between runs. Local operator scripts
under an ignored `scripts/` directory are not part of the portable test workflow.

## Options

| Option | Meaning/default |
| --- | --- |
| `--clients N` | Logical clients; 5, range 1–10,000 |
| `--duration SECS` | Shared measured interval; 30, range 3–3,600 |
| `--ramp-up SECS` | Client launch ramp; 5 |
| `--warmup SECS` | Additional time before measurement; 10 |
| `--deadline-grace SECS` | Hard watchdog allowance after measurement; 30 |
| `--server URL` | Signaling endpoint; `ws://localhost:3000/ws` |
| `--room ID`, `--rooms N` | Room prefix and round-robin room count; `load-test-room`, 1 |
| `--mode MODE` | `conference`, `webinar`, `panel`, or `classroom` |
| `--publish-ratio RATIO` | Publisher fraction; 1.0, at least one publisher overall |
| `--max-audio N`, `--max-video N` | Consumer caps per client; 4 each |
| `--churn-rate N` | Select up to `N × duration` clients to reconnect; 0 |
| `--audio-only`, `--video-only` | Generate only the selected media kind |
| `--quality PRESET`, `--fps FPS` | `480p`/`720p`/`1080p`; 15/30/60 fps |
| `--output-dir PATH` | Directory for both JSON reports; current directory |
| `--diagnostics` | Opt-in bounded RTC snapshots and lifecycle evidence; off |
| `--run-label LABEL` | Runner-supplied run identifier |
| `--server-revision SHA`, `--generator-revision SHA` | Runner-supplied source provenance |

Publishers are selected globally before round-robin room assignment. A low
publisher ratio with many rooms can leave some rooms without publishers; those
rooms are not expected to receive media. Churn rate selects a client population,
not an exact Poisson arrival rate. Churn sessions last 5–30 seconds with a 2-second
cooldown; initial sessions cover the first five measured seconds where possible.

## What constitutes success

All clients share one interval starting after `ramp-up + warmup`; late setup does
not extend the measurement. Lifetime counters remain available, but use
`measurement.*` for throughput comparisons. A non-churning client's setup that
overruns warmup fails the run; increase warmup and rerun before comparing results.

A successful process exit requires completed tasks, successful signaling and
media operations, expected capped consumer creation, and measured publisher RTP.
Every eligible established consumer must receive packets and have no gap longer
than two complete seconds. A three-second subscription/renegotiation allowance
precedes per-consumer validation. Intentional publisher/client churn ends the
consumer's eligible interval immediately, even if the server retains reconnect
state. Short-lived streams are reported as skipped, never passed; each client
must validate at least its initial expected capped subscription count. During
churn, this is a lifetime minimum, not an assertion that every replacement
subscription was established in each reconnect attempt; delivery checks still
apply individually to every established stream with an eligible interval.

These are coarse delivery/liveness assertions, **not** packet-loss, jitter, or
bitrate SLOs. The shared window, caps, and publisher lifecycle matter: aggregate
received/queued packet ratios cannot be interpreted as loss percentages. A sole
webinar publisher is correctly exempt from receiving its own stream.

Failures exit nonzero. A stalled async runtime is caught by an independent OS
watchdog, which exits **124**, not success, after a bounded two-second reporting
allowance. It attempts to write an incomplete summary and `load_test_timeout.json`
from immutable configuration, without waiting for collector locks. Reporting is
best-effort: blocked filesystem or output streams cannot postpone deadline exit.
The watchdog remains armed through normal report writes. JSON serialization/output
failures also fail the process. Require a successful exit, no timeout marker, and
`run.completed`/`run.passed`; a timeout marker invalidates even a normal summary
written concurrently with expiry. Never accept a partial report as a completed run.
Use a fresh output directory for each run; timeout markers are not automatically removed.

A server closing a generated producer before its planned lifetime ends is an
error, not intentional churn. Planned lifetimes are recorded before local
signaling/peer cleanup so valid shutdown notifications do not trigger this check.

## Reports and metric definitions

`load_test_results.json` contains per-client attempts, errors, exact signaling
histograms, measured counters, and consumer SSRC packet buckets.
`load_test_summary.json` retains legacy top-level keys and adds `schemaVersion: 2`:

- `connectionAttempts` and room-admission percentiles include **every attempt**,
  measured immediately before its WebSocket connection. They are not ICE/DTLS
  latency. The legacy per-client `connectionTimeMs` preserves the first join.
- `sendMediaReady` and `receiveMediaReady` report actual peer `Connected` events
  from that attempt, separately from signaling acknowledgments.
- `signalingLatencies.operations` merges exact millisecond histograms across all
  operations. P50/P95/P99 use nearest rank, not percentiles of per-client medians.
- `measurement` reports a common `durationMs`, `packetsQueued`, `packetsReceived`,
  `bytesQueued`, and `bytesReceived`. Queued RTP means the local writer accepted it,
  not confirmed network egress. Queued bytes include RTP headers; received bytes
  count RTP payload. Neither is a wire-bandwidth measurement.
- `validatedConsumers`, `failedConsumers`, and `skippedShortLivedConsumers` expose
  delivery coverage; per-consumer `packetsBySecond` makes stalls inspectable.
- `run` records completion, pass/failure reasons, UTC times, complete workload
  configuration, source labels, generator binary SHA-256, build profile, and host
  architecture. Unknown source labels stay explicit rather than being guessed.

For reproducible comparisons, also retain both server binaries/source revisions,
compiler/native versions, identical server configuration, CPU/RSS for server and
generator, and run order. Alternate repeated baseline/candidate runs using one
unchanged release generator; compare browser startup separately. See the
[performance methodology](../docs/performance.md).

## Diagnosing missing media

Add `--diagnostics` to a bounded local correctness run when investigating missing
RTP. This is instrumentation, not a performance-comparison mode. Each client's
optional `diagnostics` object in `load_test_results.json` contains:

- `events`: consumer creation, completed SDP renegotiation, resume requests and
  acknowledgments, peer/ICE state transitions, track callbacks, and first RTP.
- `snapshots`: pre-close send/receive transport IDs and states; allowlisted
  lifetime RTC transport, candidate-pair and RTP statistics; sanitized local/remote
  SDP media sections and consumer SSRC/MID mappings. Candidate IDs, ports and a
  loopback boolean support correlation with an owned loopback packet capture.
- `failures`: explicit incomplete capture reasons. A snapshot includes both peers
  and session-lock acquisition within a two-second timeout. Setup failure,
  unavailable stats, timeout, or capture limits fail the diagnostic run;
  `diagnosticFailures` is also included in the summary.

Entries carry a one-based `attempt` and `elapsedMs` since that collector was
created **before the launch ramp**, approximately the common run clock. This is
not the attempt-relative clock used by first-media latency. Use the snapshot's
RTC timestamp and elapsed time to align packet captures; do not assume a client's
first connection starts at diagnostic time zero. Requests mean
the client attempted to queue signaling; only `resume-ack` proves the server
acknowledged it. Snapshots occur after the planned session boundary but before
local peer close, so their lifetime counters are not shared-window metrics.
Transport packets include control traffic and do not by themselves prove RTP
delivery. Preserve failed runs; a later passing probe does not resolve an
intermittent failure.

Diagnostic JSON never retains raw SDP, candidate IP addresses, ICE credentials,
certificate IDs, or fingerprints. Avoid broad debug logging when sharing evidence:
existing WebRTC debug logs can contain raw negotiation data. Capture is capped at
4,096 lifecycle events and 128 snapshots per logical client; exceeding a cap is an
explicit failure, not silent truncation. Normal measurement and delivery checks
remain unchanged when diagnostics are enabled, and diagnostics are off by default.

For an archived generator or a timing-sensitive investigation, the local runner's
`--diagnostic-detail capture-only` mode collects bounded packet prefixes without
enabling these application diagnostics. See [capture modes and coverage
limits](../docs/performance.md#controlled-local-comparison). Error-only logs are
not proof of a clean receive path: upstream RTC processing can report errors at
WARN. If packets reach the client but do not reach its RTP reader, a focused
follow-up filter is `RUST_LOG=error,rtc::peer_connection::handler=warn`; retain
those logs privately and do not treat instrumented runs as performance results.

## Development

```bash
cargo test --locked --features load-test --bin load_test -- --test-threads=1
```

The focused tests cover histogram merging, per-attempt reconnect timing, shared
window boundaries, stalled/short-lived delivery, intentional publisher churn,
WebRTC negotiation/cleanup, live RTP across incremental consumer renegotiation,
diagnostic credential filtering and bounded capture. The two-peer loopback test
requires packets beyond post-update send watermarks on all 2→4→6→8 streams and
after unchanged-mapping replay. It tests SSRC registration and reader survival,
not mediasoup MID rewriting, browser decoding or capacity.
See [client internals](clients/README.md).
