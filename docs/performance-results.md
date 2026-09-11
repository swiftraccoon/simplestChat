# Performance results

## Quality and settings update — 2026-09-11

Compared `e054e6b` with the quality/settings working tree using frozen release
servers and web assets. The candidate server SHA-256 begins `08cfc3c0b1255845`;
its main JavaScript SHA-256 begins `cb5d25e755f952b4b`. These identify the measured
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
| Camera publish to first decoded frame | 166.84 ms | 158.43 ms |
| Main JS bundle | 390,068 B | 408,627 B |
| Main JS bundle, gzip level 6 | 78,363 B | 84,107 B |

Paint and interaction timings remained close in this small sample. The new
validation and UX have a measurable size cost: 5,744 more gzip bytes (+7.3%) and
107,262 more startup heap bytes (+4.1%). The production assets pass the new
[size budgets](performance.md#web-asset-budgets).

All six runs recorded decoded video during the five-second sample, had no page errors, and
released their media connections after leaving. First-frame timing ranged from
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
| Camera publish to first decoded frame | 146.55 ms | 147.04 ms |
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
Sixteen follow-up one-minute runs did not reproduce the failure, but its cause
has not been identified. Larger-scale regression validation remains incomplete.

A separate 50-client single-room attempt hit the room/IP admission limit;
it provides no throughput measurement. Production capacity and long-running
churn/soak behavior have not been established by these local tests.
