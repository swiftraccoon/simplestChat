# Local server diagnostics

Use opt-in operation traces to locate elapsed time, and native media snapshots
to compare worker forwarding counters with client receipt.
They are separate from [performance comparisons](performance.md): recording,
extra logs and packet capture can change timing. Nothing is uploaded.

## Capture a local session

After the [local build setup](development.md#guest-only-local-ui), run from the
repository root with a new private output directory:

```sh
mkdir -p results
diagnostic_dir=$(mktemp -d "$PWD/results/diagnostics.XXXXXX")
DIAGNOSTICS_PATH="$diagnostic_dir/server-diagnostics.jsonl" \
  build/run-local.sh --skip-web
```

Use only your own test session. Stop with Ctrl-C and wait for the server to exit
before reading the file. The launcher needs an existing web build; on Linux,
set an owned LAN `ANNOUNCE_IP` as described in the development guide.

| Setting | Meaning |
| --- | --- |
| `DIAGNOSTICS_PATH` | Absolute path to a **new** file; parent directory must exist. Existing files and symlinks are not overwritten. Unix file mode is `0600`. Unset means disabled. |
| `DIAGNOSTICS_MAX_RECORDS` | Combined operation/stage record limit: default 10,000, range 1–100,000. The terminal summary is additional. |
| `DIAGNOSTICS_DURATION_SECS` | Recorder lifetime: default 300 seconds, range 1–3,600. This does not stop the server. |

When enabled, invalid settings fail startup. Producers never wait for file I/O: the recorder
uses a 256-record queue, counts full-queue/limit losses, and allows
at most 200 ms to close the writer during shutdown. A completed write or close
is not an fsync/durability guarantee; abnormal exit can leave no terminal summary.
Recorder incompleteness does not change the application's own exit result.

For an automated owned-server run, add `--purpose diagnostic` to the
[local runner command](performance.md#controlled-local-comparison). Full mode
enables server recording, native media samples and generator transport snapshots; the runner reads
the recorder only after server shutdown. It uses a 10,000-record cap and a
300-second lifetime, extended for longer configured runs. Performance and
`--diagnostic-detail capture-only` runs do not enable the recorder.

## Read the results

The runner writes `server-diagnostics-report.json`. To summarize a manually
recorded session after shutdown:

```sh
node --input-type=module - "$diagnostic_dir/server-diagnostics.jsonl" <<'JS'
import { readDiagnosticReport } from './load_tests/diagnostic-report.mjs';
const report = await readDiagnosticReport(process.argv[2]);
console.log(JSON.stringify(report, null, 2));
process.exitCode = report.coverage.complete ? 0 : 1;
JS
```

This standalone check verifies recording only; retain the server exit status separately.

The report groups raw durations by operation, stage and outcome, with counts
and nearest-rank p50/p95/p99 in milliseconds. Nested stages can overlap or repeat:
do not add them together or average their percentiles. The interval includes
setup and cleanup, not just the load generator's shared measurement window.

Check the runner's independent results:

| Result | What it establishes |
| --- | --- |
| `workloadPassed` | Generator delivery checks and room/session cleanup passed. |
| `diagnosticCoverage.complete` | The requested recorder output is valid, nonempty and complete. |
| `mediaDiagnosticCoverage.complete` | Scheduled native media samples and their client correlation meet the coverage checks below. |
| `lifecycleDiagnosticCoverage.complete` | The lifecycle log and phase timeline contain the expected transport, departure and cleanup markers. |
| `serverShutdownPassed` / `serverExit` | The owned server exited with code 0, without a signal or spawn error. Server shutdown gets a bounded 20-second grace. |

Overall `passed` requires all applicable checks. An older server without recorder
support is unavailable coverage, not silent success. Missing/truncated output,
inconsistent counts, dropped/expired records, write failures or unfinished timers
make coverage incomplete. Valid prefix samples remain available as
`recorded_subset`, which is **not** a random sample. Even `all_emitted_records`
covers only instrumented paths during this recorder's lifetime.

## Record contract and current scope

JSONL schema version 1 has `operation` and `stage` records containing fixed
`operation`, `stage` and `outcome` names, numeric `operationId`, nullable numeric
`connectionId`, and monotonic `startedUs`/`elapsedUs`. Operation records have
`stage: null`. IDs are process-local correlation values, not account or room IDs.
The [recorder source](../src/diagnostics.rs) defines the closed vocabulary.

The terminal `summary` reports `accepted` (enqueued), `written` (completed sink
writes), `dropped`, `expired`, `unfinished` (timers still open at recording close),
and `writeFailed`. Accepted records need not all be written after an I/O failure.

Current timing boundaries cover post-admission WebSocket dispatch, reconnect
restore, selected room-join lock/policy/media-setup/membership stages, native
transport/produce/consume/resume work, and text socket writes. Shutdown stages
also log their elapsed time in the application log.

Interpret outcomes narrowly: `ok` is the instrumented result, `error` is a
returned error rather than proof of a service fault, `completed` means an
unclassified future returned, and `cancelled` means the timer was dropped.
`timeout` and `rejected` are used where explicitly classified. Some social
failures are sent as protocol responses while the dispatcher returns `Ok`.

Transport-connect success means accepted connection parameters, not completed
ICE/DTLS negotiation. Consumer-resume success is not first-RTP evidence. Use the
[receive milestones](../load_tests/README.md#locate-the-last-confirmed-receive-milestone)
to check actual client receipt before attributing missing media to a server stage.

Not yet covered: HTTP/DB-pool timing breakdowns, browser stats, password-work
dispatch/execution, and per-message queue residence or cause correlation.
Password permit waiting is measured. Socket-write records correlate by
connection only, not by originating command. Parsing failures and pre-dispatch
admission rejections do not produce operation traces. Reserved stage names do
not imply an instrumented path.

## Server forwarding snapshots

Full diagnostic runs sample `GET /diagnostics/media` while generator peers are
still connected. The endpoint is disabled by default; enabling
`MEDIA_DIAGNOSTICS_ENABLED=true` requires `METRICS_TOKEN` of at least 32 bytes.
Requests use the same Bearer token as `/metrics`. Keep this capability private:
authentication does not restrict it to loopback, and a proxy can expose the route.
There is no background poller, and this setting is independent of the operation
recorder. Performance and capture-only runs do not enable it.

Collection uses non-waiting application-lock reads, at most 64 participants,
1,024 producer/consumer entities, 16 RTP streams per entity, eight concurrent
native stats requests and a shared 750 ms native collection deadline. Only one
HTTP collection is admitted at a time, with at least 250 ms between starts.
These are collection bounds, not a hard deadline for HTTP transmission or a
stalled runtime. There are no packet-path callbacks; native stats requests still
consume worker time. An in-flight sample can briefly retain a native handle.

Responses are uncached. Disabled endpoints return 404, invalid authorization
returns 401, busy/rate-limited requests return 429, and draining returns 503.
A 200 response can still have incomplete coverage: inspect `coverage` and each
entity's `status`. Busy registries, entity limits, closed handles, missing stats,
errors and deadlines are not substituted with zero counters.

The runner saves private `server-media-sample-*.json` files and
`server-media-report.json`. Samples near the shared window's start, middle and
end are correlated with generator consumer IDs and SSRCs. Complete coverage
requires at least two valid scheduled samples and two usable counter observations
for each non-short-lived consumer. Starts more than one second late and missed
or incomplete samples remain visible as incomplete coverage. The rollup reports
`mediaDiagnosticCoverageComplete` separately from workload and shutdown results.
Sampling failures must not erase the generator's original failure.

Consumer packet/byte counters describe the worker's outbound RTP accounting;
producer counters describe worker intake. Independent producer stats are retained
in the raw snapshots, not summarized by the consumer correlation report.
RTP bytes are packet lengths, not
decoded payload size or total wire bandwidth. The report distinguishes increased,
flat, replaced/reset and unavailable counters from client measurement-window
receipt. Flat or paused counters alone are not a delivery failure. Successful
forwarding alone is not peer receipt, and an empty server snapshot is not workload
coverage. Samples and pause flags are non-atomic lifetime observations; server,
worker and generator clocks have separate origins.

Schema version 1 contains fixed field names, media kind, bounded counters and
process-scoped hashed media references; see the
[typed contract](../src/media/diagnostics.rs). The response includes a public
correlation salt, not a credential. References omit raw media UUIDs, participant
and room IDs, addresses, SDP and native errors. This is pseudonymization, not
anonymization: someone holding generator artifacts can match the media identities
within that server process. Keep samples and generator output private together.

## Native bitrate-clamping messages

The pinned media stack can log `start bitrate smaller than min bitrate` after
a transport disconnects. simplestChat sets a 100,000 bps outgoing minimum;
native disconnect handling can reset available bitrate to zero and propose its
30,000 bps fallback as the next starting rate. The controller logs the mismatch,
raises the starting rate to the minimum and continues. This is not a rejected
API request or, by itself, evidence of lost media.

By default, the load generator closes peers without sending `leaveRoom`, exercising the
30-second reconnect grace. Browser explicit leave is a different cleanup path.
Compare the message's timestamp with delivery and transport-state evidence;
appearance during active delivery still needs investigation. Error-only captures
cannot distinguish the individual ICE, DTLS or consumer-close trigger.

Do not change bitrate policy or shorten reconnect grace just to suppress this
message. A configured minimum of zero still uses the native 30,000 bps floor;
lowering today's 100,000 bps minimum changes active congestion-control behavior.
The message's source is the pinned
[constraint clamp](../vendor/mediasoup-sys-0.17.0/deps/libwebrtc/libwebrtc/modules/congestion_controller/goog_cc/goog_cc_network_control.cc).

## Compare explicit leave with disconnect

Use the local runner in full diagnostic mode, keeping the server binary and
workload identical. Run once with `--departure abrupt` (the default), then in a
new output directory with `--departure explicit-leave`. Neither option is accepted
in performance or capture-only mode. This is a cleanup experiment, not a speed
comparison; using the same binary in both runner positions gives repeated checks.

Use a non-churn scenario for this end-of-workload comparison. Explicit leave sends
`leaveRoom` through the existing signaling connection after the client's session
boundary and pre-close media snapshot, before closing peers. With churn, individual
sessions can end during the shared measurement window. The
write has a two-second deadline; failure fails the workload. A completed write is
not a server acknowledgment. Server-side markers separately establish that the
leave handler returned and that media cleanup ran. Abrupt departure still
exercises the unchanged 30-second reconnect grace.

Full diagnostic runs enable the `simplestChat::lifecycle` DEBUG target from server
startup. It records transport creation, ICE/DTLS changes, transport-handle closure,
grace cleanup, explicit leave and application media cleanup. There are no new
packet callbacks, native media commands or background pollers. These ordinary
log markers are separate from the bounded operation recorder; they do not have
its loss accounting or privacy contract. Raw logs contain UUIDs, and the enabled
legacy INFO messages can contain room-scoped identifiers; keep them private.

Read `server-lifecycle-report.json` alongside `lifecycle-timeline.json` and the
delivery report. The timeline retains runner wall-clock anchors and independent
monotonic offsets. The report uses the generator's reported start and configured
ramp-up, warmup and duration to locate the nominal measurement window. It groups
the specific bitrate-clamp message by phase and shows previously observed
transport/cleanup states. Missing boundaries, malformed or truncated input and
missing expected cleanup markers make coverage incomplete.
Phase assignments are approximate cross-process wall-clock comparisons, not a
shared monotonic clock. Treat clock adjustments and lines near boundaries with care.

Native clamp lines have no transport ID: nearby events cannot identify which
transport caused a warning. Worker notifications and Rust callbacks are
asynchronous; log order is not a native call trace. A transport-close callback or
`media_cleanup_finished` marks application handle closure, not worker
acknowledgment. The leave handler can also return successfully for stale
membership without removing it, so inspect actual media cleanup too.

Reports replace participant and transport UUIDs with report-local ordinals.
The reader accepts only private regular files, with limits of 32 MiB, 100,000
lines, 10,000 relevant records and 16 KiB per line. Complete coverage means the
expected instrumented markers were observed: each participant has a receive
transport and completed media/departure markers; each transport has observed
ICE and DTLS readiness plus closure. Readiness remains counted after a later
disconnection. This does not mean every native transition was captured or that
any particular warning caused a delivery failure.

## Metrics and privacy

Prometheus `messages_sent_total` counts completed text sink writes, including
shutdown notifications, not queue acceptance or peer receipt.
`messages_received_total` counts observed WebSocket frames, including control
and rejected frames. Essential control/chat queue rejections have separate
`outbound_queue_full_total` and `outbound_queue_closed_total` counters;
intentional coalescing/dropping of ephemeral media hints is excluded.
All these metric names have the `simplestchat_` prefix.

Closed-queue rejections can occur during normal disconnect cleanup: departure
notifications may target peers whose writers have stopped but whose reconnect
grace memberships remain. Compare counter deltas across workload and cleanup
phases; this counter alone does not establish a service fault or lost chat.

Join/leave counters are narrower than membership lifecycle totals: joins count
direct dispatcher admissions, excluding lobby admission/reconnect; leaves count
explicit dispatcher leaves, excluding implicit or forced departures. Missing
worker/participant snapshots are unknown, not zero; inspect their
`*_snapshot_complete` gauges.

The dedicated diagnostics JSONL uses a fixed-field privacy allowlist:
no message content, account/room identifiers, addresses, credentials or raw SDP.
Ordinary `server.log`, generator snapshots and packet captures have different
contents and can contain identifiers. Keep the entire output directory private;
review artifacts before sharing. There is no automatic upload or retention job.
