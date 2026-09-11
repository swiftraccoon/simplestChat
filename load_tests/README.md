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
| `--churn-rate N` | Select up to `N × duration` clients to reconnect; 0 |
| `--audio-only`, `--video-only` | Generate only the selected media kind |
| `--quality PRESET`, `--fps FPS` | `480p`/`720p`/`1080p`, 15/30/60 fps; defaults 480p/30 |
| `--output-dir PATH` | JSON report directory; current directory |
| `--diagnostics` | Bounded RTC snapshots and lifecycle events; off |
| `--run-label LABEL` | Run identifier |
| `--server-revision SHA`, `--generator-revision SHA` | Source revision labels |

Publishers are selected before room assignment; low ratios can leave rooms with
no publisher. Churn selects a population, not an exact arrival rate: sessions last
5–30 seconds, followed by a two-second reconnect cooldown.

## What constitutes success

All clients share a measurement interval beginning after `ramp-up + warmup`.
Late setup does not extend it; non-churning clients that miss warmup fail.

A pass requires successful tasks and media operations, expected capped consumer
creation, and measured publisher RTP. After a three-second setup allowance,
eligible consumers must receive packets with no more than two consecutive empty
one-second buckets. Planned churn ends eligibility; short-lived streams are
reported as skipped. Each client must validate its initial expected subscription
count. During churn, that is a lifetime minimum, not per-reconnect coverage.
Unexpected producer closure fails the run.

Accept a run only when the process exits successfully, `run.completed` and
`run.passed` are true, and no `load_test_timeout.json` exists. Watchdog expiry
exits 124; incomplete reports and output errors are failures.

## Reports and metric definitions

`load_test_results.json` holds per-client attempts, errors, counters, and consumer
packet buckets. `load_test_summary.json` aggregates them (`schemaVersion: 2`):

- `connectionAttempts` counts all attempts. Connection-time percentiles measure
  room admission from each successful WebSocket attempt, not ICE/DTLS readiness.
- `sendMediaReady` / `receiveMediaReady`: peer `Connected` timing for each attempt.
- `signalingLatencies.operations`: P50/P95/P99 from merged exact millisecond
  histograms, not averages of client percentiles.
- `measurement`: counters for the shared interval. `packetsQueued` means the
  local writer accepted RTP; `packetsReceived` counts actual incoming RTP.
  `bytesQueued` includes RTP headers; `bytesReceived` counts payload only.
- `validatedConsumers`, `failedConsumers`, `skippedShortLivedConsumers`: delivery
  coverage; inspect per-consumer `packetsBySecond` for stalls.
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

## Development

With the same build environment:

```bash
cargo test --locked --features load-test --bin load_test -- --test-threads=1
```

Tests cover metrics, reconnect timing, delivery checks, cleanup, incremental
receive renegotiation, and diagnostic filtering. See
[client internals](clients/README.md) for maintenance notes.
