# Performance results

## Adaptive path under an impaired downlink — 2026-09-21

The weekly workflow's impaired job (`web/e2e/impaired-network.cjs` under
netem on a hosted Linux runner) is the measurement behind the server's
layer-selection decision. A publisher sends three simulcast layers from a
fake 720p camera; the viewer's downlink is degraded phase by phase and its
native inbound statistics and the client's layer-change log are sampled once
a second. Three runs bracket one change: the server's fixed bitrate tiers
that cap each consumer's preferred layer, which looked redundant next to
mediasoup's own bitrate-driven selection and were removed in `cecc0d8`, then
restored in `beab4bd` after the measurement below. The client-side tile-size
cap (`e468c84`) is present in the last two runs.

| Phase | Tiers on, before (35541993704) | Tiers removed (35551539603) | Tiers restored (35552045902) |
| --- | --- | --- | --- |
| 5 % loss, 50 ± 10 ms: decoded fps | 18.9 | 9.2 | 16.0 |
| 400 kbit/s cap: seconds to layer 1 | 0.8 | 19.5 | 4.8 |
| 150 kbit/s cap: seconds to layer 0 | 3.0 | 30.9 | 2.9 |
| 150 kbit/s cap: freezes (count / seconds) | 2 / 0.86 | 2 / 4.92 | 2 / 1.06 |
| Recovery: seconds to the top layer | 15.4 | 16.7 | 17.7 |
| Join under 5 % bidirectional loss: seconds to video | 2.3 | 2.0 | 2.1 |

**mediasoup alone steps layers down far more slowly than the tiers do**: it
lowers a consumer's layer only as the estimate falls below each layer's
measured bitrate, while the tiers cut the ceiling within seconds of the
estimate dropping under 600 or 200 kbit/s. Decode rate under loss halved
without them. The tiers therefore stay; the thresholds, not their existence,
are the tuning surface, and every change must be proved on this job. Recovery
to the top layer (15–18 s in all three runs, dominated by the congestion
controller's ramp and a keyframe per step) and the time to first video for a
joiner under bidirectional loss (2–2.3 s) did not depend on the tiers. Each
run's `impaired-network-results.json` is retained as that run's
`impaired-network-<run id>` artifact for 30 days; single runs, so read the
downgrade times as an order of magnitude, not a tenth of a second.

## Production shape: 2 CPUs, 2 GiB, two workers — 2026-09-20

Production runs the server container with a 2.0-CPU quota, a 2 GiB memory
limit and two media workers. These runs put the production image (built from
`1a2f5b5`, server source unchanged since the deployed `ace63ae`) under exactly
those limits inside the Podman Linux VM on this Mac and drove it with the
load-test image over loopback in the same network namespace, one run per size
of the four-room `ring-v1` seed 17 workload with a 120-second measurement
(`load_tests/benchmark-podman.mjs`, see [performance.md](performance.md#production-shape-and-impaired-networks)).
All **3 runs passed**: every planned subscription delivered in every measured
second (800, 1,600 and 2,400 validated consumers, none failed), every client
received bandwidth estimates, no rejections, errors, worker deaths or full
outbound queues, and the server exited cleanly.

| Clients | Consumers | Received packets/second | Receive-ready P99 | Server CPU, share of the 2-CPU quota | Throttled periods | Peak RSS | Worker threads' share of server CPU |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 100 | 800 | 69,200 | 424 ms | 31.7% | 0 | 202 MiB | 98.4% |
| 200 | 1,600 | 138,395 | 427 ms | 45.1% | 0 | 378 MiB | 98.8% |
| 300 | 2,400 | 207,570 | 420 ms | 52.8% | 0 | 558 MiB | 99.5% |

**The cost is almost entirely inside the mediasoup worker threads**: the tokio
runtime and everything else used 0.4–0.6 CPU-seconds per 120-second window
against 75–125 for the workers. **Per-client cost falls as load rises**: 100
clients cost 0.63 CPU-cores in total, 300 cost 1.05, so each additional 100
clients cost roughly a third of the first 100, consistent with the workers
handling more packets per wakeup as their loops fill. No cgroup period was
throttled at any size, so the quota was never the limiter here, and memory
grew linearly at about 1.8 MiB per client from a 20 MiB base.

**What this does and does not establish.** The quota is the production shape;
the cores are Apple silicon in a VM, which are faster than the VPS's cores by
a factor this measurement cannot determine. Read the figures as headroom
ratios: at the configured `MAX_CONNECTIONS=200` the server used under half of
its CPU budget and under a fifth of its memory limit on these cores, and the
cost curve is sublinear. **Calibration:** the same 100-client, two-worker
workload on a hosted GitHub runner (AMD EPYC 7763, 4 vCPUs, no quota; run
35541253903, three same-source pairs) used a median 120.4 % of one vCPU
(117.7–121.6) with the same 207 MiB peak RSS and 424 ms receive-ready P99, so
one of these Apple-silicon VM cores did the work of 1.92 EPYC vCPUs. Scaling
the ladder by that factor puts the production quota on EPYC-class cores at
about 60 % for 100 clients, 86 % for 200 and 101 % for 300:

| Clients | Share of the 2-CPU quota, measured (Apple silicon VM) | Projected on EPYC 7763 vCPUs |
| ---: | ---: | ---: |
| 100 | 31.7% | 61% |
| 200 | 45.1% | 87% |
| 300 | 52.8% | 101% |

**The configured `MAX_CONNECTIONS=200` therefore sits near the CPU ceiling
of the production shape on EPYC-class cores for this synthetic workload, and
300 would saturate it.** Browser traffic with simulcast, congestion feedback
and TURN costs more per client than this generator, and the VPS's own core
speed is unmeasured, so 200 is a ceiling to keep, not headroom to spend;
lowering it is the safe direction until the VPS is measured. Loopback
carries no real network cost (no TURN, no packet loss, no NAT keepalives), the
generator's fixed-rate synthetic RTP is lighter than browser traffic with
congestion feedback and simulcast, and nothing here measures browser quality.
Evidence: `results/capacity-{100,200,300}c-2cpu.20260920T2*`.

## Two media workers at 100 clients — 2026-09-20

Production runs `MEDIA_WORKERS=2`, but every earlier comparison used one worker.
Four runs of the same build (`f36ff07`, source tree `5367b7ded9f1d05d`, the
deployed review-fix server plus CI-only commits; generator built from the same
tree) repeated the four-room, 100-client `ring-v1` seed 17 workload with two
workers, as a same-build pair so the spread is the measurement noise. All
**4 runs passed** on the same delivery criteria as the review-fix comparison
(800 validated consumers per run, none failed or skipped, clean shutdown, no
failed resource samples); both workers were live and both carried rooms (each
logged its own consumer teardown), though the exact split is not recorded at
the default log level. Keyframe counts (4,981–4,983 generated, 289–291 requested) match
the one-worker runs, so the offered load did not change.

| Measurement | Two workers, median (range of 4) | One worker, review-fix candidate (3) |
| --- | ---: | ---: |
| Received packets/second | 69,200.2 (69,199.8–69,200.7) | 69,199.88 (69,199.18–69,200.22) |
| Receive-ready P99 | 411 ms (410–412) | 416 ms (415–418) |
| Server CPU, percent of one core | 100.23% (99.40–106.63) | 70.33% (64.48–71.37) |
| Server sampled peak RSS | 595.45 MiB (595.22–595.72) | 594.27 MiB (594.05–595.20) |

**Two workers forward the same traffic with about 30 percentage points more
total process CPU** (whole process, both worker threads and the Rust runtime
together; the sampler has no per-thread attribution) and no change in memory,
delivery or readiness. The ranges do not overlap. Each worker therefore ran
near half a core instead of one worker near 0.7 of a core, which is the
configuration's purpose: per-worker headroom before a single event loop
saturates, at the price of total efficiency. The mechanism is not isolated
here; less per-wakeup batching in two lighter loops is consistent with the
numbers but unmeasured. Capacity planning for the production shape should use
the two-worker figures, and a two-worker build comparison needs its own pair.

Inputs: the same host and tooling as the review-fix comparison below, two
media workers, a 205-second ramp, ten-second warmup and 120-second
measurements, `results/bench-100-multiroom-ring-2workers.20260920T043842Z`.
Co-located synthetic RTP without congestion-control feedback; not production
capacity or browser quality.

## Review-fix comparison at 10, 30 and 100 clients — 2026-09-20

Three alternating baseline/candidate pairs per workload compared the last
pre-review build `f427e2f` (frozen server `44b57e57142d8712`) with the deployed
review-fix build `1cf352b` (`8bef4346eb87d0d1`), using one generator
(`5ff5515a9c316fb9`, built from the candidate) for both. All **18 runs passed**:
every expected subscription delivered in every measured second (80, 240 and 800
validated consumers per run, none failed or skipped), every publisher attempt
met the new offered-load floor, resource sampling covered each window with no
failed samples, rooms/participants/connections returned to zero, and every
server exited cleanly.

| Workload | Measurement | Baseline median (range) | Candidate median (range) |
| --- | --- | ---: | ---: |
| Conference, 10 clients, FIFO | Received packets/second | 6,920.00 (6,920.00–6,920.00) | 6,920.00 (6,919.95–6,920.02) |
| | Receive-ready P99 | 412 ms (411–412) | 412 ms (410–412) |
| | Server CPU, percent of one core | 18.81% (18.16–19.41) | 18.53% (18.47–18.76) |
| | Server sampled peak RSS | 95.00 MiB (94.80–95.58) | 94.23 MiB (93.53–94.39) |
| Four rooms, 30 clients, `ring-v1` seed 17 | Received packets/second | 20,760.00 (20,759.83–20,760.00) | 20,760.00 (20,759.82–20,760.18) |
| | Receive-ready P99 | 416 ms (414–418) | 415 ms (414–415) |
| | Server CPU, percent of one core | 48.61% (42.21–50.19) | 51.58% (41.66–52.67) |
| | Server sampled peak RSS | 210.69 MiB (210.56–210.70) | 206.45 MiB (206.44–206.52) |
| Four rooms, 100 clients, `ring-v1` seed 17 | Received packets/second | 69,200.00 (69,199.82–69,200.22) | 69,199.88 (69,199.18–69,200.22) |
| | Receive-ready P99 | 416 ms (414–418) | 416 ms (415–418) |
| | Server CPU, percent of one core | 70.92% (65.95–71.96) | 70.33% (64.48–71.37) |
| | Server sampled peak RSS | 609.44 MiB (609.39–609.48) | 594.27 MiB (594.05–595.20) |

Admission P99 was 1 ms and send-ready P99 within 408–418 ms in every run.
The received-packet rates are the generator's nominal rates (see the
[reproducible baseline](#reproducible-100-client-baseline--2026-09-14)); they
confirm complete delivery, not capacity. The 100-client graph identity was
`b916f569a92ea0ab`, the same graph as that baseline; the 30-client graph was
`08b2ac731656e145`.

**CPU is unchanged within measurement noise** at all three sizes. The 30-client
medians differ by 6.1 percent, but the ranges overlap almost entirely and the
unchanged generator's CPU moved by the same amount in the same runs, which
points at host variance rather than the server. **Peak RSS is lower on the
candidate** by 4.2 MiB at 30 clients and 15.2 MiB (2.5 percent) at 100 clients,
with no overlap across the three repeats on either side; this is consistent with
the smaller per-socket WebSocket read buffer and the shared outbound payload
buffer, but the pair does not isolate which change contributed how much.

The new keyframe counters were identical for both builds: 4,981–4,982 keyframes
generated and 289–290 requested per 100-client run (718 and 89 at 30 clients,
172–175 and 23–26 at 10), so the offered load did not differ between variants.

Inputs: Apple M5 Max, 18 logical CPUs, 128 GiB RAM, Darwin 27.2.0; Rust 1.98.1
with static OpenSSL 3.5.8; Node 26.8.1; one media worker; synthetic
480p/30 fps with four audio and four video subscriptions per client; ramps of
5, 65 and 205 seconds with a ten-second warmup and 60, 60 and 120-second
measurements; both trees clean at their revisions. No builds or other owned
workloads ran on the host during measurement. These are co-located,
single-worker, synthetic-RTP observations without congestion-control feedback;
they establish delivery and relative resource use for these builds, not
production capacity, browser quality, or multi-worker behaviour.

## Signed-in chat continuity — 2026-09-15

The original build disconnected both users at 15 minutes despite successful
token refreshes. Both sessions required a full room rejoin, and one of 286 send
attempts was not acknowledged.

With same-socket authentication renewal, the **20-minute run passed all 366
messages**. Both users refreshed and renewed their existing sockets before the
original expiry. There were zero socket closures, room rejoins, rejected resumes
or input-disabled intervals. Drafts and rosters stayed intact, the retained
300-message history matched exactly, and cleanup returned all membership counts
to zero with clean browser/server/database shutdown.

Both runs used the same harness and Chromium version, two fresh accounts, and
desktop/375px viewports on an owned local server. This covers one foreground
refresh cycle, not suspended tabs, physical mobile browsers or live media.
See [the session soak](testing.md#authenticated-chat-continuity) to repeat it.

## Concentrated publisher load — 2026-09-14

Two `hotspot-v1` repeats passed all **200 client attempts and 1,600 subscriptions**,
with delivery in every measured second and identical realized graphs. Each run
used 100 publishers across four rooms. Four early publishers per room each served
24 subscribers per kind; a fifth served four. This concentrates the same 800
subscriptions on fewer publishers than the balanced ring workload.

| Measurement | Range across two repeats |
| --- | ---: |
| Received packets/second | 69,199.91–69,200.17 |
| Receive-ready P99 | 412–413 ms |
| Server CPU, percent of one core | 66.25–66.67% |
| Server sampled peak RSS | 560.33–560.36 MiB |

Both runs recorded zero generator errors, skipped coverage, full/closed-queue send
rejections and final rooms, participants or connections. Cleanup took 30.17 seconds
and all processes exited cleanly. Native bitrate-clamping teardown logs remain
(900 / 905); bitrate policy is unchanged. Two ten-client hotspot runs also passed
with complete server/media/lifecycle diagnostics.

Inputs: seed 17, one worker, a 205-second ramp, ten-second warmup and 120-second
measurement on the same local Mac. Frozen server `d54792e46079c795`, generator
`c8606b697acdb1c9`, graph `d5879adc2be16908`; no competing owned workloads or
builds. These are same-build concentrated-load observations, not a speedup over
ring or a capacity claim. Earlier intermittent readiness failures did not recur
and remain unresolved. See [fixed graphs](../load_tests/README.md#fixed-subscription-graphs).

## Reproducible 100-client baseline — 2026-09-14

Two same-build repeats passed all **200 client attempts and 1,600 subscriptions**,
with packets in every one of the 120 measured seconds. Both used the same
realized publisher/subscriber graph: four rooms, 25 publishers per room, four
audio and four video targets per client, and fan-out exactly four per producer.
The [fixed-graph workload](../load_tests/README.md#fixed-subscription-graphs) used
`ring-v1`, seed 17, one worker, a 205-second ramp and ten-second warmup.

| Measurement | Range across two repeats |
| --- | ---: |
| Received packets/second | 69,199.18–69,200.22 |
| Receive-ready P99 | 411–417 ms |
| Server CPU, percent of one core | 67.99–69.01% |
| Server sampled peak RSS | 593.53–594.27 MiB |

Both runs had zero generator errors, skipped coverage, full/closed-queue send
rejections and remaining rooms, participants or connections. All processes exited
cleanly; cleanup took 29.66–29.67 seconds. Native bitrate-clamping teardown logs
remain (908 / 812 messages); the minimum bitrate is unchanged.

Separate ten-client diagnostic repeats passed all 160 subscriptions with complete
server/media/lifecycle evidence and identical realized graphs. The new bounded
[setup-failure capture](diagnostics.md#capture-send-readiness-failures) passed
injected-future tests; no native readiness timeout occurred in these runs.

Inputs were the local Mac described below, frozen server `d54792e46079c795`,
generator `9e01a62747614d41`, and graph `b916f569a92ea0ab`. No competing builds or
owned workloads overlapped measurement. Generator CPU varied from 126.17–136.28%
and RSS from 271.47–398.16 MiB. This is a balanced-load baseline, not an improvement
over FIFO, a production-capacity claim, or a fix for earlier intermittent setup
failures. Concentrated fan-out and churn need separate checks.

## Departure fan-out — 2026-09-14

The accepted change skips departure notifications to already-closed signaling
queues without altering bitrate limits, reconnect grace, or accounting for full
queues and close races. A separate 100-client run passed all 100 attempts and
800 subscriptions, with packets received by every consumer in all 120 measured
seconds. It recorded **zero rejected departure sends**, zero remaining rooms,
participants and connections, and clean process exits.

Throughput was 69,200.29 packets/s; server CPU was 64.84% of one core and sampled
peak RSS 567.55 MiB. Native bitrate-clamping errors remain: this run logged 893
during teardown. The frozen server was `34e7c2b84c2d3c69`, with unchanged generator
`b40d5a6cb8cff135` and the [same workload](#100-client-media-reliability--2026-09-14).

The unchanged server ran second in this order-reversed check and failed four
ICE/DTLS readiness waits and four client-coverage checks. Its process exited
cleanly, but the workload failed and has no post-grace cleanup-zero receipt.
This establishes that the intermittent setup failure also occurs without the
new changes; it does not identify its
cause. The pair is **not a valid performance comparison**, and the passing
departure run is not a production-capacity claim.

## Transport cleanup experiment — 2026-09-14

**The zero-minimum-bitrate candidate was not accepted.** The existing 100 kbps
minimum remains unchanged. The absence of observed bitrate-clamp logs did not
justify the received-throughput and CPU results below.

A contemporaneous pair used the same host and
[100-client workload](#100-client-media-reliability--2026-09-14), unchanged generator,
and frozen servers: baseline `e868474041a3a00f`, experimental `b0cf331193863411`.
No other owned builds or workloads overlapped these measurements.

| Measurement | Baseline | Experimental |
| --- | ---: | ---: |
| Received packets/second | 69,200.21 | 54,892.86 |
| Server CPU, percent of one core | 66.31% | 86.93% |
| Native bitrate-clamping errors during teardown | 892 | 0 |
| Closed-queue departure attempts during cleanup | 1,200 | 0 |

Both runs passed all 800 consumer-continuity checks and exited cleanly with zero
remaining rooms, participants and connections. However, received throughput was
**20.7% lower** and server CPU **31.1% higher** in the experiment, despite nearly
identical queued packet totals. In these runs, every consumer received packets
in each measured second; this does not establish complete packet delivery.
Queued counts are not confirmed wire output, so the difference is not a measured
network-loss rate.

Earlier candidate runs remain part of the evidence: one passed continuity with a
2.34% received-packet deficit against this workload's nominal fan-out budget;
another failed three ICE/DTLS readiness waits and four client-coverage
checks. That failed run has no post-grace cleanup-zero receipt.

This ordered pair supports withholding the policy change, not attributing the
cause conclusively. The synthetic client negotiates no congestion-control
feedback, so neither build's bandwidth estimator ran during these runs; the
difference lies elsewhere in the forwarding path. The realized publisher/subscriber graph also varied between
runs. Next checks need reproducible subscription selection and separate bounded
transport/packet-path diagnostics; buffer or timeout changes are not yet justified.

## 100-client media reliability — 2026-09-14

The original build passed all 800 media-subscription checks but recorded three
signaling resets during a 335-second session. A short real-socket regression
reproduced quiet-member idle expiry. After adding bounded, membership-gated
protocol keepalives, two identical-build repeats passed all **200 client attempts
and 1,600 subscriptions**, with no signaling resets or skipped coverage. The old
reset timestamps fit idle expiry, but the original error-only logs do not prove
that attribution directly.

Each repeat used four rooms, 100 synthetic publishers, one worker, 480p/30fps,
four audio plus four video subscriptions per client, a 205-second ramp,
10-second warmup and 120-second measurement. All expected consumers received
packets in every measured second. On the local host described below:

| Measurement | Range across two repeats |
| --- | ---: |
| Received packets/second | 69,199.79–69,200.08 |
| Receive-ready P99 | 413 ms |
| Server CPU, percent of one core | 64.42–66.98% |
| Server sampled peak RSS | 568.20–571.59 MiB |
| Post-workload membership cleanup | 29.67–30.19 s |

Both servers/generators exited zero, with zero remaining rooms, participants and
connections. **Teardown is not error-free:** the runs logged 751 / 939 native
[bitrate-clamping errors](diagnostics.md#native-bitrate-clamping-messages), all
28–30 seconds after workload completion, and each recorded 1,200 closed-queue
attempts during departure fan-out to disconnected grace sessions. Follow-up
experiments are recorded above; general worker logs are not included in the
signaling error counter.

Server `e868474041a3a00f` and unchanged generator `b40d5a6cb8cff135` were frozen
for both repeats. No builds or other owned workloads overlapped. These are
same-build reliability observations, not a CPU comparison against the failed
original, all-to-all media, browser quality or VPS capacity. The local runner's
WebSocket admission overrides were 128 connections/IP and 600 handshakes/minute;
room-join quotas remained unchanged. See [workload setup](performance.md#choose-a-workload).

## Populated chat and history replay — 2026-09-14

One local before/after pair compared the `fe05fe2` UI with batched replay and
incremental message rows. Each run used two authenticated Chromium observers
(1440px and 375px), 38 protocol guests, 960 messages at up to eight per second,
300-message history rollover and one retained-session reconnect. Both used the
same heartbeat-fixed release server, browser and harness, with fresh databases
and no competing builds/tests.

| Instrumented UI measurement | Before | After |
| --- | ---: | ---: |
| 300-entry replay, snapshot arrival → post-dispatch DOM | 3,465.7 ms | 63.6 ms |
| Main-thread task time, both pages during traffic | 46.41 s | 19.02 s |
| Full-backlog live arrival → DOM P95, desktop / 375px | 15.7 / 15.8 ms | 3.0 / 3.0 ms |
| Full-backlog trusted input → animation frame P95, desktop / 375px | 17.4 / 16.7 ms | 15.0 / 17.0 ms |

Both runs passed exact message content/identity checks for all recipients,
history bounds, drafts, scrollback, settings and cleanup. The reconnect recovered
37 / 36 missed messages; those intentional closed-queue attempts were confined
to the reconnect gap. There were no unexpected disconnects, server errors,
failed writes or full queues. Browser/server processes exited cleanly.

The task-time window excludes ramp and teardown; sends lasted 120.66 / 121.11
seconds. Replay is one observation per build, and live P95 covers the post-replay
full-backlog phase. The candidate still recorded a 64 ms replay long task.
These are encouraging local observations, not stable regression thresholds,
INP, paint guarantees or production capacity. No physical devices or capture
were used; sampled heap/DOM counts do not establish leak freedom.

Inputs: Apple M5 Max, 18 CPUs, 128 GiB RAM, Darwin 27; Chromium 153.0.8010.12,
Playwright 1.63.0, Node 26.8.1, PostgreSQL 15.19, Rust 1.98.1/static OpenSSL 3.5.8.
Frozen SHA-256 prefixes: server `e868474041a3a00f`; UI `ae224eeecddf94d9` /
`d06fa916a5ecc5a4`; harness `4594ecfdffcd74c4`.
See [the repeatable workload](performance.md#populated-room-ui-stress).

## Current-build baseline — 2026-09-12

These are repeatability measurements, **not a before/after comparison**. Native runs
used the same frozen release server (`2d6366d1ece842a1`) and corrected generator
(`aeb825d83181b14c`), with diagnostics disabled. The host was an Apple M5 Max,
18 logical CPUs, 128 GiB RAM, Darwin 27.0.0; Rust 1.98.1/static OpenSSL 3.5.8.
No builds or other owned workloads overlapped measurements.

Native workloads used one worker, synthetic 480p/30 fps, caps of four audio and
four video subscriptions per client, a five-second ramp and ten-second warmup. Values are
medians of per-run measurements; CPU is percent of one core and RSS is sampled peak.

| Workload | Runs × measured seconds | Receive-ready P99 | Received packets/s | Server CPU | Server RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| Conference, 10 clients | 6 × 60 | 411 ms | 6,920 | 13.15% | 91.80 MiB |
| Four rooms, 30 clients | 6 × 60 | 411 ms | 20,760 | 42.25% | 201.81 MiB |
| Churn, 3 clients | 2 × 120 | 410.5 ms | 713 | 5.51% | 52.83 MiB |

All 258 connection attempts and 2,008 consumer lifetimes passed coverage gates.
Rooms, participants and connections returned to zero; all native processes exited
0 without signals. Churn produced different schedules: seven versus eleven
attempts. Its coverage is scoped to stable publishers.

CPU varied widely with the same build: 7.85–19.92% for conference and
21.85–48.48% for four rooms. These local samples do not establish regression
limits or production capacity. Native bitrate-clamping errors remain: after the
stable workloads, and also during churn measurement. The historical receive
failure did not reproduce; its cause remains unresolved.

Three valid Chromium 153.0.8010.12 / Playwright 1.63.0 samples used fresh PostgreSQL
15.19 databases, two users, ten paced messages and five seconds of fake-camera
playback. Median FCP was **44 ms**, chat delivery
**41.18 ms**, and publish-to-positive-native-decoded-frame evidence **142.21 ms**
(141.24–169.60 ms). Timing includes automation/polling. Every sample decoded
81–82 additional frames and passed media, browser and server cleanup. Two initial
timings with invalid first-frame evidence were retained and excluded.

The community suite passed all **22 scenarios**, including settings, preview,
capture restart and media-preserving signaling recovery. Accessibility completed
**26 scans and 12 keyboard/layout checks**, with no automated violations;
15 incomplete contrast rule results still need manual review. These checks use
synthetic capture and resized desktop Chromium, not physical devices or full
accessibility conformance.

## Observability update, diagnostics disabled — 2026-09-11

Compared `46b1af7` with the observability working tree using frozen release
servers and the same generator. Operation recording, native media sampling and
extra logs were disabled. Server SHA-256 prefixes are `08cfc3c0b1255845`
(baseline) and `bb12f3ec44823ef1` (candidate). This measures the whole server
update, not the snapshot endpoint in isolation, and applies only to those builds.

Three alternating pairs used ten publishers, one room and worker, synthetic
480p/30 fps media, four audio and four video subscriptions per client, a
five-second ramp, ten-second warmup and 60-second measurement. Hardware was
an Apple M5 Max with 18 logical CPUs and 128 GiB RAM, running Darwin 27.0.0;
servers used Rust 1.98.1 and static OpenSSL 3.5.8. No builds or other owned test
suites ran during measurement. Values below are medians of three per-run values.

| Metric | Baseline | Candidate |
| --- | ---: | ---: |
| Send ICE/DTLS-ready P99 | 409 ms | 409 ms |
| Receive ICE/DTLS-ready P99 | 409 ms | 409 ms |
| Received packets/second | 6,920 | 6,920 |
| Server CPU, percent of one core | 11.46% | 11.31% |
| Server peak RSS | 91.31 MiB | 91.88 MiB |
| Generator CPU, percent of one core | 20.64% | 20.56% |
| Generator peak RSS | 35.13 MiB | 35.20 MiB |

All 80 subscriptions passed in all six runs, with no failed or skipped consumers.
Rooms, participants and connections returned to zero; every server exited with
code 0 and no signal. Each CPU interval had 119 samples spanning 59.67–59.73
seconds. No diagnostic artifacts were generated.

Throughput and readiness were unchanged by median. Candidate peak RSS was higher
in every pair: +0.53, +1.34 and +0.45 MiB, with a median-of-runs increase of
0.56 MiB (+0.6%). CPU changed by −0.15 percentage points (−1.3% relative) by
median, but paired relative changes were −4.4%, +4.1% and −1.3%; ranges overlapped
(11.28–11.65% baseline, 11.13–11.74% candidate). This does not establish a CPU
improvement or prove zero overhead.

Both variants logged [native bitrate-clamping errors](diagnostics.md#native-bitrate-clamping-messages)
during post-workload cleanup. Delivery and exits passed, but the logs were not
error-free; this shared cleanup behavior remains separate from the measured
forwarding results.

This paced, ten-client workload validates sustained delivery at that load, not
production capacity, enabled-diagnostics overhead, or the cause of the
[intermittent larger-room receive failure](#open-issue-intermittent-receive-failure).

## Quality and settings update — 2026-09-11

Compared `e054e6b` with the quality/settings working tree using frozen release
servers and web assets. The candidate server SHA-256 begins `08cfc3c0b1255845`;
its main JavaScript SHA-256 begins `cb5d25e755f952b4`. These identify the measured
builds independently of a later commit. Results do not cover subsequent edits.
Hardware was an Apple M5 Max with 18 logical CPUs and 128 GiB RAM
(Darwin 27.0.0). Servers used Rust 1.98.1 and static OpenSSL 3.5.8.

### Browser and API

Three alternating pairs used the same Chromium 153.0.8010.12 / Playwright 1.63.0
harness, fresh PostgreSQL 15.19 databases, two users, ten chat messages and five
seconds of fake-camera playback. Both variants used normal autoplay policy and
identical fake-device options. Builds and other test suites were stopped during
measurements. Values are medians of three per-run medians.

| Metric | Baseline | Candidate |
| --- | ---: | ---: |
| First contentful paint | 44 ms | 44 ms |
| Startup main-thread time | 32.27 ms | 31.83 ms |
| Startup JS heap | 2,589,706 B | 2,696,968 B |
| Registration request | 15.56 ms | 15.31 ms |
| Chat delivery, including automation overhead | 29.11 ms | 29.35 ms |
| Camera publish to video dimensions | 166.84 ms | 158.43 ms |
| Main JS bundle | 390,068 B | 408,627 B |
| Main JS bundle, gzip level 6 | 78,363 B | 84,107 B |

Paint and interaction timings remained close in this small sample. The new
validation and UX have a measurable size cost: 5,744 more gzip bytes (+7.3%) and
107,262 more startup heap bytes (+4.1%). The production assets pass the new
[size budgets](performance.md#web-asset-budgets).

All six runs recorded decoded video during the five-second sample, had no page errors, and
released their media connections after leaving. Video-dimensions timing ranged from
130–231 ms for the baseline and 127–174 ms for the candidate; the median change
is not an established speedup. Local browser samples do not establish production
capacity, real-device behavior or a statistically stable regression threshold.

### Media forwarding

Three alternating pairs used the same frozen generator: ten publishers in one
room and worker, synthetic 480p/30 fps video, four audio and four video
subscriptions per client, a five-second ramp, ten-second warmup and 60-second
measurement. All 80 subscriptions passed in every run, with no failed or skipped
consumers. Rooms, connections and participants returned to zero after each run.

| Metric | Baseline median | Candidate median |
| --- | ---: | ---: |
| Send ICE/DTLS-ready P99 | 406 ms | 408 ms |
| Receive ICE/DTLS-ready P99 | 406 ms | 408 ms |
| Received packets/second | 6,920 | 6,920 |
| Server CPU, percent of one core | 10.08% | 11.16% |
| Server peak RSS | 90.16 MiB | 90.03 MiB |
| Generator CPU, percent of one core | 17.53% | 21.01% |
| Generator peak RSS | 34.91 MiB | 35.11 MiB |

Throughput and memory remained close. **CPU remains inconclusive:** the server
median increased 10.7%, but baseline/candidate ranges overlapped
(9.76–11.63% versus 10.08–11.52% of one core). Within-pair server CPU changes
were −4.0%, +18.0% and −0.03%; the unchanged generator varied substantially too.
These results neither establish a code-caused CPU regression nor rule one out.
Repeat on a quiet representative runner before setting a CPU regression budget.

The ten-client workload does not resolve the [larger-room receive failure](#open-issue-intermittent-receive-failure)
or establish production capacity and long-running recovery behavior.

## Dependency update — 2026-09-09

The comparison from `b386532` to `6459846` showed no material regression in
small local workloads: browser startup and media throughput were steady, and
the JavaScript bundle became smaller. An unresolved receive failure in a
30-client test leaves larger-scale behavior uncertain.

These results apply to those revisions, not subsequent changes. See
[how to run a comparison](performance.md#controlled-local-comparison).

### Test setup

- Apple M5 Max, 18 logical CPUs, 128 GiB RAM, macOS 27.0.
- Locked release builds using Rust 1.98.1 and static OpenSSL 3.5.8.
- The same load-generator binary for both revisions; three alternating pairs
  per measured workload, with the server and generator on the same machine.
- Browser runs used Chromium 153.0.8010.12, Playwright 1.63.0, and fresh
  PostgreSQL 15.19 databases.

### Browser and API

Each run used two browser contexts, two accounts, one room, ten chat messages,
and five seconds of fake-camera playback. Values are medians across three runs
per revision.

| Metric | Baseline | Candidate |
| --- | ---: | ---: |
| First contentful paint | 44 ms | 44 ms |
| Startup main-thread time | 33.14 ms | 32.46 ms |
| Startup JS heap | 2,604,130 B | 2,587,140 B |
| Registration request | 15.31 ms | 15.09 ms |
| Chat delivery, including automation overhead | 28.84 ms | 29.34 ms |
| Camera publish to video dimensions | 146.55 ms | 147.04 ms |
| Main JS bundle | 391,284 B | 386,797 B |
| Main JS bundle, gzip | 83,726 B | 77,585 B |

All six runs recorded decoded video during the five-second sample and released
their media connections after leaving. Chat and camera timing differences were smaller than
the variation between runs.

### Media forwarding

Ten clients shared one room and worker, using synthetic 480p/30 fps video with
four audio and four video subscriptions per client. Each run had a five-second
ramp, ten-second warmup, and 60-second measurement. Values are medians across
three runs per revision.

| Metric | Baseline | Candidate |
| --- | ---: | ---: |
| Send ICE/DTLS-ready P99 | 408 ms | 407 ms |
| Receive ICE/DTLS-ready P99 | 409 ms | 407 ms |
| Received packets/second | 6,920 | 6,920 |
| Server CPU, percent of one core | 10.26% | 10.16% |
| Server peak RSS | 89.63 MiB | 89.78 MiB |
| Generator CPU, percent of one core | 18.49% | 18.23% |
| Generator peak RSS | 34.58 MiB | 34.63 MiB |

All 80 subscriptions received packets in every measured second on every run.
CPU and memory use were effectively unchanged at this load.

### Open issue: intermittent receive failure

In a 30-client, four-room baseline run, one client received no media on its eight
streams despite completing ICE/DTLS setup. The other 232 streams passed.
All eight affected source streams reached other subscribers, which narrows the
failure to that client's receive path. The failed run has no transport snapshot
or packet capture establishing whether delivery stopped at server egress,
transport processing, or the client track reader.
Sixteen follow-up one-minute runs did not reproduce the failure, but its cause
has not been identified. Larger-scale regression validation remains incomplete.

After the separate transport-ordering fix `3fc7b28`, two further 30-client,
four-room checks each delivered all 240 subscriptions in every second of a
three-minute measurement. Native diagnostics were complete and shutdown was
clean. These checks did not reproduce the older receive-only failure; they do
not establish that its cause was fixed.

A separate 50-client single-room attempt hit the room/IP admission limit;
it provides no throughput measurement. Production capacity and long-running
churn/soak behavior have not been established by these local tests.
