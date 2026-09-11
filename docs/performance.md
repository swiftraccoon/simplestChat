# Performance

Use the synthetic load test to compare server forwarding and resource use, and
the browser test to measure UI startup, API requests, chat, and decoded video.
See [recorded results](performance-results.md) for the dependency-update comparison.

## Controlled local comparison

Build the baseline and candidate in separate checkouts using the same Rust
toolchain and locked release settings. Follow the [native build setup](development.md)
in each checkout, and build [one load generator](../load_tests/README.md#build-and-run)
to use for both.

Run from the candidate checkout:

```sh
node load_tests/benchmark-local.mjs \
  --baseline-root /absolute/path/to/baseline \
  --baseline-bin /absolute/path/to/baseline/target/release/simplestChat \
  --candidate-root "$PWD" \
  --candidate-bin "$PWD/target/release/simplestChat" \
  --generator "$PWD/target/release/load_test" \
  --output /absolute/path/to/new-results-directory \
  --clients 10 --scenarios conference --repetitions 3 \
  --ramp-up 5 --warmup 10 --duration 60 --workers 1
```

The runner starts and stops its own local servers, alternates baseline/candidate
order, and records delivery, latency, CPU, memory, and cleanup results. The output
directory must be new. Default ports are HTTP 3129 and UDP 41100; override them
with `--port` and `--udp-port` if occupied.

Keep the generator, worker count, subscription caps, and workload identical.
Run without competing builds or other benchmarks. If the generator comes from
another checkout, set `--generator-source-root` to that source tree.

### Choose a workload

| Scenario | Workload |
| --- | --- |
| `conference` | Every client publishes in one room |
| `multi-room` | Publishers distributed across up to four rooms |
| `webinar` | One percent of clients publish, rounded up |
| `audio` | Audio-only conference |
| `churn` | About one fifth of clients reconnect during the run |

Start with the ten-client example. For 30 clients, use `--scenarios multi-room`.
For churn, use `--clients 5 --scenarios churn`.
Room admission allows 30 joins per IP and 10 per room/IP in 60 seconds, including
reconnects. Larger runs need a slower ramp; the runner checks the initial ramp
before starting. See [admission limits](configuration.md#additional-fixed-admission-limits).

The local runner supports up to 100 clients, four workers, five repetitions, and
180 seconds per measurement. It tests guest media without a database.

## Reading results

Compare the medians and ranges in `comparison.json`, then inspect individual
runs when a change stands out. The output also includes workload settings,
source/binary identifiers, raw reports, process samples, and server logs.

| Metric | Interpretation |
| --- | --- |
| Admission latency | WebSocket connection through room admission |
| Send/receive readiness | Time until the respective ICE/DTLS connection is ready |
| Received packets/second | Delivery during the shared interval, excluding ramp and warmup |
| CPU | Percentage of one core; server and generator are reported separately |
| Peak RSS | Highest sampled resident memory, not total allocations |

A passing media run requires expected subscriptions and sustained packet
delivery. Use only completed, passing runs for timing comparisons; retain failures
to investigate separately. Keep the default ten-second warmup because synthetic
video keyframes arrive every five seconds.

Synthetic RTP exercises forwarding, not browser encoding or visual quality.
Receive/send totals are not a packet-loss estimate because streams fan out to
multiple subscribers. See [report definitions](../load_tests/README.md#reports-and-metric-definitions)
for counters and delivery checks.

## Browser and authenticated work

Set up the [disposable browser test server](../web/e2e/README.md#install-and-run),
then run a release build:

```sh
cargo build --locked --release --bin simplestChat
PERFORMANCE_E2E=1 RUN_LABEL=local-sample \
  TEST_SERVER_BINARY="$PWD/target/release/simplestChat" \
  build/with-test-server.sh node web/e2e/performance.cjs
```

`browser-performance.json` records page startup, main-thread time, JS heap and
bundle size, account/room requests, chat delivery, and fake-camera decoding.
For an A/B comparison, use the same browser and harness, fresh databases, and
at least three alternating pairs. Local results help compare changes;
production sizing needs a representative deployment, network, and client workload.

## Diagnosing missing media

Add `--purpose diagnostic` to the local comparison command to collect transport
snapshots and signaling logs instead of resource measurements. For packet headers,
also add `--capture-interface lo0` on macOS or `lo` on Linux; this requires
`tcpdump` and existing capture permission.

`--diagnostic-detail capture-only` collects packet headers without extra
application logging. Captures are limited to the runner's loopback media ports
and may end before the workload; check timestamps in `media-headers.pcap` and
dropped-packet counts in `capture.log`. Keep diagnostic artifacts private. See
[missing-media diagnostics](../load_tests/README.md#diagnosing-missing-media)
for the report fields.
