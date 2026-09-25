# Performance

Use the synthetic load test to compare server forwarding and resource use, and
the browser test to measure UI startup, API requests, chat, and decoded video.
See [recorded results](performance-results.md) for current-build baselines and
measured before/after comparisons.

## Web asset budgets

Every production web build checks the combined size of all emitted JavaScript,
CSS and HTML, including lazy chunks and the help page. Splitting an asset does
not avoid the limit. The initial budgets leave room for small product changes:

| Asset group | Uncompressed limit | Gzip limit |
| ----------- | ------------------ | ---------- |
| JavaScript  | 512 KiB            | 112 KiB    |
| CSS         | 64 KiB             | 12 KiB     |
| HTML        | 48 KiB             | 10 KiB     |

Limits are defined in [bundle-budget.json](../web/bundle-budget.json). Gzip uses
level 6 independently for each file; the checker then sums each group. Run
`npm --prefix web run check:bundle` to inspect an existing build, or
`npm --prefix web run build` to rebuild and check it. Missing or empty entry/help pages,
empty JavaScript or CSS output, and symbolic links fail the check.

An exceeded budget needs an explanation of the user benefit and a review of
avoidable dependencies, duplicate code and unused assets before raising a limit.
These gates constrain delivery and parsing size; they do **not** establish
startup latency, memory use, media quality or server capacity. Images, fonts,
source maps and other non-JS/CSS/HTML files are not covered by these budgets.
Measure runtime performance separately with the workflows below.

The September 2026 account-security and complete-call-outcome additions raised
the raw JavaScript ceiling from 450 to 480 KiB, retaining the 100 KiB gzip limit.
Isolated builds measured 450,677 bytes at `4216538` and 469,222 bytes in the first
candidate (95,541 and 100,698 gzip bytes). Source-map attribution assigned about
10.4 kB to the new account-security workflow, 4.4 kB to call observation, 1.9 kB
to room lifecycle tracking and 1.2 kB to strict response decoders; existing
mediasoup dependencies were unchanged. Subsequent lifecycle race fixes are
included in the same ceiling. These figures explain the reviewed feature growth,
not a runtime-performance guarantee.

The subsequent seven product improvements raise the JavaScript ceiling to
512 KiB raw and 112 KiB gzip: shared-account synchronization, confirmed room
controls, duplicate-safe chat reconciliation, incoming-media repair, screen-share
results, live audio-device controls, and explicit room navigation. The previous
build measured 472,453 bytes raw / 101,607 gzip; the validated product build
measured 505,486 / 110,161. These additions use browser APIs and the existing
libraries; no runtime dependency was added. All emitted chunks remain counted,
including optional code. The revised ceiling accommodates these workflows while
keeping a bounded margin for final correctness fixes; it is not evidence of
unchanged runtime performance.

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

To hold publisher selection fixed, append
`--subscription-plan ring-v1 --subscription-seed 17` to the comparison command.
Use this opt-in mode with `conference`, `multi-room`, or `audio`: it supports
stable, all-publisher workloads on the runner's owned loopback servers, not churn.
The same seed, room assignment, media kinds and caps produce the same graph;
FIFO discovery remains the default. Replace `ring-v1` with `hotspot-v1` to
concentrate subscriptions on early-joining publishers instead of balancing fan-out.
Both modes require the same exact full-window delivery proof; compare repetitions
of the same graph, not ring and hotspot as equivalent workloads.

### Choose a workload

| Scenario     | Workload                                                             |
| ------------ | -------------------------------------------------------------------- |
| `conference` | Every client publishes in one room                                   |
| `multi-room` | Publishers distributed across up to four rooms                       |
| `webinar`    | One percent of clients publish, rounded up                           |
| `audio`      | Audio-only conference                                                |
| `churn`      | About one fifth of clients repeatedly join with fresh media sessions |

Start with the ten-client example. For 30 clients, use `--scenarios multi-room`.
For churn, start with `--clients 3 --scenarios churn --duration 120`.
Room admission allows 30 joins per IP and 10 per room/IP in 60 seconds, including
reconnects. Larger runs need a slower ramp; the runner checks the initial ramp
before starting. See [admission limits](configuration.md#additional-fixed-admission-limits).

Churn is not retained-session signaling recovery. Each selected churner needs a
passing, eligible second or later join with measured delivery from stable peers.
Keep stable publishers in each churner's room and nonzero consumer caps; an
all-churn population cannot establish this proof. Sessions still use the 5–30
second range, so slow setup can leave a short attempt without validated media.
Inspect per-attempt failures rather than weakening the gate. See
[success criteria and scope](../load_tests/README.md#what-constitutes-success).
The [browser suite](../web/e2e/README.md#coverage-and-diagnostics) checks signaling
recovery with retained media separately.

The local runner supports up to 100 clients, four workers, five repetitions, and
180 seconds per measurement. It tests guest media without a database.

For a larger sustained test, use the comparison command above with
`--clients 100 --scenarios multi-room --ramp-up 205 --warmup 10 --duration 120`.
This creates four rooms with 25 publishers each and validates up to four audio
and four video subscriptions per client. It is not all-to-all media. The local
runner raises only its test server's WebSocket admission limits; room join
quotas remain unchanged. Stop and investigate failed runs before increasing load.

## Reading results

Compare the medians and ranges in `comparison.json`, then inspect individual
runs when a change stands out. The output also includes workload settings,
source/binary identifiers, raw reports, process samples, and server logs.

Timing summaries require both `workloadPassed` and `serverShutdownPassed`.
The runner saves `server-exit.json` after stopping each owned server; a nonzero,
signaled or unavailable exit prevents a valid comparison, even if delivery and
room cleanup passed. Failed runs retain their evidence and original failure.

| Metric                  | Interpretation                                                       |
| ----------------------- | -------------------------------------------------------------------- |
| Admission latency       | WebSocket connection through room admission                          |
| Send/receive readiness  | Time until the respective ICE/DTLS connection is ready               |
| Received packets/second | Delivery during the shared interval, excluding ramp and warmup       |
| CPU                     | Percentage of one core; server and generator are reported separately |
| Peak RSS                | Highest sampled resident memory, not total allocations               |

CPU and RSS come from process samples every 500 ms (`/proc/<pid>/stat` on Linux
at clock-tick resolution, `ps` on macOS). A resource summary is refused, and the
run has no comparison row, unless surviving samples cover the measurement window
to within 1.5 s and at most 10 percent of samples failed; `serverResources`
records the sample count, failures and covered span for every valid run.

A passing media run requires expected subscriptions and sustained packet
delivery. Use only completed, passing runs for timing comparisons; retain failures
to investigate separately. Keep the default ten-second warmup because synthetic
video keyframes arrive every five seconds.

New summaries include `attemptCoverage` version 1 (`stable-publishers` scope).
Historical reports without it do not establish per-attempt coverage. Dynamic
streams occupying consumer caps can leave this scoped proof incomplete without
establishing an application regression; full dynamic fan-out is not measured.

For either fixed graph, the runner also verifies the planned graph and realized
publisher/kind mappings, records `subscriptionPlanSha256`, and requires matching
graph identities for comparisons. Every planned edge must deliver in every
complete measurement second, with publisher and consumer lifetimes covering
the full window. Missing peers cannot be substituted or skipped. See
[fixed subscription graphs](../load_tests/README.md#fixed-subscription-graphs)
for bounds and report fields.

Synthetic RTP exercises forwarding, not browser encoding or visual quality.
The generator negotiates transport-wide congestion control, so the server's
bandwidth estimators run and the summary counts their reports, but it never
adapts its fixed send rate and offers no simulcast, so bitrate adaptation and
layer selection stay unexercised; see
[what the generator does not model](../load_tests/README.md).
Receive/send totals are not a packet-loss estimate because streams fan out to
multiple subscribers. See [report definitions](../load_tests/README.md#reports-and-metric-definitions)
for counters and delivery checks.

## Continuous comparison

The [Performance workflow](../.github/workflows/performance.yml) runs weekly and
on demand. Its comparison job builds a baseline (`HEAD~1` unless a ref is
given) and the candidate commit on one hosted runner, runs the ten-client
conference (or the 30-client four-room `ring-v1` graph when selected) three
alternating pairs deep, and fails when `build/performance-thresholds.mjs` finds
the candidate's median worse than the baseline's by more than 25 percent for
receive-ready or send-ready P99 and server CPU, or 15 percent for server peak
RSS, or when any run fails the runner's own gates. The budgets are wide on
purpose: hosted runners are noisy and the comparison is relative to the
baseline measured in the same run, so it catches changes of the size already
recorded below, not single-digit drifts. The `comparison.json` and every run's
evidence are retained as a workflow artifact for 30 days.

The same workflow's soak job runs the 20-minute authenticated session soak
against the CI database and stops PostgreSQL for eight seconds at the midpoint
(revalidation runs every five seconds and a connection tolerates fifteen
seconds of validator unavailability), so the bounded credential-uncertainty
allowance is exercised by a real outage rather than a mocked validator.
Layer selection is shared: mediasoup's transport distributes the estimated
downlink bitrate across a viewer's simulcast consumers on every estimate
change, and the server additionally caps each consumer's preferred layer
from fixed tiers of the same estimate (below 200 kbit/s layer 0, below
600 kbit/s layer 1). The tiers looked redundant, but measurement showed
otherwise: with them removed, the weekly job's downgrade under a 400 kbit/s
cap took 19 s instead of about one, under 150 kbit/s 31 s instead of three,
and decode rate under loss halved, so they stay. The browser caps each remote
camera tile to the layer its rendered size can show (`web/src/layer-cap.ts`,
with hysteresis), so a grid of small tiles no longer asks for 720p per tile
and the estimator has less to take back under congestion.

The workflow's impaired job degrades the viewer's downlink with netem on the
runner's loopback (UDP leaving the server's media port only) and runs
[`web/e2e/impaired-network.cjs`](../web/e2e/impaired-network.cjs): a
publisher sends simulcast from a fake camera while a viewer's native inbound
statistics and the client's layer-change log are sampled through a clean
baseline, five percent loss with 50 ms of jitter, a 400 kbit/s cap, a
150 kbit/s cap and recovery. It asserts that decoding continues under loss
with retransmission requests, that the server steps the consumer's spatial
layer down under each cap and back to the top afterwards, and records the
seconds each switch took. Two further browser steps reuse the scenario: with
`IMPAIRED_SILENT_AUDIO=1` the publisher's microphone is a file of silence and
the viewer's audio packet rate must show Opus DTX at work, and with
`IMPAIRED_BLOCK_UDP=1` against a server started with `WEBRTC_SERVER_TCP=true`
UDP to and from the media port is dropped before anyone joins, so the
publisher and the viewer must both connect over the server's ICE-TCP
candidates and still exchange video. The same job then runs
[`build/impaired-generator.sh`](../build/impaired-generator.sh): the 30-client
four-room generator workload with five percent loss and jitter in both
directions. The generator's exact-delivery gates describe a lossless loopback,
so under impairment its summary and per-client reports are retained as
evidence (connections, validated consumers, keyframe requests, bandwidth
estimates) and the step passes when the run completes and the server exits
cleanly. Neither job measures production capacity or real device media
quality.

## Production shape and impaired networks

Production runs two media workers under a 2-CPU, 2 GiB container. The Mac
runner above has neither the quota nor Linux, so
[`load_tests/benchmark-podman.mjs`](../load_tests/benchmark-podman.mjs) runs
the same workloads inside the Podman Linux VM: the production image gets
`--cpus` and `--memory`, the load-test image shares its network namespace over
loopback, and one streaming sampler per container reads the process, the
cgroup's `cpu.stat` (usage and throttled periods) and every thread's CPU. The
row therefore carries `serverCpuPercentOfQuota`, `serverThrottledPeriodFraction`
and `serverThreadCpuSeconds` split between mediasoup workers, tokio workers and
everything else, next to the usual delivery and readiness figures. Build both
images from the checkout, then for example:

```sh
podman build --target production -t localhost/simplestchat-production:dev .
podman build --target loadtest -t localhost/simplestchat-loadtest:dev .
node load_tests/benchmark-podman.mjs \
  --server-image localhost/simplestchat-production:dev \
  --generator-image localhost/simplestchat-loadtest:dev \
  --output results/capacity-200c.$(date -u +%Y%m%dT%H%M%SZ) \
  --clients 200 --scenarios multi-room --subscription-plan ring-v1 --subscription-seed 17 \
  --workers 2 --cpus 2 --memory 2g --ramp-up 405 --warmup 10 --duration 120
```

The ramp must respect the server's join limits from one address (about two
seconds per client above 30). The `webinar` scenario is the one-to-many shape:
one room, one publisher and every other client a viewer, each viewer joining
from its own loopback address (`--source-addresses 250`, as distinct viewers
would), so the per-address limits do not shape the ramp and a room of several
hundred viewers fills in minutes; because a room lives on one worker, it also
measures the single-worker ceiling of a large room. The quota is the
production shape; the cores are Apple silicon, so absolute figures do not
transfer to the VPS, while the per-client cost, throttling and the
worker/runtime split do inform limits.
`--netem "loss 5% delay 50ms 10ms"` adds a NET_ADMIN sidecar built from
[`load_tests/netem.Containerfile`](../load_tests/netem.Containerfile) that
impairs UDP inside the server's namespace, when the VM kernel ships `sch_netem`
(the pinned Podman 5.5 machine image does not; the CI runner does).

[`build/impair.sh`](../build/impair.sh) applies the same profiles to an owned
native server on Linux (netem on loopback, sudo), and
[`build/impaired-check.sh`](../build/impaired-check.sh) starts a guest-only
server and runs the browser scenario against it. macOS pf dummynet does not act
on loopback, so on a Mac the scenario only exercises its mechanics
(`IMPAIR_SCRIPT=none`).

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

### Populated-room UI stress

With release server/UI builds and the pinned Chromium tooling installed:

```sh
UI_STRESS_E2E=1 TEST_SERVER_BINARY="$PWD/target/release/simplestChat" \
  build/with-test-postgres.sh build/with-test-server.sh \
  npm --prefix web/e2e run test:stress
```

The full profile uses two authenticated browser observers and 38 protocol guests,
then sends 960 messages over at least two minutes. It checks every recipient,
300-message history rollover, retained-session reconnect/replay, drafts, scrollback,
settings and mobile-width containment. No camera or microphone is activated.
Joins are paced within the unchanged room quotas; allow about seven minutes.

Use `UI_STRESS_PROFILE=smoke` for six participants and 48 messages when checking
the harness. Smoke does not cover history rollover or long quiet sessions.
See [browser stress coverage](../web/e2e/README.md#populated-room-ui-stress) for
reports, cleanup gates and measurement limits.

## Diagnosing missing media

Add `--purpose diagnostic` to the local comparison command to collect transport
snapshots, bounded native forwarding samples, server operation traces and signaling logs instead of resource
measurements. See [local server diagnostics](diagnostics.md) for configuration,
coverage and shutdown results. For packet headers,
also add `--capture-interface lo0` on macOS or `lo` on Linux; this requires
`tcpdump` and existing capture permission.

`--diagnostic-detail capture-only` collects packet headers without extra
application logging. Captures are limited to the runner's loopback media ports
and may end before the workload; check timestamps in `media-headers.pcap` and
dropped-packet counts in `capture.log`. Keep diagnostic artifacts private. See
[missing-media diagnostics](../load_tests/README.md#diagnosing-missing-media)
for the report fields.
