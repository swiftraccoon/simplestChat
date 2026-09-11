# Performance results

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

All six runs decoded video throughout the sample and released their media
connections after leaving. Chat and camera timing differences were smaller than
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
