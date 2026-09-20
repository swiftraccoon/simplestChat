# Performance results

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
