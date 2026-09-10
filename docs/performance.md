# Performance measurement

Current capacity has not been established. Historical load results and short
functional smokes cannot rule out regressions. [Recorded comparisons](performance-results.md)
state exactly what was tested, including failures and remaining gaps.

## What the generator measures

The optional Rust client creates real ICE/DTLS connections and synthetic RTP
traffic. Its payloads exercise transport and forwarding, not browser encoding,
decoding, visual quality, end-to-end audiovisual latency or production capacity.
Default subscriptions are capped at **4 audio + 4 video** per participant, not
every publisher in a large room. Keep these caps fixed across comparisons.

Schema 2 reports distinguish:

| Field | Meaning |
| --- | --- |
| Connection latency | Each WebSocket attempt through room admission; no pre-task scheduling time and no ICE/DTLS guarantee |
| Send/receive media-ready latency | Each attempt through the corresponding PeerConnection's connected event |
| Signaling histograms | Exact millisecond sample frequencies merged across all operations, not percentiles of client medians |
| Shared measurement counters | Only the common interval after ramp-up + warmup, including churn within that interval |
| Queued/sent packets | Accepted by the local RTP writer, not confirmed network egress |
| Received packets | Packets observed by the receiving client, with per-consumer one-second coverage |
| Byte counters | Queued RTP packet bytes versus received payload bytes; not comparable wire-byte totals |
| Completion/result | Explicit completed/passed flags, failure reasons, configuration and binary/source provenance |

Do not interpret aggregate receive/send ratio as packet loss: fan-out, subscription
caps and publisher/consumer lifetimes change the denominator. The generator
does not currently measure server-ingress RTP bytes or sequence-normalized loss.

Every eligible consumer must receive packets, with no gap over two complete
seconds after a three-second setup allowance. Reports identify short-lived
streams excluded from that check. Initial capped subscription coverage is also
required. During churn this is a **lifetime minimum**, not proof that every
replacement attempt acquired every possible subscription. Readiness distributions
still retain individual attempts. Random churn schedules are not seed-replayed.

Synthetic video emits a keyframe every five seconds and does not answer PLI
feedback. A late subscription can therefore wait for the next keyframe. Keep the
default ten-second warmup; a shorter warmup can correctly fail the sustained
delivery gate while streams are still starting. Do not hide that by weakening
the receiver assertions.

Client/task/media errors and invalid arguments fail the command. The independent
hard watchdog writes a best-effort incomplete report and exits 124; it must never
be treated as a successful partial benchmark. A timeout marker invalidates any
racing summary. A report must be schema 2, completed and passing
before comparing its timings. See [harness details](../load_tests/README.md).

## Controlled local comparison

The tracked [benchmark-local.mjs](../load_tests/benchmark-local.mjs) starts only
its own loopback servers and generator, checks port availability, samples both
processes, and waits for connections/rooms/participants to return to zero. It
does not SSH, deploy, stop existing services or accept a remote target URL.

1. Choose immutable baseline and candidate revisions and create an isolated
   baseline worktree. Preserve the user's main worktree and record any candidate
   runtime-source diff.
2. Build both production servers with locked release builds, the same compiler,
   target and optimization settings. Configure the pinned static OpenSSL/Python
   environment separately for each tree. Record compiler/native versions and
   command lines; do not compare an old debug binary with a new release binary.
3. Build **one** corrected generator and use that exact binary for both servers.
   Upgrading both generators alongside both servers confounds the comparison.
4. First run an excluded small compatibility smoke. Then stop builds, browser
   tests and other heavy local work during measurement. Watch generator headroom.
5. Run repeated alternating A/B pairs with fixed workers, quality/FPS, room counts,
   caps and windows. Do not increase load merely because the process stayed alive.

Example after building both servers and the current generator:

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

The output directory must not already exist. Defaults use HTTP3129/UDP41100;
override `--port`/`--udp-port` with unused local ports if needed. The tool bounds
clients to 100, workers to 4, duration to 180 seconds and repetitions to 5. Its
loopback-only server configuration permits 128 connections/IP and 600 handshakes
per minute so this bounded co-located workload can run; it does not modify the
application's production defaults. Authentication/database/TURN settings are
not inherited. This particular workload is guest media signaling.

Room joins additionally have fixed limits of 30 attempts/IP and 10 attempts per
room/IP in 60 seconds. The runner checks the initial join ramp against those
limits before starting any processes. For a larger quick-ramp comparison, use
`--clients 30 --scenarios multi-room` (four rooms). Fifty clients in one room
need at least `--ramp-up 303`; fifty across four rooms need at least 101 seconds.
Do not classify a faster rejected workload as a performance regression. Churn
also consumes the join budget: use fewer than ten local clients and inspect
admission failures on longer runs. See [configuration](configuration.md).

The generator source defaults to the candidate checkout. If measuring an archived
executable while editing its source, point `--generator-source-root` at the exact
archived tree containing its `load_tests/` sources. The runner records source and
orchestrator hashes and verifies the executable hash before every run and in
every report; mismatched binaries cannot silently enter a comparison.

For a receive-path investigation, add `--purpose diagnostic`. This enables the
generator's bounded, pre-cleanup transport snapshots and signaling acknowledgments,
retains server transport-state logs, and skips process sampling. Its results are
explicitly excluded from performance comparisons. The runner still stops on the
first failure; fix the number/order of attempts before starting, and retain failures.

To observe an archived generator without adding application instrumentation, use
`--purpose diagnostic --diagnostic-detail capture-only --capture-interface lo0`
(or `lo` on Linux). This leaves both processes at `RUST_LOG=error`, omits the
generator's `--diagnostics` flag and does not require its optional snapshot fields.
It still validates media delivery and binary/source provenance, skips resource
sampling and excludes results from performance comparisons. Quiet logs cannot
rule out receive errors: some upstream RTC pipeline errors are logged at WARN.
Packet capture itself can still perturb timing.

Optionally add `--capture-interface lo0` on macOS or `--capture-interface lo` on
Linux when the current user already has packet-capture access. The runner never
requests elevated privileges. It starts `tcpdump` only after its own server is
ready and captures only IPv4 loopback UDP matching its media port range, with
64-byte packet prefixes. The cap is 500,000 packets with full application
diagnostics or 2,000,000 in capture-only mode. Neither limit guarantees full-run
coverage: check capture timestamps against the measurement window as well as
`capture.log` for kernel drops. Truncated prefixes do not prove SRTP decryption
or completion of a DTLS handshake. Captures and diagnostic metadata are private
local artifacts; new output directories are owner-only. Capture failure fails
the diagnostic run. After the generator exits, the runner allows two seconds for
buffered packets to reach the capture process before stopping it; this grace is
not part of the measurement window and does not replace timestamp checks.
`metrics-stop.txt` preserves the last available server metrics on success or failure.

The test HTTP listener is loopback-only and all generated traffic targets loopback;
the unchanged server's media worker still binds UDP on all IPv4 interfaces.

Other scenarios: `multi-room` (up to 4 rooms), `webinar` (one percent publishers,
rounded up), `audio`, and `churn` (approximately one fifth of clients reconnect
on randomized schedules). Use at least three repeated pairs before interpreting
their noisy resource/latency differences. Short single pairs are correctness
smokes only. The `--duration` flag now means shared steady-state measurement
time, not an independent lifetime starting after each client's setup.

Outputs include binary/lock/source hashes, machine/configuration metadata,
per-run commands, raw generator reports, process samples, Prometheus snapshots,
cleanup checks and a descriptive median/range comparison. Raw artifacts remain
local/ignored; publish a sanitized summary with enough provenance to reproduce it.

Resource samples use `ps` about every 500 ms. CPU is user+system process time as a
percentage of one core (values above 100% are valid). Server samples include its
in-process native workers; generator CPU/RSS is reported separately. RSS is a
sampled peak, not an allocation profile. Sampling omits boundary fractions and
has a small startup/hash offset relative to the generator clock. There is no
child-process resource attribution or control over unrelated laptop workloads.

## Browser and authenticated work

Synthetic guest signaling does not exercise account hashing, database room APIs,
chat delivery, JavaScript startup or browser decoding. Use the pinned browser
tooling separately; do not run it concurrently with resource measurements.

The loopback-only [browser performance harness](../web/e2e/performance.cjs)
records cold-context navigation/paint, CDP task time and heap, hashed assets,
authenticated account/room operations, paced chat delivery and actual camera
decode/cleanup with fake devices. See [browser testing](../web/e2e/README.md)
for its disposable-server setup and measurement command. These are small local
measurements, not WebRTC network quality, real-camera encoding cost or a browser
compatibility matrix.

## Historical evidence and future gates

The old README reported 100/5,000/10,000 clients on an AMD Ryzen 9 8945HS,
16 workers and 80,000 consumers at the largest load. Its apparent 223 ms P99 was
room admission, not media latency. Run SHA, complete workload and repeatability
were not recorded, and the generator has since changed. Those numbers must not
be used as a current 10,000-client capacity promise.

Local July artifacts also contain 500- and 1,000-client runs with 4,000 and 8,000
consumers and admission P99s of 6 ms and 76 ms. Older 1,000-client results had 29,000
consumers and different lifetimes: they are not equivalent baselines. Retain
historical files without using raw receive/send ratios as loss estimates.

CI should gate a small **media correctness** smoke, not noisy hosted-runner
performance. Establish a dedicated, explicitly authorized runner, fixed browser
and network conditions, longer churn/soak coverage, and measurement noise before
setting performance budgets. Use both relative and absolute materiality limits;
a 1 ms increase on a 1 ms baseline is not by itself a meaningful 100% regression.
