# Performance results

## Browser-faithful calibration of the Mac VM — 2026-09-26

The first full run of [`build/capacity.py`](../build/capacity.py) ([sizing a
host](performance.md#sizing-a-host)): the 8-CPU, 16 GiB Podman VM on Apple
silicon, both images built at `ff74dbb` (server `e5f3e35d280946ba`, generator
`ffd019dfcb980e1d`), every workload on two media workers under a 2-CPU quota
beside a 5.8-CPU generator emulating browsers (three simulcast layers, DTX
microphones, stateful tile layers), joins at 1, 1.5 and 4 a second for meetings,
the one room and the webinar, a 30-second warmup and a 60-second window, and 1 MiB
worker socket buffers with the VM's `net.core.rmem_max` raised. It took an hour.

| Workload | Size | Verdict | Busiest worker | Worker socket drops | Generator (cores) | Egress | Receive-ready P99 |
| --- | ---: | --- | ---: | ---: | ---: | ---: | ---: |
| meetings of 5 | 125 | pass | 0.33 | 0 | 1.14 | 301 Mbit/s | 419 ms |
| meetings of 5 | 175 | pass | 0.39 | 0.030 % | 1.50 | 420 Mbit/s | 496 ms |
| meetings of 5 | 185 | fail | 0.45 | 1.103 % | 1.70 | 442 Mbit/s | 786 ms |
| meetings of 5 | 200 | fail | 0.44 | 0.168 % | 1.81 | 478 Mbit/s | 625 ms |
| meetings of 5 | 230 | fail | 0.53 | 0.341 % | 1.96 | 541 Mbit/s | 479 ms |
| one room | 22 | pass | 0.18 | 0 | 0.55 | 70 Mbit/s | 426 ms |
| one room | 26 | pass | 0.17 | 0 | 0.65 | 83 Mbit/s | 424 ms |
| one room | 30 | fail | 0.27 | 0 | 0.77 | 98 Mbit/s | 450 ms |
| one room | 38 | fail | 0.53 | 4.350 % | 1.40 | 115 Mbit/s | 635 ms |
| webinar | 460 | pass | 0.40 | 0 | 1.38 | 460 Mbit/s | 492 ms |
| webinar | 500 | pass | 0.41 | 0 | 1.49 | 491 Mbit/s | 502 ms |
| webinar | 545 | fail | 0.45 | 0 | 1.72 | 514 Mbit/s | 437 ms |
| webinar | 715 | fail | 0.64 | 0.774 % | 1.86 | 489 Mbit/s | 1,870 ms |

**Meetings bind on socket drops, not CPU: 175 participants on two workers.** No
generator was throttled and the server's quota never ran out, yet from 185 up the
workers' sockets dropped 0.17–1.1 % of what the clients sent while the busiest
worker was at 0.44–0.53 cores, below the 0.7 guard: millisecond bursts overflow the
1 MiB buffers before a 10-second load average shows them. The spread between 185
and 200 is the burst regime's run-to-run variance, so this ceiling wants repeats.
Each participant costs 2.4 Mbit/s of egress, 1.7 times what a final-grid layer
model gave, because tiles that appeared while a room filled keep the top layer.

**One all-publishing room stops at the per-viewer cap: 26 participants.** At 30
every browser lost two of its 29 video tiles while the workers idled at 0.27
cores: 29 tiles need 2.9 Mbit/s even at the lowest layer (100 kbit/s), and the
server sends each viewer at most 3 Mbit/s with audio beside it. No host raises
this; a higher per-viewer cap, a cheaper lowest layer or fewer live tiles per
viewer would. The public template's 80 was sized with a synthetic client that
consumes four videos, not the 32 a browser shows.

**Webinars carry 500 viewers on two workers; the next failure is unexplained.** At
545 five viewers scattered through the room had their send-side estimate fall to
the 100 kbit/s floor and their video stop, three of them within the same few
seconds, with no drop, no throttling and the workers at 0.45 cores. Whether the
generator's transport-wide feedback ran late under 546 clients or the server's
egress stalled is open. At 715 the guard refused 86 joins at the ramp's end.

Projected to seven app CPUs this host would carry about 612 meeting participants
(`SIMPLESTCHAT_CPUS=7`, `MEDIA_WORKERS=7`, `MAX_PARTICIPANTS_PER_ROOM=23`,
`SIMPLESTCHAT_MEMORY_LIMIT=3072m`). The cores are Apple silicon and the VPS spends
2.4–2.7 times the worker CPU per consumer, so the VPS needs its own run.

## An all-publishing room on the VPS's own cores — 2026-09-26

`MAX_PARTICIPANTS_PER_ROOM` is 80 in the public template, sized from the Mac VM's
200-publisher result scaled by the 2.5 factor measured for viewers. These runs measure it
on the VPS itself: the deployed server image (`aebd7f7`, image `d0c99e8fbbc11079`) as an
owned loopback-only container under the production quota (2 CPUs, 2 GiB, two workers),
the CI-built generator (`809b022`) in its network namespace with 1.9 CPUs, one room of N
publishers (camera and microphone each, `ring-v1` seed 17, four audio and four video
subscriptions per client, 250 loopback source addresses), a ramp of about one client a
second and a 120-second window. The public app was untouched. The deployed server has no
socket buffer setting and the host's `net.core.rmem_max` is the Linux default, so these
are the 208 KiB default buffers.

| Publishers | Runs | Drops at worker 0's socket / inbound | Clients lost | Consumers validated / failed | Worker 0 / 1 (cores) | Receive-ready P99 | Generator (cores; throttled) |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 60 | 1 | 143 / 9,550,457 (0.0015 %) | 0 | 480 / 0 | 0.51 / 0.34 | 446 ms | 0.77; never |
| 80 | 2 | 2,871 / 13,270,378 (0.022 %); 3,447 / 13,311,116 (0.026 %) | 0; 0 | 640 / 0; 640 / 0 | 0.60 / 0.47; 0.65 / 0.51 | 471, 452 ms | 0.96, 0.92; never |
| 100 | 1 | 23,322 / 16,418,709 (0.14 %) | 0 | 800 / 0 | 0.69 / 0.53 | 488 ms | 1.00; never |

**The 80-participant limit holds on the VPS with some margin.** Both 80-publisher runs
delivered every consumer and lost no handshake, with the primary worker at 0.60–0.65
cores, under the 0.7 worker guard. The drops at the primary worker's socket grow about
sixfold per 20 publishers. At 100 every consumer was still delivered, but the primary
worker reached 0.69 cores, where the 0.7 guard starts refusing joins, and its socket
dropped a steady trickle: the drop timeline puts 575 of the 23,322 drops in the ramp and
21,118 in the measurement window, some in every five-second interval, rather than the
Mac VM's collapse at the end of the ramp. Browsers publish three simulcast layers where
the generator publishes one, so a browser room costs the primary worker more per
publisher; that is the reason to keep 80 rather than the synthetic 100.

**The 2.5 factor holds for all-publishing rooms, and it is the worker's alone.** The
workers spent 1.6–1.8 millicores per consumer (0.86 cores for 480 consumers, 1.10 for
640, 1.25 for 800), 2.4–2.7 times the Mac VM's 0.66. The generator spent 0.77–1.00
cores for 60–100 publishers, about 0.0125 cores per client, the same as on the Mac, and
was never throttled, so on this host it fits beside the server; at that rate its 1.9-core
share would bind near 150 publishers. The next release's 1 MiB socket buffer applies here
only once the host's ceilings are raised, and at 80 there is little for it to absorb.

Evidence: `results/vps-conference.20260926T000429Z` (runs `c60`, `c80a`, `c80b` and
`c100`, whose `drop-samples.txt` holds the five-second drop timeline, plus the scripts).
Not measured: browsers, and rooms beyond 100 publishers on the VPS.

## Where the 200-publisher handshake failures come from — 2026-09-25

The all-publishing room of 200 below lost one to five clients per run to ICE/DTLS
timeouts. A packet capture of the handshakes (`benchmark-podman.mjs --capture-handshakes
true`: STUN and DTLS handshake records on the namespace's loopback, worker DTLS debug
logging on) and the kernel's per-namespace UDP counters (`netns-udp.txt`, read from the
capture sidecar while the sockets still exist) locate the loss on both sides of the wire.

**The server side: the kernel discards inbound datagrams at the worker's socket.** Every
run is the same shape (200 publishers, `ring-v1` seed 17, two workers, 2 CPUs, 2 GiB,
200-second ramp, 120-second window, generator `f27c7c3a4ef3632f`). The drop column is the
kernel's `RcvbufErrors` on the primary worker's UDP socket over the whole run against the
namespace's inbound datagrams; the STUN columns are the share of the clients' mid-call
binding requests that got no answer and the worker's answer latency to the rest (request to
response in the capture, so queueing plus processing); clients lost counts handshake
timeouts and joins the worker CPU guard refused.

| Run | Worker socket buffer | Drops at worker 0's socket / inbound | STUN unanswered | STUN answer p50 / p99 / max | Clients lost | Consumers validated / failed | Worker 0 / 1 (cores) | Peak RSS |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| A (`36ff777fd54be5e8`) | kernel default, 208 KiB | not counted | 0.5 % | 0.3 / 2.2 / 6 ms | 2 (DTLS) | 1,568 / 14 | 0.59 / 0.50 | 473 MiB |
| B (`36ff777fd54be5e8`, worker `info,dtls` debug tags, host busy) | kernel default | 1,516,841 / 35,747,176 (4.2 %) | 3.3 % | 0.4 / 3.6 / 140 ms | 11 (DTLS) | 956 / 536 | 0.78 / 0.60 | 550 MiB |
| C (`73ee79b411fdfa36`) | 4 MiB requested, 8 MiB granted | 6,893,911 / 36,768,823 (18.7 %) | 14.6 % | 0.4 / 259 / 1,214 ms | 4 (refused) | 772 / 716 | 0.86 / 0.70 | 1,350 MiB |
| D (`73ee79b411fdfa36`, control) | kernel default | 1,045,152 / 35,071,059 (3.0 %) | 2.3 % | 0.3 / 3.0 / 73 ms | 11 (2 DTLS, 9 refused) | 1,328 / 16 | 0.52 / 0.43 | 518 MiB |
| E (`73ee79b411fdfa36`) | 1 MiB requested, 2 MiB granted | 0 / 39,359,628 | 0 | 0.3 / 2.0 / 27 ms | 0 | 1,600 / 0 | 0.57 / 0.49 | 466 MiB |
| F (`73ee79b411fdfa36`) | 512 KiB requested, 1 MiB granted | 265 / 39,366,363 (0.0007 %) | 0 | 0.3 / 2.5 / 8 ms | 0 | 1,600 / 0 | 0.56 / 0.49 | 471 MiB |

The Linux default receive buffer (`net.core.rmem_default`, 208 KiB) holds about 14 ms of
the inbound media of 200 publishers at the plateau (about 120,000 datagrams a second into
the namespace, most of them for the primary worker, which owns all 400 producers). Those
single runs suggested that 1 MiB removes the loss, so the comparison was repeated three
times each, alternating the committed 1 MiB default and the kernel default on one image
(`9288838f3d086ce5`, ceilings raised to 2 MiB in the VM):

| Run | Buffer | Drops at worker 0's socket / inbound | Loss episodes | STUN unanswered | STUN answer p99 / max | Clients lost | Consumers validated / failed | Worker 0 / 1 (cores) | Peak RSS |
| --- | --- | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 1 MiB | 492,247 / 39,541,028 (1.2 %) | two, of 3 s and 5 s, after the ramp | 1.2 % | 20 / 231 ms | 0 | 1,593 / 7 | 0.63 / 0.53 | 580 MiB |
| 2 | 1 MiB | 6,841,156 / 24,885,617 (27 %) | sustained | 16.1 % | 132 / 496 ms | 17 (refused) | 657 / 461 | 0.72 / 0.53 | 794 MiB |
| 3 | 1 MiB | 0 / 39,399,615 | none | 0 | 2.8 / 8 ms | 0 | 1,600 / 0 | 0.65 / 0.55 | 467 MiB |
| 1 | kernel default | 482,223 / 38,229,117 (1.3 %) | 103, mostly one request, from 104 s on | 2.0 % | 2.7 / 60 ms | 5 (DTLS) | 1,522 / 22 | 0.68 / 0.56 | 548 MiB |
| 2 | kernel default | 7,433,127 / 22,204,221 (33 %) | sustained | 16.9 % | 21 / 501 ms | 34 (refused) | 628 / 162 | 0.42 / 0.32 | 716 MiB |
| 3 | kernel default | 8,835,175 / 35,882,670 (25 %) | sustained | 14.1 % | 15 / 175 ms | 2 (DTLS) | 634 / 942 | 0.81 / 0.63 | 1,192 MiB |

Medians and ranges: with 1 MiB, drops 1.2 % (0–27 %), clients lost 0 (0–17), failed
consumers 7 (0–461); with the kernel default, drops 25 % (1.3–33 %), clients lost 5 (2–34),
failed consumers 162 (22–942).

**This shape has two regimes, and the buffer matters in one of them.** In the normal
regime the worker keeps up on average and the kernel default sheds millisecond bursts
throughout the loaded phase: 103 loss episodes in kernel-default run 1, most of them a
single request, from 104 s into the ramp onward, and the worker's answers otherwise
arrive within 3 ms. Handshakes happen during the ramp, so those bursts are what cost
handshakes their final flight (5 in that run, 2 in run A, 2 in run D). **A 1 MiB buffer
absorbs them:** runs E and 3 dropped nothing at all, and run 1 lost only two episodes of
three and five seconds after the ramp, when every handshake was already done, so no
client was lost in any normal-regime 1 MiB run against clients lost in every
kernel-default run. In the overload regime, which this shape entered in one 1 MiB run and
two kernel-default runs (and in the 4 MiB run), worker 0 falls behind for tens of seconds
at the end of the ramp, when the last clients join and every publisher's bandwidth
estimate ramps: a quarter to a third of its inbound is dropped whatever the buffer, the
worker guard refuses joins, consumers fail, and the process grows to 0.7–1.2 GiB (traced
below to libwebrtc's transport feedback history). **The buffer is not a lever there;
room size is** (80 in the public template). A queue only adds delay to that regime: 1 MiB
run 2 answered at a 132 ms P99 where the kernel-default collapses answered within 21 ms,
and the 4 MiB run C queued for a quarter of a second at the P99 and up to 1.2 s, so the
buffer must stay far below what a sustained overload would fill. The default is now 1 MiB
(`WEBRTC_RECV_BUFFER_BYTES`, `WEBRTC_SEND_BUFFER_BYTES`). The kernel silently clamps a
request to `net.core.rmem_max`/`wmem_max` (208 KiB by default, not namespaced) and grants
twice the request, so the deployment raises the ceilings on the host
(`ops/ansible/tasks/host.yml`, [deployment](deployment.md)); the VM ran C–F with 8 MiB
ceilings and the six runs with 2 MiB. Run B carries a caveat: a native build ran on the
Mac during its ramp, and its worker also formatted every `info`-tagged debug message, so
it shows the mechanism and not a clean number. The loss episodes show no 15-second
period, so the quality sampler's statistics requests are not what stalls the worker.

**The client side: the generator never resends a lost final flight.** The two failed
handshakes of run A show the same pattern: the client's flight 5 (Certificate,
ClientKeyExchange, CertificateVerify, ChangeCipherSpec and Finished in one 659-byte
datagram) left the client once, the worker never logged `read client certificate`, and it
retransmitted its own flight thirteen times at 0.1, 0.2, 0.4 … 6.4 s and then every 4 s
until OpenSSL's retransmission budget ran out 37 s later (`DTLSv1_handle_timeout() failed`).
The client kept its ICE keepalives going for ten seconds and never resent its flight. That is
the generator's DTLS implementation (`rtc-dtls` 0.20.5, `handshaker.rs`): receiving any
handshake packet clears its retransmission timer, and the timer is re-armed only when it
sends a flight, so mediasoup's first retransmission 100 ms later cancels the client's 1 s
timer for good. A browser (BoringSSL, NSS) retransmits and would have recovered about a
second later, so the generator's ICE/DTLS timeouts under datagram loss overstate what
browsers see; the loss itself is what browsers would notice, as late or missing media. Five
other handshakes in run A had their first ClientHello unanswered and succeeded on the
client's repeat 0.8 s later, the same loss one flight earlier.

Evidence: `results/conference-capture-200.20260925T200440Z` (A, `handshakes.pcap`),
`results/conference-drops-200.20260925T202813Z` (B), `results/conference-buffers-200.20260925T203608Z` (C),
`results/conference-control-200.20260925T204148Z` (D), `results/conference-buffer1048576-200.20260925T204731Z` (E),
`results/conference-buffer524288-200.20260925T205335Z` (F), and the six repeated runs
`results/conference-default-200-1.20260925T213323Z`, `results/conference-default-200-2.20260925T214436Z`, `results/conference-default-200-3.20260925T215558Z` (1 MiB) and
`results/conference-control-200-1.20260925T213900Z`, `results/conference-control-200-2.20260925T215019Z`, `results/conference-control-200-3.20260925T220203Z` (kernel default);
each holds `netns-udp.txt` except A. Not measured: the VPS itself (its ceilings are the
Linux default until the host task runs) and browsers.

### Where the memory goes under overload (same day)

The overload runs above grew the process from about 470 MiB to 0.7–1.6 GiB and never
gave it back. A memory sampler in the benchmark (every five seconds: `smaps_rollup`, every
mapping of 2 MiB or more, and the cgroup's `memory.stat`) on a forced overload, 240
publishers on the same shape, shows what it is. The cgroup charges all of it as anonymous
memory (socket buffers stayed at 2 MiB, kernel memory at 6 MiB); it sits in about 25
glibc arena heaps filled to their 62 MiB cap; it grew by 1,067 MiB in the 65 s after the
ramp ended (17 MB/s, while the server sent about 155,000 datagrams a second) and then
stopped growing exactly, although the overload continued for another minute (28 % of the
primary worker's inbound dropped over the run).

That signature is libwebrtc's transport feedback history. Each receive transport's
`TransportFeedbackAdapter` keeps one entry of about 150 bytes per sent packet until the
viewer's transport-cc feedback covers it or the entry is 60 s old
(`kSendTimeHistoryWindowMs`). Feedback is inbound RTCP, so an overloaded worker that
drops inbound datagrams at its socket also drops the feedback, and every viewer transport
then holds a minute of sent packets: 155,000 datagrams a second times 150 bytes is about
23 MB/s, the order of the 17 MB/s observed, and the plateau arrives one window after the
loss started.
The memory is not leaked (the entries age out) but the process never shrinks, because
glibc keeps the arenas. The maintained worker now uses a 10 s window
(`vendor/README.md`): feedback older than that is useless to the estimator, and the
worst case per transport falls from a minute to ten seconds of its sending rate.

With the 10 s window the same overload (240 publishers, 23 % of the primary worker's
inbound dropped, worker 0 at 0.80 cores) grew the process from 551 MiB at the end of the
ramp to a 716 MiB peak: 165 MiB against 1,067 MiB, in line with ten seconds of the sending
rate at 150 bytes a packet. Delivery under that overload was as poor as before (288 of
1,920 consumers validated); the window changes what an overload costs in memory, not
whether it happens. The two runs are `results/conference-overload-240.20260925T231737Z`
(60 s window, server `9288838f3d086ce5`, generator `f27c7c3a4ef3632f`) and
`results/conference-overload-240-window10.20260925T234601Z` (10 s window, server `73928f3865382231`, generator `3fd66d9c7dd822ed`, rebuilt
from the same source after an image prune).

## A single all-publishing room under viewer spreading — 2026-09-25

The spreading change moves receive transports, not producers, so a room where everyone
publishes keeps every producer on its primary worker. These runs put one room of N
all-publishing clients (camera and microphone each, the balanced `ring-v1` seed 17 graph,
four audio and four video subscriptions per client, distinct loopback addresses) on the
production shape in the Podman VM, two workers, 120-second measurement, and compare the
deployed server (`86382b1`, image `bf5fd1a09f15ce84`) with the last image in the local
store from before the change (built 2026-09-21).

| Clients | Server | Runs | ICE/DTLS failures (clients) | Consumers validated / failed | Receive-ready P99 | Worker 0 / worker 1 (cores) |
| ---: | --- | ---: | ---: | ---: | ---: | ---: |
| 100 | spreading | 1 | 0 | 800 / 0 | 419 ms | 0.29 / 0.25 |
| 200 | spreading | 3 | 1, 5, 2 | 1,584 / 8; 1,516 / 28; 1,560 / 20 | 423, 422, 425 ms | 0.53 / 0.47 (third run) |
| 200 | 2026-09-21 image | 1 | 13 | 1,392 / 70 | 1,210 ms | 0.64 / 0.00 |

**At 100 publishers the room splits almost evenly** (producers keep the primary worker
0.04 cores busier) and every consumer delivers. **At 200 publishers the shape is beyond
the reliable capacity of this quota with or without spreading:** on the older server 13 of
200 clients never completed ICE/DTLS on either of their transports and delivery stretched
to a 1.2 s P99 with the whole room on one worker; with spreading the same shape loses one
to five clients per run, delivery for everyone else stays at a 420 ms P99, and the workers
share the load. The failing clients' transports were on the primary worker in every
spreading run, with no pipe or placement error logged, and the per-worker gauges showed
neither worker saturated (0.53 and 0.47 cores), so the residual failures are the
pre-existing ICE/DTLS handshake fragility of a busy worker with 400 producers, not a fault
of the pipes; the section above locates it: the kernel drops inbound datagrams at the
worker's default socket buffer, and the generator never resends a lost final flight. A 60-second ramp
and a 200-second ramp gave the same picture. On the VPS's slower cores the equivalent
all-publishing room is smaller by the 2.5 factor measured above, so **rooms of about 80
publishers are the comparable limit there**, and `max_participants` is the knob to hold a
public room under it.

Evidence: `results/conference-spread.20260925T094308Z` (100), `results/conference-spread-200.20260925T095058Z` (200, first two runs), `results/conference-spread-200b.20260925T095953Z` (200, third run, with
the mid-window `metrics-during.txt` scrape that records the split) and `results/conference-baseline-200.20260925T100703Z` (baseline).
Not measured: the conference shape on the VPS itself, and browser clients, whose ICE
agents differ from the generator's.

## One-to-many rooms on the VPS's own cores — 2026-09-25

Every earlier figure for the production shape came from Apple silicon scaled by a
calibration factor measured on a hosted CI runner. These two runs measured the deployed
server on the VPS itself (research.clinic host: AMD EPYC 7R13, 4 vCPUs, 12 GiB): the exact
deployed production image (`86382b1`, image `de5ed1ee…`) ran as an owned loopback-only
container under the production quota (2 CPUs, 2 GiB, two media workers, `--network none`,
`BIND_ADDR`/`ANNOUNCE_IP` 127.0.0.1, ad-hoc rooms allowed, connection limits raised) and the
CI-built Linux load-test image (`809b022`, workflow `load-generator-image.yml`) shared its
network namespace, exactly as the Podman runs do; the public app kept serving beside them
and nothing touched it. One publisher (480p30 plus Opus), N viewers from distinct loopback
addresses, 100–120 s ramps, 10 s warmup, a 120 s measurement, one run per size. A sampler
read the server's cgroup `cpu.stat`, RSS and per-thread CPU every five seconds.

| Viewers | Consumers validated / failed | Receive-ready P50 / P99 | Server, share of the 2-CPU quota | Worker 0 / worker 1 (cores) | Throttled periods | Peak RSS |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 250 | 498 / 0 | 414 / 454 ms | 37.8% | 0.38 / 0.38 | 0 | 219 MiB |
| 350 | 698 / 0 | 414 / 470 ms | 53.1% | 0.52 / 0.52 | 0 | 296 MiB |

**Per viewer the VPS costs 3.0 millicores of worker CPU, against 1.2 on the Apple-silicon
VM (0.29 cores for 250 viewers there):** these cores are about 2.5 times slower per viewer
than the Mac's, not the 1.92 the CI-runner calibration suggested, and the cost is linear
between the two sizes. Receive readiness stayed within 60 ms of the Mac's. The room split
evenly across both workers, as in the VM. Projected from these points, a one-to-many room
on the VPS reaches about 600 viewers at 90% of the quota, so **roughly 500 viewers is the
practical ceiling of one room on the deployed shape**, and `MAX_CONNECTIONS=200` keeps a
wide margin for it; memory is not a factor at these sizes (0.85 MiB per viewer).

Not measured: the public network path (TURN, real ICE candidates, packet loss), browser
decode, and the conference shape on these cores. The runs shared the host with the live
app, whose own load was under 3% of a core, so contention from the generator (up to 2.2
cores) rather than the server could only have hurt the generator's own timings.

Evidence: `results/vps-webinar.20260925T095312Z` (`r250/`, `r350/`: generator reports, `samples.txt` from the
sampler, `metrics-finish.txt`, server and generator logs) and the script that ran them,
`vps-webinar.sh`, in the same directory.

## One-to-many rooms on one worker — 2026-09-25

The webinar shape from the project goals (one presenter, many viewers) had never been
measured. These runs put the production image (server source tree `8ebbda9fd24dc80c`,
generator `f27c7c3a4ef3632f`, both built from `98dddf6`) under the production shape in the
Podman Linux VM on this Mac (2-CPU quota, 2 GiB, two media workers, Apple silicon cores)
and drove one room with one publisher (480p30 video plus Opus audio) and N viewers, each
viewer joining from its own loopback address (`--scenarios webinar`,
`--source-addresses 250`), with a 250-second ramp, 10 seconds of warmup and a 120-second
measurement; 100 viewers ran once more with a 30-second ramp. One run per size.

| Viewers | Consumers validated / failed | Receive-ready P99 | Received packets/s | Server, share of the 2-CPU quota | Room's worker (cores) | Other worker | Peak RSS |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 100 | 198 / 0 | 414 ms | 17,127 | 10.1% | 0.20 | 0.00 | 110 MiB |
| 250 | 498 / 0 | 417 ms | 43,075 | 14.9% | 0.29 | 0.00 | 227 MiB |
| 500 | 998 / 0 | 609 ms | 86,094 | 30.7% | 0.60 | 0.00 | 446 MiB |
| 750 | 1,302 / 196 | 4,693 ms | failed | 42.9% | 0.84 | 0.00 | 1,565 MiB |

**A room lives on one worker, so a one-to-many room scales with one core.** Every run
put the whole room on worker 0 while worker 1 stayed idle and the quota kept more than a
core free. At 500 viewers the worker thread used 0.60 cores and receive readiness already
stretched (P99 609 ms against 414 at 100 and 250). At 750 the thread reached 0.84 cores
and delivery broke: 196 of 1,498 consumers failed sustained delivery, 101 clients had no
validated consumer, receive-ready P99 was 4.7 s with outliers to 28 s, and the server
logged ICE consent expiring on viewer transports while no cgroup period was throttled.
The generator used 1.8 of its 5.5 CPUs, so it was not the limit. Per viewer the marginal
worker cost was about 0.6 millicores (0.20 cores at 100, 0.60 at 500), and memory grew by
about 0.85 MiB per viewer up to 500 and then jumped (queued RTP for stalled viewers).

**Ceiling on this shape:** between 500 and 750 viewers per room on one Apple-silicon
core; with the 1.92 EPYC calibration from the 2026-09-20 section, roughly 250–350 viewers
per room on the VPS, although its own cores remain unmeasured.

### The same ladder with viewers spread across both workers (same day)

The server now places a room's receive transports by worker load once the room's primary
worker carries 64 consumers, creating one viewer router per other worker and piping each
producer to it once (`RouterManager::place_viewer` / `ensure_piped`; producers and the audio
observers stay on the primary router). Server source tree `bf5fd1a09f15ce84` (the placement change
on top of `98dddf6`), generator `f27c7c3a4ef3632f`, same shape and workload:

| Viewers | Consumers validated / failed | Receive-ready P99 (P50) | Received packets/s | Server, share of the 2-CPU quota | Worker 0 / worker 1 (cores) | Peak RSS |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 500 | 998 / 0 | 420 ms (408) | 86,327 | 30.3% | 0.30 / 0.30 | 422 MiB |
| 750 | 1,498 / 0 | 439 ms (408) | 129,452 | 46.3% | 0.47 / 0.47 | 628 MiB |
| 1,000 | 1,998 / 0 | 1,207 ms (409) | 172,178 | 66.2% | 0.64 / 0.64 | 881 MiB |

**All three passed with the room split evenly across both workers.** 750 viewers, which
failed on one worker, delivered every consumer with receive-ready P99 439 ms; 1,000
viewers delivered every consumer at 66% of the quota with P99 1.2 s, the first sign of the
next ceiling (the P50 stayed at 409 ms). The pipes cost nothing measurable: 500 viewers
used 0.596 worker cores split over two threads against 0.602 on one. Memory per viewer
stayed at about 0.85 MiB. The ceiling of a one-to-many room on this shape moved from one
core to the whole quota, so the projected VPS figure is roughly 500–700 viewers per room
instead of 250–350, pending a measurement on its cores.

**Where the spread room fails next (same day, `results/webinar-ceiling.20260925T093657Z`).**
A 1,250-viewer attempt (300-second ramp, `--max-connections 1700`) never reached its
measurement: the server was OOM-killed at the 2 GiB limit 243 s into the ramp, at about
970 connections. RSS had grown at the steady 0.85 MiB per viewer to 695 MiB by 750
viewers, then jumped to 970, 1,509 and 2,038 MiB in the next 45 s while the two worker
threads together reached 1.7 cores (three throttled periods) and admissions kept
arriving at four per second. Late joins are dearer than steady forwarding (transport and
consumer creation on already busy workers), so the workers fell behind, outbound RTP
queued in the process, and memory rather than the CPU quota ended the run. The practical
ceiling of the shape is therefore about 1,000 viewers per room, and the failure mode
matters more than the number: the process has CPU-saturation admission control but no
memory-pressure guard, so an over-admitted room takes the whole server down instead of
being refused.

**A memory-pressure guard alone does not save it (same day, `results/webinar-memory-guard.20260925T191900Z`).**
The same ramp against a server with `MEMORY_SATURATION_FRACTION=0.85` (refuse joins and fail
readiness at 85 % of the cgroup limit) died the same way. The guard was active (the limit was
readable, usage 1 % at start), joins arrived at 4.1 per second, RSS was 73 % of the limit at
211 s, and the last log line is from 210 s: the process stopped logging, sampling and
answering as cgroup memory (which also charges socket buffers) hit the limit, and it was
killed at about 228 s without ever recording the transition. The growth from 73 % to the
limit took under ten seconds, faster than a two-second sampler with hysteresis can act, and
the per-worker CPU guard (0.85 of a core over ten seconds) had not fired either. The guard
stays for slow growth; the protection for this shape is admission on the leading signal
(worker CPU) and the room and connection limits.

**The worker CPU guard at 0.7 does save it (same day, `results/webinar-worker-guard.20260925T192500Z`).**
The same ramp with `CPU_SATURATION_WORKER_UTILIZATION=0.7` ended with the server alive:
both workers crossed the threshold at about 180 s and 728 viewers, the process logged
"every media worker is saturated: refusing new joins and reporting not ready", 522 later
joins were refused with the retry message, memory plateaued at 669 MiB (33 % of the limit),
the workers settled at 0.75 and 0.73 cores, and the admitted viewers kept receiving: four
consumers failed out of the room's 1,456 and receive-ready P99 stayed at 459 ms through the
measurement window. 0.7 is now the default; the earlier 0.85 left no time between the
threshold and the stall.

Evidence: `results/webinar-100v.20260925T070339Z`, `results/webinar-ladder.20260925T070800Z`
(single worker; the 750 row from its `webinar-750-1/load_test_summary.json` and
`resources.json`, and the run stopped there) and `results/webinar-spread.20260925T073333Z`
(spread). Loopback carries no real network cost, the generator's fixed-rate synthetic RTP
is lighter than browser traffic, and nothing here measures browser decode or visual
quality.

## Outgoing bitrate floor: 100 kbit/s against mediasoup's own — 2026-09-21

Three impaired-job runs each at commit `3f43600` on hosted Linux runners, the
default `WEBRTC_MIN_OUTGOING_BITRATE` (100 000, runs 35566649914,
35566654915, 35566659954) against `0`, which leaves mediasoup's 30 kbit/s
floor (runs 35566690792, 35566695380, 35566700001). Medians with ranges of
the browser scenario's phases; one run in each group failed the
"constrained" assertion only because the layer was already at 1 when that
phase began (the scenario now counts the entering layer), so its other phases
are included.

| phase, measure | floor 100 kbit/s | floor 0 (mediasoup default) |
|---|---|---|
| lossy, frames/s | 17.4 [10.2–19.0] | 18.0 [14.4–19.1] |
| 400 kbit/s, seconds to layer ≤ 1 | 0.7 [0.7–0.7] (n=2) | 0.8 [0.7–0.9] (n=2) |
| 150 kbit/s, seconds to layer 0 | 2.9 [2.8–4.4] | 4.3 [2.9–4.3] |
| 150 kbit/s, frames/s | 12.5 [11.3–12.5] | 8.9 [5.8–10.2] |
| 150 kbit/s, freeze seconds | 0.9 [0.6–1.0] | 2.0 [0.9–2.2] |
| recovery, seconds to the top layer | 16.6 [9.9–17.2] | 9.8 [3.7–11.9] |
| recovery, freeze seconds | 2.6 [0.4–2.8] | 0.2 [0.0–4.3] |
| join under loss, seconds to first frame | 2.2 [2.0–3.4] | 2.0 [2.0–2.2] |

At the 150 kbit/s cap the floor keeps the lowest layer flowing (frame rate
and freezes do not overlap between the groups); recovery to the top layer
looks slower with the floor, but the ranges touch and three runs cannot
settle it. The floor stays at 100 kbit/s. Its cost was the worker's
`ClampConstraints` error line on every bitrate update of a transport whose
estimate sat at 30 kbit/s (941 lines in the 100-client single-room run
above); the vendored worker now bounds its start bitrate by the configured
floor (`vendor/README.md`), which removes the line without changing
behaviour: the same run with the patched worker
(`results/capacity-100c-1room-2cpu.20260921T064121Z`) logged 0 lines at
recv-ready p99 426 ms and worker 0 at 0.354 of a core, against 418–422 ms
and 0.309–0.339 in the two runs before it.

## One room on one worker, and the per-worker gauges — 2026-09-21

A mediasoup router lives on one worker thread. Two runs of the same
single-room workload on the production shape (Podman VM, `--cpus 2
--memory 2g`, two workers, 100 clients in one room, ring plan `ring-v1` seed
17, 800 consumers, 605 s ramp, 120 s window) show what that means:

| run | source | worker 0 | worker 1 | mediasoup threads | of quota | recv-ready p99 |
|---|---|---|---|---|---|---|
| `results/capacity-100c-1room-2cpu.20260921T050407Z` | 973636d, images from an earlier build | 33.9 % of a core (per-thread `/proc` samples, 112 s) | 0.0 % | 41.5 CPU-s / 119.6 s | 17.7 % | 422 ms |
| `results/capacity-100c-1room-2cpu.20260921T054621Z` | a06e843 | 0.309 of a core (`simplestchat_media_worker_cpu{worker="0"}`) | 0 | 38.9 CPU-s / 119.6 s | 16.6 % | 418 ms |

The whole room runs on one thread while the other worker idles; the cgroup
quota shows headroom throughout, so the cgroup monitor could never have
refused a join for it. The second run is the first with the per-worker
monitor (`a06e843`): its gauges reproduce the per-thread sampling of the
first run from inside the server, `simplestchat_media_worker_saturated` stayed
0 for both workers (0.31 is below the 0.85 threshold), and
`simplestchat_consumer_layer_requests_total` finished at 0: with the viewer
ceiling and the bandwidth tier merged server-side, the generator's clients
(which never send a ceiling and whose estimates stay above the top tier)
caused no layer requests at all, where the previous code wrote the top layer
to every consumer on each participant's first bandwidth event (8 per client,
about 800 per run) and repeated it for audio consumers on every tier change.
The 4-room ladder run of 2026-09-20 with the same 800 consumers and packet
rate used 74.5 CPU-s on the worker threads on older images, so the two are
not a controlled per-consumer comparison; these runs establish placement and
the new counters only. Synthetic RTP forwarding; no browser decode.

## Congestion-controller field trials, single runs — 2026-09-21

`LIBWEBRTC_FIELD_TRIALS` now reaches the media workers, and the weekly
workflow's impaired job accepts a candidate string, so five variants ran
once each on hosted Linux runners at commit `e020004` (runs 35557553288,
35557557261, 35557561516, 35557565768 and 35557569332). Every variant keeps
mediasoup's default `WebRTC-Bwe-AlrLimitedBackoff/Enabled/`. The rows are
the browser scenario's phases; the run status was a scenario bug (the new
silent-microphone phase ran without a silent capture) and does not affect
these numbers.

| Variant | 5 % loss fps | 400 kbit/s: seconds to layer 1 | 150 kbit/s: seconds to layer 0 / freeze s | Recovery: seconds to top / freeze s |
| --- | ---: | ---: | ---: | ---: |
| Control (default) | 15.9 | 11.0 | 3.1 / 0.57 | 11.9 / 3.59 |
| AdaptiveBweThreshold 0.05,0.005 | 13.6 | 17.9 | 4.1 / 1.46 | 16.7 / 3.79 |
| BweRapidRecoveryExperiment | 14.7 | 2.8 | 2.9 / 1.50 | 15.1 / 4.84 |
| BweBackOffFactor 0.92 | 16.0 | 3.1 | 4.9 / 1.01 | 16.8 / 2.00 |
| All three combined | 14.1 | 1.9 | 4.6 / 0.59 | 7.5 / 6.94 |

**No variant wins on single runs.** The control's own downgrade time ranged
from 0.8 to 11 s across the day's runs, which is larger than most
between-variant differences here; the combined variant recovered to the top
layer fastest (7.5 s) but froze longest doing so (6.9 s), and the adaptive
threshold made everything slower. Every viewer stayed at layer 1 under
5 % loss with jitter in every variant, so none of these trials changes the
half-resolution-under-jitter behaviour. Production keeps mediasoup's default.
The next step, if this is pursued, is three runs per variant on the same day
and a decision on which metric matters more, recovery time or freeze time.

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
