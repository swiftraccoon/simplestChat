# Media load tests

The optional Rust generator tests SFU forwarding over real ICE/DTLS/SRTP
connections using synthetic Opus/VP8-shaped RTP. It measures signaling and media
delivery, not browser encoding, rendering, or visual quality.

## Build and run

Install the pinned Rust/native dependencies from the
[development setup](../docs/development.md), then run from the repository root:

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

Use an owned test server and a fresh output directory for each run. The generator
joins rooms and sends media; it does not manage the server. Server join limits
still apply: 10 attempts per room/IP and 30 per IP in 60 seconds. For larger runs
and repeated baseline/candidate comparisons, use the
[local benchmark workflow](../docs/performance.md).

### Linux container

Docker host networking reaches your owned local test server; reports are written
to the fresh host directory in `load_test_results_dir`:

```sh
docker build --pull --target loadtest -t simplestchat-loadtest .
load_test_results_dir="$(mktemp -d)"
docker run --rm --network host --user "$(id -u):$(id -g)" \
  --mount "type=bind,src=$load_test_results_dir,dst=/results" \
  simplestchat-loadtest \
  --server ws://127.0.0.1:3000/ws --clients 2 --duration 30
```

## Options

| Option | Meaning/default |
| --- | --- |
| `--clients N` | Logical clients; 5, range 1–10,000 |
| `--duration SECS` | Shared measured interval; 30, range 3–3,600 |
| `--ramp-up SECS`, `--warmup SECS` | Launch ramp and additional warmup; 5 and 10 |
| `--deadline-grace SECS` | Watchdog allowance after measurement; 30 |
| `--server URL` | Signaling endpoint; `ws://localhost:3000/ws` |
| `--room ID`, `--rooms N` | Room prefix and round-robin room count; `load-test-room`, 1 |
| `--mode MODE` | Publisher preset: `conference` (100%), `webinar` (1%), `panel` (10%), `classroom` (20%) |
| `--publish-ratio RATIO` | Publisher fraction; 1.0, at least one publisher overall |
| `--max-audio N`, `--max-video N` | Consumer caps per client; 4 each |
| `--churn-rate N` | Select `min(clients, floor(N × duration))` clients to repeatedly join; 0 |
| `--audio-only`, `--video-only` | Generate only the selected media kind |
| `--quality PRESET`, `--fps FPS` | `480p`/`720p`/`1080p`, 15/30/60 fps; defaults 480p/30 |
| `--output-dir PATH` | JSON report directory; current directory |
| `--diagnostics` | Bounded RTC snapshots and lifecycle events; off |
| `--run-label LABEL` | Run identifier |
| `--server-revision SHA`, `--generator-revision SHA` | Source revision labels |

Publishers are selected before room assignment; low ratios can leave rooms with
no publisher. Churn selects a population, not an exact arrival rate: sessions last
5–30 seconds, followed by a two-second reconnect cooldown.
The first session is held through the initial measured cohort when needed.
Positive churn rates that select zero clients are rejected. Use a long enough
measurement interval for the selected clients to complete another measured join;
short runs cannot establish churn coverage.
Each iteration performs a fresh room join with new media transports; it does not
exercise credential-based signaling reconnection. With abrupt departure, the old
membership can remain in reconnect grace while the new session joins.

Existing and newly announced producers share bounded audio/video FIFO queues.
Starting immediately, each 100 ms dispatch tick inspects at most two items in
total. Known owned-retired producers are skipped before requesting a consumer;
already requested slots stay occupied until the server's `ProducerClosed`
notification frees capacity. Discovery deduplication retains at most 20,000
identities and fails the run on overflow. This scheduling does not change
consumer caps, coverage thresholds, keyframe cadence, or session lengths.

## What constitutes success

All clients share a measurement interval beginning after `ramp-up + warmup`.
Late setup does not extend it; non-churning clients that miss warmup fail.

A pass requires successful tasks and media operations, expected capped consumer
creation, and measured publisher RTP. Each eligible connection attempt must
validate delivery from distinct stable (non-churning) publishers in its room,
separately for audio and video up to the configured caps. Publisher attempts
must also queue RTP during their own part of the shared measurement window.
Evidence ends at the planned deadline even if setup or cleanup runs late.

Expectations and the session deadline are fixed before connection setup. Attempt
eligibility excludes the first three seconds, intersects the shared window, and
counts complete seconds only. Delayed setup cannot shorten that planned interval
or turn missing media into a skip. An admitted attempt with no planned eligible
seconds is a skipped short tail; failed setup still fails. A five-second session
with slow setup can therefore honestly fail coverage.

Individual consumer checks remain in place, including streams from churning
publishers: after their subscription/publication settling allowance, eligible
buckets allow no more than two consecutive empty seconds. Owned publisher or
client departure ends eligibility; unexpected producer closure fails the run.
These checks do not prove complete dynamic-publisher fan-out. Dynamic streams
occupying consumer caps can prevent the stable-peer proof; inspect the evidence
before treating incomplete scoped coverage as an application regression.

Every selected churner must complete an eligible, admitted, passing second or
later attempt with a nonzero stable receiver expectation. All-churn populations,
rooms without a stable counterpart, and zero effective consumer caps cannot
establish measured churn. Earlier successful attempts cannot cover a later
attempt's missing delivery.

Accept a run only when the process exits successfully, `run.completed` and
`run.passed` are true, and no `load_test_timeout.json` exists. Watchdog expiry
exits 124; incomplete reports and output errors are failures.

## Reports and metric definitions

`load_test_results.json` holds per-client attempts, errors, counters, and consumer
packet buckets. `load_test_summary.json` aggregates them (`schemaVersion: 2`):

- `connectionAttempts` counts all attempts. Connection-time percentiles measure
  room admission from each successful WebSocket attempt, not ICE/DTLS readiness.
- Per-client `connectionAttempts[].coverage` records planned eligible seconds,
  expected/validated stable publishers by kind, queued packets, pass/short-tail
  status, and failure reasons.
- Summary `attemptCoverage` version 1 has scope `stable-publishers`, attempt
  outcomes, and requested/validated churner counts. It is additive to summary
  schema 2; older artifacts without it or per-attempt coverage are unavailable
  evidence for this guarantee, not successful coverage.
- `sendMediaReady` / `receiveMediaReady`: peer `Connected` timing for each attempt.
- `signalingLatencies.operations`: P50/P95/P99 from merged exact millisecond
  histograms, not averages of client percentiles.
- `measurement`: counters for the shared interval. `packetsQueued` means the
  local writer accepted RTP; `packetsReceived` counts actual incoming RTP.
  `bytesQueued` includes RTP headers; `bytesReceived` counts payload only.
- `validatedConsumers`, `failedConsumers`, `skippedShortLivedConsumers`: delivery
  coverage; inspect per-consumer `packetsBySecond` for stalls.
- Per-client `consumerDelivery[].attempt` is the immutable one-based connection
  attempt; `isAudio` identifies its media kind. Older artifacts may omit both.
- `run`: completion, failures, timestamps, workload configuration, revision
  labels, and generator binary hash.

Use `measurement.*`, not lifetime counters, for throughput comparisons. Queued
counts are not confirmed network egress, and byte counts are not wire bandwidth.
Consumer caps and publisher lifetimes also mean received/queued ratios are not
packet-loss percentages. These checks establish delivery liveness, not capacity.

## Diagnosing missing media

Add `--diagnostics` to a small correctness run. Per-client `diagnostics` includes
consumer creation, SDP installation, resume acknowledgments, connection changes,
first RTP, and pre-close RTC snapshots with SSRC/MID mappings and candidate ports.
Missing snapshots or capture limits fail the run (`diagnosticFailures` in the
summary). Diagnostic counters are lifetime observations, not measurement-window
throughput; `elapsedMs` is relative to collector creation before the launch ramp.

Diagnostic JSON omits raw SDP, candidate IPs, ICE credentials, and fingerprints.
Debug logs can contain them; keep those private. For receive-path warnings, use
`RUST_LOG=error,rtc::peer_connection::handler=warn`. Instrumented runs are for
diagnosis, not performance comparisons. The local benchmark workflow also offers
`--diagnostic-detail capture-only` for packet capture without client diagnostics.

Full local benchmark diagnostic runs also save bounded native server samples in
`server-media-sample-*.json` and correlate worker RTP counters with client receipt
in `server-media-report.json`. Matching uses process-scoped hashed media references
and SSRCs; missing samples or unmatched streams are unknown, not zero. Native
forwarding counters do not establish client receipt. See
[server forwarding snapshots](../docs/diagnostics.md#server-forwarding-snapshots)
for limits, coverage gates and counter definitions.

### Capture a stalled receiver

With `--diagnostics`, the generator checks existing receive buckets once per
completed measurement second. The first live consumer with three latest complete,
eligible empty seconds triggers at most one receive-peer RTC capture attempt per
connection attempt. The detector uses the delivery check's settling,
publisher-lifetime and session-deadline rules; it does not change whether delivery passes.

Successful captures appear in `diagnostics.receiverStalls`, separate from the
128 pre-close snapshot slots. Each entry identifies the attempt and a `trigger`
with a one-based `consumerOrdinal` into the full `consumerDelivery` array, SSRC,
media kind, and end-exclusive `[beginBucket, endBucket)` range relative to the
shared measurement start. `details.triggerElapsedMs` marks the trigger;
the entry's `elapsedMs` marks capture completion, both on the collector clock.

The run admits at most eight captures, with two concurrent and no waiting queue
or retries. Each has a two-second timeout, including session-lock acquisition,
clipped to the session deadline. Entries exceeding 256 KiB of compact serialized
JSON are rejected; pretty-printed artifacts can be larger. The
`receiver-stall-capture` event records `captured`, `busy`, `budget_exhausted`,
`timed_out`, `attempt_ended`, `unavailable`, `snapshot_rejected`, or `cancelled`.
Any non-captured outcome fails diagnostics without concealing the delivery gap.

These are sanitized local receiver observations, not additional server samples.
Media can recover or its publisher can depart before capture completes. Use the
bucket range to locate the gap; do not treat the snapshot as simultaneous evidence
or a root cause. Missing subscriptions, partial seconds, and gaps outside the
latest three complete eligible buckets are not captured. Monitoring excludes
observations at or after session/window end, so a final gap can fail delivery
without a capture. An absent `receiverStalls` field, including in older artifacts,
does not establish continuous receive-path health.

Full local benchmark diagnostics also write
[`receiver-stall-report.json`](../docs/diagnostics.md#correlate-receiver-stalls),
matching recorded triggers to retained SFU producer and consumer observations.
Failed captures remain visible; these scheduled snapshots are not observations
taken at the trigger and cannot establish its cause.

### Locate the last confirmed receive milestone

Inspect each client and connection attempt separately; a working publisher does
not establish a working receive path for every subscriber.

| Evidence | What it establishes |
| --- | --- |
| Receive peer `connected` | Local ICE/DTLS readiness, not RTP delivery |
| `consumer-created`, `renegotiation-applied`, `resume-requested`, `resume-ack` | Subscription setup milestones; an acknowledgment is not a received packet |
| `track-first-rtp` | The track reader observed RTP for that SSRC, not browser decoding |
| Consumer `packetsBySecond` | Delivery during eligible measurement seconds, including sustained gaps |

Match consumer IDs within an attempt, then use SSRC mappings to locate first RTP.
The SDP event describes a batch, not individual consumers. Compare elapsed times
only within the same collector; server operation clocks are independent.
Synthetic video keeps a periodic keyframe every five seconds of generated frames
and responds to PLI/FIR on scheduled frames, with additional keyframes spaced by
at least one second of generated frames after the preceding keyframe. Diagnostic
`keyframe-requested` marks a newly pending request; repeated requests coalesce.
`video-keyframe-queued` records the frame index, RTP timestamp, SSRC and whether a
request was satisfied, only after all frame packets enter the local writer.
Neither event confirms egress or decoding. Added keyframes change synthetic
traffic; use the same generator for comparisons. Resume-to-first-video timing
is not a browser startup benchmark.
Feedback and writer callbacks can run concurrently; event ordering alone cannot
pair a request with a frame. Use the SSRC and the frame's `requested` flag.

Transport/candidate-pair bytes include control traffic. Zero remote RTP-report
counters alone do not prove missing forwarding; check local inbound RTP and
application receipt. A pre-close lifetime snapshot cannot locate a transient
stall in time. If every stream fails for one client while those same producers
reach other clients, investigate that receiver's path without assuming whether
the fault is server egress, transport processing, or track delivery.

For cleanup diagnosis, the local runner's full diagnostic mode accepts
`--departure abrupt` (default) or `--departure explicit-leave`. The latter sends
`leaveRoom` after each session boundary and before peer closure; churn can end
sessions inside the shared measurement window. It does not change normal
performance runs. Check the server-side lifecycle report, not just write success.
See [departure comparison and evidence limits](../docs/diagnostics.md#compare-explicit-leave-with-disconnect).

## Development

With the same build environment:

```bash
cargo test --locked --features load-test --bin load_test -- --test-threads=1
```

Tests cover metrics, reconnect timing, delivery checks, cleanup, incremental
receive renegotiation, and diagnostic filtering. Native loopback tests also force
ICE to start DTLS before signaling supplies the remote fingerprint and verify
both transport directions reach native DTLS `Connected`. They also check that
invalid parameters remain retryable and that an accepted connect is idempotent
even before ICE starts. A paused-consumer fixture verifies one bounded stall
capture, resumed RTP, preservation of the original failing gap, and independent
pre-close evidence. See
[client internals](clients/README.md) for maintenance notes.
